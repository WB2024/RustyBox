//! Folders of `.torrent` files: browse them, look inside one, pick which of its files to fetch, and
//! hand it to qBittorrent with only those files switched on.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query, State},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};
use crate::{
    error::Error,
    grabber::{
        qbit::AddSource,
        release,
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
        .route("/api/torrents/live", get(live))
        .route("/api/wanted/{id}/search-files", post(search_files))
        .route("/api/wanted/{id}/grab-file", post(grab_file))
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
    let (offset, limit) = (q.offset.unwrap_or(0), q.limit.unwrap_or(200).clamp(1, 5000));
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

/// What `add_chosen` did.
struct Added {
    hash: String,
    title: String,
    size: u64,
    files: usize,
    /// The torrent was already in qBittorrent: the chosen files were switched on in it.
    existed: bool,
}

/// Add the torrent to qBittorrent with only the chosen files switched on. A torrent that is
/// already there gets the chosen files switched on (the others are left as they are).
fn add_chosen(
    q: &crate::grabber::qbit::Qbit,
    root: &Path,
    file: &str,
    select: Option<Vec<usize>>,
) -> Result<Added, Error> {
    let (bytes, t) = open_torrent(root, file)?;
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
    let existed = !q.info(std::slice::from_ref(&t.info_hash))?.is_empty();
    if existed {
        q.set_priority(&t.info_hash, &wanted, 1)?;
        q.start(&t.info_hash)?;
    } else {
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
    }
    let size: u64 = wanted.iter().map(|i| t.files[*i].size).sum();
    let title = if wanted.len() == 1 {
        let f = &t.files[wanted[0]].path;
        f.rsplit('/').next().unwrap_or(f).to_string()
    } else {
        format!("{} ({} files)", t.name, wanted.len())
    };
    Ok(Added {
        hash: t.info_hash.clone(),
        title,
        size,
        files: wanted.len(),
        existed,
    })
}

/// Record the download so it is followed and imported. If an open grab already follows this
/// torrent (the same hash), that one is kept instead of making a second.
async fn record(
    st: &Arc<AppState>,
    a: &Added,
    wanted_id: i64,
    score: i32,
    indexer: &'static str,
) -> Result<i64, Error> {
    let (h, t, size) = (a.hash.clone(), a.title.clone(), a.size);
    st.db
        .run(move |c| {
            if let Some(g) = store::open_grabs(c)?
                .into_iter()
                .find(|g| g.sab_id.as_deref() == Some(h.as_str()))
            {
                return Ok(g.id);
            }
            store::add_grab(
                c,
                &NewGrab {
                    wanted_id,
                    guid: &h,
                    title: &t,
                    indexer,
                    size,
                    score,
                    sab_id: &h,
                    protocol: "torrent",
                },
            )
        })
        .await
}

fn need_qbit(st: &AppState) -> Result<crate::grabber::qbit::Qbit, ApiError> {
    let q = st.grabber.get().qbit;
    if !q.configured() {
        return Err(ApiError::bad(
            "qBittorrent isn't set up yet. Add it under Wanted → Setup.",
        ));
    }
    Ok(q)
}

/// Add the torrent to qBittorrent with only the chosen files switched on, and follow it like any
/// other download (it is imported into the library when it finishes).
async fn send(State(st): S, Json(r): Json<SendReq>) -> ApiResult<Json<Value>> {
    let q = need_qbit(&st)?;
    let root = dir_at(&st, r.dir)?;
    let (file, select) = (r.file.clone(), r.select.clone());
    let a = blocking(move || add_chosen(&q, &root, &file, select)).await?;
    let id = record(&st, &a, 0, 0, "Torrent file").await?;
    Ok(Json(
        json!({"grab": id, "title": a.title, "files": a.files, "hash": a.hash, "existed": a.existed}),
    ))
}

// ---- Searching inside the torrent files ----

/// Parsed torrents, so a second search doesn't read every `.torrent` again. Keyed by path, and
/// valid while the file's size and modified time are the same.
type Cached = (u64, i64, Arc<torrent::Torrent>);
static PARSED: std::sync::Mutex<Option<std::collections::HashMap<PathBuf, Cached>>> =
    std::sync::Mutex::new(None);

fn parsed(path: &Path, size: u64, mtime: i64) -> Option<Arc<torrent::Torrent>> {
    {
        let g = PARSED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((s, m, t)) = g.as_ref().and_then(|c| c.get(path))
            && *s == size
            && *m == mtime
        {
            return Some(t.clone());
        }
    }
    if size as usize > torrent::MAX_TORRENT_BYTES {
        return None;
    }
    let t = Arc::new(torrent::parse(&std::fs::read(path).ok()?).ok()?);
    let mut g = PARSED.lock().unwrap_or_else(|e| e.into_inner());
    let c = g.get_or_insert_with(Default::default);
    if c.len() >= 300 {
        c.clear();
    }
    c.insert(path.to_path_buf(), (size, mtime, t.clone()));
    Some(t)
}

/// A torrent that looks like it holds Xbox 360 games (used when no name filter is given).
fn looks_like_360(rel: &str) -> bool {
    let l = rel.to_lowercase();
    let yes = l.contains("xbox 360") || l.contains("xbox360") || l.contains("x360");
    let no = ["title update", "dlc", "addon", "add-on", "bitsavers"]
        .iter()
        .any(|w| l.contains(w));
    yes && !no
}

/// The file name without its folders and extension: what a release would be called.
fn stem(path: &str) -> &str {
    let f = path.rsplit('/').next().unwrap_or(path);
    f.rsplit_once('.').map(|(a, _)| a).unwrap_or(f)
}

#[derive(Deserialize)]
struct SearchFilesReq {
    /// Only this folder (its position in the settings); all of them when left out.
    dir: Option<usize>,
    /// Only torrent files whose name contains all these words.
    #[serde(default)]
    filter: String,
    /// Exactly these torrent files (names inside the folder, as the file list gives them). Needs
    /// `dir`. When given, `filter` is not used.
    #[serde(default)]
    files: Vec<String>,
}

const MAX_HITS: usize = 300;

/// Search the files listed inside the user's `.torrent` files for a wanted game.
async fn search_files(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Json(r): Json<SearchFilesReq>,
) -> ApiResult<Json<Value>> {
    need_qbit(&st)?;
    let cfg = st.grabber.get();
    let (w, blocked) = st
        .db
        .run(move |c| Ok((store::get_wanted(c, id)?, store::blocked_keys(c)?)))
        .await?;
    let names = super::grabber::names_for(&w);
    let mut dirs: Vec<(usize, PathBuf)> = cfg
        .torrent_dirs
        .iter()
        .enumerate()
        .map(|(i, d)| (i, PathBuf::from(d)))
        .collect();
    if let Some(d) = r.dir {
        dirs.retain(|(i, _)| *i == d);
        if dirs.is_empty() {
            return Err(Error::not_found("That torrent folder isn't in the list any more").into());
        }
    }
    if dirs.is_empty() {
        return Err(ApiError::bad(
            "No torrent folders are set up. Add one on the Torrents page.",
        ));
    }
    if !r.files.is_empty() && r.dir.is_none() {
        return Err(ApiError::bad("Choose a folder to pick torrent files from"));
    }
    let only: std::collections::HashSet<String> = r.files.iter().cloned().collect();
    let words: Vec<String> = r
        .filter
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    let profile = cfg.profile.clone();
    let out =
        blocking(move || Ok(run_search(&dirs, &words, &only, &names, &profile, &blocked))).await?;
    Ok(Json(json!({
        "wanted": w,
        "torrents": out.torrents,
        "files": out.files,
        "capped": out.capped,
        "notes": out.notes,
        "results": out.hits,
    })))
}

#[derive(Default)]
struct SearchOut {
    torrents: usize,
    files: usize,
    capped: bool,
    notes: Vec<String>,
    hits: Vec<Value>,
}

fn judge_file(
    t: &torrent::Torrent,
    f: &torrent::TorrentFile,
    names: &[String],
    profile: &release::Profile,
    blocked: &std::collections::HashSet<String>,
) -> release::Verdict {
    let title = stem(&f.path);
    let mut v = release::judge(
        &release::Candidate {
            title,
            size: f.size,
            categories: &[1050],
            age_days: None,
            grabs: None,
            indexer_priority: 0,
            seeders: None,
        },
        names,
        profile,
    );
    if blocked.contains(&title.to_lowercase()) {
        v.rejected
            .push("On the blocklist (an earlier download of it failed)".into());
    }
    let _ = t;
    v
}

fn run_search(
    dirs: &[(usize, PathBuf)],
    words: &[String],
    only: &std::collections::HashSet<String>,
    names: &[String],
    profile: &release::Profile,
    blocked: &std::collections::HashSet<String>,
) -> SearchOut {
    let mut out = SearchOut::default();
    // A file must contain the longest word of the wanted name, whatever the format, before it is
    // worth judging (4,000 zips per torrent, so this keeps it quick).
    let needles: Vec<String> = names
        .iter()
        .filter_map(|n| {
            release::tokens(n)
                .into_iter()
                .max_by_key(|t| t.len())
                .filter(|t| t.len() >= 2)
        })
        .collect();
    let mut hits: Vec<(Value, (bool, i32, u64))> = Vec::new();
    for (di, root) in dirs {
        let found = list_torrents(root);
        let total = found.len();
        let chosen: Vec<&Found> = if !only.is_empty() {
            found.iter().filter(|f| only.contains(&f.rel)).collect()
        } else if words.is_empty() {
            let xbox: Vec<&Found> = found.iter().filter(|f| looks_like_360(&f.rel)).collect();
            if xbox.is_empty() {
                found.iter().take(60).collect()
            } else {
                xbox
            }
        } else {
            found
                .iter()
                .filter(|f| {
                    let l = f.rel.to_lowercase();
                    words.iter().all(|w| l.contains(w.as_str()))
                })
                .collect()
        };
        if only.is_empty() && chosen.len() < total && words.is_empty() && chosen.len() == 60 {
            out.notes.push(format!(
                "{}: no Xbox 360 torrents recognised by name, so the first 60 of {} were searched. Type part of a torrent file's name to narrow it.",
                root.display(),
                total
            ));
        }
        for f in chosen {
            let Ok(path) = crate::fsops::contained(root, &f.rel) else {
                continue;
            };
            let Some(t) = parsed(&path, f.size, f.mtime) else {
                continue;
            };
            out.torrents += 1;
            out.files += t.files.len();
            for file in &t.files {
                let lower = file.path.to_lowercase();
                if !needles.iter().any(|n| lower.contains(n.as_str())) {
                    continue;
                }
                let v = judge_file(&t, file, names, profile, blocked);
                // A file that is simply another game isn't listed; "Halo 3 - ODST" for "Halo 3" is.
                if v.rejected
                    .iter()
                    .any(|x| x.contains("a different game: \""))
                {
                    continue;
                }
                let key = (v.rejected.is_empty(), v.score, file.size);
                hits.push((
                    json!({
                        "dir": di,
                        "folder": root.to_string_lossy(),
                        "torrent_file": f.rel,
                        "torrent_name": t.name,
                        "info_hash": t.info_hash,
                        "index": file.index,
                        "path": file.path,
                        "title": stem(&file.path),
                        "size": file.size,
                        "verdict": v,
                    }),
                    key,
                ));
            }
        }
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.1));
    if hits.len() > MAX_HITS {
        out.capped = true;
        hits.truncate(MAX_HITS);
    }
    out.hits = hits.into_iter().map(|h| h.0).collect();
    out
}

#[derive(Deserialize)]
struct GrabFileReq {
    dir: usize,
    torrent_file: String,
    index: usize,
    #[serde(default)]
    force: bool,
}

/// Fetch one file of one of the user's torrents for a wanted game. The file is judged again here
/// (nothing the browser says is trusted), then only it is switched on in qBittorrent.
async fn grab_file(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Json(r): Json<GrabFileReq>,
) -> ApiResult<Json<Value>> {
    let q = need_qbit(&st)?;
    let cfg = st.grabber.get();
    let root = dir_at(&st, r.dir)?;
    let (w, blocked, busy) = st
        .db
        .run(move |c| {
            Ok((
                store::get_wanted(c, id)?,
                store::blocked_keys(c)?,
                store::has_open_grab(c, id)?,
            ))
        })
        .await?;
    if busy {
        return Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "ALREADY_DOWNLOADING",
            format!("{} is already downloading", w.name),
            true,
        ));
    }
    let names = super::grabber::names_for(&w);
    let (file, index, profile, force) = (
        r.torrent_file.clone(),
        r.index,
        cfg.profile.clone(),
        r.force,
    );
    let (a, score) = blocking(move || {
        let (_, t) = open_torrent(&root, &file)?;
        let f = t
            .files
            .get(index)
            .ok_or_else(|| Error::validation("That file isn't in the torrent"))?;
        let v = judge_file(&t, f, &names, &profile, &blocked);
        if !v.rejected.is_empty() && !force {
            return Err(Error::validation(format!(
                "This file was rejected: {}",
                v.rejected.join("; ")
            )));
        }
        let a = add_chosen(&q, &root, &file, Some(vec![index]))?;
        Ok((a, v.score))
    })
    .await?;
    let gid = record(&st, &a, id, score, "Torrent file").await?;
    Ok(Json(json!({"grab": gid, "title": a.title, "hash": a.hash})))
}

/// What qBittorrent is doing now, for a dashboard. Always answers 200: when qBittorrent isn't set
/// up or can't be reached, `online` is false and `message` says why.
async fn live(State(st): S) -> Json<Value> {
    let q = st.grabber.get().qbit;
    if !q.configured() {
        return Json(
            json!({"configured": false, "online": false, "message": "qBittorrent isn't set up"}),
        );
    }
    match blocking(move || q.overview()).await {
        Err(e) => Json(json!({"configured": true, "online": false, "message": e.to_string()})),
        Ok(o) => {
            let count =
                |f: &dyn Fn(&str) -> bool| o.torrents.iter().filter(|t| f(&t.state)).count();
            let downloading = count(&|s| {
                matches!(
                    s,
                    "downloading"
                        | "forcedDL"
                        | "metaDL"
                        | "forcedMetaDL"
                        | "stalledDL"
                        | "queuedDL"
                        | "checkingDL"
                )
            });
            let seeding = count(&|s| {
                matches!(
                    s,
                    "uploading" | "forcedUP" | "stalledUP" | "queuedUP" | "checkingUP"
                )
            });
            let errored = count(&|s| matches!(s, "error" | "missingFiles"));
            let paused = count(&|s| s.starts_with("paused") || s.starts_with("stopped"));
            // The ones worth showing: anything moving first, then the newest.
            let mut shown: Vec<_> = o.torrents.iter().collect();
            shown.sort_by_key(|t| {
                (
                    std::cmp::Reverse(t.down + t.up > 0),
                    std::cmp::Reverse(t.added),
                )
            });
            Json(json!({
                "configured": true, "online": true, "version": o.version,
                "down": o.down, "up": o.up, "free": o.free,
                "counts": {"total": o.torrents.len(), "downloading": downloading, "seeding": seeding, "paused": paused, "errored": errored},
                "torrents": shown.into_iter().take(8).collect::<Vec<_>>(),
            }))
        }
    }
}
