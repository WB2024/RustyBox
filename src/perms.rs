//! Who owns the files RustyBox creates, and who may change them.
//!
//! The container runs as root (it has to, to reach the drive and mount sticks), so anything it
//! creates would otherwise be root-only. Three settings fix that, read from the environment:
//!
//! * `RUSTYBOX_PUID` / `RUSTYBOX_PGID`: the owner and group new files are given.
//! * `RUSTYBOX_UMASK`: which permission bits to withhold (default `002` once an owner is set,
//!   so the owner and group can write and others can read).
//!
//! `PUID`, `PGID` and `UMASK` (the names many containers use) work too.

use std::{
    os::unix::fs::{MetadataExt, PermissionsExt, chown, lchown},
    path::Path,
    sync::OnceLock,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub umask: Option<u32>,
}

impl Config {
    pub fn is_set(&self) -> bool {
        self.uid.is_some() || self.gid.is_some() || self.umask.is_some()
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(format!("RUSTYBOX_{name}"))
        .ok()
        .or_else(|| std::env::var(name).ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn parse(uid: Option<&str>, gid: Option<&str>, umask: Option<&str>) -> Config {
    let id = |v: Option<&str>| v.and_then(|s| s.parse::<u32>().ok());
    let uid = id(uid);
    let gid = id(gid);
    let umask = umask
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0o"), 8).ok())
        .filter(|m| *m <= 0o777)
        .or(if uid.is_some() || gid.is_some() {
            Some(0o002)
        } else {
            None
        });
    Config { uid, gid, umask }
}

pub fn config() -> Config {
    static C: OnceLock<Config> = OnceLock::new();
    *C.get_or_init(|| {
        parse(
            env("PUID").as_deref(),
            env("PGID").as_deref(),
            env("UMASK").as_deref(),
        )
    })
}

/// Apply the umask to this process, so every file it (and ffmpeg, cdparanoia, xorriso…) creates
/// gets the right permissions. Call once at startup.
pub fn init() {
    if let Some(m) = config().umask {
        // SAFETY: umask only changes this process's file-creation mask.
        unsafe { libc::umask(m as libc::mode_t) };
    }
}

fn wanted_owner(meta: &std::fs::Metadata) -> Option<(Option<u32>, Option<u32>)> {
    let c = config();
    let uid = c.uid.filter(|u| *u != meta.uid());
    let gid = c.gid.filter(|g| *g != meta.gid());
    (uid.is_some() || gid.is_some()).then_some((uid, gid))
}

/// Give one path the configured owner (no-op when nothing is configured or it already matches).
pub fn own(path: &Path) {
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && let Some((u, g)) = wanted_owner(&meta)
    {
        let _ = lchown(path, u, g);
    }
}

/// Own `path` and every folder above it that doesn't have the right owner yet (folders created
/// on the way to a new file), stopping at the first one that is already right.
pub fn own_parents(path: &Path) {
    let mut p = path.parent();
    while let Some(d) = p {
        let Ok(meta) = std::fs::symlink_metadata(d) else {
            break;
        };
        if wanted_owner(&meta).is_none() {
            break;
        }
        own(d);
        p = d.parent();
    }
}

/// Own everything under `path` (and `path` itself). Returns how many items changed.
pub fn own_tree(path: &Path) -> usize {
    if !config().is_set() {
        return 0;
    }
    walk(path, false)
}

/// Like `own_tree`, and also widen permissions to what the umask allows (never narrows).
pub fn fix_tree(path: &Path) -> usize {
    walk(path, true)
}

fn walk(path: &Path, widen: bool) -> usize {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 0;
    }
    let mut changed = 0;
    if let Some((u, g)) = wanted_owner(&meta)
        && chown(path, u, g).is_ok()
    {
        changed += 1;
    }
    if widen && let Some(mask) = config().umask {
        let base = if meta.is_dir() { 0o777 } else { 0o666 };
        let want = meta.mode() & 0o7777 | (base & !mask);
        if want != meta.mode() & 0o7777
            && std::fs::set_permissions(path, std::fs::Permissions::from_mode(want)).is_ok()
        {
            changed += 1;
        }
    }
    if meta.is_dir()
        && let Ok(rd) = std::fs::read_dir(path)
    {
        for e in rd.flatten() {
            changed += walk(&e.path(), widen);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_set_means_nothing_happens() {
        assert_eq!(
            parse(None, None, None),
            Config {
                uid: None,
                gid: None,
                umask: None
            }
        );
    }

    #[test]
    fn an_owner_brings_a_group_writable_default() {
        let c = parse(Some("1000"), Some("100"), None);
        assert_eq!(
            (c.uid, c.gid, c.umask),
            (Some(1000), Some(100), Some(0o002))
        );
    }

    #[test]
    fn umask_is_octal_and_validated() {
        assert_eq!(parse(None, None, Some("022")).umask, Some(0o022));
        assert_eq!(parse(None, None, Some("0000")).umask, Some(0));
        assert_eq!(parse(None, None, Some("999")).umask, None);
    }

    #[test]
    fn widening_never_narrows() {
        // exercise the mode arithmetic used by `walk`
        let mask = 0o002u32;
        let widened = 0o755u32 | (0o777 & !mask);
        assert_eq!(widened, 0o775 | 0o755);
        assert_eq!(0o600u32 | (0o666 & !mask), 0o664 | 0o600);
    }
}
