//! Finding what is in a library path, and keeping the index in step with it.
//!
//! Scans are incremental in effect (unchanged items are left as they are) and cautious: a
//! path that cannot be read marks its items unavailable instead of deleting them, and a path
//! that suddenly looks empty (an NFS share that failed to mount leaves an empty folder) is
//! treated the same way.

use std::{
    collections::HashMap,
    fs, io,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Library, health};
use crate::{
    db::Db,
    error::Error,
    jobs::Job,
    xbox::meta::{self, Meta},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scanner {
    /// Every `.iso` file is an item.
    Iso,
    /// Every Games on Demand title folder (`<TitleID>/<content type>/...`) is an item.
    God,
    /// Every file is an item.
    Files,
}

/// What reading a game file's header gave.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MetaState {
    /// Not read this time (nothing changed, or this kind has no headers): leave what is stored.
    Keep,
    Set(Meta),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub relpath: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub mtime: i64,
    pub title_id: Option<String>,
    pub meta: MetaState,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct Applied {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
    /// The path looked empty but had items before: nothing was deleted.
    pub suspicious_empty: bool,
}

impl Scanner {
    pub fn as_str(self) -> &'static str {
        match self {
            Scanner::Iso => "iso",
            Scanner::God => "god",
            Scanner::Files => "files",
        }
    }
}

const MAX_DEPTH: usize = 12;

fn is_hex8(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn hidden(name: &str) -> bool {
    name.starts_with('.') || name.ends_with(".part")
}

pub fn mtime_secs(m: &fs::Metadata) -> i64 {
    mtime_of(m)
}

fn mtime_of(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Total size and newest modification time of everything under `dir`.
fn dir_totals(dir: &Path) -> (u64, i64) {
    let (mut size, mut newest) = (0u64, 0i64);
    let Ok(rd) = fs::read_dir(dir) else {
        return (0, 0);
    };
    for e in rd.flatten() {
        // The entry itself says what it is and, with one `fstatat`, how big: much cheaper than a
        // `stat` of its full path for each of the hundreds of files in a game, on a network share.
        // A symlink is followed to its target.
        let Ok(ft) = e.file_type() else { continue };
        let m = if ft.is_symlink() {
            fs::metadata(e.path())
        } else {
            e.metadata()
        };
        let Ok(m) = m else { continue };
        if m.is_dir() {
            let (s, t) = dir_totals(&e.path());
            size += s;
            newest = newest.max(t);
        } else {
            size += m.len();
            newest = newest.max(mtime_of(&m));
        }
    }
    (size, newest)
}

/// Is `dir` a GOD title folder: an 8-hex-digit name holding an 8-hex-digit content-type folder.
fn is_god_title(dir: &Path, name: &str) -> bool {
    is_hex8(name)
        && fs::read_dir(dir).is_ok_and(|rd| {
            rd.flatten().any(|e| {
                e.file_type().is_ok_and(|t| t.is_dir()) && is_hex8(&e.file_name().to_string_lossy())
            })
        })
}

pub struct Walk<'a> {
    pub scanner: Scanner,
    pub cancelled: &'a dyn Fn() -> bool,
    pub progress: &'a dyn Fn(usize),
}

/// Walk `root`. Fails only if the root itself can't be read; unreadable sub-folders are skipped
/// (and counted in the second value).
pub fn walk(root: &Path, w: &Walk) -> io::Result<(Vec<Found>, usize)> {
    fs::read_dir(root)?;
    let mut out = Vec::new();
    let mut skipped = 0;
    visit(root, "", 0, w, &mut out, &mut skipped)?;
    Ok((out, skipped))
}

fn visit(
    dir: &Path,
    rel: &str,
    depth: usize,
    w: &Walk,
    out: &mut Vec<Found>,
    skipped: &mut usize,
) -> io::Result<()> {
    if (w.cancelled)() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let Ok(rd) = fs::read_dir(dir) else {
        *skipped += 1;
        return Ok(());
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let fname = e.file_name().to_string_lossy().to_string();
        if hidden(&fname) {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        let relpath = if rel.is_empty() {
            fname.clone()
        } else {
            format!("{rel}/{fname}")
        };
        if ft.is_dir() {
            if w.scanner == Scanner::God && is_god_title(&e.path(), &fname) {
                let (size, mtime) = dir_totals(&e.path());
                let parent = rel
                    .rsplit('/')
                    .next()
                    .filter(|p| !p.is_empty() && !is_hex8(p));
                let title = fname.to_uppercase();
                out.push(Found {
                    relpath,
                    name: parent.unwrap_or(&title).to_string(),
                    kind: "god".into(),
                    size,
                    mtime,
                    title_id: Some(title),
                    meta: MetaState::Keep,
                });
                (w.progress)(out.len());
            } else if depth < MAX_DEPTH {
                visit(&e.path(), &relpath, depth + 1, w, out, skipped)?;
            }
            continue;
        }
        // Files (a symlink to a file counts; a symlink to a folder is not followed, to avoid loops).
        let m = if ft.is_symlink() {
            fs::metadata(e.path())
        } else {
            e.metadata()
        };
        let Ok(m) = m else {
            continue;
        };
        if !m.is_file() {
            continue;
        }
        let item = match w.scanner {
            Scanner::Iso if fname.to_lowercase().ends_with(".iso") => Some(Found {
                relpath,
                name: fname[..fname.len() - 4].to_string(),
                kind: "iso".into(),
                size: m.len(),
                mtime: mtime_of(&m),
                title_id: None,
                meta: MetaState::Keep,
            }),
            Scanner::Files => Some(Found {
                relpath,
                name: fname,
                kind: "file".into(),
                size: m.len(),
                mtime: mtime_of(&m),
                title_id: None,
                meta: MetaState::Keep,
            }),
            _ => None,
        };
        if let Some(f) = item {
            out.push(f);
            (w.progress)(out.len());
        }
    }
    Ok(())
}

/// What the index already holds for an item, so unchanged files aren't read again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Existing {
    pub size: i64,
    pub mtime: i64,
    pub meta_ok: bool,
}

pub fn load_existing(conn: &Connection, path_id: i64) -> Result<HashMap<String, Existing>, Error> {
    let mut stmt =
        conn.prepare("SELECT relpath, size, mtime, meta FROM items WHERE path_id = ?1")?;
    let rows = stmt.query_map([path_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Existing {
                size: r.get(1)?,
                mtime: r.get(2)?,
                meta_ok: r.get::<_, Option<String>>(3)?.as_deref() == Some("ok"),
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Read the headers of new and changed game files (and retry ones that failed before).
/// Returns how many were read.
pub fn enrich(
    root: &Path,
    scanner: Scanner,
    found: &mut [Found],
    existing: &HashMap<String, Existing>,
    w: &Walk,
) -> io::Result<usize> {
    if scanner == Scanner::Files {
        return Ok(0);
    }
    let mut read = 0;
    for f in found.iter_mut() {
        if (w.cancelled)() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        if existing
            .get(&f.relpath)
            .is_some_and(|e| e.meta_ok && e.size == f.size as i64 && e.mtime == f.mtime)
        {
            continue;
        }
        let full = root.join(&f.relpath);
        let result = match scanner {
            Scanner::Iso => meta::read_iso_file(&full),
            Scanner::God => meta::read_god_dir(&full),
            Scanner::Files => continue,
        };
        f.meta = match result {
            Ok(m) => MetaState::Set(m),
            Err(e) => MetaState::Failed(e),
        };
        read += 1;
        (w.progress)(read);
    }
    Ok(read)
}

fn stamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Bring the index for one path in line with what the scan found.
pub fn apply(
    conn: &mut Connection,
    library_id: i64,
    path_id: i64,
    found: &[Found],
) -> Result<Applied, Error> {
    let mut existing: HashMap<String, (i64, i64)> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT relpath, size, mtime FROM items WHERE path_id = ?1")?;
        for r in stmt.query_map([path_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?),
            ))
        })? {
            let (k, v) = r?;
            existing.insert(k, v);
        }
    }
    let mut a = Applied::default();
    if found.is_empty() && !existing.is_empty() {
        conn.execute(
            "UPDATE items SET available = 0 WHERE path_id = ?1",
            [path_id],
        )?;
        a.suspicious_empty = true;
        return Ok(a);
    }
    let seen = stamp();
    let tx = conn.transaction()?;
    {
        let mut up = tx.prepare(
            "INSERT INTO items(library_id, path_id, relpath, name, kind, size, mtime, title_id, available, seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9)
             ON CONFLICT(path_id, relpath) DO UPDATE SET name = excluded.name, kind = excluded.kind, size = excluded.size,
                 mtime = excluded.mtime, available = 1, seen = excluded.seen",
        )?;
        let mut set_meta = tx.prepare(
            "UPDATE items SET title_id = ?1, media_id = ?2, disc = ?3, discs = ?4, platform = ?5, game_name = ?6, meta = 'ok', meta_error = NULL,
                 content_type = ?9, health = ?10
             WHERE path_id = ?7 AND relpath = ?8",
        )?;
        let mut fail_meta = tx.prepare(
            "UPDATE items SET title_id = ?1, media_id = NULL, disc = NULL, discs = NULL, platform = NULL, game_name = ?5, meta = 'error', meta_error = ?2,
                 content_type = NULL, health = ?6
             WHERE path_id = ?3 AND relpath = ?4",
        )?;
        for f in found {
            match existing.get(&f.relpath) {
                None => a.added += 1,
                Some(&(s, m)) if s == f.size as i64 && m == f.mtime => a.unchanged += 1,
                Some(_) => a.updated += 1,
            }
            up.execute(params![
                library_id,
                path_id,
                f.relpath,
                f.name,
                f.kind,
                f.size as i64,
                f.mtime,
                f.title_id,
                seen
            ])?;
            match &f.meta {
                MetaState::Keep => {}
                MetaState::Set(m) => {
                    set_meta.execute(params![
                        m.title_id,
                        m.media_id,
                        m.disc,
                        m.discs,
                        m.platform,
                        m.game_name,
                        path_id,
                        f.relpath,
                        m.content_type.map(|c| format!("{c:08X}")),
                        m.health
                    ])?;
                }
                MetaState::Failed(e) => {
                    // The title ID (from the folder) still names the game, and the reason is the health note.
                    let name = f
                        .title_id
                        .as_deref()
                        .and_then(|t| crate::xbox::catalog::get().name(t, None))
                        .map(str::to_string);
                    fail_meta.execute(params![f.title_id, e, path_id, f.relpath, name, e])?;
                }
            }
        }
    }
    a.removed = tx.execute(
        "DELETE FROM items WHERE path_id = ?1 AND seen <> ?2",
        params![path_id, seen],
    )?;
    tx.execute(
        "UPDATE library_paths SET last_scan = ?1 WHERE id = ?2",
        params![crate::jobs::now() as i64, path_id],
    )?;
    tx.commit()?;
    Ok(a)
}

/// Forget the index entries of a path (never any files).
pub fn clear_path(conn: &Connection, path_id: i64) -> Result<usize, Error> {
    let n = conn.execute("DELETE FROM items WHERE path_id = ?1", [path_id])?;
    conn.execute(
        "UPDATE library_paths SET last_scan = ?1 WHERE id = ?2",
        params![crate::jobs::now() as i64, path_id],
    )?;
    Ok(n)
}

pub fn mark_offline(conn: &Connection, path_id: i64) -> Result<(), Error> {
    conn.execute(
        "UPDATE items SET available = 0 WHERE path_id = ?1",
        [path_id],
    )?;
    Ok(())
}

/// The scan job body: every path of a library in turn.
pub async fn scan_library(db: Db, job: Arc<Job>, lib: Library) -> Result<(), Error> {
    let scanner = super::kind(&lib.kind)
        .map(|k| k.scanner)
        .unwrap_or(Scanner::Files);
    let total = lib.paths.len().max(1) as f32;
    let (mut added, mut updated, mut removed, mut offline) = (0, 0, 0, 0);
    for (i, p) in lib.paths.iter().enumerate() {
        let label = if p.label.is_empty() {
            p.path.clone()
        } else {
            format!("{} ({})", p.label, p.path)
        };
        job.step(format!("Scanning {label}"));
        let path_id = p.id;
        // The console's own Content folder, trainers, mods, homebrew and emulators aren't games:
        // listing their folders as GOD or ISO games would put saves and DLC in the games list.
        if matches!(scanner, Scanner::God | Scanner::Iso)
            && !matches!(p.role.as_str(), "" | "dlc" | "updates")
        {
            let n = db.run(move |c| clear_path(c, path_id)).await?;
            job.log(format!(
                "{label} is not a games folder, so it isn't listed as games{}.",
                if n > 0 {
                    format!(" ({n} earlier entries removed from the list; no files were touched)")
                } else {
                    String::new()
                }
            ));
            continue;
        }
        let h = health::check_path(p, Duration::from_secs(5)).await;
        if !h.online {
            job.log(format!(
                "{label} is offline: {}. Its items are kept and marked unavailable.",
                h.problem.unwrap_or_default()
            ));
            db.run(move |c| mark_offline(c, path_id)).await?;
            offline += 1;
            continue;
        }
        let existing = db.run(move |c| load_existing(c, path_id)).await?;
        let walked: io::Result<(Vec<Found>, usize, usize)> = if let Some(remote) = p.remote() {
            // A drive on another machine is scanned there, where the files are.
            job.step(format!("Scanning {label} on the drive agent"));
            let kind = scanner.as_str();
            tokio::task::spawn_blocking(move || {
                remote
                    .scan(kind, &existing)
                    .map(|found| {
                        let read = found
                            .iter()
                            .filter(|f| !matches!(f.meta, MetaState::Keep))
                            .count();
                        (found, 0, read)
                    })
                    .map_err(|e| io::Error::other(e.to_string()))
            })
            .await
            .map_err(|e| Error::backend(e.to_string()))?
        } else {
            let root = p.path.clone();

            let (j, token) = (job.clone(), job.cancel_token());
            tokio::task::spawn_blocking(move || {
                let cancelled = || token.is_cancelled();
                let progress = |n: usize| {
                    if n.is_multiple_of(250) {
                        j.step(format!("Scanning… {n} found"));
                    }
                };
                let (mut found, skipped) = walk(
                    Path::new(&root),
                    &Walk {
                        scanner,
                        cancelled: &cancelled,
                        progress: &progress,
                    },
                )?;
                let j2 = j.clone();
                let reading = |n: usize| {
                    if n.is_multiple_of(10) {
                        j2.step(format!("Reading game headers… {n}"));
                    }
                };
                let read = enrich(
                    Path::new(&root),
                    scanner,
                    &mut found,
                    &existing,
                    &Walk {
                        scanner,
                        cancelled: &cancelled,
                        progress: &reading,
                    },
                )?;
                Ok::<_, io::Error>((found, skipped, read))
            })
            .await
            .map_err(|e| Error::backend(e.to_string()))?
        };
        let (found, skipped, headers_read) = match walked {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(()),
            Err(e) => {
                job.log(format!(
                    "Could not read {label}: {e}. Its items are kept and marked unavailable."
                ));
                db.run(move |c| mark_offline(c, path_id)).await?;
                offline += 1;
                continue;
            }
        };
        let unreadable = found
            .iter()
            .filter(|f| matches!(f.meta, MetaState::Failed(_)))
            .count();
        if headers_read > 0 {
            job.log(format!(
                "Read {headers_read} game header(s){}",
                if unreadable > 0 {
                    format!(", {unreadable} couldn't be read (see the item for why)")
                } else {
                    String::new()
                }
            ));
        }
        if skipped > 0 {
            job.log(format!(
                "{skipped} folder(s) in {label} couldn't be read and were skipped."
            ));
        }
        let lib_id = lib.id;
        let ap = db.run(move |c| apply(c, lib_id, path_id, &found)).await?;
        if ap.suspicious_empty {
            job.log(format!("{label} looks empty but had items before. Nothing was removed; check that the disk or share is mounted."));
        }
        job.log(format!(
            "{label}: {} new, {} changed, {} unchanged, {} removed",
            ap.added, ap.updated, ap.unchanged, ap.removed
        ));
        added += ap.added;
        updated += ap.updated;
        removed += ap.removed;
        job.progress((i as f32 + 1.0) / total * 100.0);
    }
    job.result(
        "scan",
        json!({"added": added, "updated": updated, "removed": removed, "offline_paths": offline}),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn tree(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rustybox_scan_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7u8; bytes]).unwrap();
    }

    fn run(root: &Path, scanner: Scanner) -> Vec<Found> {
        walk(
            root,
            &Walk {
                scanner,
                cancelled: &|| false,
                progress: &|_| {},
            },
        )
        .unwrap()
        .0
    }

    #[test]
    fn iso_scanner_finds_isos_and_ignores_the_rest() {
        let d = tree("iso");
        put(&d.join("Halo 3.iso"), 10);
        put(&d.join("sub/Gears.ISO"), 20);
        put(&d.join("notes.txt"), 1);
        put(&d.join(".hidden.iso"), 1);
        put(&d.join("half.iso.part"), 1);
        let found = run(&d, Scanner::Iso);
        let names: Vec<_> = found
            .iter()
            .map(|f| (f.name.as_str(), f.relpath.as_str(), f.size))
            .collect();
        assert_eq!(
            names,
            vec![("Halo 3", "Halo 3.iso", 10), ("Gears", "sub/Gears.ISO", 20)]
        );
    }

    #[test]
    fn god_scanner_finds_title_folders_with_names_and_sizes() {
        let d = tree("god");
        // Name/TitleID/00007000/<container> + <container>.data/
        put(&d.join("Alan Wake/4d530805/00007000/ABC123"), 100);
        put(
            &d.join("Alan Wake/4d530805/00007000/ABC123.data/Data0000"),
            400,
        );
        // Bare title ID at the top
        put(&d.join("415607D2/00007000/X"), 50);
        // Not GOD: 8-hex name without a content-type folder
        put(&d.join("DEADBEEF/readme.txt"), 5);
        let found = run(&d, Scanner::God);
        assert_eq!(found.len(), 2);
        let aw = found
            .iter()
            .find(|f| f.title_id.as_deref() == Some("4D530805"))
            .unwrap();
        assert_eq!(
            (aw.name.as_str(), aw.size, aw.relpath.as_str()),
            ("Alan Wake", 500, "Alan Wake/4d530805")
        );
        let bare = found
            .iter()
            .find(|f| f.title_id.as_deref() == Some("415607D2"))
            .unwrap();
        assert_eq!(bare.name, "415607D2");
    }

    #[test]
    fn apply_counts_changes_and_removes_deleted_files() {
        let db = Db::open_memory().unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO libraries(id,name,kind,created) VALUES (1,'a','iso',0)",
                [],
            )?;
            c.execute(
                "INSERT INTO library_paths(id,library_id,path) VALUES (1,1,'/x')",
                [],
            )?;
            let f = |n: &str, size| Found {
                relpath: n.into(),
                name: n.into(),
                kind: "iso".into(),
                size,
                mtime: 1,
                title_id: None,
                meta: MetaState::Keep,
            };
            let a = apply(c, 1, 1, &[f("a", 1), f("b", 2)])?;
            assert_eq!((a.added, a.updated, a.removed), (2, 0, 0));
            let a = apply(c, 1, 1, &[f("a", 1), f("b", 3), f("c", 1)])?;
            assert_eq!((a.added, a.updated, a.unchanged, a.removed), (1, 1, 1, 0));
            let a = apply(c, 1, 1, &[f("c", 1)])?;
            assert_eq!((a.added, a.removed), (0, 2));
            let n: i64 = c.query_row("SELECT count(*) FROM items", [], |r| r.get(0))?;
            assert_eq!(n, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn an_empty_scan_of_a_known_path_deletes_nothing() {
        let db = Db::open_memory().unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO libraries(id,name,kind,created) VALUES (1,'a','iso',0)",
                [],
            )?;
            c.execute(
                "INSERT INTO library_paths(id,library_id,path) VALUES (1,1,'/x')",
                [],
            )?;
            let f = Found {
                relpath: "a".into(),
                name: "a".into(),
                kind: "iso".into(),
                size: 1,
                mtime: 1,
                title_id: None,
                meta: MetaState::Keep,
            };
            apply(c, 1, 1, &[f])?;
            let a = apply(c, 1, 1, &[])?;
            assert!(a.suspicious_empty);
            let (n, avail): (i64, i64) =
                c.query_row("SELECT count(*), sum(available) FROM items", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            assert_eq!((n, avail), (1, 0));
            // The share comes back: the item is available again.
            let f = Found {
                relpath: "a".into(),
                name: "a".into(),
                kind: "iso".into(),
                size: 1,
                mtime: 1,
                title_id: None,
                meta: MetaState::Keep,
            };
            apply(c, 1, 1, &[f])?;
            let avail: i64 = c.query_row("SELECT sum(available) FROM items", [], |r| r.get(0))?;
            assert_eq!(avail, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn cancel_stops_the_walk() {
        let d = tree("cancel");
        put(&d.join("a/b.iso"), 1);
        let r = walk(
            &d,
            &Walk {
                scanner: Scanner::Iso,
                cancelled: &|| true,
                progress: &|_| {},
            },
        );
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn headers_are_read_once_and_failures_are_explained() {
        use crate::xbox::{xex, xiso::testimg::disc};
        let d = tree("meta");
        // A real-looking disc (title 465307D3 is in the title list) and a file that isn't one.
        let img = disc(0, &xex::build(0x08FE_F3C1, 0x4653_07D3, 1, 1));
        fs::write(d.join("Game.iso"), img.to_bytes()).unwrap();
        put(&d.join("Broken.iso"), 100);

        let db = Db::open_memory().unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO libraries(id,name,kind,created) VALUES (1,'a','iso',0)",
                [],
            )?;
            c.execute(
                "INSERT INTO library_paths(id,library_id,path) VALUES (1,1,'/x')",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let w = Walk {
            scanner: Scanner::Iso,
            cancelled: &|| false,
            progress: &|_| {},
        };
        let (mut found, _) = walk(&d, &w).unwrap();
        let existing = db.with(|c| load_existing(c, 1)).unwrap();
        assert_eq!(
            enrich(&d, Scanner::Iso, &mut found, &existing, &w).unwrap(),
            2
        );
        db.with(|c| apply(c, 1, 1, &found)).unwrap();

        let (title, name, media, status): (Option<String>, Option<String>, Option<String>, String) = db
            .with(|c| Ok(c.query_row("SELECT title_id, game_name, media_id, meta FROM items WHERE relpath = 'Game.iso'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?))
            .unwrap();
        assert_eq!(
            (title.as_deref(), media.as_deref(), status.as_str()),
            (Some("465307D3"), Some("08FEF3C1"), "ok")
        );
        assert!(name.unwrap().contains("eNCHANT"));
        let (title, err): (Option<String>, String) = db
            .with(|c| {
                Ok(c.query_row(
                    "SELECT title_id, meta_error FROM items WHERE relpath = 'Broken.iso'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        assert!(
            title.is_none() && err.contains("Not an Xbox disc image"),
            "{err}"
        );

        // Second scan: the good one is not read again, the broken one is retried.
        let (mut found, _) = walk(&d, &w).unwrap();
        let existing = db.with(|c| load_existing(c, 1)).unwrap();
        assert_eq!(
            enrich(&d, Scanner::Iso, &mut found, &existing, &w).unwrap(),
            1
        );
        let a = db.with(|c| apply(c, 1, 1, &found)).unwrap();
        assert_eq!((a.added, a.unchanged), (0, 2));
        // ...and the good one kept what it had.
        let title: Option<String> = db
            .with(|c| {
                Ok(c.query_row(
                    "SELECT title_id FROM items WHERE relpath = 'Game.iso'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(title.as_deref(), Some("465307D3"));
    }
}
