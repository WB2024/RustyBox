//! Installing a mod, trainer, save or app on the console: work out where each file goes, then
//! (in a job) download, unpack and send it over FTP.
//!
//! Install paths come from a database on the internet and from files, so they are never trusted:
//! they are turned into plain console paths, must stay inside a real drive, and zip entries that
//! try to climb out of their folder are skipped.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use serde::Serialize;

use super::{Db, Item};
use crate::{
    console::{
        Console,
        ftp::{Ftp, ftp_path},
        send_tree,
    },
    convert::{Ctl, plan::safe_name},
    error::Error,
    transfer::Loc,
};

/// Where a file to install comes from.
#[derive(Clone)]
pub enum Source {
    Url(String),
    Local { loc: Loc, rel: String },
}

#[derive(Clone)]
pub struct SpecFile {
    pub name: String,
    pub source: Source,
    /// Install paths with the placeholders filled in, as console paths.
    pub targets: Vec<String>,
}

#[derive(Clone)]
pub struct Spec {
    pub title: String,
    pub files: Vec<SpecFile>,
}

#[derive(Debug, Serialize)]
pub struct PlanFile {
    pub name: String,
    pub from: String,
    pub targets: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Plan {
    pub title: String,
    pub files: Vec<PlanFile>,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    pub ok: bool,
}

/// Fill in the placeholders in an install path and make it a console path inside a drive.
pub fn resolve_template(
    t: &str,
    item_name: &str,
    category: &str,
    title_id: Option<&str>,
    console: &Console,
) -> Result<String, Error> {
    let aurora = format!("/{}", console.aurora_path.trim_matches('/'));
    let filled = t
        .replace("{AURORAPATH}", &aurora)
        .replace("{CATEGORYID}", category)
        .replace("{NAME}", &safe_name(item_name))
        .replace("{TITLEID}", &title_id.unwrap_or("").to_uppercase());
    if let (Some(a), Some(b)) = (filled.find('{'), filled.find('}'))
        && a < b
    {
        return Err(Error::validation(format!(
            "The install path \"{t}\" uses {} which RustyBox doesn't know how to fill in",
            &filled[a..=b]
        )));
    }
    let keep_dir = filled.ends_with(['/', '\\']);
    let mut p = ftp_path(&filled);
    if filled.contains('\0') || p.split('/').any(|s| s == "..") {
        return Err(Error::validation(format!(
            "The install path \"{t}\" isn't valid"
        )));
    }
    let drive = p
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .to_lowercase();
    if !matches!(drive.as_str(), "hdd1" | "usb0" | "usb1") || p.matches('/').count() < 2 {
        return Err(Error::validation(format!(
            "The install path \"{t}\" doesn't go to a folder on the console's drives"
        )));
    }
    if keep_dir && !p.ends_with('/') {
        p.push('/');
    }
    Ok(p)
}

/// A spec for something in the database.
pub fn spec_for(item: &Item, console: &Console) -> Result<Spec, Vec<String>> {
    let mut problems = Vec::new();
    let mut files = Vec::new();
    if item.files.is_empty() || item.files.iter().all(|f| f.install_paths.is_empty()) {
        return Err(vec![format!(
            "{} has nothing to install on the console. Use Download to keep it in a library.",
            item.name
        )]);
    }
    for f in &item.files {
        let mut targets = Vec::new();
        for t in &f.install_paths {
            match resolve_template(
                t,
                &item.name,
                &item.category,
                item.title_id.as_deref(),
                console,
            ) {
                Ok(p) => targets.push(p),
                Err(e) => problems.push(e.to_string()),
            }
        }
        files.push(SpecFile {
            name: if f.name.is_empty() {
                item.name.clone()
            } else {
                f.name.clone()
            },
            source: Source::Url(f.url.clone()),
            targets,
        });
    }
    if problems.is_empty() {
        Ok(Spec {
            title: item.name.clone(),
            files,
        })
    } else {
        Err(problems)
    }
}

/// A spec for a file or folder from a library, to go to a folder on the console.
pub fn spec_for_local(
    name: &str,
    loc: Loc,
    rel: &str,
    dest: &str,
    console: &Console,
) -> Result<Spec, Vec<String>> {
    let target =
        resolve_template(dest, name, "", None, console).map_err(|e| vec![e.to_string()])?;
    Ok(Spec {
        title: name.to_string(),
        files: vec![SpecFile {
            name: name.to_string(),
            source: Source::Local {
                loc,
                rel: rel.to_string(),
            },
            targets: vec![if target.ends_with('/') {
                target
            } else {
                format!("{target}/")
            }],
        }],
    })
}

pub fn plan(spec: &Spec, base: &str) -> Plan {
    let mut warnings = vec![
        "Files that are already on the console with the same name are replaced.".to_string(),
        "RustyBox can't see the console's free space: make sure there is room.".to_string(),
    ];
    let mut problems = Vec::new();
    let files: Vec<PlanFile> = spec
        .files
        .iter()
        .map(|f| {
            if f.targets.is_empty() {
                problems.push(format!("{} has no place to go on the console", f.name));
            }
            PlanFile {
                name: f.name.clone(),
                from: match &f.source {
                    Source::Url(u) if u.starts_with("mock://") => {
                        "a pretend download (mock mode)".into()
                    }
                    Source::Url(u) => u.replace(base, "the Arisen Studio database"),
                    Source::Local { rel, .. } => format!("your library: {rel}"),
                },
                targets: f.targets.clone(),
            }
        })
        .collect();
    if spec
        .files
        .iter()
        .any(|f| matches!(f.source, Source::Url(_)))
    {
        warnings.push("Zip files are unpacked first, then each file is sent.".into());
    }
    Plan {
        title: spec.title.clone(),
        ok: problems.is_empty(),
        files,
        problems,
        warnings,
    }
}

fn is_dir_path(p: &str) -> bool {
    p.ends_with('/') || !p.rsplit('/').next().unwrap_or("").contains('.')
}

/// Map downloaded files onto install paths. One path: everything goes under it (or, if it names
/// a file, the single file goes there). As many paths as files: one each, in order. Otherwise
/// every file goes to every path.
pub fn map_files(targets: &[String], files: &[String]) -> Result<Vec<(String, String)>, Error> {
    let mut out = Vec::new();
    let put = |t: &str, rel: &str, only_name: bool| -> String {
        if is_dir_path(t) {
            let name = if only_name {
                rel.rsplit('/').next().unwrap_or(rel)
            } else {
                rel
            };
            format!("{}/{name}", t.trim_end_matches('/'))
        } else {
            t.to_string()
        }
    };
    match targets.len() {
        0 => {}
        1 => {
            if !is_dir_path(&targets[0]) && files.len() > 1 {
                // The path names one file but the download has more (a readme, say): send the
                // one with that name, or the only one with that kind of extension.
                let want = targets[0].rsplit('/').next().unwrap_or("").to_lowercase();
                let base = |f: &str| f.rsplit('/').next().unwrap_or(f).to_lowercase();
                let ext = |f: &str| base(f).rsplit_once('.').map(|(_, e)| e.to_string());
                let by_name: Vec<&String> = files.iter().filter(|f| base(f) == want).collect();
                let by_ext: Vec<&String> = files
                    .iter()
                    .filter(|f| ext(f).is_some() && ext(f) == ext(&want))
                    .collect();
                let pick = match (by_name.len(), by_ext.len()) {
                    (1, _) => by_name[0],
                    (0, 1) => by_ext[0],
                    _ => {
                        return Err(Error::validation(format!(
                            "{} names a single file but the download has {} files and none is clearly the one",
                            targets[0],
                            files.len()
                        )));
                    }
                };
                out.push((pick.clone(), targets[0].clone()));
            } else {
                for f in files {
                    out.push((f.clone(), put(&targets[0], f, false)));
                }
            }
        }
        n if n == files.len() => {
            for (t, f) in targets.iter().zip(files) {
                out.push((f.clone(), put(t, f, true)));
            }
        }
        _ => {
            for t in targets {
                for f in files {
                    out.push((f.clone(), put(t, f, is_dir_path(t))));
                }
            }
        }
    }
    Ok(out)
}

/// Unpack a zip into `dest`, skipping entries that would land outside it. Returns the files
/// (relative paths) it made.
pub fn unzip(zip_path: &Path, dest: &Path) -> Result<Vec<String>, Error> {
    let f = fs::File::open(zip_path)?;
    let mut z =
        zip::ZipArchive::new(f).map_err(|e| Error::backend(format!("That zip is damaged: {e}")))?;
    if z.len() > 20_000 {
        return Err(Error::backend("That zip has too many files"));
    }
    let mut out = Vec::new();
    let mut total = 0u64;
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| Error::backend(e.to_string()))?;
        let Some(rel) = e.enclosed_name() else {
            continue;
        };
        let target = dest.join(&rel);
        if e.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        total += e.size();
        if total > 8 * 1024 * 1024 * 1024 {
            return Err(Error::backend("That zip unpacks to too much data"));
        }
        if let Some(p) = target.parent() {
            fs::create_dir_all(p)?;
        }
        let mut w = fs::File::create(&target)?;
        std::io::copy(&mut e.by_ref().take(8 * 1024 * 1024 * 1024), &mut w)?;
        out.push(rel.to_string_lossy().replace('\\', "/"));
    }
    out.sort();
    Ok(out)
}

fn looks_like_zip(p: &Path) -> bool {
    let mut magic = [0u8; 4];
    fs::File::open(p)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && &magic == b"PK\x03\x04"
}

pub struct Report {
    pub files: usize,
    pub notes: Vec<String>,
}

/// Install everything in the spec. `staging` is a folder this job may fill and empty.
pub fn run(
    db: &Db,
    ftp: &mut Ftp,
    spec: &Spec,
    staging: &Path,
    ctl: &Ctl,
) -> Result<Report, Error> {
    fs::create_dir_all(staging)?;
    let mut sent = 0usize;
    let mut notes = Vec::new();
    let n = spec.files.len().max(1);
    let result = (|| -> Result<(), Error> {
        for (i, f) in spec.files.iter().enumerate() {
            ctl.check()?;
            let lo = i as f32 / n as f32;
            let scale = |frac: f32, what: &str| (ctl.progress)(lo + frac / n as f32, what);
            let sub = Ctl {
                cancelled: ctl.cancelled,
                progress: &scale,
            };
            let work = staging.join(format!("f{i}"));
            fs::create_dir_all(&work)?;
            // Get the files to send, as a folder here.
            let (root, rels): (PathBuf, Vec<String>) = match &f.source {
                Source::Url(url) => {
                    let dl = work.join("download");
                    db.download(url, &dl, &sub)?;
                    if looks_like_zip(&dl) {
                        (sub.progress)(0.5, "Unpacking");
                        let out = work.join("unpacked");
                        let rels = unzip(&dl, &out)?;
                        (out, rels)
                    } else {
                        let name = if f.name.contains('.') {
                            safe_name(&f.name)
                        } else {
                            format!("{}.xex", safe_name(&f.name))
                        };
                        let out = work.join("single");
                        fs::create_dir_all(&out)?;
                        fs::rename(&dl, out.join(&name))?;
                        (out, vec![name])
                    }
                }
                Source::Local { loc, rel } => {
                    let tree = loc.tree(rel)?;
                    let is_file = tree.len() == 1 && tree[0].rel.is_empty();
                    // Copy it here first so a zip can be unpacked and one pass reads everything.
                    let out = work.join("local");
                    fs::create_dir_all(&out)?;
                    let name = rel.rsplit('/').next().unwrap_or("file").to_string();
                    let files: Vec<(String, String)> = if is_file {
                        vec![(rel.clone(), name.clone())]
                    } else {
                        tree.iter()
                            .filter(|e| !e.is_dir)
                            .map(|e| {
                                (
                                    format!("{}/{}", rel.trim_matches('/'), e.rel),
                                    e.rel.clone(),
                                )
                            })
                            .collect()
                    };
                    let mut rels = Vec::new();
                    for (src, dst) in files {
                        let size = loc.stat(&src)?.map(|s| s.size).unwrap_or(0);
                        let target = out.join(&dst);
                        if let Some(p) = target.parent() {
                            fs::create_dir_all(p)?;
                        }
                        let mut w = fs::File::create(&target)?;
                        let mut off = 0;
                        while off < size {
                            ctl.check()?;
                            let chunk = loc.read(&src, off, (4 * 1024 * 1024).min(size - off))?;
                            if chunk.is_empty() {
                                return Err(Error::backend(format!("{src} ended early")));
                            }
                            std::io::Write::write_all(&mut w, &chunk)?;
                            off += chunk.len() as u64;
                        }
                        rels.push(dst);
                    }
                    if rels.len() == 1 && looks_like_zip(&out.join(&rels[0])) {
                        let up = work.join("unpacked");
                        let r = unzip(&out.join(&rels[0]), &up)?;
                        (up, r)
                    } else {
                        (out, rels)
                    }
                }
            };
            if rels.is_empty() {
                return Err(Error::backend(format!("{} contained no files", f.name)));
            }
            let mappings = map_files(&f.targets, &rels)?;
            let loc = Loc::Local(root);
            for (j, (local, remote)) in mappings.iter().enumerate() {
                ctl.check()?;
                (sub.progress)(0.6 + 0.4 * j as f32 / mappings.len() as f32, local);
                send_tree(ftp, &loc, local, remote, true, &sub)?;
                sent += 1;
            }
            notes.push(format!("{}: {} file(s) sent", f.name, mappings.len()));
            let _ = fs::remove_dir_all(&work);
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(staging);
    result?;
    Ok(Report { files: sent, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c() -> Console {
        Console {
            host: "x".into(),
            ..Default::default()
        }
    }

    #[test]
    fn templates_are_filled_and_unsafe_ones_refused() {
        let r = |t: &str| resolve_template(t, "Legacy Menu", "gta5", Some("4d530805"), &c());
        assert_eq!(
            r("Hdd:\\Arisen Studio\\{CATEGORYID}\\{NAME}\\Menu.xex").unwrap(),
            "/Hdd1/Arisen Studio/gta5/Legacy Menu/Menu.xex"
        );
        assert_eq!(
            r("{AURORAPATH}\\User\\Trainers\\{TITLEID}\\T\\").unwrap(),
            "/Hdd1/Aurora/User/Trainers/4D530805/T/"
        );
        assert!(r("/{USBDEV}/PS3/x").is_err(), "unknown placeholder");
        assert!(r("Hdd:\\..\\x").is_err());
        assert!(r("C:\\Windows").is_err());
        assert!(r("Hdd:\\").is_err(), "never a whole drive");
        assert!(r("/etc/passwd").is_err());
    }

    #[test]
    fn files_are_mapped_onto_install_paths() {
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // One folder: everything goes under it, keeping subfolders.
        let m = map_files(&f(&["/Hdd1/Apps/Tool/"]), &f(&["a.xex", "data/b.bin"])).unwrap();
        assert_eq!(m[1].1, "/Hdd1/Apps/Tool/data/b.bin");
        // One file path and one file: exactly there.
        assert_eq!(
            map_files(&f(&["/Hdd1/Apps/Tool/x.xex"]), &f(&["a.xex"])).unwrap()[0].1,
            "/Hdd1/Apps/Tool/x.xex"
        );
        // One file path but many files: the one with that name wins; a readme is left behind.
        let m = map_files(
            &f(&["/Hdd1/Apps/Tool/x.xex"]),
            &f(&["readme.txt", "sub/X.XEX"]),
        )
        .unwrap();
        assert_eq!(
            m,
            vec![("sub/X.XEX".to_string(), "/Hdd1/Apps/Tool/x.xex".to_string())]
        );
        // ...and when nothing is clearly the one, it says so rather than guessing.
        assert!(map_files(&f(&["/Hdd1/Apps/Tool/x.xex"]), &f(&["a.xex", "b.xex"])).is_err());
        // As many paths as files: one each.
        let m = map_files(&f(&["/Hdd1/A/", "/Hdd1/B/"]), &f(&["one.xex", "two.xex"])).unwrap();
        assert_eq!(
            (m[0].1.as_str(), m[1].1.as_str()),
            ("/Hdd1/A/one.xex", "/Hdd1/B/two.xex")
        );
        // Otherwise every file to every path.
        assert_eq!(
            map_files(&f(&["/Hdd1/A/", "/Hdd1/B/"]), &f(&["x", "y", "z"]))
                .unwrap()
                .len(),
            6
        );
    }

    #[test]
    fn unzip_never_writes_outside_its_folder() {
        use std::io::Write;
        let d = std::env::temp_dir().join(format!("rustybox_unzip_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let zp = d.join("t.zip");
        {
            let mut z = zip::ZipWriter::new(fs::File::create(&zp).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("ok/a.txt", o).unwrap();
            z.write_all(b"1").unwrap();
            z.start_file("../evil.txt", o).unwrap();
            z.write_all(b"2").unwrap();
            z.finish().unwrap();
        }
        let out = d.join("out");
        let files = unzip(&zp, &out).unwrap();
        assert_eq!(files, vec!["ok/a.txt"]);
        assert!(!d.join("evil.txt").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
