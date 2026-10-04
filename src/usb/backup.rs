//! USB backups: a whole FAT32 stick (or any image file) is saved with `partclone.vfat` (which only
//! stores the used blocks) and compressed with `zstd`, and can be restored byte for byte.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{convert::Ctl, error::Error};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Meta {
    pub name: String,
    pub label: Option<String>,
    pub source: String,
    /// Size of the stick the backup came from.
    pub size: u64,
    pub created: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Backup {
    pub name: String,
    pub bytes: u64,
    pub meta: Option<Meta>,
}

pub fn have_tools() -> Result<(), Error> {
    for t in ["partclone.vfat", "zstd"] {
        if !super::which(t) {
            return Err(Error::coded(
                501,
                "NO_TOOL",
                format!("{t} isn't installed here, so backups can't be made"),
            ));
        }
    }
    Ok(())
}

/// A backup name that is safe as a file name.
pub fn clean_name(name: &str) -> Result<String, Error> {
    let n: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let n = n.trim_matches(|c| c == '.' || c == '-').to_string();
    if n.is_empty() || n.len() > 80 {
        return Err(Error::validation(
            "Give the backup a name (letters, digits, dashes)",
        ));
    }
    Ok(n)
}

pub fn path_of(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.img.zst"))
}

pub fn list(dir: &Path) -> Vec<Backup> {
    let mut out: Vec<Backup> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let f = e.file_name().to_string_lossy().to_string();
            let name = f.strip_suffix(".img.zst")?.to_string();
            let meta = fs::read(dir.join(format!("{name}.json")))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            Some(Backup {
                name,
                bytes: e.metadata().ok()?.len(),
                meta,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.meta
            .as_ref()
            .map(|m| m.created)
            .cmp(&a.meta.as_ref().map(|m| m.created))
            .then(a.name.cmp(&b.name))
    });
    out
}

/// Wait for a child, stopping it if a cancel comes in. Returns its exit status.
fn wait(
    child: &mut Child,
    ctl: &Ctl,
    watch: Option<(&Path, &str)>,
) -> Result<std::process::ExitStatus, Error> {
    loop {
        if let Some(s) = child.try_wait()? {
            return Ok(s);
        }
        if (ctl.cancelled)() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Backend("cancelled".into()));
        }
        if let Some((p, what)) = watch
            && let Ok(m) = fs::metadata(p)
        {
            (ctl.progress)(0.5, &format!("{what}: {} MB written", m.len() / 1_048_576));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn stderr_of(c: &mut Child) -> String {
    let mut s = String::new();
    if let Some(mut e) = c.stderr.take() {
        let _ = e.read_to_string(&mut s);
    }
    s.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string()
}

/// Back up the filesystem at `src` (a device or an image file) to `dir/name.img.zst`.
pub fn backup(
    src: &Path,
    dir: &Path,
    name: &str,
    label: Option<String>,
    ctl: &Ctl,
) -> Result<Meta, Error> {
    have_tools()?;
    fs::create_dir_all(dir)?;
    let name = clean_name(name)?;
    let out = path_of(dir, &name);
    if out.exists() {
        return Err(Error::conflict(format!(
            "A backup called {name} already exists. Choose another name or delete it first."
        )));
    }
    let part = PathBuf::from(format!("{}.part", out.display()));
    let log = std::env::temp_dir().join(format!("rustybox-partclone-{}.log", std::process::id()));
    let mut pc = Command::new("partclone.vfat")
        .args(["-c", "-s"])
        .arg(src)
        .args(["-o", "-", "-L"])
        .arg(&log)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::backend(format!("Couldn't start partclone: {e}")))?;
    let mut z = Command::new("zstd")
        .args(["-T0", "-q", "-o"])
        .arg(&part)
        .stdin(pc.stdout.take().ok_or_else(|| Error::backend("No pipe"))?)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::backend(format!("Couldn't start zstd: {e}")))?;
    let r = (|| -> Result<(), Error> {
        let zs = wait(&mut z, ctl, Some((&part, "Backing up")))?;
        let ps = wait(&mut pc, ctl, None)?;
        if !ps.success() {
            return Err(Error::backend(format!(
                "partclone failed: {}",
                stderr_of(&mut pc)
            )));
        }
        if !zs.success() {
            return Err(Error::backend(format!(
                "zstd failed: {}",
                stderr_of(&mut z)
            )));
        }
        Ok(())
    })();
    let _ = fs::remove_file(&log);
    if let Err(e) = r {
        let _ = pc.kill();
        let _ = z.kill();
        let _ = fs::remove_file(&part);
        return Err(if (ctl.cancelled)() {
            Error::backend("Cancelled")
        } else {
            e
        });
    }
    fs::rename(&part, &out)?;
    let meta = Meta {
        name: name.clone(),
        label,
        source: src.to_string_lossy().to_string(),
        size: fs::metadata(src).map(|m| m.len()).unwrap_or(0),
        created: crate::jobs::now(),
    };
    fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&meta)?,
    )?;
    Ok(meta)
}

/// Restore a backup onto `dest` (a device, or an image file that already exists).
pub fn restore(file: &Path, dest: &Path, ctl: &Ctl) -> Result<(), Error> {
    have_tools()?;
    if !file.is_file() {
        return Err(Error::not_found("That backup isn't there"));
    }
    let is_dev = dest.starts_with("/dev");
    let mut z = Command::new("zstd")
        .args(["-dc", "-q"])
        .arg(file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::backend(format!("Couldn't start zstd: {e}")))?;
    let log = std::env::temp_dir().join(format!("rustybox-partclone-r-{}.log", std::process::id()));
    let mut pc = Command::new("partclone.vfat")
        .args(["-r", "-s", "-"])
        .arg(if is_dev { "-o" } else { "-O" })
        .arg(dest)
        .arg("-L")
        .arg(&log)
        .stdin(z.stdout.take().ok_or_else(|| Error::backend("No pipe"))?)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::backend(format!("Couldn't start partclone: {e}")))?;
    (ctl.progress)(0.5, "Restoring");
    let r = (|| {
        let ps = wait(&mut pc, ctl, None)?;
        let _ = wait(&mut z, ctl, None)?;
        if !ps.success() {
            let hint = fs::read_to_string(&log)
                .ok()
                .and_then(|l| {
                    l.lines()
                        .rev()
                        .find(|x| x.contains("error") || x.contains("fail") || x.contains("access"))
                        .map(String::from)
                })
                .unwrap_or_default();
            return Err(Error::backend(format!(
                "Restoring failed: {} {hint}. Restoring onto a device needs root and a stick at least as big as the original.",
                stderr_of(&mut pc)
            )));
        }
        Ok(())
    })();
    let _ = fs::remove_file(&log);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet<T>(f: impl FnOnce(&Ctl) -> T) -> T {
        let c = || false;
        let p = |_: f32, _: &str| {};
        f(&Ctl {
            cancelled: &c,
            progress: &p,
        })
    }

    #[test]
    fn a_stick_image_is_backed_up_and_its_files_come_back_on_restore() {
        if have_tools().is_err() || !super::super::which("mkfs.vfat") {
            eprintln!("partclone/zstd/mkfs.vfat missing: skipping");
            return;
        }
        let d = std::env::temp_dir().join(format!("rustybox_bk_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let img = d.join("stick.img");
        fs::File::create(&img)
            .unwrap()
            .set_len(64 * 1024 * 1024)
            .unwrap();
        super::super::mkfs_fat32(&img, "BACKUPME").unwrap();
        let backups = d.join("backups");
        let meta =
            quiet(|c| backup(&img, &backups, "my stick!", Some("BACKUPME".into()), c)).unwrap();
        assert_eq!(meta.name, "my-stick");
        let l = list(&backups);
        assert_eq!(l.len(), 1);
        assert!(
            l[0].bytes < 1_000_000,
            "only the used blocks are stored: {}",
            l[0].bytes
        );
        assert!(
            quiet(|c| backup(&img, &backups, "my-stick", None, c)).is_err(),
            "never overwrites a backup"
        );
        // Put a file on the stick, back up, wreck the stick, restore: the file is back. (Unused
        // blocks aren't stored, so only the filesystem, not every byte, is compared.)
        let has_mtools = super::super::which("mcopy") && super::super::which("mtype");
        let src_file = d.join("hello.txt");
        fs::write(&src_file, b"hello from the stick").unwrap();
        if has_mtools {
            assert!(
                Command::new("mcopy")
                    .arg("-i")
                    .arg(&img)
                    .arg(&src_file)
                    .arg("::/hello.txt")
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let _ = fs::remove_dir_all(&backups);
        quiet(|c| backup(&img, &backups, "my-stick", Some("BACKUPME".into()), c)).unwrap();
        fs::write(&img, vec![0xAAu8; 64 * 1024 * 1024]).unwrap();
        assert!(
            !has_mtools
                || !Command::new("mtype")
                    .arg("-i")
                    .arg(&img)
                    .arg("::/hello.txt")
                    .output()
                    .unwrap()
                    .status
                    .success()
        );
        quiet(|c| restore(&path_of(&backups, "my-stick"), &img, c)).unwrap();
        if has_mtools {
            let out = Command::new("mtype")
                .arg("-i")
                .arg(&img)
                .arg("::/hello.txt")
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), "hello from the stick");
        }
        let head = fs::read(&img).unwrap();
        assert!(&head[0x52..0x5A] == b"FAT32   ", "the boot sector is back");
        assert_eq!(clean_name("../../etc").unwrap(), "etc");
        assert!(clean_name("...").is_err());
        let _ = fs::remove_dir_all(&d);
    }
}
