//! Scanning a drive that can only be read through a [`Remote`] and not scanned on its own side:
//! one shared from a web browser. It does the same walk as `scan`, but with folder listings and
//! ranged reads of just the bytes the headers are in, so a game is never read whole.

use std::{collections::HashMap, io, sync::Mutex};

use super::scan::{Existing, Found, MetaState, Scanner};
use crate::{
    error::Error,
    fsops::Entry,
    remote::Remote,
    xbox::{ReadAt, meta},
};

const MAX_DEPTH: usize = 12;
const BLOCK: u64 = 256 * 1024;

fn is_hex8(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn join(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{rel}/{name}")
    }
}

/// A file on the drive, read in blocks as the parsers ask for bytes.
struct RemoteFile<'a> {
    remote: &'a Remote,
    rel: &'a str,
    len: u64,
    blocks: Mutex<HashMap<u64, Vec<u8>>>,
}

impl ReadAt for RemoteFile<'_> {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|e| *e <= self.len)
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        let mut at = offset;
        while at < end {
            let b = at / BLOCK;
            let data = {
                let mut cache = self.blocks.lock().unwrap_or_else(|e| e.into_inner());
                if cache.len() > 64 {
                    cache.clear();
                }
                match cache.get(&b) {
                    Some(d) => d.clone(),
                    None => {
                        let start = b * BLOCK;
                        let want = BLOCK.min(self.len - start);
                        let mut d = Vec::with_capacity(want as usize);
                        io::Read::read_to_end(
                            &mut self
                                .remote
                                .read(self.rel, start, Some(want))
                                .map_err(io::Error::other)?,
                            &mut d,
                        )?;
                        if (d.len() as u64) < want.min(end - start) {
                            return Err(io::ErrorKind::UnexpectedEof.into());
                        }
                        cache.insert(b, d.clone());
                        d
                    }
                }
            };
            let from = (at - b * BLOCK) as usize;
            let n = ((end - at) as usize).min(data.len().saturating_sub(from));
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            buf[(at - offset) as usize..(at - offset) as usize + n]
                .copy_from_slice(&data[from..from + n]);
            at += n as u64;
        }
        Ok(())
    }
}

/// Total size and newest modification time of everything under `rel`.
fn totals(r: &Remote, rel: &str) -> (u64, i64) {
    let (mut size, mut newest) = (0u64, 0i64);
    for e in r.list(rel).unwrap_or_default() {
        if e.is_dir {
            let (s, t) = totals(r, &join(rel, &e.name));
            size += s;
            newest = newest.max(t);
        } else {
            size += e.size;
            newest = newest.max(e.mtime);
        }
    }
    (size, newest)
}

fn visit(
    r: &Remote,
    rel: &str,
    depth: usize,
    scanner: Scanner,
    out: &mut Vec<Found>,
) -> Result<(), Error> {
    let mut entries = match r.list(rel) {
        Ok(e) => e,
        // The top folder must be readable; a sub-folder that isn't is skipped.
        Err(e) if depth == 0 => return Err(e),
        Err(_) => return Ok(()),
    };
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    for e in entries {
        let relpath = join(rel, &e.name);
        if e.is_dir {
            let is_title = scanner == Scanner::God
                && is_hex8(&e.name)
                && r.list(&relpath)
                    .is_ok_and(|l| l.iter().any(|x| x.is_dir && is_hex8(&x.name)));
            if is_title {
                let (size, mtime) = totals(r, &relpath);
                let parent = rel
                    .rsplit('/')
                    .next()
                    .filter(|p| !p.is_empty() && !is_hex8(p));
                let title = e.name.to_uppercase();
                out.push(Found {
                    relpath,
                    name: parent.unwrap_or(&title).to_string(),
                    kind: "god".into(),
                    size,
                    mtime,
                    title_id: Some(title),
                    meta: MetaState::Keep,
                });
            } else if depth < MAX_DEPTH {
                visit(r, &relpath, depth + 1, scanner, out)?;
            }
            continue;
        }
        let item = match scanner {
            Scanner::Iso if e.name.to_lowercase().ends_with(".iso") => Some(Found {
                relpath,
                name: e.name[..e.name.len() - 4].to_string(),
                kind: "iso".into(),
                size: e.size,
                mtime: e.mtime,
                title_id: None,
                meta: MetaState::Keep,
            }),
            Scanner::Files => Some(Found {
                relpath,
                name: e.name.clone(),
                kind: "file".into(),
                size: e.size,
                mtime: e.mtime,
                title_id: None,
                meta: MetaState::Keep,
            }),
            _ => None,
        };
        out.extend(item);
    }
    Ok(())
}

/// The container file of a GOD title folder, as `meta::find_god_container` picks it, with its size.
fn god_container(r: &Remote, title: &str) -> Result<(String, Entry), String> {
    let mut best: Vec<(u8, String, Entry)> = Vec::new();
    for ct in r.list(title).map_err(|e| e.to_string())? {
        if !ct.is_dir || ct.name.len() != 8 {
            continue;
        }
        let dir = join(title, &ct.name);
        for f in r.list(&dir).map_err(|e| e.to_string())? {
            if !f.is_dir && !f.name.contains('.') {
                best.push((meta::god_rank(&ct.name), join(&dir, &f.name), f));
            }
        }
    }
    best.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    best.into_iter()
        .next()
        .map(|(_, p, e)| (p, e))
        .ok_or_else(|| "No container file found (expected <content type>/<container>)".to_string())
}

fn read_god(r: &Remote, title: &str) -> Result<meta::Meta, String> {
    let (path, entry) = god_container(r, title)?;
    let ct = path
        .rsplit('/')
        .nth(1)
        .and_then(|n| u32::from_str_radix(n, 16).ok());
    let file = RemoteFile {
        remote: r,
        rel: &path,
        len: entry.size,
        blocks: Default::default(),
    };
    let sizes: Option<Vec<u64>> = meta::god_has_data_folder(ct).then(|| {
        let by_name: HashMap<String, u64> = r
            .list(&format!("{path}.data"))
            .unwrap_or_default()
            .into_iter()
            .filter(|e| !e.is_dir)
            .map(|e| (e.name, e.size))
            .collect();
        let mut sizes = Vec::new();
        while let Some(s) = by_name.get(&format!("Data{:04}", sizes.len())) {
            sizes.push(*s);
        }
        sizes
    });
    let parts = meta::god_parts(&file, entry.size);
    let problems = meta::god_problem_list(entry.size, ct, sizes.as_deref(), parts);
    meta::god_meta(&file, entry.size, problems)
}

/// Scan the drive (below the remote's folder) and read the headers of new and changed games.
pub fn scan(
    r: &Remote,
    scanner: Scanner,
    existing: &HashMap<String, Existing>,
) -> Result<Vec<Found>, Error> {
    let mut found = Vec::new();
    visit(r, "", 0, scanner, &mut found)?;
    if scanner == Scanner::Files {
        return Ok(found);
    }
    for f in found.iter_mut() {
        if existing
            .get(&f.relpath)
            .is_some_and(|e| e.meta_ok && e.size == f.size as i64 && e.mtime == f.mtime)
        {
            continue;
        }
        let result = match scanner {
            Scanner::Iso => {
                let file = RemoteFile {
                    remote: r,
                    rel: &f.relpath,
                    len: f.size,
                    blocks: Default::default(),
                };
                meta::read_iso(&file)
            }
            Scanner::God => read_god(r, &f.relpath),
            Scanner::Files => continue,
        };
        f.meta = match result {
            Ok(m) => MetaState::Set(m),
            Err(e) => MetaState::Failed(e),
        };
    }
    Ok(found)
}
