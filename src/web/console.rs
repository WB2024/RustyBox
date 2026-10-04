//! Console API: connection profiles, a file browser, the games on the console, comparing them
//! with a library, and sending games or fetching folders (as jobs that wait for the console).

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query, State},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};
use crate::{
    console::{
        self, Console, ConsoleGame, fake,
        ftp::{self, Ftp, ftp_path},
        send,
    },
    convert::Ctl,
    convert::plan::ItemRef,
    error::Error,
    library,
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/consoles", get(list).post(create))
        .route("/api/consoles/test", post(test))
        .route("/api/consoles/{id}", put(update).delete(remove))
        .route("/api/consoles/{id}/ls", get(ls))
        .route("/api/consoles/{id}/selftest", post(selftest))
        .route("/api/consoles/{id}/mkdir", post(mkdir))
        .route("/api/consoles/{id}/rename", post(rename))
        .route("/api/consoles/{id}/delete", post(delete))
        .route("/api/consoles/{id}/scan", post(scan))
        .route("/api/consoles/{id}/games", get(games))
        .route("/api/consoles/{id}/compare", get(compare))
        .route("/api/consoles/{id}/send/plan", post(send_plan))
        .route("/api/consoles/{id}/send/start", post(send_start))
        .route("/api/consoles/{id}/fetch", post(fetch))
}

/// Slow network work runs off the async threads, with a limit so a dead console can't hang a page.
async fn limited<T: Send + 'static>(
    secs: u64,
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    match tokio::time::timeout(Duration::from_secs(secs), tokio::task::spawn_blocking(f)).await {
        Ok(r) => r.map_err(|e| Error::backend(e.to_string()))?,
        Err(_) => Err(Error::coded(
            504,
            "CONSOLE_TIMEOUT",
            "The console didn't answer in time. Is it on, with Aurora running?",
        )),
    }
}

// ── Profiles ─────────────────────────────────────────────────────────────────

async fn list(State(st): S) -> Json<Value> {
    Json(json!(
        st.consoles
            .all()
            .iter()
            .map(|c| c.public())
            .collect::<Vec<_>>()
    ))
}

async fn create(State(st): S, Json(c): Json<Console>) -> ApiResult<Json<Value>> {
    let c = st.consoles.put(Console { id: 0, ..c })?;
    Ok(Json(c.public()))
}

async fn update(
    State(st): S,
    UrlPath(id): UrlPath<u32>,
    Json(c): Json<Console>,
) -> ApiResult<Json<Value>> {
    st.consoles.get(id)?;
    Ok(Json(st.consoles.put(Console { id, ..c })?.public()))
}

async fn remove(State(st): S, UrlPath(id): UrlPath<u32>) -> ApiResult<Json<Value>> {
    st.consoles.remove(id)?;
    let _ = std::fs::remove_file(games_cache(&st, id));
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct TestReq {
    /// Test a saved console (its stored password is used)...
    id: Option<u32>,
    /// ...or details typed in but not saved yet.
    #[serde(flatten)]
    console: Console,
}

/// Try a connection and list the drives the console shows.
async fn test(State(st): S, Json(req): Json<TestReq>) -> ApiResult<Json<Value>> {
    let mut c = req.console;
    if let Some(id) = req.id {
        let saved = st.consoles.get(id)?;
        if c.password.is_empty() {
            c.password = saved.password;
        }
    }
    if c.host.trim().is_empty() {
        return Err(ApiError::bad("Enter the console's IP address"));
    }
    let c2 = c.clone();
    let drives = limited(20, move || {
        let mut f = Ftp::connect(&c2.login())?;
        let d = f.list("/")?;
        f.quit();
        Ok(d)
    })
    .await?;
    Ok(Json(json!({
        "ok": true,
        "drives": drives.iter().filter(|e| e.is_dir).map(|e| &e.name).collect::<Vec<_>>(),
    })))
}

// ── Browsing and file operations ─────────────────────────────────────────────

#[derive(Deserialize)]
struct PathQuery {
    path: Option<String>,
}

async fn ls(
    State(st): S,
    UrlPath(id): UrlPath<u32>,
    Query(q): Query<PathQuery>,
) -> ApiResult<Json<Value>> {
    let c = st.consoles.get(id)?;
    let path = ftp_path(&q.path.unwrap_or_default());
    let p2 = path.clone();
    let mut entries = limited(30, move || {
        let mut f = c.connect()?;
        let e = f.list(&p2)?;
        f.quit();
        Ok(e)
    })
    .await?;
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let parent = (path != "/").then(|| ftp::parent(&path));
    Ok(Json(json!({
        "path": path,
        "parent": parent,
        "entries": entries.iter().map(|e| json!({"name": e.name, "is_dir": e.is_dir, "size": e.size})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct MkdirReq {
    path: String,
}

async fn mkdir(
    State(st): S,
    UrlPath(id): UrlPath<u32>,
    Json(req): Json<MkdirReq>,
) -> ApiResult<Json<Value>> {
    let c = st.consoles.get(id)?;
    let path = valid_target(&req.path)?;
    limited(30, move || {
        let mut f = c.connect()?;
        f.mkdir_p(&path)?;
        f.quit();
        Ok(())
    })
    .await?;
    Ok(Json(json!({"ok": true})))
}

/// A path the user typed, which must be inside a drive (not the top, not a drive itself).
fn valid_target(raw: &str) -> Result<String, Error> {
    let p = ftp_path(raw);
    if p.split('/').any(|s| s == "..") {
        return Err(Error::validation("Invalid path"));
    }
    if p.matches('/').count() < 2 {
        return Err(Error::validation(
            "Choose a folder inside a drive, such as Hdd1/Games",
        ));
    }
    Ok(p)
}

#[derive(Deserialize)]
struct RenameReq {
    from: String,
    to: String,
}

async fn rename(
    State(st): S,
    UrlPath(id): UrlPath<u32>,
    Json(req): Json<RenameReq>,
) -> ApiResult<Json<Value>> {
    let c = st.consoles.get(id)?;
    let (from, to) = (valid_target(&req.from)?, valid_target(&req.to)?);
    limited(30, move || {
        let mut f = c.connect()?;
        if f.stat(&to)?.is_some() {
            return Err(Error::conflict(format!(
                "{to} already exists on the console"
            )));
        }
        f.rename(&from, &to)?;
        f.quit();
        Ok(())
    })
    .await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct DeleteReq {
    path: String,
    #[serde(default)]
    confirm: bool,
}

/// Delete a file or folder on the console. Without `confirm` it only says what would go.
async fn delete(
    State(st): S,
    UrlPath(id): UrlPath<u32>,
    Json(req): Json<DeleteReq>,
) -> ApiResult<Json<Value>> {
    let c = st.consoles.get(id)?;
    let path = valid_target(&req.path)?;
    let (c2, p2) = (c.clone(), path.clone());
    let (is_dir, files, bytes) = limited(120, move || {
        let mut f = c2.connect()?;
        let s = f
            .stat(&p2)?
            .ok_or_else(|| Error::not_found(format!("{p2} isn't on the console")))?;
        let (files, bytes) = if s.is_dir {
            let t = console::remote_tree(&mut f, &p2)?;
            (
                t.iter().filter(|e| !e.is_dir).count(),
                t.iter().map(|e| e.size).sum::<u64>(),
            )
        } else {
            (1, s.size)
        };
        f.quit();
        Ok((s.is_dir, files, bytes))
    })
    .await?;
    if !req.confirm {
        return Ok(Json(
            json!({"path": path, "is_dir": is_dir, "files": files, "bytes": bytes}),
        ));
    }
    let job = st.jobs.create_queued(
        "console",
        &format!("Delete {path} from {}", c.name),
        vec![format!("console:{id}")],
    );
    let job_id = job.id;
    st.jobs.spawn(job, move |job| async move {
        job.step(format!("Deleting {path}"));
        let p = path.clone();
        tokio::task::spawn_blocking(move || -> Result<(), Error> {
            let mut f = c.connect()?;
            if is_dir {
                f.delete_tree(&p)?;
            } else {
                f.delete_file(&p)?;
            }
            f.quit();
            Ok(())
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
        job.log(format!("Deleted {path}"));
        Ok(())
    });
    Ok(Json(json!({"job": job_id})))
}

// ── Games on the console ─────────────────────────────────────────────────────

fn games_cache(st: &AppState, id: u32) -> std::path::PathBuf {
    st.cfg.config_dir.join(format!("console-{id}-games.json"))
}

fn load_games(st: &AppState, id: u32) -> Option<(i64, Vec<ConsoleGame>)> {
    let v: Value = serde_json::from_slice(&std::fs::read(games_cache(st, id)).ok()?).ok()?;
    Some((
        v["scanned"].as_i64()?,
        serde_json::from_value(v["games"].clone()).ok()?,
    ))
}

async fn games(State(st): S, UrlPath(id): UrlPath<u32>) -> ApiResult<Json<Value>> {
    st.consoles.get(id)?;
    Ok(Json(match load_games(&st, id) {
        Some((at, g)) => json!({"scanned": at, "games": g}),
        None => json!({"scanned": null, "games": []}),
    }))
}

/// Look through the console's games folders for title folders (a job; the result is kept).
async fn scan(State(st): S, UrlPath(id): UrlPath<u32>) -> ApiResult<Json<Value>> {
    let c = st.consoles.get(id)?;
    let job = st.jobs.create_queued(
        "console",
        &format!("Scan {}", c.name),
        vec![format!("console:{id}")],
    );
    let job_id = job.id;
    let cache = games_cache(&st, id);
    st.jobs.spawn(job, move |job| async move {
        let (j, token) = (job.clone(), job.cancel_token());
        let games = tokio::task::spawn_blocking(move || -> Result<Vec<ConsoleGame>, Error> {
            let mut f = c.connect()?;
            let cancelled = || token.is_cancelled();
            let progress = |_: f32, what: &str| j.step(what.to_string());
            let g = console::scan_games(
                &mut f,
                &c,
                &Ctl {
                    cancelled: &cancelled,
                    progress: &progress,
                },
            )?;
            f.quit();
            Ok(g)
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
        job.log(format!(
            "{} game folder(s) found on the console",
            games.len()
        ));
        store_games(&cache, &games);
        job.result("console_scan", json!({"games": games.len()}));
        Ok(())
    });
    Ok(Json(json!({"job": job_id})))
}

fn store_games(cache: &std::path::Path, games: &[ConsoleGame]) {
    let _ = std::fs::write(
        cache,
        serde_json::to_vec(&json!({"scanned": crate::jobs::now(), "games": games}))
            .unwrap_or_default(),
    );
}

#[derive(Deserialize)]
struct CompareQuery {
    library: i64,
}

/// What a library has that the console doesn't, and the other way round, from the last scan.
async fn compare(
    State(st): S,
    UrlPath(id): UrlPath<u32>,
    Query(q): Query<CompareQuery>,
) -> ApiResult<Json<Value>> {
    st.consoles.get(id)?;
    let Some((at, on_console)) = load_games(&st, id) else {
        return Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "NOT_SCANNED",
            "Scan the console first so RustyBox knows what is on it.",
            true,
        ));
    };
    let lib = q.library;
    let rows = st
        .db
        .run(move |c| {
            library::get_library(c, lib)?;
            let mut s = c.prepare(
                "SELECT id, title_id, game_name, name, kind, size, available, disc, relpath FROM items
                 WHERE library_id = ?1 AND title_id IS NOT NULL AND kind IN ('iso', 'god')
                   AND (content_type IS NULL OR content_type IN ('00007000', '00005000', '000D0000', '00004000'))
                 ORDER BY title_id, disc, relpath",
            )?;
            let r = s
                .query_map([lib], |r| {
                    Ok(json!({
                        "item_id": r.get::<_, i64>(0)?,
                        "library_id": lib,
                        "title_id": r.get::<_, String>(1)?.to_uppercase(),
                        "name": r.get::<_, Option<String>>(2)?.unwrap_or(r.get::<_, String>(3)?),
                        "kind": r.get::<_, String>(4)?,
                        "size": r.get::<_, i64>(5)?,
                        "available": r.get::<_, i64>(6)? != 0,
                        "disc": r.get::<_, Option<i64>>(7)?,
                        "relpath": r.get::<_, String>(8)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(r)
        })
        .await?;
    let console_ids: std::collections::HashSet<String> = on_console
        .iter()
        .map(|g| g.title_id.to_uppercase())
        .collect();
    let lib_ids: std::collections::HashSet<String> = rows
        .iter()
        .map(|r| r["title_id"].as_str().unwrap_or("").to_string())
        .collect();
    let (both, only_library): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .partition(|r| console_ids.contains(r["title_id"].as_str().unwrap_or("")));
    let only_console: Vec<_> = on_console
        .into_iter()
        .filter(|g| !lib_ids.contains(&g.title_id.to_uppercase()))
        .collect();
    Ok(Json(json!({
        "scanned": at,
        "both": both,
        "only_library": only_library,
        "only_console": only_console,
    })))
}

// ── Sending games ────────────────────────────────────────────────────────────

#[derive(Deserialize, Clone, Default)]
pub(super) struct SendReq {
    /// Games from libraries...
    #[serde(default)]
    pub(super) items: Vec<ItemRef>,
    /// ...or games found in a folder (a mounted USB stick, a download folder): the folder, and
    /// the ids `Import` found in it.
    folder: Option<FolderReq>,
    /// One of the console's games folders (default: its first).
    pub(super) dest: Option<String>,
    layout: Option<String>,
    #[serde(default)]
    replace: bool,
}

impl SendReq {
    /// Send these library games to a games folder on the console (its first if `dest` is `None`).
    pub(super) fn games(items: Vec<ItemRef>, dest: Option<String>) -> SendReq {
        SendReq {
            items,
            dest,
            ..Default::default()
        }
    }
}

#[derive(Deserialize, Clone)]
struct FolderReq {
    path: String,
    select: Vec<String>,
}

async fn plan_for(st: &AppState, id: u32, req: &SendReq) -> ApiResult<(Console, send::Plan)> {
    let c = st.consoles.get(id)?;
    let layout = req
        .layout
        .clone()
        .unwrap_or_else(|| st.settings.get().god_layout);
    if !crate::settings::LAYOUTS.contains(&layout.as_str()) {
        return Err(ApiError::bad(format!("Unknown GOD layout '{layout}'")));
    }
    let mut items: Vec<send::SendItem> = Vec::new();
    if let Some(f) = req.folder.clone() {
        let (roots, source) = (
            st.cfg.roots.clone(),
            crate::transfer::plan::Source::Folder { path: f.path },
        );
        let resolved = st
            .db
            .run(move |c| crate::transfer::plan::resolve(c, &source, &roots))
            .await?;
        let wanted: std::collections::HashSet<String> = f.select.into_iter().collect();
        let found = limited(300, move || {
            let all = crate::transfer::plan::discover(&resolved)?;
            Ok((all, resolved))
        })
        .await?;
        let (all, resolved) = found;
        for id in &wanted {
            if !all.iter().any(|c| &c.id == id) {
                return Err(ApiError::bad(format!(
                    "{id} wasn't found in the folder any more. Look inside it again."
                )));
            }
        }
        for c in all.into_iter().filter(|c| wanted.contains(&c.id)) {
            items.push(send::SendItem {
                name: c.name,
                title_id: c.title_id,
                kind: c.kind,
                relpath: if resolved.base.is_empty() {
                    c.id
                } else {
                    format!("{}/{}", resolved.base, c.id)
                },
                size: c.size as i64,
                available: true,
                content_kind: c.content_kind,
                health: c.health,
                discs: c.discs,
                loc: resolved.loc.clone(),
            });
        }
    }
    let refs = req.items.clone();
    let lib_items = st
        .db
        .run(move |conn| {
            refs.iter()
                .map(|r| library::get_item(conn, r.library_id, r.item_id))
                .collect::<Result<Vec<_>, _>>()
        })
        .await?;
    items.extend(
        lib_items
            .iter()
            .map(|(i, lp)| send::SendItem::from_library(i, lp)),
    );
    let (c2, dest, replace) = (c.clone(), req.dest.clone(), req.replace);
    let plan = limited(300, move || {
        let mut f = c2.connect()?;
        let p = send::build(
            &c2,
            &mut f,
            &items,
            &send::Options {
                dest: dest.as_deref(),
                layout: &layout,
                replace,
            },
        )?;
        f.quit();
        Ok(p)
    })
    .await?;
    Ok((c, plan))
}

async fn send_plan(
    State(st): S,
    UrlPath(id): UrlPath<u32>,
    Json(req): Json<SendReq>,
) -> ApiResult<Json<Value>> {
    let (_, plan) = plan_for(&st, id, &req).await?;
    Ok(Json(json!(plan)))
}

async fn send_start(
    State(st): S,
    UrlPath(id): UrlPath<u32>,
    Json(req): Json<SendReq>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"job": start_send(&st, id, req).await?})))
}

/// Plan a send to the console again and, if it is sound, run it as a job. Returns the job's id.
pub(super) async fn start_send(st: &Arc<AppState>, id: u32, req: SendReq) -> ApiResult<u64> {
    // Planned again here, so what runs is what is checked now.
    let (c, plan) = plan_for(st, id, &req).await?;
    if !plan.ok {
        let mut why = plan.problems.clone();
        for e in &plan.entries {
            why.extend(e.problems.iter().map(|p| format!("{}: {p}", e.name)));
        }
        return Err(ApiError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "PLAN_HAS_PROBLEMS",
            why.join("\n"),
            true,
        ));
    }
    let title = match plan.entries.as_slice() {
        [one] => format!("Send to {}: {}", c.name, one.name),
        many => format!("Send to {}: {} games", c.name, many.len()),
    };
    let job = st
        .jobs
        .create_queued("console", &title, vec![format!("console:{id}")]);
    let job_id = job.id;
    let (overwrite, cache) = (req.replace, games_cache(st, id));
    st.jobs.spawn(job, move |job| async move {
        let n = plan.entries.len();
        let mut failures = Vec::new();
        let mut done = 0;
        for (i, e) in plan.entries.iter().enumerate() {
            if job.is_cancelled() {
                return Ok(());
            }
            let Some(w) = e.work.clone() else { continue };
            job.step(format!("{} of {n}: {}", i + 1, e.name));
            let (j, token, c2, label) =
                (job.clone(), job.cancel_token(), c.clone(), e.name.clone());
            let res = tokio::task::spawn_blocking(move || {
                let mut f = c2.connect()?;
                let cancelled = || token.is_cancelled();
                let progress = |frac: f32, what: &str| {
                    j.progress((i as f32 + frac.clamp(0.0, 1.0)) / n as f32 * 97.0);
                    if !what.is_empty() && what != label {
                        j.step(format!("{} of {n}: {label}: {what}", i + 1));
                    }
                };
                let r = send::run_entry(
                    &mut f,
                    &w,
                    overwrite,
                    &Ctl {
                        cancelled: &cancelled,
                        progress: &progress,
                    },
                );
                f.quit();
                r
            })
            .await
            .map_err(|er| Error::backend(er.to_string()))?;
            match res {
                Ok(how) => {
                    done += 1;
                    job.log(format!("{}: {how}", e.name));
                }
                Err(_) if job.is_cancelled() => return Ok(()),
                Err(er) => {
                    job.log(format!("{} failed: {er}", e.name));
                    failures.push(format!("{}: {er}", e.name));
                }
            }
        }
        // Refresh what RustyBox knows is on the console.
        job.step("Updating the list of games on the console");
        let c3 = c.clone();
        if let Ok(Ok(games)) = tokio::task::spawn_blocking(move || {
            let mut f = c3.connect()?;
            let never = || false;
            let quiet = |_: f32, _: &str| {};
            let g = console::scan_games(
                &mut f,
                &c3,
                &Ctl {
                    cancelled: &never,
                    progress: &quiet,
                },
            );
            f.quit();
            g
        })
        .await
        {
            store_games(&cache, &games);
        }
        job.result(
            "console_send",
            json!({"done": done, "failed": failures.len(), "total": n}),
        );
        if failures.is_empty() {
            Ok(())
        } else {
            Err(Error::backend(format!(
                "{} of {n} failed:\n{}",
                failures.len(),
                failures.join("\n")
            )))
        }
    });
    Ok(job_id)
}

// ── Fetching from the console ────────────────────────────────────────────────

#[derive(Deserialize)]
struct FetchReq {
    /// A file or folder on the console.
    path: String,
    library_id: i64,
    path_id: i64,
    /// Where inside that library folder to put it (default: its name).
    rel: Option<String>,
}

/// Copy a folder or file from the console into a library folder on this server.
async fn fetch(
    State(st): S,
    UrlPath(id): UrlPath<u32>,
    Json(req): Json<FetchReq>,
) -> ApiResult<Json<Value>> {
    let c = st.consoles.get(id)?;
    let path = valid_target(&req.path)?;
    let (lib, pid) = (req.library_id, req.path_id);
    let lp = st
        .db
        .run(move |conn| library::get_path(conn, lib, pid))
        .await?;
    if lp.is_remote() {
        return Err(ApiError::bad(
            "Fetching straight into a drive on another computer isn't supported: choose a folder on this server, then send it on",
        ));
    }
    if !lp.writable {
        return Err(ApiError::new(
            axum::http::StatusCode::FORBIDDEN,
            "READ_ONLY",
            "That library folder is read-only. Turn on Writable for it first.",
            true,
        ));
    }
    let rel = req
        .rel
        .clone()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| ftp::file_name(&path).to_string());
    crate::fsops::join_rel(std::path::Path::new("/"), &rel)?;
    let job = st.jobs.create_queued(
        "console",
        &format!("Copy {} from {}", ftp::file_name(&path), c.name),
        vec![format!("console:{id}"), format!("dest:{pid}")],
    );
    let job_id = job.id;
    let db = st.db.clone();
    st.jobs.spawn(job, move |job| async move {
        let (j, token) = (job.clone(), job.cancel_token());
        let (p2, root, r2) = (
            path.clone(),
            std::path::PathBuf::from(&lp.path),
            rel.clone(),
        );
        let sent = tokio::task::spawn_blocking(move || {
            let mut f = c.connect()?;
            let cancelled = || token.is_cancelled();
            let progress = |frac: f32, what: &str| {
                j.progress(frac * 95.0);
                if !what.is_empty() {
                    j.step(format!("Copying {what}"));
                }
            };
            let r = console::fetch_tree(
                &mut f,
                &p2,
                &root,
                &r2,
                &Ctl {
                    cancelled: &cancelled,
                    progress: &progress,
                },
            );
            f.quit();
            r
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
        job.log(format!(
            "{} file(s) copied, {} already there",
            sent.files, sent.skipped
        ));
        if let Ok(lib) = db.run(move |c| library::get_library(c, lib)).await {
            job.step("Updating the library");
            library::scan::scan_library(db.clone(), job.clone(), lib).await?;
        }
        Ok(())
    });
    Ok(Json(json!({"job": job_id})))
}

// ── Mock mode ────────────────────────────────────────────────────────────────

/// With `--mock`: start a pretend Aurora on a sample folder and add it as a console.
pub fn start_mock(st: &AppState) -> Result<(), Error> {
    let root = st.cfg.config_dir.join("mock-console");
    console::seed_mock(&root)?;
    let srv = fake::start(&root, "xbox", "xbox")?;
    let port = srv.addr.port();
    let existing = st
        .consoles
        .all()
        .into_iter()
        .find(|c| c.name == "Mock Xbox 360");
    st.consoles.put(Console {
        id: existing.map(|c| c.id).unwrap_or(0),
        name: "Mock Xbox 360".into(),
        host: "127.0.0.1".into(),
        port,
        ..Default::default()
    })?;
    Ok(())
}

/// Exercise every operation RustyBox needs on the console, in a throwaway folder it cleans up.
async fn selftest(State(st): S, UrlPath(id): UrlPath<u32>) -> ApiResult<Json<Value>> {
    let c = st.consoles.get(id)?;
    let job = st.jobs.create_queued(
        "console",
        &format!("Self-test of {}", c.name),
        vec![format!("console:{id}")],
    );
    let job_id = job.id;
    st.jobs.spawn(job, move |job| async move {
        job.step("Testing the connection and every file operation");
        let steps = tokio::task::spawn_blocking(move || console::selftest(&c))
            .await
            .map_err(|e| Error::backend(e.to_string()))?;
        let bad = steps.iter().filter(|s| !s.ok).count();
        for s in &steps {
            job.log(format!(
                "{} {} ({} ms): {}",
                if s.ok { "✓" } else { "✗" },
                s.name,
                s.ms,
                s.detail
            ));
        }
        job.result("selftest", json!({"steps": steps, "failed": bad}));
        if bad == 0 {
            Ok(())
        } else {
            Err(Error::backend(format!(
                "{bad} of {} checks failed. The log shows which, and what the console said.",
                steps.len()
            )))
        }
    });
    Ok(Json(json!({"job": job_id})))
}
