//! File operations on a folder tree, kept apart from the web layer so the same careful code
//! serves RustyBox's own libraries and the agent that exposes a drive on another machine.
//!
//! Everything takes a *root* and a path *relative to it*, and refuses anything that would leave
//! the root (`..`, absolute paths, symlinks pointing elsewhere).

use std::{
    collections::HashSet,
    ffi::OsString,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

use axum::body::Bytes;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::{error::Error, library::health, perms};

/// The largest piece accepted in one request.
pub const MAX_CHUNK: usize = 64 * 1024 * 1024;

/// FAT32 can't hold a single file of 4 GiB or more.
pub const FAT32_MAX_FILE: u64 = 4 * 1024 * 1024 * 1024 - 1;

/// Join a relative path onto `root`, refusing `..`, absolute paths and empty names.
pub fn join_rel(root: &Path, rel: &str) -> Result<PathBuf, Error> {
    let rel = rel.trim_start_matches('/');
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err(Error::validation(format!("Invalid path: {rel}"))),
        }
    }
    Ok(root.join(rel))
}

/// A path to write to: no empty, hidden or oversize parts, and not a `.part` file.
pub fn valid_write_rel(rel: &str) -> Result<String, Error> {
    let rel = rel.trim().trim_matches('/');
    if rel.is_empty()
        || rel.len() > 1024
        || rel.split('/').any(|c| {
            c.is_empty() || c.starts_with('.') || c.len() > 255 || c.chars().any(char::is_control)
        })
    {
        return Err(Error::validation("That file name isn't allowed"));
    }
    if rel.ends_with(".part") {
        return Err(Error::validation("File names can't end in .part"));
    }
    Ok(rel.to_string())
}

/// An existing path, with symlinks resolved, that must still be inside `root`.
pub fn contained(root: &Path, rel: &str) -> Result<PathBuf, Error> {
    let full = join_rel(root, rel)?;
    let (canon_root, canon) = (
        std::fs::canonicalize(root)?,
        std::fs::canonicalize(&full)
            .map_err(|_| Error::not_found(format!("{rel} doesn't exist")))?,
    );
    if !canon.starts_with(&canon_root) {
        return Err(Error::validation("That path leads outside the folder"));
    }
    Ok(canon)
}

/// Replace `path` with `bytes`, readable by its owner only from the first byte, and never half
/// written: a new file with those permissions is written, flushed to disk, then renamed over it.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut s: OsString = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

// ── Information about the filesystem ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FsInfo {
    pub fs_type: String,
    pub total: u64,
    pub free: u64,
    /// The biggest single file the filesystem can hold, if it has a limit.
    pub max_file: Option<u64>,
}

/// The filesystem type of the mount holding `path`, from `/proc/self/mountinfo`.
pub fn fs_type_of(path: &Path) -> String {
    let Ok(canon) = std::fs::canonicalize(path) else {
        return "unknown".into();
    };
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return "unknown".into();
    };
    let mut best: Option<(usize, String)> = None;
    for line in text.lines() {
        // id parent major:minor root mountpoint options [optional fields] - fstype source superopts
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let mount = left.split(' ').nth(4).map(unescape).unwrap_or_default();
        let fstype = right.split(' ').next().unwrap_or("").to_string();
        if canon.starts_with(&mount) && best.as_ref().is_none_or(|(len, _)| mount.len() >= *len) {
            best = Some((mount.len(), fstype));
        }
    }
    best.map(|(_, t)| t).unwrap_or_else(|| "unknown".into())
}

/// `\040` style escapes used for spaces and other odd characters in mount points.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

pub fn max_file_for(fs_type: &str) -> Option<u64> {
    matches!(fs_type, "vfat" | "msdos" | "fat" | "fat32").then_some(FAT32_MAX_FILE)
}

pub fn fs_info(root: &Path) -> FsInfo {
    let fs_type = fs_type_of(root);
    let (total, free) = health::space(root).unwrap_or((0, 0));
    FsInfo {
        max_file: max_file_for(&fs_type),
        fs_type,
        total,
        free,
    }
}

// ── Listing ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

/// A directory entry's metadata, following a symlink to its target. For an ordinary entry this
/// avoids a second path lookup, which adds up on a network share with thousands of files.
pub fn entry_meta(e: &std::fs::DirEntry) -> std::io::Result<std::fs::Metadata> {
    match e.file_type() {
        Ok(t) if !t.is_symlink() => e.metadata(),
        _ => std::fs::metadata(e.path()),
    }
}

pub fn list_dir(root: &Path, rel: &str) -> Result<Vec<Entry>, Error> {
    let dir = contained(root, rel)?;
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir)?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        // Skip hidden and half-written files, and anything we can't stat.
        if name.starts_with('.') || name.ends_with(".part") {
            continue;
        }
        let Ok(m) = entry_meta(&e) else {
            continue;
        };
        let mtime = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(Entry {
            name,
            is_dir: m.is_dir(),
            size: if m.is_dir() { 0 } else { m.len() },
            mtime,
        });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

// ── Chunked, resumable writes ────────────────────────────────────────────────

/// Files that currently have an upload in progress, so two requests never write the same one.
#[derive(Default)]
pub struct Locks(Mutex<HashSet<PathBuf>>);

pub struct LockGuard<'a>(&'a Locks, PathBuf);

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        self.0
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.1);
    }
}

impl Locks {
    pub fn acquire(&self, path: &Path) -> Option<LockGuard<'_>> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_path_buf())
            .then(|| LockGuard(self, path.to_path_buf()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WriteStatus {
    /// How many bytes of this file are already there (resume from here).
    pub offset: u64,
    pub exists: bool,
    pub free: Option<u64>,
}

pub async fn write_status(root: &Path, rel: &str) -> Result<WriteStatus, Error> {
    let rel = valid_write_rel(rel)?;
    let dest = join_rel(root, &rel)?;
    Ok(WriteStatus {
        offset: tokio::fs::metadata(part_path(&dest))
            .await
            .map(|m| m.len())
            .unwrap_or(0),
        exists: tokio::fs::metadata(&dest).await.is_ok(),
        free: health::space(root).map(|s| s.1),
    })
}

#[derive(Debug)]
pub struct ChunkResult {
    pub done: bool,
    pub offset: u64,
}

/// Write one piece of a file at `offset`. The data goes to `<name>.part`, which is renamed to the
/// real name when the last byte arrives, so a half-sent file is never visible under its real name.
pub async fn write_chunk<St, E>(
    locks: &Locks,
    root: &Path,
    rel: &str,
    offset: u64,
    total: u64,
    overwrite: bool,
    body: St,
) -> Result<ChunkResult, Error>
where
    St: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    if offset > total {
        return Err(Error::validation("offset is past the end of the file"));
    }
    let rel = valid_write_rel(rel)?;
    let dest = join_rel(root, &rel)?;
    let part = part_path(&dest);
    let _lock = locks.acquire(&part).ok_or_else(|| {
        Error::coded(
            409,
            "UPLOAD_IN_PROGRESS",
            "That file is already being uploaded",
        )
    })?;

    let have = tokio::fs::metadata(&part)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    if offset != have {
        return Err(Error::coded(
            409,
            "OFFSET_MISMATCH",
            format!(
                "The server has {have} bytes of this file, not {offset}. Ask for the upload status and continue from there."
            ),
        ));
    }
    if offset == 0 {
        if tokio::fs::metadata(&dest).await.is_ok() && !overwrite {
            return Err(Error::coded(
                409,
                "EXISTS",
                "A file with that name is already there",
            ));
        }
        let info = fs_info(root);
        if let Some(max) = info.max_file
            && total > max
        {
            return Err(Error::coded(
                413,
                "FILE_TOO_BIG",
                format!(
                    "This drive is {} and can't hold a file of {total} bytes (the limit is 4 GiB).",
                    info.fs_type.to_uppercase()
                ),
            ));
        }
        if info.free < total {
            return Err(Error::coded(
                507,
                "NO_SPACE",
                format!(
                    "Not enough free space: the file is {total} bytes and only {} are free.",
                    info.free
                ),
            ));
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
            // A symlinked folder must not lead out of the root.
            let (canon_parent, canon_root) = (
                tokio::fs::canonicalize(parent).await?,
                tokio::fs::canonicalize(root).await?,
            );
            if !canon_parent.starts_with(&canon_root) {
                return Err(Error::validation("That path leads outside the folder"));
            }
            let p = part.clone();
            tokio::task::spawn_blocking(move || perms::own_parents(&p))
                .await
                .ok();
        }
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(offset > 0)
        .truncate(offset == 0)
        .open(&part)
        .await?;
    if offset == 0 {
        let p = part.clone();
        tokio::task::spawn_blocking(move || perms::own(&p))
            .await
            .ok();
    }
    let mut written = 0u64;
    let mut body = body;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| Error::validation(format!("Upload interrupted: {e}")))?;
        written += chunk.len() as u64;
        if offset + written > total {
            file.set_len(offset).await?;
            return Err(Error::validation(
                "More data arrived than the file's declared size",
            ));
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;

    let now = offset + written;
    if now < total {
        return Ok(ChunkResult {
            done: false,
            offset: now,
        });
    }
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&part, &dest).await?;
    let d = dest.clone();
    tokio::task::spawn_blocking(move || perms::own(&d))
        .await
        .ok();
    Ok(ChunkResult {
        done: true,
        offset: now,
    })
}

// ── Other changes ────────────────────────────────────────────────────────────

pub fn make_dir(root: &Path, rel: &str) -> Result<(), Error> {
    let rel = valid_write_rel(rel)?;
    let dir = join_rel(root, &rel)?;
    std::fs::create_dir_all(&dir)?;
    let (canon, canon_root) = (std::fs::canonicalize(&dir)?, std::fs::canonicalize(root)?);
    if !canon.starts_with(&canon_root) {
        let _ = std::fs::remove_dir(&dir);
        return Err(Error::validation("That path leads outside the folder"));
    }
    perms::own_parents(&dir);
    perms::own(&dir);
    Ok(())
}

/// Rename within the root. The target must not exist.
pub fn rename(root: &Path, from: &str, to: &str) -> Result<(), Error> {
    let src = contained(root, from)?;
    let to = valid_write_rel(to)?;
    let dest = join_rel(root, &to)?;
    // On FAT, `alan wake` and `Alan Wake` are the same folder: a rename that only changes the
    // case is fine, anything else already there is in the way.
    let same_file = from.trim_matches('/') != to
        && from.trim_matches('/').eq_ignore_ascii_case(&to)
        && dest.exists()
        && std::fs::canonicalize(&dest).is_ok_and(|d| d == src);
    if dest.exists() && !same_file {
        return Err(Error::coded(
            409,
            "EXISTS",
            "Something with that name is already there",
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
        if !std::fs::canonicalize(parent)?.starts_with(std::fs::canonicalize(root)?) {
            return Err(Error::validation("That path leads outside the folder"));
        }
    }
    std::fs::rename(src, &dest)?;
    perms::own_parents(&dest);
    Ok(())
}

/// Remove a file, or a folder and everything in it. The root itself can never be removed.
pub fn remove(root: &Path, rel: &str) -> Result<(), Error> {
    let target = contained(root, rel)?;
    if target == std::fs::canonicalize(root)? {
        return Err(Error::validation("The top folder can't be removed"));
    }
    if target.is_dir() {
        std::fs::remove_dir_all(&target)?
    } else {
        std::fs::remove_file(&target)?
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rustybox_fsops_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(&d).unwrap()
    }

    fn body(parts: &[&[u8]]) -> impl Stream<Item = Result<Bytes, String>> + Unpin {
        stream::iter(
            parts
                .iter()
                .map(|p| Ok(Bytes::copy_from_slice(p)))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn paths_cannot_leave_the_root() {
        let root = tmp("paths");
        assert!(join_rel(&root, "../x").is_err() && join_rel(&root, "a/../../x").is_err());
        assert_eq!(join_rel(&root, "/a/b").unwrap(), root.join("a/b"));
        std::fs::write(root.join("f.txt"), b"x").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        assert!(contained(&root, "f.txt").is_ok());
        std::os::unix::fs::symlink("/etc", root.join("sub/out")).unwrap();
        assert!(
            contained(&root, "sub/out").is_err(),
            "a symlink out of the root is refused"
        );
        assert!(contained(&root, "missing").is_err());
        for bad in ["", ".hidden", "a//b", "x.part", "../x"] {
            assert!(valid_write_rel(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn writes_resume_and_complete_atomically() {
        let root = tmp("write");
        let locks = Locks::default();
        let r = write_chunk(&locks, &root, "a/b.bin", 0, 10, false, body(&[b"01234"]))
            .await
            .unwrap();
        assert_eq!((r.done, r.offset), (false, 5));
        assert!(root.join("a/b.bin.part").exists() && !root.join("a/b.bin").exists());
        assert_eq!(write_status(&root, "a/b.bin").await.unwrap().offset, 5);
        let e = write_chunk(&locks, &root, "a/b.bin", 3, 10, false, body(&[b"x"]))
            .await
            .unwrap_err();
        assert!(matches!(&e, Error::Coded { code, status: 409, .. } if code == "OFFSET_MISMATCH"));
        let r = write_chunk(
            &locks,
            &root,
            "a/b.bin",
            5,
            10,
            false,
            body(&[b"56", b"789"]),
        )
        .await
        .unwrap();
        assert!(r.done);
        assert_eq!(std::fs::read(root.join("a/b.bin")).unwrap(), b"0123456789");
        assert!(!root.join("a/b.bin.part").exists());
        // Replacing needs permission; too much data is refused.
        let e = write_chunk(&locks, &root, "a/b.bin", 0, 3, false, body(&[b"abc"]))
            .await
            .unwrap_err();
        assert!(matches!(&e, Error::Coded { code, .. } if code == "EXISTS"));
        assert!(
            write_chunk(&locks, &root, "a/b.bin", 0, 3, true, body(&[b"abc"]))
                .await
                .unwrap()
                .done
        );
        assert!(
            write_chunk(&locks, &root, "c.bin", 0, 2, false, body(&[b"toolong"]))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn two_writers_cannot_share_a_file() {
        let root = tmp("lock");
        let locks = Locks::default();
        let _held = locks.acquire(&part_path(&root.join("x.bin"))).unwrap();
        let e = write_chunk(&locks, &root, "x.bin", 0, 1, false, body(&[b"a"]))
            .await
            .unwrap_err();
        assert!(matches!(&e, Error::Coded { code, .. } if code == "UPLOAD_IN_PROGRESS"));
    }

    #[test]
    fn rename_and_remove_stay_inside() {
        let root = tmp("ops");
        make_dir(&root, "Games/Alan Wake").unwrap();
        std::fs::write(root.join("Games/Alan Wake/f"), b"x").unwrap();
        rename(&root, "Games/Alan Wake", "Games/Better/Alan Wake").unwrap();
        assert!(root.join("Games/Better/Alan Wake/f").exists());
        assert!(rename(&root, "Games/Better", "Games/Better").is_err());
        // Only the case changes (the same folder on FAT): allowed; a different folder: not.
        rename(&root, "Games/Better", "Games/better").unwrap();
        rename(&root, "Games/better", "Games/Better").unwrap();
        assert!(remove(&root, "").is_err() && remove(&root, "../x").is_err());
        remove(&root, "Games/Better").unwrap();
        assert!(!root.join("Games/Better").exists());
        let list = list_dir(&root, "Games").unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn private_files_are_private_even_when_replacing_a_loose_one() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp("private");
        let f = root.join("secrets.json");
        // A leftover temp file that anyone can read must not leak the new content.
        std::fs::write(f.with_extension("tmp"), b"old").unwrap();
        std::fs::set_permissions(
            f.with_extension("tmp"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        write_private(&f, b"secret").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"secret");
        assert_eq!(
            std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        write_private(&f, b"newer").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"newer");
        assert!(!f.with_extension("tmp").exists());
    }

    #[test]
    fn knows_fat32_and_its_file_size_limit() {
        assert_eq!(max_file_for("vfat"), Some(FAT32_MAX_FILE));
        assert_eq!(max_file_for("ext4"), None);
        let info = fs_info(&std::env::temp_dir());
        assert!(info.total > 0 && info.fs_type != "unknown", "{info:?}");
        assert_eq!(unescape("/media/a\\040b"), "/media/a b");
    }
}
