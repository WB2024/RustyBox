//! Folders of `.torrent` files: browse them, look inside one, pick which of its files to fetch, and
//! hand it to qBittorrent with only those files switched on.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};
use crate::{
    error::Error,
    grabber::{
        qbit::AddSource,
        store::{self, NewGrab},
        torrent,
    },
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/torrents/status", get(status))
        .route("/api/torrents/dirs", put(set_dirs))
        .route("/api/torrents/files", get(files))
        .route("/api/torrents/inspect", get(inspect))
        .route("/api/torrents/send", post(send))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    match tokio::time::timeout(Duration::from_secs(120), tokio::task::spawn_blocking(f)).await {
        Ok(r) => r.map_err(|e| Error::backend(e.to_string()))?,
        Err(_) => Err(Error::backend(
            "That folder isn't responding (a network share may be down)",
        )),
    }
}

/// The folder at this position in the settings.
fn dir_at(st: &AppState, index: usize) -> Result<PathBuf, Error> {
    st.grabber
        .get()
        .torrent_dirs
        .get(index)
        .map(PathBuf::from)
        .ok_or_else(|| Error::not_found("That torrent folder isn't in the list any more"))
}

/// One `.torrent` file in a folder, found by looking a few levels down.
struct Found {
    rel: String,
    size: u64,
    mtime: i64,
}

fn list_torrents(root: &Path) -> Vec<Found> {
    fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<Found>) {
        if depth > 4 || out.len() > 50_000 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(m) = crate::fsops::entry_meta(&e) else {
                continue;
            };
            if m.is_dir() {
                walk(root, &e.path(), depth + 1, out);
            } else if name.to_lowercase().ends_with(".torrent")
                && let Ok(rel) = e.path().strip_prefix(root)
            {
                out.push(Found {
                    rel: rel.to_string_lossy().to_string(),
                    size: m.len(),
                    mtime: crate::library::scan::mtime_secs(&m),
                });
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, 0, &mut out);
    out.sort_by_key(|f| f.rel.to_lowercase());
    out
}

/// A `.torrent` file inside a configured folder, read and parsed. The name is a path inside the
/// folder: it can't leave it, and it must really be a `.torrent`.
fn open_torrent(root: &Path, rel: &str) -> Result<(Vec<u8>, torrent::Torrent), Error> {
    if !rel.to_lowercase().ends_with(".torrent") {
        return Err(Error::validation("That isn't a .torrent file"));
    }
    let path = crate::fsops::contained(root, rel)?;
    let meta = std::fs::metadata(&path)?;
    if !meta.is_file() || meta.len() as usize > torrent::MAX_TORRENT_BYTES {
        return Err(Error::validation("That isn't a usable .torrent file"));
    }
    let bytes = std::fs::read(&path)?;
    let t = torrent::parse(&bytes)?;
    Ok((bytes, t))
}

async fn status(State(st): S) -> ApiResult<Json<Value>> {
    let cfg = st.grabber.get();
    let dirs = cfg.torrent_dirs.clone();
    let counts = blocking(move || {
        Ok(dirs
            .iter()
            .map(|d| {
                let p = PathBuf::from(d);
                (
                    p.is_dir(),
                    if p.is_dir() {
                        list_torrents(&p).len()
                    } else {
                        0
                    },
                )
            })
            .collect::<Vec<_>>())
    })
    .await?;
    Ok(Json(json!({
        "qbit": cfg.qbit.configured(),
        "import_library": cfg.import_library.is_some(),
        "dirs": cfg.torrent_dirs.iter().zip(counts).enumerate().map(|(i, (d, (ok, n)))| json!({"index": i, "path": d, "online": ok, "count": n})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct DirsReq {
    dirs: Vec<String>,
}

/// Set the folders of torrent files. Each has to exist and be inside a folder RustyBox may use.
async fn set_dirs(State(st): S, Json(r): Json<DirsReq>) -> ApiResult<Json<Value>> {
    let mut clean = Vec::new();
    for d in r.dirs.iter().filter(|d| !d.trim().is_empty()) {
        let canon = super::libraries::validate_path(&st.cfg.roots, d.trim()).await?;
        let s = canon.to_string_lossy().to_string();
        if !clean.contains(&s) {
            clean.push(s);
        }
    }
    st.grabber.update(|c| {
        c.torrent_dirs = clean;
        Ok(())
    })?;
    status(State(st)).await
}

#[derive(Deserialize)]
struct FilesQuery {
    dir: usize,
    q: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}

async fn files(State(st): S, Query(q): Query<FilesQuery>) -> ApiResult<Json<Value>> {
    let root = dir_at(&st, q.dir)?;
    let all = blocking(move || {
        if !root.is_dir() {
            return Err(Error::not_found("That folder isn't there right now"));
        }
        Ok(list_torrents(&root))
    })
    .await?;
    let needle = q.q.unwrap_or_default().trim().to_lowercase();
    let words: Vec<&str> = needle.split_whitespace().collect();
    let hits: Vec<&Found> = all
        .iter()
        .filter(|f| {
            let l = f.rel.to_lowercase();
            words.iter().all(|w| l.contains(w))
        })
        .collect();
    let (offset, limit) = (q.offset.unwrap_or(0), q.limit.unwrap_or(200).clamp(1, 500));
    Ok(Json(json!({
        "total": hits.len(),
        "all": all.len(),
        "files": hits.iter().skip(offset).take(limit).map(|f| json!({"file": f.rel, "size": f.size, "mtime": f.mtime})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct FileQuery {
    dir: usize,
    file: String,
}

/// What is inside a torrent: its name, hash and every file with its size.
async fn inspect(State(st): S, Query(q): Query<FileQuery>) -> ApiResult<Json<Value>> {
    let root = dir_at(&st, q.dir)?;
    let file = q.file.clone();
    let t = blocking(move || open_torrent(&root, &file).map(|(_, t)| t)).await?;
    let known = {
        let cfg = st.grabber.get();
        if cfg.qbit.configured() {
            let (qb, h) = (cfg.qbit.clone(), t.info_hash.clone());
            blocking(move || qb.info(&[h]).map(|v| !v.is_empty()))
                .await
                .unwrap_or(false)
        } else {
            false
        }
    };
    Ok(Json(json!({"torrent": t, "already_in_client": known})))
}

#[derive(Deserialize)]
struct SendReq {
    dir: usize,
    file: String,
    /// The numbers of the files to fetch (from `inspect`); all of them when left out.
    select: Option<Vec<usize>>,
}

/// Add the torrent to qBittorrent with only the chosen files switched on, and follow it like any
/// other download (it is imported into the library when it finishes).
async fn send(State(st): S, Json(r): Json<SendReq>) -> ApiResult<Json<Value>> {
    let cfg = st.grabber.get();
    if !cfg.qbit.configured() {
        return Err(ApiError::bad(
            "qBittorrent isn't set up yet. Add it under Wanted → Setup.",
        ));
    }
    let root = dir_at(&st, r.dir)?;
    let (file, select, q) = (r.file.clone(), r.select.clone(), cfg.qbit.clone());
    let sent = blocking(move || {
        let (bytes, t) = open_torrent(&root, &file)?;
        let all: Vec<usize> = (0..t.files.len()).collect();
        let mut wanted: Vec<usize> = select.unwrap_or_else(|| all.clone());
        wanted.sort_unstable();
        wanted.dedup();
        if wanted.is_empty() {
            return Err(Error::validation("Choose at least one file"));
        }
        if wanted.iter().any(|i| *i >= t.files.len()) {
            return Err(Error::validation("A chosen file isn't in the torrent"));
        }
        if !q.info(std::slice::from_ref(&t.info_hash))?.is_empty() {
            return Err(Error::coded(
                409,
                "ALREADY_IN_CLIENT",
                "That torrent is already in qBittorrent. Change which of its files to fetch there.",
            ));
        }
        let partial = wanted.len() < t.files.len();
        // Stopped first, so nothing is fetched before the unwanted files are switched off.
        q.add(
            AddSource::File {
                name: "chosen.torrent",
                bytes: &bytes,
            },
            partial,
        )?;
        if partial {
            // Wait for qBittorrent to list the files (it reads them from the torrent at once).
            let mut have = 0;
            for _ in 0..40 {
                have = q.files(&t.info_hash)?.len();
                if have == t.files.len() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            if have != t.files.len() {
                // Don't leave a stopped torrent behind that nobody will start.
                let _ = q.remove(&t.info_hash, false);
                return Err(Error::backend(
                    "qBittorrent didn't list the torrent's files in time",
                ));
            }
            let skip: Vec<usize> = all
                .iter()
                .copied()
                .filter(|i| !wanted.contains(i))
                .collect();
            let done = q
                .skip_files(&t.info_hash, &skip)
                .and_then(|_| q.start(&t.info_hash));
            if let Err(e) = done {
                let _ = q.remove(&t.info_hash, false);
                return Err(e);
            }
        }
        let size: u64 = wanted.iter().map(|i| t.files[*i].size).sum();
        let title = if wanted.len() == 1 {
            let f = &t.files[wanted[0]].path;
            f.rsplit('/').next().unwrap_or(f).to_string()
        } else {
            format!("{} ({} files)", t.name, wanted.len())
        };
        Ok((t.info_hash.clone(), title, size, wanted.len()))
    })
    .await?;
    let (hash, title, size, n) = sent;
    let (h2, t2) = (hash.clone(), title.clone());
    let id = st
        .db
        .run(move |c| {
            store::add_grab(
                c,
                &NewGrab {
                    wanted_id: 0,
                    guid: &h2,
                    title: &t2,
                    indexer: "Torrent file",
                    size,
                    score: 0,
                    sab_id: &h2,
                    protocol: "torrent",
                },
            )
        })
        .await?;
    Ok(Json(
        json!({"grab": id, "title": title, "files": n, "hash": hash}),
    ))
}
