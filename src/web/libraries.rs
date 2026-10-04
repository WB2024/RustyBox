//! Libraries API: create and edit libraries and their paths, browse folders, list items, scan.

use std::{path::PathBuf, time::Duration};

use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query, State},
    routing::{get, post, put},
};
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, S};
use crate::{
    error::Error,
    library::{self, ItemQuery, NewPath, health},
};

pub fn routes() -> Router<std::sync::Arc<super::AppState>> {
    Router::new()
        .route("/api/library-kinds", get(kinds))
        .route("/api/fs/browse", get(browse))
        .route("/api/remote/test", post(remote_test))
        .route("/api/libraries", get(list).post(create))
        .route(
            "/api/libraries/{id}",
            get(get_one).put(rename).delete(remove),
        )
        .route("/api/libraries/{id}/paths", post(add_path))
        .route("/api/libraries/{id}/reconnect", post(reconnect))
        .route(
            "/api/libraries/{id}/paths/{pid}",
            put(update_path).delete(remove_path),
        )
        .route("/api/libraries/{id}/items", get(items))
        .route("/api/libraries/{id}/scan", post(scan))
        .route("/api/libraries/{id}/items/remove", post(remove_items))
        .route("/api/libraries/{id}/duplicates", get(duplicates))
        .route("/api/games", get(games))
        .route("/api/compare", get(compare))
        .route("/api/libraries/{id}/tidy/plan", post(tidy_plan))
        .route("/api/libraries/{id}/tidy/start", post(tidy_start))
        .route("/api/libraries/{id}/misplaced/plan", post(misplaced_plan))
        .route("/api/libraries/{id}/misplaced/start", post(misplaced_start))
}

const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

/// `validate_path` on a blocking thread with a timeout, since `canonicalize` can hang on a dead share.
pub async fn validate_path(roots: &[PathBuf], raw: &str) -> Result<PathBuf, Error> {
    let (roots, raw) = (roots.to_vec(), raw.to_string());
    match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || library::validate_path(&roots, &raw)),
    )
    .await
    {
        Ok(r) => r.map_err(|e| Error::backend(e.to_string()))?,
        Err(_) => Err(Error::backend(
            "That folder isn't responding (a network share may be down)",
        )),
    }
}

async fn kinds() -> Json<Value> {
    Json(json!(
        library::KINDS
            .iter()
            .map(|k| json!({"id": k.id, "label": k.label, "icon": k.icon, "blurb": k.blurb}))
            .collect::<Vec<_>>()
    ))
}

async fn library_json(lib: &library::Library, stats: Option<&library::Stats>) -> Value {
    let healths = join_all(
        lib.paths
            .iter()
            .map(|p| health::check_path_cached(p, HEALTH_TIMEOUT)),
    )
    .await;
    let k = library::kind(&lib.kind);
    let paths: Vec<Value> = lib
        .paths
        .iter()
        .zip(healths)
        .map(|(p, h)| {
            let mut v = serde_json::to_value(p).unwrap_or_default();
            v["health"] = serde_json::to_value(h).unwrap_or_default();
            v
        })
        .collect();
    json!({
        "id": lib.id, "name": lib.name, "kind": lib.kind,
        "kind_label": k.map(|k| k.label), "icon": k.map(|k| k.icon),
        "items": stats.map(|s| s.items).unwrap_or(0), "bytes": stats.map(|s| s.bytes).unwrap_or(0),
        "paths": paths,
    })
}

async fn list(State(st): S) -> ApiResult<Json<Value>> {
    let (libs, stats) = st
        .db
        .run(|c| Ok((library::list_libraries(c)?, library::stats(c)?)))
        .await?;
    Ok(Json(json!(
        join_all(libs.iter().map(|l| library_json(l, stats.get(&l.id)))).await
    )))
}

async fn get_one(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    let (lib, stats) = st
        .db
        .run(move |c| Ok((library::get_library(c, id)?, library::stats(c)?)))
        .await?;
    Ok(Json(library_json(&lib, stats.get(&id)).await))
}

#[derive(Deserialize)]
struct CreateReq {
    name: String,
    kind: String,
    #[serde(default)]
    paths: Vec<NewPath>,
}

/// Check a folder to add: a local one must exist inside the allowed roots; a remote one must answer
/// (with the right token). Returns the path to store and the folder as it should be saved.
pub async fn resolve_new_path(
    roots: &[PathBuf],
    mut p: NewPath,
) -> Result<(PathBuf, NewPath), Error> {
    if p.role == "games" {
        p.role.clear();
    }
    let Some(mut r) = p.remote.take() else {
        return Ok((validate_path(roots, &p.path).await?, p));
    };
    r.url = crate::remote::clean_url(&r.url)?;
    r.token = r.token.trim().to_string();
    if r.token.is_empty() {
        return Err(Error::validation("Enter the drive agent's token"));
    }
    // A folder on the drive, if one was given, must be a real folder there.
    let subdir = r
        .subdir
        .take()
        .map(|d| d.trim().trim_matches('/').to_string())
        .filter(|d| !d.is_empty());
    if let Some(d) = &subdir {
        crate::fsops::join_rel(std::path::Path::new("/"), d)?;
    }
    let remote = crate::remote::Remote::new(&r.url, &r.token);
    let (r2, sub2, create) = (remote.clone(), subdir.clone(), r.create);
    let info = tokio::task::spawn_blocking(move || -> Result<crate::remote::AgentInfo, Error> {
        let info = r2.info()?;
        if let Some(d) = sub2 {
            let st = r2.stat(&d)?;
            if !st.exists && create {
                r2.mkdir(&d)?;
            } else if !st.exists || !st.is_dir {
                return Err(Error::validation(format!(
                    "There's no folder called \"{d}\" on the drive"
                )));
            }
        }
        Ok(info)
    })
    .await
    .map_err(|e| Error::backend(e.to_string()))??;
    if p.label.trim().is_empty() {
        p.label = info.name.clone();
    }
    // A drive shared read-only can't be made writable from this side.
    if info.read_only {
        p.writable = false;
    }
    p.path = match &subdir {
        Some(d) => format!("{}/{d}", r.url),
        None => r.url.clone(),
    };
    let stored = PathBuf::from(&p.path);
    r.subdir = subdir;
    p.remote = Some(r);
    Ok((stored, p))
}

/// A new drive folder can say "the same drive as folder #n": its address and token are filled in
/// here, so the token never has to come back from the browser.
async fn fill_same_as(st: &super::AppState, p: &mut NewPath) -> Result<(), Error> {
    if let Some(r) = p.remote.as_mut()
        && let Some(pid) = r.same_as
    {
        let known = st.db.run(move |c| library::path_by_id(c, pid)).await?;
        match (&known.remote_url, &known.remote_token) {
            (Some(url), Some(token)) => {
                r.url = url.clone();
                r.token = token.clone();
            }
            _ => return Err(Error::validation("That folder isn't on a drive agent")),
        }
    }
    Ok(())
}

async fn create(State(st): S, Json(req): Json<CreateReq>) -> ApiResult<Json<Value>> {
    let mut paths = Vec::new();
    for mut p in req.paths {
        fill_same_as(&st, &mut p).await?;
        paths.push(resolve_new_path(&st.cfg.roots, p).await?);
    }
    let id = st
        .db
        .run(move |c| library::create_library(c, &req.name, &req.kind, &paths))
        .await?;
    Ok(Json(json!({"id": id})))
}

#[derive(Deserialize)]
struct RenameReq {
    name: String,
}

async fn rename(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Json(req): Json<RenameReq>,
) -> ApiResult<Json<Value>> {
    st.db
        .run(move |c| library::rename_library(c, id, &req.name))
        .await?;
    Ok(Json(json!({"ok": true})))
}

/// Removes the library and its index only; files on disk are never touched.
async fn remove(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    st.db.run(move |c| library::delete_library(c, id)).await?;
    Ok(Json(json!({"ok": true})))
}

async fn add_path(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Json(mut p): Json<NewPath>,
) -> ApiResult<Json<Value>> {
    fill_same_as(&st, &mut p).await?;
    let (canon, p) = resolve_new_path(&st.cfg.roots, p).await?;
    let pid = st
        .db
        .run(move |c| library::add_path(c, id, &canon, &p))
        .await?;
    Ok(Json(json!({"id": pid})))
}

#[derive(Deserialize)]
struct UpdatePathReq {
    label: Option<String>,
    writable: Option<bool>,
    /// `games` (or empty), `dlc` or `updates`.
    role: Option<String>,
    /// Make this the library's primary folder.
    #[serde(default)]
    primary: bool,
}

async fn update_path(
    State(st): S,
    UrlPath((id, pid)): UrlPath<(i64, i64)>,
    Json(req): Json<UpdatePathReq>,
) -> ApiResult<Json<Value>> {
    st.db
        .run(move |c| {
            let role = req
                .role
                .as_deref()
                .map(|r| if r == "games" { "" } else { r });
            library::update_path(c, id, pid, req.label.as_deref(), req.writable, role)?;
            if req.primary {
                library::make_primary(c, id, pid)?;
            }
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok": true})))
}

async fn remove_path(
    State(st): S,
    UrlPath((id, pid)): UrlPath<(i64, i64)>,
) -> ApiResult<Json<Value>> {
    st.db.run(move |c| library::delete_path(c, id, pid)).await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct ItemsQuery {
    q: Option<String>,
    path_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
    sort: Option<String>,
    /// `desc` for a reversed order.
    dir: Option<String>,
}

async fn items(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Query(q): Query<ItemsQuery>,
) -> ApiResult<Json<Value>> {
    let query = ItemQuery {
        q: q.q.unwrap_or_default(),
        path_id: q.path_id,
        limit: q.limit.unwrap_or(200).clamp(1, 1000),
        offset: q.offset.unwrap_or(0).max(0),
        sort: q.sort.unwrap_or_default(),
        descending: q.dir.as_deref() == Some("desc"),
    };
    let (items, total) = st
        .db
        .run(move |c| {
            library::get_library(c, id)?;
            library::list_items(c, id, &query)
        })
        .await?;
    Ok(Json(json!({"items": items, "total": total})))
}

/// Start scanning a library as a job. `Err` holds the job already scanning it.
pub(super) fn start_scan(
    st: &std::sync::Arc<super::AppState>,
    lib: library::Library,
) -> Result<u64, std::sync::Arc<crate::jobs::Job>> {
    let job = st.jobs.create(
        "scan",
        &format!("Scan {}", lib.name),
        vec![format!("scan:{}", lib.id)],
    )?;
    let job_id = job.id;
    let (db, st2) = (st.db.clone(), st.clone());
    st.jobs.spawn(job, move |job| async move {
        library::scan::scan_library(db, job, lib).await?;
        super::igdb::kick(&st2); // look up any games this scan found
        Ok(())
    });
    Ok(job_id)
}

async fn scan(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    let lib = st.db.run(move |c| library::get_library(c, id)).await?;
    let job_id = start_scan(&st, lib).map_err(|b| ApiError::busy(&b))?;
    Ok(Json(json!({"job": job_id})))
}

// ── Folder picker ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct BrowseQuery {
    path: Option<String>,
}

/// Lists sub-folders (never files), only inside the allowed roots. With no path, lists the roots.
async fn browse(State(st): S, Query(q): Query<BrowseQuery>) -> ApiResult<Json<Value>> {
    let roots = st.cfg.roots.clone();
    let path = q.path.unwrap_or_default();
    let work = tokio::task::spawn_blocking(move || -> Result<Value, Error> {
        if path.trim().is_empty() {
            let dirs: Vec<Value> = roots
                .iter()
                .filter(|r| r.is_dir())
                .map(|r| json!({"name": r.to_string_lossy(), "path": r.to_string_lossy()}))
                .collect();
            return Ok(json!({"path": "", "parent": null, "dirs": dirs}));
        }
        let canon = library::validate_path(&roots, &path)?;
        let mut dirs: Vec<(String, String)> = std::fs::read_dir(&canon)?
            .flatten()
            .filter(|e| {
                e.file_type()
                    .is_ok_and(|t| t.is_dir() || (t.is_symlink() && e.path().is_dir()))
            })
            .map(|e| {
                (
                    e.file_name().to_string_lossy().to_string(),
                    e.path().to_string_lossy().to_string(),
                )
            })
            .filter(|(n, _)| !n.starts_with('.'))
            .collect();
        dirs.sort_by_key(|(n, _)| n.to_lowercase());
        let parent = canon
            .parent()
            .filter(|p| library::is_within_roots(&roots, p))
            .map(|p| p.to_string_lossy().to_string());
        Ok(
            json!({"path": canon.to_string_lossy(), "parent": parent, "dirs": dirs.iter().map(|(n, p)| json!({"name": n, "path": p})).collect::<Vec<_>>()}),
        )
    });
    match tokio::time::timeout(Duration::from_secs(8), work).await {
        Ok(r) => Ok(Json(r.map_err(|e| Error::backend(e.to_string()))??)),
        Err(_) => {
            Err(Error::backend("That folder isn't responding (a network share may be down)").into())
        }
    }
}

// ── Games across all libraries ───────────────────────────────────────────────

#[derive(Deserialize)]
struct GamesQuery {
    q: Option<String>,
    /// duplicates | iso_only | god_only | both | unknown (no name in the title list)
    only: Option<String>,
    /// At most this many games (for a search-as-you-type box); `total` still says how many match.
    limit: Option<usize>,
}

async fn games(State(st): S, Query(q): Query<GamesQuery>) -> ApiResult<Json<Value>> {
    let needle = q.q.unwrap_or_default();
    let all = st.db.run(move |c| library::games(c, &needle)).await?;
    let total = all.len();
    let only = q.only.unwrap_or_default();
    let shown: Vec<_> = all
        .into_iter()
        .filter(|g| match only.as_str() {
            "duplicates" => g.duplicate,
            "iso_only" => g.has_iso && !g.has_god,
            "god_only" => g.has_god && !g.has_iso,
            "both" => g.has_iso && g.has_god,
            "unknown" => g.name.is_none(),
            _ => true,
        })
        .take(q.limit.unwrap_or(usize::MAX))
        .collect();
    Ok(Json(json!({"games": shown, "total": total})))
}

// ── Tidy a GOD library ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TidyReq {
    layout: Option<String>,
}

async fn make_tidy(
    st: &super::AppState,
    id: i64,
    layout: Option<String>,
) -> ApiResult<(
    library::tidy::TidyPlan,
    std::collections::HashMap<i64, library::LibPath>,
)> {
    let layout = layout.unwrap_or_else(|| st.settings.get().god_layout);
    if !crate::settings::LAYOUTS.contains(&layout.as_str()) {
        return Err(ApiError::bad(format!("Unknown GOD layout '{layout}'")));
    }
    let (rows, paths) = st.db.run(move |c| library::tidy::gather(c, id)).await?;
    // Checking whether new names are free touches the disk, so it runs off the async threads.
    let work = tokio::task::spawn_blocking(move || {
        let plan = library::tidy::plan(&rows, &paths, &layout);
        (plan, paths)
    });
    match tokio::time::timeout(Duration::from_secs(60), work).await {
        Ok(r) => Ok(r.map_err(|e| Error::backend(e.to_string()))?),
        Err(_) => Err(Error::backend(
            "Checking the folders took too long (a network share may be down)",
        )
        .into()),
    }
}

async fn tidy_plan(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Json(req): Json<TidyReq>,
) -> ApiResult<Json<Value>> {
    let (plan, _) = make_tidy(&st, id, req.layout).await?;
    Ok(Json(json!(plan)))
}

async fn tidy_start(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Json(req): Json<TidyReq>,
) -> ApiResult<Json<Value>> {
    let (plan, paths) = make_tidy(&st, id, req.layout).await?;
    let todo = plan.moves.iter().filter(|m| m.problem.is_none()).count();
    if todo == 0 {
        return Err(ApiError::bad("Nothing to tidy"));
    }
    let lib = st.db.run(move |c| library::get_library(c, id)).await?;
    let mut resources: Vec<String> = vec![format!("scan:{id}")];
    resources.extend(paths.keys().map(|pid| format!("dest:{pid}")));
    resources.sort();
    resources.dedup();
    let job = st
        .jobs
        .create_queued("tidy", &format!("Tidy {}", lib.name), resources);
    let job_id = job.id;
    let (db, st2) = (st.db.clone(), st.clone());
    st.jobs.spawn(job, move |job| async move {
        job.step(format!("Renaming {todo} folder(s)"));
        let (moved, failed) =
            tokio::task::spawn_blocking(move || library::tidy::apply(&plan, &paths))
                .await
                .map_err(|e| Error::backend(e.to_string()))?;
        job.log(format!("{moved} folder(s) renamed"));
        for f in &failed {
            job.log(format!("Could not rename {f}"));
        }
        job.step("Updating the library");
        library::scan::scan_library(db, job.clone(), lib).await?;
        super::igdb::kick(&st2);
        job.result("tidy", json!({"moved": moved, "failed": failed.len()}));
        if failed.is_empty() {
            Ok(())
        } else {
            Err(Error::backend(format!(
                "{} of {} could not be renamed:\n{}",
                failed.len(),
                moved + failed.len(),
                failed.join("\n")
            )))
        }
    });
    Ok(Json(json!({"job": job_id})))
}

// ── Misplaced items on an Xbox drive ─────────────────────────────────────────

async fn make_misplaced(
    st: &super::AppState,
    id: i64,
) -> ApiResult<(
    library::misplaced::MisplacedPlan,
    std::collections::HashMap<i64, library::LibPath>,
)> {
    let libs = st.db.run(|c| library::list_libraries(c)).await?;
    let map: std::collections::HashMap<_, _> = libs
        .iter()
        .flat_map(|l| l.paths.iter().cloned())
        .map(|p| (p.id, p))
        .collect();
    let work = tokio::task::spawn_blocking(move || {
        let lib = libs
            .iter()
            .find(|l| l.id == id)
            .ok_or_else(|| Error::not_found(format!("No library #{id}")))?;
        // A drive with a Content folder is checked as an Xbox drive; any other library looks for
        // content that belongs in another library.
        if lib.paths.iter().any(|p| p.role == "content") {
            library::misplaced::plan(&lib.paths)
        } else {
            library::misplaced::plan_library(&libs, id)
        }
    });
    match tokio::time::timeout(Duration::from_secs(180), work).await {
        Ok(r) => Ok((r.map_err(|e| Error::backend(e.to_string()))??, map)),
        Err(_) => Err(Error::backend(
            "Looking through the folders took too long (is a drive offline?)",
        )
        .into()),
    }
}

async fn misplaced_plan(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    let (plan, _) = make_misplaced(&st, id).await?;
    Ok(Json(json!(plan)))
}

async fn misplaced_start(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    let (plan, paths) = make_misplaced(&st, id).await?;
    let todo = plan.moves.iter().filter(|m| m.problem.is_none()).count();
    if todo == 0 {
        return Err(ApiError::bad("Nothing to move"));
    }
    let lib = st.db.run(move |c| library::get_library(c, id)).await?;
    // Lock every folder (and library scan) the moves touch, so it can't run alongside an import, a
    // scan or another tidy working in the same places.
    let mut resources: Vec<String> = vec![format!("scan:{id}")];
    for m in plan.moves.iter().filter(|m| m.problem.is_none()) {
        for pid in std::iter::once(m.from_path_id).chain(m.to_path_id) {
            resources.push(format!("dest:{pid}"));
            if let Some(p) = paths.get(&pid) {
                resources.push(format!("scan:{}", p.library_id));
            }
        }
    }
    resources.sort();
    resources.dedup();
    let job = st.jobs.create_queued(
        "tidy",
        &format!("Move misplaced items on {}", lib.name),
        resources,
    );
    let job_id = job.id;
    let db = st.db.clone();
    st.jobs.spawn(job, move |job| async move {
        job.step(format!("Moving {todo} item(s)"));
        let (moved, failed) =
            tokio::task::spawn_blocking(move || library::misplaced::apply(&plan, &paths))
                .await
                .map_err(|e| Error::backend(e.to_string()))?;
        job.log(format!("{moved} item(s) moved"));
        job.step("Updating the library");
        library::scan::scan_library(db, job.clone(), lib).await?;
        job.result("tidy", json!({"moved": moved, "failed": failed.len()}));
        if failed.is_empty() {
            Ok(())
        } else {
            Err(Error::backend(format!(
                "{} could not be moved:\n{}",
                failed.len(),
                failed.join("\n")
            )))
        }
    });
    Ok(Json(json!({"job": job_id})))
}

// ── Drive agents ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RemoteTestReq {
    url: String,
    token: String,
}

/// Try an agent's address and token before adding it.
async fn remote_test(Json(req): Json<RemoteTestReq>) -> ApiResult<Json<Value>> {
    let url = crate::remote::clean_url(&req.url)?;
    let remote = crate::remote::Remote::new(&url, req.token.trim());
    let info = tokio::task::spawn_blocking(move || remote.info())
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
    Ok(Json(json!({
        "name": info.name, "root": info.root, "read_only": info.read_only, "version": info.version,
        "fs_type": info.fs.fs_type, "total": info.fs.total, "free": info.fs.free, "max_file": info.fs.max_file,
    })))
}

#[derive(Deserialize)]
struct ReconnectReq {
    url: String,
    token: String,
}

/// Point a library's drive folders at a different connection (for example, from the agent program
/// to a browser), keeping the library, its folders' roles and everything scanned so far.
async fn reconnect(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Json(req): Json<ReconnectReq>,
) -> ApiResult<Json<Value>> {
    let url = crate::remote::clean_url(&req.url)?;
    let token = req.token.trim().to_string();
    let remote = crate::remote::Remote::new(&url, &token);
    let info = tokio::task::spawn_blocking(move || remote.info())
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
    let (u2, t2) = (url.clone(), token.clone());
    let n = st
        .db
        .run(move |c| {
            let mut n = 0;
            for p in library::get_library(c, id)?.paths.iter().filter(|p| p.remote_url.is_some()) {
                let sub = p.remote_subdir.clone().unwrap_or_default();
                let path = if sub.is_empty() { u2.clone() } else { format!("{u2}/{sub}") };
                // A drive shared read-only can't be made writable from this side.
                let writable = p.writable && !info.read_only;
                n += c.execute(
                    "UPDATE library_paths SET remote_url = ?1, remote_token = ?2, path = ?3, writable = ?4 WHERE id = ?5 AND library_id = ?6",
                    rusqlite::params![u2, t2, path, writable, p.id, id],
                )?;
            }
            Ok(n)
        })
        .await?;
    Ok(Json(json!({"updated": n})))
}

// ── Comparing two libraries ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct CompareQuery {
    a: i64,
    b: i64,
}

/// What two libraries have in common, and what only one of them has (for example your library against the Xbox's drive).
async fn compare(State(st): S, Query(q): Query<CompareQuery>) -> ApiResult<Json<Value>> {
    let (a, b) = (q.a, q.b);
    if a == b {
        return Err(ApiError::bad("Choose two different libraries to compare"));
    }
    let c = st.db.run(move |conn| library::compare(conn, a, b)).await?;
    Ok(Json(json!(c)))
}

/// Games that are in this library more than once, with the copy to keep chosen and why. Nothing is
/// removed: the browser shows this, and removal goes through the confirmed remove endpoint.
async fn duplicates(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    let layout = st.settings.get().god_layout;
    let d = st
        .db
        .run(move |c| library::dedupe::find(c, id, &layout))
        .await?;
    Ok(Json(json!(d)))
}

// ── Removing games ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RemoveReq {
    item_ids: Vec<i64>,
    /// Without this, nothing is removed: the answer says what would be.
    #[serde(default)]
    confirm: bool,
}

/// Remove games from a library folder: the files themselves, here or on a drive. Without `confirm`
/// it only reports what would go, so the browser can ask first.
async fn remove_items(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Json(req): Json<RemoveReq>,
) -> ApiResult<Json<Value>> {
    if req.item_ids.is_empty() {
        return Err(ApiError::bad("Choose something to remove"));
    }
    let ids = req.item_ids.clone();
    let rows = st
        .db
        .run(move |c| {
            ids.iter()
                .map(|i| library::get_item(c, id, *i))
                .collect::<Result<Vec<_>, _>>()
        })
        .await?;
    let mut summary = Vec::new();
    for (item, lp) in &rows {
        if !lp.writable {
            return Err(ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                "READ_ONLY",
                format!(
                    "{} is in a read-only folder. Turn on Writable for it first.",
                    item.name
                ),
                true,
            ));
        }
        let rel = item.relpath.trim_matches('/');
        if rel.is_empty()
            || rel.split('/').any(|c| c == ".." || c.is_empty())
            || !["iso", "god"].contains(&item.kind.as_str())
        {
            return Err(ApiError::bad(format!(
                "{} can't be removed from here",
                item.name
            )));
        }
        summary.push(json!({
            "item_id": item.id, "name": item.game_name.clone().unwrap_or_else(|| item.name.clone()), "relpath": item.relpath,
            "size": item.size, "folder": if lp.label.is_empty() { lp.path.clone() } else { lp.label.clone() }, "kind": item.kind,
        }));
    }
    let bytes: i64 = rows.iter().map(|(i, _)| i.size).sum();
    if !req.confirm {
        return Ok(Json(json!({"items": summary, "bytes": bytes})));
    }
    let lib = st.db.run(move |c| library::get_library(c, id)).await?;
    let mut resources: Vec<String> = rows
        .iter()
        .map(|(_, lp)| format!("dest:{}", lp.id))
        .collect();
    resources.dedup();
    let job = st.jobs.create_queued(
        "remove",
        &format!("Remove {} game(s) from {}", rows.len(), lib.name),
        resources,
    );
    let job_id = job.id;
    let db = st.db.clone();
    st.jobs.spawn(job, move |job| async move {
        let (mut done, mut failed) = (0usize, Vec::new());
        let mut removed_ids: Vec<i64> = Vec::new();
        let n = rows.len();
        for (i, (item, lp)) in rows.into_iter().enumerate() {
            if job.is_cancelled() {
                return Ok(());
            }
            job.step(format!("{} of {n}: {}", i + 1, item.name));
            job.progress(i as f32 / n as f32 * 100.0);
            let loc = crate::transfer::Loc::of(&lp);
            let rel = item.relpath.trim_matches('/').to_string();
            let kind = item.kind.clone();
            let res = tokio::task::spawn_blocking(move || -> Result<Option<String>, Error> {
                if kind == "god" {
                    // Only the game itself: saves, add-ons and title updates kept in the same
                    // title folder are somebody's data and stay.
                    let note = crate::transfer::run::remove_old(&crate::transfer::plan::Removal {
                        loc: loc.clone(),
                        rel: rel.clone(),
                        kind: "god".into(),
                        also: false,
                        before: false,
                        label: String::new(),
                    })?;
                    return Ok(note.starts_with("removed the game").then_some(note));
                }
                loc.delete(&rel)?;
                // Tidy up the folders this leaves empty (the game's own name folder), never the top.
                let mut parent = rel.rsplit_once('/').map(|(p, _)| p.to_string());
                while let Some(p) = parent {
                    if p.is_empty() || !matches!(loc.tree(&p), Ok(t) if t.is_empty()) {
                        break;
                    }
                    loc.delete(&p)?;
                    parent = p.rsplit_once('/').map(|(q, _)| q.to_string());
                }
                Ok(None)
            })
            .await
            .map_err(|e| Error::backend(e.to_string()))?;
            match res {
                Ok(note) => {
                    done += 1;
                    removed_ids.push(item.id);
                    job.log(note.unwrap_or_else(|| format!("Removed {}", item.relpath)));
                }
                Err(e) => {
                    job.log(format!("{} failed: {e}", item.name));
                    failed.push(format!("{}: {e}", item.name));
                }
            }
        }
        // Forget them in the index straight away: a scan that finds a folder empty keeps its old entries
        // (to protect against an unmounted share), so it wouldn't drop the last game by itself.
        let ids = removed_ids.clone();
        db.run(move |c| {
            for i in ids {
                c.execute("DELETE FROM items WHERE id = ?1", [i])?;
            }
            Ok(())
        })
        .await?;
        job.step("Updating the library");
        library::scan::scan_library(db, job.clone(), lib).await?;
        job.result("remove", json!({"done": done, "failed": failed.len()}));
        if failed.is_empty() {
            Ok(())
        } else {
            Err(Error::backend(format!(
                "{} could not be removed:\n{}",
                failed.len(),
                failed.join("\n")
            )))
        }
    });
    Ok(Json(json!({"job": job_id})))
}
