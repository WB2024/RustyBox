//! USB sticks: finding the ones that are safe to touch, formatting them as FAT32, and building a
//! Bad Avatar stick. Nothing here runs on a disk that isn't a removable, unmounted-from-the-system
//! USB device from the detection list.

pub mod backup;
pub mod badavatar;
pub mod payload;

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde::Serialize;
use serde_json::Value;

use crate::error::Error;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Device {
    /// `/dev/sdd` (or a mock name).
    pub path: String,
    pub size: u64,
    pub model: String,
    pub vendor: String,
    pub label: Option<String>,
    pub fstype: Option<String>,
    /// Where the disk or its partitions are mounted.
    pub mountpoints: Vec<String>,
    /// The disk or partition that holds the filesystem (what a backup reads).
    pub fs_path: Option<String>,
    /// Safe to format and write to.
    pub eligible: bool,
    /// Why not, in words.
    pub why_not: Option<String>,
}

const MAX_STICK: u64 = 1024 * 1024 * 1024 * 1024; // 1 TB

/// Mount points of the running system, which a device must never hold.
fn is_system_mount(m: &str) -> bool {
    m == "/"
        || [
            "/boot", "/home", "/usr", "/var", "/etc", "/opt", "/srv", "/nix", "/snap",
        ]
        .iter()
        .any(|p| m == *p || m.starts_with(&format!("{p}/")))
}

/// Pick the removable disks out of `lsblk -J` output. `protect` are folders (RustyBox's own
/// config) that must not be on the disk.
pub fn parse_lsblk(json: &str, protect: &[PathBuf]) -> Result<Vec<Device>, Error> {
    let v: Value = serde_json::from_str(json)
        .map_err(|e| Error::backend(format!("lsblk gave something unexpected: {e}")))?;
    let mut out = Vec::new();
    for d in v["blockdevices"].as_array().into_iter().flatten() {
        if d["type"] != "disk" {
            continue;
        }
        let tran = d["tran"].as_str().unwrap_or("");
        let removable = d["rm"].as_bool().unwrap_or(false) || d["rm"] == "1";
        let hotplug = d["hotplug"].as_bool().unwrap_or(false) || d["hotplug"] == "1";
        let size = d["size"]
            .as_u64()
            .or_else(|| d["size"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);
        if size == 0 {
            continue; // an empty card reader slot
        }
        let mut mounts: Vec<String> = Vec::new();
        let mut fstype = d["fstype"].as_str().map(String::from);
        let mut fs_path = fstype
            .as_ref()
            .map(|_| d["path"].as_str().unwrap_or("").to_string());
        let mut label = d["label"].as_str().map(String::from);
        let mut collect = |n: &Value| {
            for m in n["mountpoints"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m.as_str())
            {
                mounts.push(m.to_string());
            }
        };
        collect(d);
        for c in d["children"].as_array().into_iter().flatten() {
            collect(c);
            if fs_path.is_none() && c["fstype"].as_str().is_some() {
                fs_path = c["path"].as_str().map(String::from);
            }
            fstype = fstype.or_else(|| c["fstype"].as_str().map(String::from));
            label = label.or_else(|| c["label"].as_str().map(String::from));
        }
        let mut why = None;
        if !(removable || (hotplug && tran == "usb")) {
            why = Some("Not a removable drive (it may be a hard disk or SSD)".to_string());
        } else if tran != "usb" && !removable {
            why = Some("Not connected by USB".to_string());
        } else if size > MAX_STICK {
            why = Some("Bigger than 1 TB: that isn't a USB stick, so it is left alone".to_string());
        } else if mounts.iter().any(|m| is_system_mount(m)) {
            why = Some("It holds part of the running system".to_string());
        } else if mounts
            .iter()
            .any(|m| protect.iter().any(|p| p.starts_with(m) && m != "/"))
        {
            why = Some("RustyBox's own data is on it".to_string());
        }
        out.push(Device {
            path: d["path"].as_str().unwrap_or("").to_string(),
            size,
            model: d["model"].as_str().unwrap_or("").trim().to_string(),
            vendor: d["vendor"].as_str().unwrap_or("").trim().to_string(),
            label,
            fstype,
            mountpoints: mounts,
            fs_path,
            eligible: why.is_none(),
            why_not: why,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The removable disks the system sees now.
pub fn detect(protect: &[PathBuf]) -> Result<Vec<Device>, Error> {
    let out = Command::new("lsblk")
        .args(["-J", "-b", "-o", "NAME,PATH,SIZE,TYPE,RM,HOTPLUG,TRAN,FSTYPE,LABEL,MOUNTPOINTS,MODEL,VENDOR"])
        .output()
        .map_err(|e| Error::coded(501, "NO_LSBLK", format!("Can't list drives here ({e}). USB tools need the container to see the host's drives (privileged, with /dev access).")))?;
    if !out.status.success() {
        return Err(Error::backend(format!(
            "lsblk failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    parse_lsblk(&String::from_utf8_lossy(&out.stdout), protect)
}

/// A FAT32 volume label: up to 11 capital letters, digits, dashes and underscores.
pub fn valid_label(label: &str) -> Result<String, Error> {
    let l = label.trim().to_uppercase();
    if l.is_empty()
        || l.len() > 11
        || !l
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::validation(
            "A drive name is 1 to 11 letters, digits, dashes or underscores",
        ));
    }
    Ok(l)
}

/// Format a disk (or an image file, for tests) as one FAT32 volume with no partition table, the
/// way the Xbox 360's exploit expects a Bad Update stick.
pub fn mkfs_fat32(target: &Path, label: &str) -> Result<(), Error> {
    let label = valid_label(label)?;
    let tool = if which("mkfs.vfat") {
        "mkfs.vfat"
    } else {
        "mkfs.fat"
    };
    let out = Command::new(tool)
        .args(["-F", "32", "-I", "-n", &label])
        .arg(target)
        .output()
        .map_err(|e| Error::coded(501, "NO_MKFS", format!("mkfs.vfat isn't installed ({e})")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let hint = if err.contains("ermission") || err.contains("denied") {
            " Formatting needs root: run RustyBox in a privileged container with the USB device passed through."
        } else {
            ""
        };
        return Err(Error::backend(format!(
            "Formatting failed: {}.{hint}",
            err.trim()
        )));
    }
    Ok(())
}

pub(crate) fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
        || Path::new("/usr/sbin").join(name).is_file()
        || Path::new("/sbin").join(name).is_file()
}

/// Unmount everything of a device that is mounted (so it can be formatted).
pub fn unmount_all(d: &Device) -> Result<(), Error> {
    for m in &d.mountpoints {
        let r = Command::new("umount").arg(m).output();
        let ok = matches!(&r, Ok(o) if o.status.success())
            || Command::new("udisksctl")
                .args(["unmount", "--block-device"])
                .arg(&d.path)
                .output()
                .is_ok_and(|o| o.status.success());
        if !ok {
            return Err(Error::backend(format!(
                "Couldn't unmount {m}. Close anything using it and try again."
            )));
        }
    }
    Ok(())
}

/// Mount a freshly formatted device and return where. Uses `udisksctl` on a desktop; inside a
/// container (running as root, with no desktop services) it uses plain `mount`.
pub fn mount(d: &Device) -> Result<PathBuf, Error> {
    if let Ok(out) = Command::new("udisksctl")
        .args(["mount", "--block-device"])
        .arg(&d.path)
        .output()
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if let Some((_, p)) = text.rsplit_once(" at ") {
            return Ok(PathBuf::from(p.trim().trim_end_matches('.')));
        }
    }
    let name = d.path.rsplit('/').next().unwrap_or("usb");
    let dir = PathBuf::from("/media/rustybox").join(name);
    std::fs::create_dir_all(&dir).map_err(|e| Error::backend(format!("Can't make a place to mount the stick ({e}). Mount it yourself and build onto its folder.")))?;
    let out = Command::new("mount")
        .args(["-t", "vfat", "-o", "umask=000"])
        .arg(&d.path)
        .arg(&dir)
        .output()
        .map_err(|e| Error::backend(format!("Can't mount the stick here ({e}).")))?;
    if !out.status.success() {
        return Err(Error::backend(format!(
            "Couldn't mount the stick: {}. Mounting needs root.",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(dir)
}

/// Sample sticks for `--mock`: two folders standing in for mounted devices.
pub fn mock_devices(config_dir: &Path) -> Vec<Device> {
    let root = config_dir.join("mock-usb");
    let mk = |n: &str, vendor: &str, model: &str, size: u64, fs: &str| {
        let dir = root.join(n);
        let _ = std::fs::create_dir_all(&dir);
        // A small FAT32 image stands in for the stick's filesystem, so backups really run.
        let img = root.join(format!("{n}.img"));
        if !img.exists()
            && (which("mkfs.vfat") || which("mkfs.fat"))
            && std::fs::File::create(&img)
                .and_then(|f| f.set_len(64 * 1024 * 1024))
                .is_ok()
            && mkfs_fat32(&img, "MOCKSTICK").is_err()
        {
            let _ = std::fs::remove_file(&img);
        }
        Device {
            path: format!("/mock/{n}"),
            size,
            model: model.into(),
            vendor: vendor.into(),
            label: Some("MOCKSTICK".into()),
            fstype: Some(fs.into()),
            mountpoints: vec![dir.to_string_lossy().to_string()],
            fs_path: img.exists().then(|| img.to_string_lossy().to_string()),
            eligible: true,
            why_not: None,
        }
    };
    vec![
        mk(
            "stick1",
            "SanDisk",
            "Cruzer Blade (pretend)",
            32_000_000_000,
            "vfat",
        ),
        mk(
            "stick2",
            "Kingston",
            "DataTraveler (pretend)",
            16_000_000_000,
            "vfat",
        ),
        Device {
            path: "/mock/disk".into(),
            size: 2_000_000_000_000,
            model: "Pretend system disk".into(),
            vendor: "ACME".into(),
            label: None,
            fstype: Some("ext4".into()),
            mountpoints: vec!["/".into()],
            fs_path: None,
            eligible: false,
            why_not: Some("It holds part of the running system".into()),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSBLK: &str = r#"{"blockdevices":[
      {"name":"sda","path":"/dev/sda","size":0,"type":"disk","rm":true,"hotplug":true,"tran":"usb","mountpoints":[null]},
      {"name":"sdb","path":"/dev/sdb","size":30784094208,"type":"disk","rm":true,"hotplug":true,"tran":"usb","model":"Cruzer Blade","vendor":"SanDisk ","fstype":null,"label":null,"mountpoints":[null],
        "children":[{"name":"sdb1","path":"/dev/sdb1","size":30783045632,"type":"part","rm":true,"fstype":"vfat","label":"STICK","mountpoints":["/media/will/STICK"]}]},
      {"name":"sdc","path":"/dev/sdc","size":2000398934016,"type":"disk","rm":false,"hotplug":true,"tran":"usb","model":"Xbox HDD","fstype":"vfat","mountpoints":[null]},
      {"name":"nvme0n1","path":"/dev/nvme0n1","size":1000204886016,"type":"disk","rm":false,"hotplug":false,"tran":"nvme","mountpoints":[null],
        "children":[{"name":"p5","path":"/dev/nvme0n1p5","type":"part","fstype":"ext4","mountpoints":["/"]}]},
      {"name":"sde","path":"/dev/sde","size":16000000000,"type":"disk","rm":true,"hotplug":true,"tran":"usb","mountpoints":[null],
        "children":[{"name":"sde1","path":"/dev/sde1","type":"part","fstype":"ext4","mountpoints":["/home"]}]},
      {"name":"sdf","path":"/dev/sdf","size":16000000000,"type":"disk","rm":true,"hotplug":true,"tran":"usb","mountpoints":[null],
        "children":[{"name":"sdf1","path":"/dev/sdf1","type":"part","fstype":"vfat","mountpoints":["/media/will/CONFIG"]}]}
    ]}"#;

    #[test]
    fn only_removable_usb_sticks_are_eligible() {
        let protect = vec![PathBuf::from("/media/will/CONFIG/rustybox")];
        let d = parse_lsblk(LSBLK, &protect).unwrap();
        let by = |p: &str| d.iter().find(|x| x.path == p).unwrap();
        assert!(
            d.iter().all(|x| x.path != "/dev/sda"),
            "an empty reader slot is not listed"
        );
        assert!(by("/dev/sdb").eligible);
        assert_eq!(by("/dev/sdb").mountpoints, vec!["/media/will/STICK"]);
        assert_eq!(by("/dev/sdb").fstype.as_deref(), Some("vfat"));
        assert!(
            !by("/dev/sdc").eligible,
            "a 2 TB USB hard disk (the Xbox drive) is never offered"
        );
        assert!(!by("/dev/nvme0n1").eligible);
        assert!(
            by("/dev/sde")
                .why_not
                .as_deref()
                .unwrap()
                .contains("running system")
        );
        assert!(
            by("/dev/sdf")
                .why_not
                .as_deref()
                .unwrap()
                .contains("RustyBox's own data")
        );
    }

    #[test]
    fn labels_are_checked_and_a_fat32_volume_can_be_made_on_an_image() {
        assert_eq!(valid_label("badupdate").unwrap(), "BADUPDATE");
        assert!(
            valid_label("").is_err()
                && valid_label("far too long label").is_err()
                && valid_label("a b").is_err()
        );
        if !which("mkfs.vfat") && !which("mkfs.fat") {
            return;
        }
        let img = std::env::temp_dir().join(format!("rustybox_usb_{}.img", std::process::id()));
        let f = std::fs::File::create(&img).unwrap();
        f.set_len(64 * 1024 * 1024).unwrap();
        mkfs_fat32(&img, "BADUPDATE").unwrap();
        let head = std::fs::read(&img).unwrap();
        assert_eq!(
            &head[0x52..0x5A],
            b"FAT32   ",
            "FAT32 with no partition table"
        );
        assert!(String::from_utf8_lossy(&head[0x47..0x52]).starts_with("BADUPDATE"));
        let _ = std::fs::remove_file(&img);
    }
}
