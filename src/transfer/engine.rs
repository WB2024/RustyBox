//! Copying, linking and moving folder trees between places.

use std::{fs, os::unix::fs::MetadataExt, path::Path};

use super::loc::{Loc, TreeEntry};
use crate::{convert::Ctl, error::Error, perms};

/// The size of each piece when copying a file.
const CHUNK: u64 = 8 * 1024 * 1024;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Stats {
    pub files: usize,
    /// Files that were already there with the right size and so weren't copied again.
    pub skipped: usize,
    pub bytes: u64,
}

/// Progress as a running total of bytes handled.
pub struct Progress<'a> {
    pub ctl: &'a Ctl<'a>,
    pub total: u64,
    pub base: u64,
    pub label: &'a str,
}

impl Progress<'_> {
    fn report(&self, done: u64) {
        let f = if self.total == 0 {
            1.0
        } else {
            (self.base + done) as f32 / self.total as f32
        };
        (self.ctl.progress)(f.min(1.0), self.label);
    }
}

fn join(base: &str, rel: &str) -> String {
    match (base.trim_matches('/').is_empty(), rel.is_empty()) {
        (true, _) => rel.to_string(),
        (false, true) => base.trim_matches('/').to_string(),
        (false, false) => format!("{}/{rel}", base.trim_matches('/')),
    }
}

/// Copy one file, resuming a half-written copy and leaving one that is already complete alone.
struct Copy<'a, 'b> {
    src: &'a Loc,
    dst: &'a Loc,
    overwrite: bool,
    prog: &'a Progress<'b>,
}

fn copy_file(
    c: &Copy,
    spath: &str,
    dpath: &str,
    e: &TreeEntry,
    done: &mut u64,
    stats: &mut Stats,
) -> Result<(), Error> {
    let (src, dst, overwrite, prog) = (c.src, c.dst, c.overwrite, c.prog);
    let size = e.size;
    let (have, exists) = dst.write_state(dpath)?;
    if exists {
        let same = dst
            .stat(dpath)?
            .is_some_and(|s| !s.is_dir && s.size == size);
        if same && !overwrite {
            stats.skipped += 1;
            *done += size;
            prog.report(*done);
            return Ok(());
        }
        if !overwrite {
            return Err(Error::coded(
                409,
                "EXISTS",
                format!("{dpath} is already there with a different size"),
            ));
        }
    }
    let mut offset = if exists { 0 } else { have.min(size) };
    *done += offset;
    let mtime = (e.mtime > 0).then_some(e.mtime);
    if size == 0 {
        dst.write(dpath, 0, 0, overwrite, &[], mtime)?;
    }
    while offset < size {
        prog.ctl.check()?;
        let len = CHUNK.min(size - offset);
        let data = src.read(spath, offset, len)?;
        if data.len() as u64 != len {
            return Err(Error::backend(format!(
                "{spath} ended early: expected {len} bytes at {offset}, got {}",
                data.len()
            )));
        }
        dst.write(dpath, offset, size, overwrite, &data, mtime)?;
        offset += len;
        *done += len;
        prog.report(*done);
    }
    match dst.stat(dpath)? {
        Some(s) if !s.is_dir && s.size == size => {}
        other => {
            return Err(Error::backend(format!(
                "{dpath} didn't come out right (expected {size} bytes, found {other:?})"
            )));
        }
    }
    stats.files += 1;
    stats.bytes += size;
    Ok(())
}

/// The total size of the files in a tree.
pub fn tree_bytes(tree: &[TreeEntry]) -> u64 {
    tree.iter().filter(|e| !e.is_dir).map(|e| e.size).sum()
}

/// Copy `srel` (a file or a folder) to `drel`. Already-complete files are skipped, so running it
/// again after an interruption carries on where it stopped.
pub fn copy_tree(
    src: &Loc,
    srel: &str,
    dst: &Loc,
    drel: &str,
    overwrite: bool,
    prog: &Progress,
) -> Result<Stats, Error> {
    let tree = src.tree(srel)?;
    let is_file = tree.len() == 1 && tree[0].rel.is_empty();
    let c = Copy {
        src,
        dst,
        overwrite,
        prog,
    };
    let mut stats = Stats::default();
    let mut done = 0u64;
    if is_file {
        copy_file(&c, srel, drel, &tree[0], &mut done, &mut stats)?;
        return Ok(stats);
    }
    dst.mkdir(drel)?;
    for e in tree.iter().filter(|e| e.is_dir) {
        dst.mkdir(&join(drel, &e.rel))?;
    }
    for e in tree.iter().filter(|e| !e.is_dir) {
        copy_file(
            &c,
            &join(srel, &e.rel),
            &join(drel, &e.rel),
            e,
            &mut done,
            &mut stats,
        )?;
    }
    Ok(stats)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LinkKind {
    Hard,
    Soft,
}

fn link_error(kind: LinkKind, e: std::io::Error) -> Error {
    match e.raw_os_error() {
        Some(libc::EXDEV) => Error::validation(
            "Hard links only work within one filesystem, and these folders are on different ones. Use copy or move instead.",
        ),
        Some(libc::EPERM) | Some(libc::ENOTSUP) => Error::validation(format!(
            "This filesystem doesn't support {} (FAT32 and exFAT don't). Use copy or move instead.",
            if kind == LinkKind::Hard {
                "hard links"
            } else {
                "symbolic links"
            }
        )),
        _ => e.into(),
    }
}

/// Make `dst` look like `src` without copying the data: real folders, with each file a hard link
/// (or a symlink to the original). Symlinks are made per file, not per folder, so the library still
/// sees a normal folder. Both ends must be on this server.
pub fn link_tree(
    src_root: &Path,
    srel: &str,
    dst_root: &Path,
    drel: &str,
    kind: LinkKind,
    overwrite: bool,
    ctl: &Ctl,
) -> Result<Stats, Error> {
    let src = Loc::Local(src_root.to_path_buf());
    let tree = src.tree(srel)?;
    let is_file = tree.len() == 1 && tree[0].rel.is_empty();
    let src_base = crate::fsops::contained(src_root, srel)?;
    let dst_base = crate::fsops::join_rel(dst_root, drel)?;
    let mut stats = Stats::default();
    let one = |from: &Path, to: &Path, stats: &mut Stats| -> Result<(), Error> {
        ctl.check()?;
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
            perms::own_parents(to);
        }
        if fs::symlink_metadata(to).is_ok() {
            let same = match kind {
                LinkKind::Hard => fs::metadata(to)
                    .ok()
                    .zip(fs::metadata(from).ok())
                    .is_some_and(|(a, b)| a.ino() == b.ino() && a.dev() == b.dev()),
                LinkKind::Soft => fs::read_link(to).is_ok_and(|t| t == from),
            };
            if same {
                stats.skipped += 1;
                return Ok(());
            }
            if !overwrite {
                return Err(Error::coded(
                    409,
                    "EXISTS",
                    format!("{} is already there", to.display()),
                ));
            }
            fs::remove_file(to)?;
        }
        match kind {
            LinkKind::Hard => fs::hard_link(from, to),
            LinkKind::Soft => std::os::unix::fs::symlink(from, to),
        }
        .map_err(|e| link_error(kind, e))?;
        stats.files += 1;
        Ok(())
    };
    if is_file {
        one(&src_base, &dst_base, &mut stats)?;
        return Ok(stats);
    }
    fs::create_dir_all(&dst_base)?;
    for e in tree.iter().filter(|e| e.is_dir) {
        fs::create_dir_all(dst_base.join(&e.rel))?;
    }
    perms::own_tree(&dst_base);
    for e in tree.iter().filter(|e| !e.is_dir) {
        one(&src_base.join(&e.rel), &dst_base.join(&e.rel), &mut stats)?;
        stats.bytes += e.size;
    }
    Ok(stats)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Moved {
    /// Same filesystem: the folder was renamed, so it was instant and nothing was copied.
    Renamed,
    /// Copied, checked, and then the original was removed.
    Copied,
}

/// Move `srel` to `drel`. On one filesystem that is a rename; otherwise everything is copied, every
/// file is checked to have arrived at the right size, and only then is the original removed.
pub fn move_tree(
    src: &Loc,
    srel: &str,
    dst: &Loc,
    drel: &str,
    overwrite: bool,
    prog: &Progress,
) -> Result<Moved, Error> {
    if srel.trim_matches('/').is_empty() {
        return Err(Error::validation("The top folder can't be moved"));
    }
    // On one filesystem, and with nothing in the way, a move is a rename. If the destination folder
    // already exists (another disc of the same game, say) the contents are merged by copying below.
    if let (Loc::Local(sr), Loc::Local(dr)) = (src, dst)
        && src.device(srel).is_some()
        && src.device(srel) == dst.device(drel)
        && !dst.local_path(drel).is_some_and(|p| p.exists())
    {
        let from = crate::fsops::contained(sr, srel)?;
        let to = crate::fsops::join_rel(dr, drel)?;
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
            if !fs::canonicalize(parent)?.starts_with(fs::canonicalize(dr)?) {
                return Err(Error::validation("That path leads outside the folder"));
            }
            perms::own_parents(&to);
        }
        fs::rename(&from, &to)?;
        (prog.ctl.progress)(1.0, prog.label);
        return Ok(Moved::Renamed);
    }
    let before = src.tree(srel)?;
    copy_tree(src, srel, dst, drel, overwrite, prog)?;
    // Check everything arrived before the original goes.
    let is_file = before.len() == 1 && before[0].rel.is_empty();
    for e in before.iter().filter(|e| !e.is_dir) {
        let path = if is_file {
            drel.to_string()
        } else {
            join(drel, &e.rel)
        };
        match dst.stat(&path)? {
            Some(s) if !s.is_dir && s.size == e.size => {}
            other => {
                return Err(Error::backend(format!(
                    "{path} didn't arrive intact ({other:?}); the original was left alone"
                )));
            }
        }
    }
    src.delete(srel)?;
    Ok(Moved::Copied)
}

/// Where a path on the server is, for messages.
pub fn show(p: &Path) -> String {
    p.to_string_lossy().to_string()
}
