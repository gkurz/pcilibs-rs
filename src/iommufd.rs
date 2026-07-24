// Copyright (c) Kata Containers contributors
//
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};

/// Root of the IOMMUFD character device tree; cdevs live at
/// `<IOMMUFD_VFIO_DIR>/devices/vfioN`.  A kernel contract, not configuration.
pub const IOMMUFD_VFIO_DIR: &str = "/dev/vfio";

/// Sysfs class for VFIO character devices; `<IOMMUFD_SYSFS_CLASS>/vfioN/device`
/// links to the PCI function that backs each cdev.  A kernel contract.
pub const IOMMUFD_SYSFS_CLASS: &str = "/sys/class/vfio-dev";

/// One IOMMUFD character device: the file at `<vfio_dir>/devices/vfioN`,
/// together with the PCI identity read from sysfs.
#[derive(Clone, Debug)]
pub struct IommufdDev {
    /// Kernel-assigned index: the `N` in `/dev/vfio/devices/vfioN`.
    pub num: u32,
    /// Absolute path to the character device.
    pub path: PathBuf,
    /// PCI vendor ID (e.g. `0x10de` for NVIDIA).
    pub vendor: u16,
    /// PCI device ID (e.g. `0x2330` for an H100 SXM5 80GB).
    pub device: u16,
    /// Full 24-bit PCI class code (class byte | subclass byte | prog-if byte).
    pub class: u32,
}

impl IommufdDev {
    /// Base class and subclass — the upper 16 bits of the 24-bit class code,
    /// e.g. `0x0302` for a 3D controller.  This is the granularity device
    /// classification usually wants; the low byte (prog-if) varies by
    /// programming interface, not device kind.
    pub fn class_prefix(&self) -> u16 {
        (self.class >> 8) as u16
    }

    /// The device's name in the PCI ID database (compile-time `pci-ids`
    /// crate), e.g. "GH100 [H100 SXM5 80GB]" for 10de:2330.  None if the
    /// database does not know the (vendor, device) pair.
    pub fn device_name(&self) -> Option<&'static str> {
        pci_ids::Device::from_vid_pid(self.vendor, self.device).map(|d| d.name())
    }
}

/// Enumerate all IOMMUFD character devices under `<vfio_dir>/devices/` and
/// resolve their PCI identity from `sysfs_dir`.  Entries whose sysfs files
/// are absent or unparseable are silently skipped.  Result is sorted by
/// device number.
pub fn enumerate_iommufd(vfio_dir: &Path, sysfs_dir: &Path) -> Vec<IommufdDev> {
    let devices_dir = vfio_dir.join("devices");
    let Ok(rd) = std::fs::read_dir(&devices_dir) else {
        return vec![];
    };
    let mut devs: Vec<IommufdDev> = rd
        .flatten()
        .filter_map(|e| {
            let num = e
                .file_name()
                .to_str()?
                .strip_prefix("vfio")?
                .parse::<u32>()
                .ok()?;
            let device = sysfs_dir.join(format!("vfio{num}")).join("device");
            let read = |f: &str| std::fs::read_to_string(device.join(f)).unwrap_or_default();
            let vendor =
                u16::from_str_radix(read("vendor").trim().trim_start_matches("0x"), 16).ok()?;
            let dev_id =
                u16::from_str_radix(read("device").trim().trim_start_matches("0x"), 16).ok()?;
            let class =
                u32::from_str_radix(read("class").trim().trim_start_matches("0x"), 16).ok()?;
            Some(IommufdDev {
                num,
                path: devices_dir.join(format!("vfio{num}")),
                vendor,
                device: dev_id,
                class,
            })
        })
        .collect();
    devs.sort_by_key(|d| d.num);
    devs
}

/// Fake IOMMUFD node layout for tests, mirroring what `enumerate_iommufd`
/// reads, under one root:
///   `<root>/devices/vfio<n>`                       — the cdev entry
///   `<root>/sysfs/vfio<n>/device/{vendor,device,class}`   — fake sysfs
///
/// Enable with the `testfs` feature (dev-dependencies only — this writes
/// fake sysfs trees and belongs nowhere near production code).
#[cfg(any(test, feature = "testfs"))]
pub mod testfs {
    use std::path::{Path, PathBuf};

    /// The sysfs root to pass alongside `root` to `enumerate_iommufd`.
    pub fn sysfs(root: &Path) -> PathBuf {
        root.join("sysfs")
    }

    /// Add one fake cdev `vfio<n>` with the given sysfs `vendor`, `device`,
    /// and `class` contents (as sysfs prints them, e.g. "0x10de", "0x2330",
    /// "0x030200").
    pub fn add(root: &Path, n: u32, vendor: &str, device: &str, class: &str) {
        let devices = root.join("devices");
        std::fs::create_dir_all(&devices).unwrap();
        std::fs::write(devices.join(format!("vfio{n}")), b"").unwrap();
        let dev_dir = sysfs(root).join(format!("vfio{n}")).join("device");
        std::fs::create_dir_all(&dev_dir).unwrap();
        std::fs::write(dev_dir.join("vendor"), format!("{vendor}\n")).unwrap();
        std::fs::write(dev_dir.join("device"), format!("{device}\n")).unwrap();
        std::fs::write(dev_dir.join("class"), format!("{class}\n")).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::testfs::add;
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn enumerates_and_sorts_by_num() {
        let root = TempDir::new().unwrap();
        add(root.path(), 42, "0x10de", "0x2330", "0x030200");
        add(root.path(), 7, "0x10de", "0x22a3", "0x068000");
        add(root.path(), 3, "0x15b3", "0x101e", "0x020000");

        let sysfs = root.path().join("sysfs");
        let devs = enumerate_iommufd(root.path(), &sysfs);
        assert_eq!(
            devs.iter().map(|d| d.num).collect::<Vec<_>>(),
            vec![3, 7, 42]
        );
        assert_eq!(devs[1].vendor, 0x10de);
        assert_eq!(devs[1].device, 0x22a3);
        assert_eq!(devs[1].class, 0x068000);
        assert!(devs[2].path.ends_with("devices/vfio42"));
    }

    #[test]
    fn device_name_resolves_from_pci_ids() {
        let root = TempDir::new().unwrap();
        add(root.path(), 0, "0x10de", "0x2330", "0x030200");
        // A device id the database cannot know.
        add(root.path(), 1, "0x10de", "0xdead", "0x030200");

        let devs = enumerate_iommufd(root.path(), &root.path().join("sysfs"));
        let h100 = devs[0].device_name().expect("10de:2330 must be known");
        assert!(h100.contains("GH100"), "unexpected name: {}", h100);
        assert_eq!(devs[1].device_name(), None);
    }

    #[test]
    fn missing_sysfs_entry_skipped() {
        let root = TempDir::new().unwrap();
        let devices = root.path().join("devices");
        fs::create_dir_all(&devices).unwrap();
        fs::write(devices.join("vfio0"), b"").unwrap();
        // no sysfs entry — filter_map returns None

        assert!(enumerate_iommufd(root.path(), &root.path().join("sysfs")).is_empty());
    }

    #[test]
    fn missing_devices_dir_returns_empty() {
        let root = TempDir::new().unwrap();
        assert!(enumerate_iommufd(root.path(), &root.path().join("sysfs")).is_empty());
    }
}
