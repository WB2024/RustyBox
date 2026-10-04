//! Taking a disc image out of a `.zip`. Redump's Xbox 360 collection (and many torrents) is one zip
//! per game with the ISO inside, which has to come out before it can be imported.
//!
//! Zips from the internet are not trusted: only `.iso` entries are read, only the file name of an
//! entry is used (never its folders), and nothing is written outside the folder given.

use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use crate::{convert::Ctl, error::Error, library::health, perms};

/// One zip that holds disc images.
#[derive(Debug, Clone, PartialEq)]
pub struct ZipPlan {
    pub zip: PathBuf,
    /// The `.iso` entries: (index in the zip, file name, uncompressed size).
    pub isos: Vec<(usize, String, u64)>,
}

impl ZipPlan {
    pub fn bytes(&self) -> u64 {
        self.isos.iter().map(|i| i.2).sum()
    }
}

fn is_iso(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, e)| e.eq_ignore_ascii_case("iso"))
}

/// Look inside one zip for disc images.
pub fn inspect(zip: &Path) -> Result<ZipPlan, Error> {
    let f = File::open(zip)?;
    let mut a = zip::ZipArchive::new(f)
        .map_err(|e| Error::backend(format!("{} isn't a readable zip: {e}", zip.display())))?;
    let mut isos = Vec::new();
    for i in 0..a.len() {
        let Ok(e) = a.by_index_raw(i) else { continue };
        if e.is_dir() {
            continue;
        }
        // Only the last part of the entry's name is used.
        let name = e
            .name()
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_string();
        if !name.is_empty() && name != "." && name != ".." && is_iso(&name) {
            isos.push((i, name, e.size()));
        }
    }
    Ok(ZipPlan {
        zip: zip.to_path_buf(),
        isos,
    })
}

/// Zips (looking a few folders down) that hold at least one disc image. A path that is itself a
/// zip counts. Unreadable zips are skipped.
pub fn find(root: &Path) -> Vec<ZipPlan> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 6 || out.len() > 5000 {
            return;
        }
        let Ok(rd) = fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(m) = crate::fsops::entry_meta(&e) else {
                continue;
            };
            if m.is_dir() {
                walk(&e.path(), depth + 1, out);
            } else if name.to_lowercase().ends_with(".zip") {
                out.push(e.path());
            }
        }
    }
    let mut zips = Vec::new();
    if root.is_file() {
        zips.push(root.to_path_buf());
    } else {
        walk(root, 0, &mut zips);
    }
    zips.iter()
        .filter_map(|z| inspect(z).ok())
        .filter(|p| !p.isos.is_empty())
        .collect()
}

/// Extract the disc images of `plans` into folders under `dest` (one per zip, named after it).
/// Each file is written as `.part` and renamed when complete. Returns the folders made.
pub fn extract(plans: &[ZipPlan], dest: &Path, ctl: &Ctl) -> Result<Vec<PathBuf>, Error> {
    let total: u64 = plans.iter().map(ZipPlan::bytes).sum();
    if let Some((_, free)) =
        health::space(dest.parent().unwrap_or(dest)).or_else(|| health::space(dest))
        && free < total + total / 50
    {
        return Err(Error::coded(
            507,
            "NO_SPACE",
            format!(
                "Not enough free space to unpack: about {} MB are needed and {} MB are free.",
                total / 1_048_576,
                free / 1_048_576
            ),
        ));
    }
    fs::create_dir_all(dest)?;
    perms::own(dest);
    let (mut done, mut made) = (0u64, Vec::new());
    for plan in plans {
        let stem = plan
            .zip
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unpacked".into());
        let dir = dest.join(crate::convert::plan::safe_name(&stem));
        fs::create_dir_all(&dir)?;
        let mut a = zip::ZipArchive::new(File::open(&plan.zip)?).map_err(|e| {
            Error::backend(format!("{} isn't a readable zip: {e}", plan.zip.display()))
        })?;
        for (index, name, _) in &plan.isos {
            let out = dir.join(crate::convert::plan::safe_name(name));
            if out.exists() {
                continue; // an earlier try finished this one
            }
            let part = crate::fsops::part_path(&out);
            let mut src = a
                .by_index(*index)
                .map_err(|e| Error::backend(format!("{name} can't be read from the zip: {e}")))?;
            let mut f = File::create(&part)?;
            let mut buf = vec![0u8; 4 * 1024 * 1024];
            loop {
                if let Err(e) = ctl.check() {
                    drop(f);
                    let _ = fs::remove_file(&part);
                    return Err(e);
                }
                let n = src.read(&mut buf).map_err(|e| {
                    Error::backend(format!("{name} is damaged inside the zip: {e}"))
                })?;
                if n == 0 {
                    break;
                }
                f.write_all(&buf[..n])?;
                done += n as u64;
                (ctl.progress)(
                    if total == 0 {
                        1.0
                    } else {
                        done as f32 / total as f32
                    },
                    &format!("Unpacking {name}"),
                );
            }
            f.sync_all()?;
            drop(f);
            fs::rename(&part, &out)?;
            perms::own(&out);
        }
        made.push(dir);
    }
    Ok(made)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        for (n, d) in entries {
            w.start_file(*n, zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(d).unwrap();
        }
        w.finish().unwrap();
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rustybox_unpack_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_zips_with_isos_and_unpacks_only_the_iso() {
        let d = tmp("basic");
        fs::create_dir_all(d.join("Redump/Xbox 360")).unwrap();
        make_zip(
            &d.join("Redump/Xbox 360/Halo 3 (USA).zip"),
            &[
                ("Halo 3 (USA).iso", &[7u8; 5000]),
                ("Halo 3 (USA).dat", b"x"),
                ("../../evil.iso", &[1u8; 10]),
            ],
        );
        make_zip(
            &d.join("Redump/Xbox 360/Docs.zip"),
            &[("readme.txt", b"hi")],
        );
        std::fs::write(d.join("Redump/Xbox 360/notzip.zip"), b"junk").unwrap();
        let plans = find(&d);
        assert_eq!(plans.len(), 1, "{plans:?}");
        // The entry that tries to climb out is only ever used by its last name part.
        assert_eq!(plans[0].isos.len(), 2);
        assert!(plans[0].isos.iter().any(|i| i.1 == "evil.iso"));

        let out = d.join("unpacked");
        let made = extract(
            &plans,
            &out,
            &Ctl {
                cancelled: &|| false,
                progress: &|_, _| {},
            },
        )
        .unwrap();
        assert_eq!(made.len(), 1);
        assert_eq!(
            fs::read(made[0].join("Halo 3 (USA).iso")).unwrap().len(),
            5000
        );
        assert!(!made[0].join("Halo 3 (USA).dat").exists());
        assert!(made[0].join("evil.iso").exists());
        assert!(
            !d.join("evil.iso").exists() && !out.join("evil.iso").exists(),
            "nothing escaped the folder"
        );
        // Doing it again changes nothing and doesn't fail.
        assert!(
            extract(
                &plans,
                &out,
                &Ctl {
                    cancelled: &|| false,
                    progress: &|_, _| {}
                }
            )
            .is_ok()
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_cancel_leaves_no_half_file_behind() {
        let d = tmp("cancel");
        make_zip(&d.join("g.zip"), &[("g.iso", &vec![3u8; 9_000_000])]);
        let plans = find(&d);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let cancelled = || calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1;
        let r = extract(
            &plans,
            &d.join("out"),
            &Ctl {
                cancelled: &cancelled,
                progress: &|_, _| {},
            },
        );
        assert!(r.is_err());
        let left: Vec<_> = walk_files(&d.join("out"));
        assert!(left.is_empty(), "{left:?}");
        let _ = fs::remove_dir_all(&d);
    }

    fn walk_files(p: &Path) -> Vec<PathBuf> {
        let mut v = vec![];
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    v.extend(walk_files(&e.path()));
                } else {
                    v.push(e.path());
                }
            }
        }
        v
    }
}
