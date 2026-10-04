//! Building a Bad Avatar USB stick: the exploit package (which you supply, and RustyBox never
//! ships) is copied onto a FAT32 stick, the Aurora folder is renamed to the name the launch file
//! expects, `launch.ini` is optionally set to start Aurora, and an `info.txt` is written.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{convert::Ctl, error::Error, fsops, library::health};

/// What the console starts when `launch.ini`'s `Default =` points at Aurora.
pub const AURORA_LAUNCH: &str = r"Usb:\Apps\Aurora\Aurora.xex";
const AURORA_SRC: &str = "Aurora 0.7b.2";
const AURORA_DEST: &str = "Aurora";

#[derive(Debug, Clone, Serialize)]
pub struct SourceCheck {
    pub dir: String,
    pub ok: bool,
    pub message: String,
    pub files: usize,
    pub bytes: u64,
    pub has_aurora: bool,
    /// `BadUpdatePayload/default.xex`: the program the exploit runs (XeUnshackle, FreeMyXe...).
    /// The exploit packages don't include one.
    pub has_payload: bool,
    /// The avatar/profile `Content` folder the exploit loads from.
    pub has_content: bool,
}

fn walk(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = fs::metadata(e.path()) else {
                continue;
            };
            if m.is_dir() {
                stack.push(e.path());
            } else {
                out.push((e.path(), m.len()));
            }
        }
    }
    out.sort();
    out
}

/// Is this folder a usable exploit package? It must hold the `BadUpdatePayload` folder.
pub fn check_source(dir: &Path) -> SourceCheck {
    let s = dir.to_string_lossy().to_string();
    if !dir.is_dir() {
        return SourceCheck {
            dir: s,
            ok: false,
            message: "That folder doesn't exist. Put the extracted exploit package there (see the docs); RustyBox never ships it.".into(),
            files: 0,
            bytes: 0,
            has_aurora: false,
            has_payload: false,
            has_content: false,
        };
    }
    if !dir.join("BadUpdatePayload").is_dir() {
        return SourceCheck {
            dir: s,
            ok: false,
            message:
                "This doesn't look like the exploit package: it has no BadUpdatePayload folder."
                    .into(),
            files: 0,
            bytes: 0,
            has_aurora: false,
            has_payload: false,
            has_content: false,
        };
    }
    let files = walk(dir);
    SourceCheck {
        dir: s,
        ok: true,
        message: "Looks right".into(),
        files: files.len(),
        bytes: files.iter().map(|f| f.1).sum(),
        has_aurora: dir.join("Apps").join(AURORA_SRC).is_dir()
            || dir.join("Apps").join(AURORA_DEST).is_dir(),
        has_payload: dir.join("BadUpdatePayload/default.xex").is_file(),
        has_content: dir.join("Content").is_dir(),
    }
}

#[derive(Debug, Serialize)]
pub struct Plan {
    pub steps: Vec<String>,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    pub files: usize,
    pub bytes: u64,
    pub free: u64,
    pub ok: bool,
}

/// What building onto `dest` (a mounted stick) would do. `fresh` means it was just formatted, so
/// its free space is the whole stick.
pub fn plan(
    src: &Path,
    dest: &Path,
    set_default: bool,
    format_first: bool,
    check_fs: bool,
) -> Plan {
    let chk = check_source(src);
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    if !chk.ok {
        problems.push(chk.message.clone());
    }
    let (_, free) = health::space(dest).unwrap_or((0, 0));
    let fstype = fsops::fs_type_of(dest);
    if check_fs
        && !format_first
        && !matches!(
            fstype.as_str(),
            "vfat" | "msdos" | "fat" | "fat32" | "unknown"
        )
    {
        problems.push(format!(
            "The stick is {fstype}, not FAT32. Tick Format first, or the Xbox won't read it."
        ));
    }
    if chk.ok {
        let big = walk(src)
            .into_iter()
            .find(|(_, s)| *s > fsops::FAT32_MAX_FILE);
        if let Some((p, _)) = big {
            problems.push(format!("{} is over 4 GB, too big for FAT32", p.display()));
        }
        if !format_first && free > 0 && free < chk.bytes + 16 * 1024 * 1024 {
            problems.push(format!(
                "Not enough room on the stick: {} bytes needed, {free} free",
                chk.bytes
            ));
        }
        if !chk.has_payload {
            problems.push("There is no BadUpdatePayload/default.xex. The exploit runs that file, and the packages don't include one: add XeUnshackle or FreeMyXe as default.xex (retail format, restrictions removed).".into());
        }
        if !chk.has_content {
            warnings.push("The package has no Content folder (the avatar profile the exploit loads), so the stick won't trigger it.".into());
        }
        if set_default && !chk.has_aurora {
            problems.push("\"Make Aurora start automatically\" needs an Aurora folder (Apps/Aurora) in the package. Add one or untick it.".into());
        } else if !chk.has_aurora {
            warnings.push("There is no Aurora folder (Apps/Aurora), so no homebrew dashboard goes on the stick.".into());
        }
    }
    if !format_first
        && fs::read_dir(dest)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    {
        warnings.push("The stick isn't empty. Files with the same names are replaced and the rest are left; format it first for a clean stick.".into());
    }
    let mut steps = Vec::new();
    if format_first {
        steps.push(
            "Format the stick as FAT32 (named BADUPDATE). Everything on it is erased.".to_string(),
        );
    }
    steps.push(format!(
        "Copy {} files ({} bytes) from {}",
        chk.files,
        chk.bytes,
        src.display()
    ));
    if chk.has_aurora {
        if src.join("Apps").join(AURORA_SRC).is_dir() {
            steps.push(format!("Rename Apps/{AURORA_SRC} to Apps/{AURORA_DEST}"));
        }
        steps.push(if set_default {
            format!("Set launch.ini to start Aurora ({AURORA_LAUNCH})")
        } else {
            "Leave launch.ini as it is (set the default program in DashLaunch on the console)"
                .into()
        });
    }
    steps.push("Write info.txt, flush to the stick".into());
    Plan {
        ok: problems.is_empty(),
        steps,
        problems,
        warnings,
        files: chk.files,
        bytes: chk.bytes,
        free,
    }
}

/// Set `Default = ...` in launch.ini, keeping its line endings (it ships with Windows ones).
pub fn patch_launch_ini(dest: &Path) -> Result<bool, Error> {
    let ini = dest.join("launch.ini");
    if !ini.is_file() {
        // The packages don't ship one: make the smallest file that names Aurora.
        fs::write(&ini, format!("[Paths]\r\nDefault = {AURORA_LAUNCH}\r\n"))?;
        return Ok(true);
    }
    let raw = fs::read(&ini)?;
    let text = String::from_utf8_lossy(&raw).to_string();
    let crlf = text.contains("\r\n");
    let mut changed = false;
    let out: Vec<String> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(|l| {
            let t = l.trim_start();
            if let Some(rest) = t.strip_prefix("Default")
                && rest.trim_start().starts_with('=')
            {
                changed = true;
                format!("Default = {AURORA_LAUNCH}")
            } else {
                l.to_string()
            }
        })
        .collect();
    if changed {
        let sep = if crlf { "\r\n" } else { "\n" };
        fs::write(&ini, out.join(sep))?;
    }
    Ok(changed)
}

/// `2026-10-02 17:30 UTC` from unix seconds (no date library needed).
pub fn utc_stamp(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}

pub struct Report {
    pub files: usize,
    pub notes: Vec<String>,
}

/// Copy the package onto the stick and finish it off.
pub fn build(src: &Path, dest: &Path, set_default: bool, ctl: &Ctl) -> Result<Report, Error> {
    let chk = check_source(src);
    if !chk.ok {
        return Err(Error::validation(chk.message));
    }
    let files = walk(src);
    let total: u64 = files.iter().map(|f| f.1).sum::<u64>().max(1);
    let mut done = 0u64;
    let mut notes = Vec::new();
    for (p, size) in &files {
        ctl.check()?;
        let rel = p
            .strip_prefix(src)
            .map_err(|e| Error::backend(e.to_string()))?;
        let target = fsops::join_rel(dest, &rel.to_string_lossy())?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let part = PathBuf::from(format!("{}.part", target.display()));
        fs::copy(p, &part)?;
        fs::rename(&part, &target)?;
        done += size;
        (ctl.progress)(done as f32 / total as f32 * 0.95, &rel.to_string_lossy());
    }
    let apps = dest.join("Apps");
    let (a, b) = (apps.join(AURORA_SRC), apps.join(AURORA_DEST));
    if a.is_dir() && !b.exists() {
        fs::rename(&a, &b)?;
        notes.push(format!("Renamed {AURORA_SRC} to {AURORA_DEST}"));
    }
    if set_default {
        if b.join("Aurora.xex").is_file() && patch_launch_ini(dest)? {
            notes.push("launch.ini now starts Aurora".into());
        } else {
            notes.push("launch.ini was not changed (no Aurora, or no Default line)".into());
        }
    }
    fs::write(
        dest.join("info.txt"),
        format!(
            "This drive was created with RustyBox.\nExploit package: {}\nCreated: {}\nDefault program: {}\n",
            src.display(),
            utc_stamp(crate::jobs::now()),
            if set_default {
                AURORA_LAUNCH
            } else {
                "(set in DashLaunch)"
            }
        ),
    )?;
    // SAFETY: sync(2) takes no arguments and only asks the kernel to write out buffers.
    unsafe { libc::sync() };
    (ctl.progress)(1.0, "");
    Ok(Report {
        files: files.len(),
        notes,
    })
}

/// A small pretend package for `--mock` and tests.
pub fn write_sample_package(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir.join("BadUpdatePayload"))?;
    fs::write(dir.join("BadUpdatePayload/default.xex"), b"pretend payload")?;
    fs::create_dir_all(dir.join("Content/E0002FF78DFBDE7B/FFFE07D1/00010000"))?;
    fs::write(
        dir.join("Content/E0002FF78DFBDE7B/FFFE07D1/00010000/E0002FF78DFBDE7B"),
        b"pretend profile",
    )?;
    fs::create_dir_all(dir.join("Apps").join(AURORA_SRC))?;
    fs::write(
        dir.join("Apps").join(AURORA_SRC).join("Aurora.xex"),
        b"pretend aurora",
    )?;
    fs::write(
        dir.join("launch.ini"),
        "[Paths]\r\nDefault = \r\nFreestyle = \r\n",
    )?;
    Ok(())
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
    fn the_date_stamp_is_right() {
        assert_eq!(utc_stamp(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc_stamp(1_790_958_776), "2026-10-02 16:32 UTC");
    }

    #[test]
    fn a_stick_is_built_from_a_package_and_aurora_is_set_as_default() {
        let d = std::env::temp_dir().join(format!("rustybox_ba_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let (src, dest) = (d.join("pkg"), d.join("stick"));
        fs::create_dir_all(&dest).unwrap();
        // Not a package yet.
        assert!(!check_source(&src).ok);
        fs::create_dir_all(&src).unwrap();
        assert!(!check_source(&src).ok && check_source(&src).message.contains("BadUpdatePayload"));
        write_sample_package(&src).unwrap();
        let c = check_source(&src);
        assert!(
            (c.ok, c.files, c.has_aurora, c.has_payload) == (true, 4, true, true),
            "{c:?}"
        );
        let p = plan(&src, &dest, true, false, false);
        assert!(p.ok, "{p:?}");
        assert!(p.steps.iter().any(|s| s.contains("Rename Apps")));
        quiet(|ctl| build(&src, &dest, true, ctl)).unwrap();
        assert!(dest.join("BadUpdatePayload/default.xex").is_file());
        assert!(
            dest.join("Apps/Aurora/Aurora.xex").is_file()
                && !dest.join("Apps/Aurora 0.7b.2").exists()
        );
        let ini = fs::read_to_string(dest.join("launch.ini")).unwrap();
        assert!(
            ini.contains("Default = Usb:\\Apps\\Aurora\\Aurora.xex"),
            "{ini:?}"
        );
        assert!(
            ini.contains("\r\n") && ini.contains("Freestyle"),
            "line endings and other keys are kept: {ini:?}"
        );
        assert!(dest.join("info.txt").is_file());
        // No half-copied files are left behind.
        assert!(
            walk(&dest)
                .iter()
                .all(|(p, _)| !p.to_string_lossy().ends_with(".part"))
        );
        // Without the default option the ini is untouched.
        let dest2 = d.join("stick2");
        fs::create_dir_all(&dest2).unwrap();
        quiet(|ctl| build(&src, &dest2, false, ctl)).unwrap();
        assert!(
            fs::read_to_string(dest2.join("launch.ini"))
                .unwrap()
                .contains("Default = \r\n")
        );
        // A non-empty stick is a warning, and a cancel stops the copy.
        assert!(
            plan(&src, &dest, false, false, false)
                .warnings
                .iter()
                .any(|w| w.contains("isn't empty"))
        );
        let cancel = || true;
        let p = |_: f32, _: &str| {};
        let dest3 = d.join("stick3");
        fs::create_dir_all(&dest3).unwrap();
        assert!(
            build(
                &src,
                &dest3,
                false,
                &Ctl {
                    cancelled: &cancel,
                    progress: &p
                }
            )
            .is_err()
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_real_avatar_package_layout_needs_a_payload_and_aurora_is_optional() {
        // What ABadAvatar v1.3 ships: BadUpdatePayload (no default.xex) and Content. Nothing else.
        let d = std::env::temp_dir().join(format!("rustybox_ba_real_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let (src, dest) = (d.join("pkg"), d.join("stick"));
        fs::create_dir_all(src.join("BadUpdatePayload")).unwrap();
        fs::create_dir_all(src.join("Content/E0002FF78DFBDE7B/FFFE07D1/00010000")).unwrap();
        fs::create_dir_all(&dest).unwrap();
        for f in [
            "GamerProfile.xex",
            "update_data.bin",
            "xke_update.bin",
            "BadUpdateExploit-4thStage.bin",
        ] {
            fs::write(src.join("BadUpdatePayload").join(f), b"x").unwrap();
        }
        fs::write(
            src.join("Content/E0002FF78DFBDE7B/FFFE07D1/00010000/E0002FF78DFBDE7B"),
            b"p",
        )
        .unwrap();
        let c = check_source(&src);
        assert!(
            c.ok && !c.has_payload && c.has_content && !c.has_aurora,
            "{c:?}"
        );
        let p = plan(&src, &dest, false, false, false);
        assert!(
            !p.ok && p.problems.iter().any(|x| x.contains("default.xex")),
            "{p:?}"
        );
        // With a payload it can be built; Aurora is only a warning...
        fs::write(src.join("BadUpdatePayload/default.xex"), b"payload").unwrap();
        let p = plan(&src, &dest, false, false, false);
        assert!(
            p.ok && p.warnings.iter().any(|x| x.contains("Aurora")),
            "{p:?}"
        );
        // ...unless you ask for it to be the default.
        assert!(!plan(&src, &dest, true, false, false).ok);
        quiet(|ctl| build(&src, &dest, false, ctl)).unwrap();
        assert!(
            dest.join("Content/E0002FF78DFBDE7B/FFFE07D1/00010000/E0002FF78DFBDE7B")
                .is_file()
        );
        assert!(
            !dest.join("launch.ini").exists(),
            "no launch.ini is invented when Aurora isn't wanted"
        );
        // With Aurora in the package and the default option, a minimal launch.ini is made.
        fs::create_dir_all(src.join("Apps/Aurora")).unwrap();
        fs::write(src.join("Apps/Aurora/Aurora.xex"), b"a").unwrap();
        let dest2 = d.join("stick2");
        fs::create_dir_all(&dest2).unwrap();
        quiet(|ctl| build(&src, &dest2, true, ctl)).unwrap();
        let ini = fs::read_to_string(dest2.join("launch.ini")).unwrap();
        assert_eq!(
            ini,
            "[Paths]\r\nDefault = Usb:\\Apps\\Aurora\\Aurora.xex\r\n"
        );
        let _ = fs::remove_dir_all(&d);
    }
}
