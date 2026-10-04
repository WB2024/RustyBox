//! A place files can be read from or written to.

use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{FileExt, MetadataExt},
    path::{Path, PathBuf},
};

use crate::{
    error::Error,
    fsops::{self, FsInfo},
    library::LibPath,
    perms,
    remote::Remote,
};

#[derive(Clone)]
pub enum Loc {
    /// A folder on this server.
    Local(PathBuf),
    /// A folder on a drive served by an agent (already limited to its subfolder).
    Remote(Remote),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreeEntry {
    /// Relative to the folder that was listed.
    pub rel: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stat {
    pub is_dir: bool,
    pub size: u64,
}

fn mtime_of(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Loc {
    pub fn of(lp: &LibPath) -> Loc {
        match lp.remote() {
            Some(r) => Loc::Remote(r),
            None => Loc::Local(PathBuf::from(&lp.path)),
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Loc::Remote(_))
    }

    pub fn describe(&self, rel: &str) -> String {
        match self {
            Loc::Local(root) => root
                .join(rel.trim_start_matches('/'))
                .to_string_lossy()
                .to_string(),
            Loc::Remote(r) => format!("{}/{}", r.url(), rel.trim_start_matches('/')),
        }
    }

    pub fn stat(&self, rel: &str) -> Result<Option<Stat>, Error> {
        match self {
            Loc::Local(root) => {
                let p = fsops::join_rel(root, rel)?;
                match fs::metadata(&p) {
                    Ok(m) => Ok(Some(Stat {
                        is_dir: m.is_dir(),
                        size: if m.is_dir() { 0 } else { m.len() },
                    })),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
            Loc::Remote(r) => {
                let s = r.stat(rel)?;
                Ok(s.exists.then_some(Stat {
                    is_dir: s.is_dir,
                    size: s.size,
                }))
            }
        }
    }

    /// Everything below `rel` (files and folders), with paths relative to `rel`. A single file gives itself.
    pub fn tree(&self, rel: &str) -> Result<Vec<TreeEntry>, Error> {
        let Some(st) = self.stat(rel)? else {
            return Err(Error::not_found(format!("{rel} doesn't exist")));
        };
        if !st.is_dir {
            return Ok(vec![TreeEntry {
                rel: String::new(),
                is_dir: false,
                size: st.size,
                mtime: 0,
            }]);
        }
        let mut out = Vec::new();
        let mut stack = vec![String::new()];
        while let Some(dir) = stack.pop() {
            let here = match (rel.trim_matches('/').is_empty(), dir.is_empty()) {
                (true, _) => dir.clone(),
                (false, true) => rel.trim_matches('/').to_string(),
                (false, false) => format!("{}/{dir}", rel.trim_matches('/')),
            };
            let entries: Vec<(String, bool, u64, i64)> = match self {
                Loc::Local(root) => {
                    let base = fsops::join_rel(root, &here)?;
                    fs::read_dir(&base)?
                        .flatten()
                        .filter_map(|e| {
                            let name = e.file_name().to_string_lossy().to_string();
                            if name.starts_with('.') || name.ends_with(".part") {
                                return None;
                            }
                            let m = fsops::entry_meta(&e).ok()?;
                            Some((
                                name,
                                m.is_dir(),
                                if m.is_dir() { 0 } else { m.len() },
                                mtime_of(&m),
                            ))
                        })
                        .collect()
                }
                Loc::Remote(r) => r
                    .list(&here)?
                    .into_iter()
                    .map(|e| (e.name, e.is_dir, e.size, e.mtime))
                    .collect(),
            };
            for (name, is_dir, size, mtime) in entries {
                let child = if dir.is_empty() {
                    name
                } else {
                    format!("{dir}/{name}")
                };
                if is_dir {
                    stack.push(child.clone());
                }
                out.push(TreeEntry {
                    rel: child,
                    is_dir,
                    size,
                    mtime,
                });
            }
        }
        out.sort_by(|a, b| a.rel.cmp(&b.rel));
        Ok(out)
    }

    pub fn read(&self, rel: &str, offset: u64, len: u64) -> Result<Vec<u8>, Error> {
        match self {
            Loc::Local(root) => {
                let p = fsops::contained(root, rel)?;
                let f = File::open(p)?;
                let mut buf = vec![0u8; len as usize];
                let mut got = 0;
                while got < buf.len() {
                    let n = f.read_at(&mut buf[got..], offset + got as u64)?;
                    if n == 0 {
                        break;
                    }
                    got += n;
                }
                buf.truncate(got);
                Ok(buf)
            }
            Loc::Remote(r) => {
                let mut out = Vec::with_capacity(len as usize);
                r.read(rel, offset, Some(len))?
                    .take(len)
                    .read_to_end(&mut out)?;
                Ok(out)
            }
        }
    }

    /// How many bytes of `rel` are already written as a partial file, and whether the whole file exists.
    pub fn write_state(&self, rel: &str) -> Result<(u64, bool), Error> {
        match self {
            Loc::Local(root) => {
                let dest = fsops::join_rel(root, rel)?;
                Ok((
                    fs::metadata(fsops::part_path(&dest))
                        .map(|m| m.len())
                        .unwrap_or(0),
                    dest.exists(),
                ))
            }
            Loc::Remote(r) => {
                let s = r.write_status(rel)?;
                Ok((s.offset, s.exists))
            }
        }
    }

    /// Write a piece of a file; the file appears under its name when the last piece arrives.
    pub fn write(
        &self,
        rel: &str,
        offset: u64,
        total: u64,
        overwrite: bool,
        data: &[u8],
        mtime: Option<i64>,
    ) -> Result<bool, Error> {
        match self {
            Loc::Remote(r) => Ok(r.write_chunk(rel, offset, total, overwrite, data)?.0),
            Loc::Local(root) => {
                let rel = fsops::valid_write_rel(rel)?;
                let dest = fsops::join_rel(root, &rel)?;
                let part = fsops::part_path(&dest);
                if offset == 0 {
                    if dest.exists() && !overwrite {
                        return Err(Error::coded(
                            409,
                            "EXISTS",
                            "A file with that name is already there",
                        ));
                    }
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent)?;
                        if !fs::canonicalize(parent)?.starts_with(fs::canonicalize(root)?) {
                            return Err(Error::validation("That path leads outside the folder"));
                        }
                        perms::own_parents(&part);
                    }
                }
                let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                if have != offset {
                    return Err(Error::coded(
                        409,
                        "OFFSET_MISMATCH",
                        format!("{have} bytes are already written, not {offset}"),
                    ));
                }
                let mut f = fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(offset == 0)
                    .open(&part)?;
                f.seek(SeekFrom::Start(offset))?;
                f.write_all(data)?;
                if offset + data.len() as u64 >= total {
                    f.set_len(total)?;
                    f.sync_all()?;
                    if let Some(t) = mtime {
                        let _ = f.set_modified(
                            std::time::UNIX_EPOCH + std::time::Duration::from_secs(t.max(0) as u64),
                        );
                    }
                    drop(f);
                    fs::rename(&part, &dest)?;
                    perms::own(&dest);
                    return Ok(true);
                }
                Ok(false)
            }
        }
    }

    pub fn mkdir(&self, rel: &str) -> Result<(), Error> {
        match self {
            Loc::Local(root) => fsops::make_dir(root, rel),
            Loc::Remote(r) => r.mkdir(rel),
        }
    }

    pub fn delete(&self, rel: &str) -> Result<(), Error> {
        match self {
            Loc::Local(root) => fsops::remove(root, rel),
            Loc::Remote(r) => r.delete(rel),
        }
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        match self {
            Loc::Local(root) => fsops::rename(root, from, to),
            Loc::Remote(r) => r.rename(from, to),
        }
    }

    pub fn info(&self) -> Result<FsInfo, Error> {
        match self {
            Loc::Local(root) => Ok(fsops::fs_info(root)),
            Loc::Remote(r) => Ok(r.info()?.fs),
        }
    }

    /// The device a local path is on (to tell whether two places share a filesystem). `None` for remote ones.
    pub fn device(&self, rel: &str) -> Option<u64> {
        let Loc::Local(root) = self else { return None };
        // The nearest existing parent: the destination may not exist yet.
        let mut p = fsops::join_rel(root, rel).ok()?;
        loop {
            if let Ok(m) = fs::metadata(&p) {
                return Some(m.dev());
            }
            p = p.parent()?.to_path_buf();
        }
    }

    pub fn local_path(&self, rel: &str) -> Option<PathBuf> {
        match self {
            Loc::Local(root) => fsops::join_rel(root, rel).ok(),
            Loc::Remote(_) => None,
        }
    }
}

/// Where `path` is relative to `root`, as a string with `/`.
pub fn rel_of(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}
