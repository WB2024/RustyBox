//! Title updates API: the ones in a library, finding more on XboxUnity, and installing them on the
//! console.

use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};
use crate::{
    console::{self, ftp},
    convert::{Ctl, plan::safe_name},
    error::Error,
    library,
    transfer::Loc,
    updates::{self, Compat, unity},
    xbox::stfs,
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/updates/local", get(local))
        .route("/api/updates/unity/search", get(unity_search))
        .route("/api/updates/unity/title", get(unity_title))
        .route("/api/updates/unity/download", post(unity_download))
        .route("/api/updates/install/plan", post(install_plan))
        .route("/api/updates/install/start", post(install_start))
        .route("/api/updates/console", get(on_console))
}

async fn limited<T: Send + 'static>(
    secs: u64,
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    match tokio::time::timeout(Duration::from_secs(secs), tokio::task::spawn_blocking(f)).await {
        Ok(r) => r.map_err(|e| Error::backend(e.to_string()))?,
        Err(_) => Err(Error::backend("That took too long")),
    }
}

/// Media IDs of the games you have, by title ID.
async fn owned_media(st: &AppState) -> Result<HashMap<String, Vec<String>>, Error> {
    st.db
        .run(|c| {
            let mut s = c.prepare("SELECT upper(title_id), upper(media_id) FROM items WHERE title_id IS NOT NULL AND media_id IS NOT NULL AND kind IN ('iso', 'god')")?;
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for r in s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                let (t, med) = r?;
                m.entry(t).or_default().push(med);
            }
            Ok(m)
        })
        .await
}

// ── In a library ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct LocalQuery {
    library_id: i64,
}

/// Title updates in a library's folders on this server (read from their headers).
async fn local(State(st): S, Query(q): Query<LocalQuery>) -> ApiResult<Json<Value>> {
    let lib = q.library_id;
    let library = st.db.run(move |c| library::get_library(c, lib)).await?;
    let owned = owned_media(&st).await?;
    let found = limited(120, move || {
        let mut out = Vec::new();
        for p in library.paths.iter().filter(|p| !p.is_remote()) {
            for tu in updates::scan_dir(std::path::Path::new(&p.path)) {
                out.push((p.id, p.label.clone(), tu));
            }
        }
        Ok(out)
    })
    .await?;
    let cat = crate::xbox::catalog::get();
    Ok(Json(
        json!({"updates": found.iter().map(|(pid, label, tu)| {
        let c = updates::compat(&tu.media_id, &tu.title_id, owned.get(&tu.title_id).map(|v| v.as_slice()).unwrap_or(&[]));
        json!({"path_id": pid, "path_label": label, "game": cat.name(&tu.title_id, Some(&tu.media_id)), "compat": c, "tu": tu})
    }).collect::<Vec<_>>()}),
    ))
}

// ── XboxUnity ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn unity_search(State(st): S, Query(q): Query<SearchQuery>) -> ApiResult<Json<Value>> {
    if q.q.trim().len() < 2 {
        return Err(ApiError::bad("Type at least two letters"));
    }
    let mock = st.cfg.mock;
    let titles = limited(40, move || unity::Client::new(mock).search(&q.q)).await?;
    Ok(Json(json!({"titles": titles})))
}

#[derive(Deserialize)]
struct TitleQuery {
    titleid: String,
}

async fn unity_title(State(st): S, Query(q): Query<TitleQuery>) -> ApiResult<Json<Value>> {
    let tid = q.titleid.to_uppercase();
    let owned = owned_media(&st).await?;
    let mock = st.cfg.mock;
    let t2 = tid.clone();
    let ups = limited(40, move || unity::Client::new(mock).updates(&t2)).await?;
    let mine = owned.get(&tid).cloned().unwrap_or_default();
    Ok(Json(json!({
        "title_id": tid,
        "name": crate::xbox::catalog::get().name(&tid, None),
        "i_have": !mine.is_empty(),
        "updates": ups.iter().map(|u| json!({"update": u, "compat": updates::compat(&u.media_id, &tid, &mine), "mine": mine.iter().any(|m| m.eq_ignore_ascii_case(&u.media_id))})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct DownloadReq {
    title_id: String,
    /// XboxUnity update IDs to fetch.
    tuids: Vec<String>,
    library_id: i64,
    path_id: i64,
}

/// Download updates from XboxUnity into a library folder, as `TitleID - Name/TitleID_TUn_id`.
async fn unity_download(State(st): S, Json(req): Json<DownloadReq>) -> ApiResult<Json<Value>> {
    if req.tuids.is_empty() {
        return Err(ApiError::bad("Choose at least one update"));
    }
    let (lib, pid) = (req.library_id, req.path_id);
    let lp = st.db.run(move |c| library::get_path(c, lib, pid)).await?;
    if lp.is_remote() || !lp.writable {
        return Err(ApiError::bad(
            "Choose a writable library folder on this server",
        ));
    }
    let tid = req.title_id.to_uppercase();
    let (mock, t2) = (st.cfg.mock, tid.clone());
    let all = limited(40, move || unity::Client::new(mock).updates(&t2)).await?;
    let wanted: Vec<unity::Update> = all
        .into_iter()
        .filter(|u| req.tuids.contains(&u.tuid))
        .collect();
    if wanted.len() != req.tuids.len() {
        return Err(ApiError::bad(
            "One of those updates isn't listed on XboxUnity any more. Look the game up again.",
        ));
    }
    let name = crate::xbox::catalog::get()
        .name(&tid, None)
        .map(safe_name)
        .unwrap_or_default();
    let folder = if name.is_empty() {
        tid.clone()
    } else {
        format!("{tid} - {name}")
    };
    let job = st.jobs.create_queued(
        "updates",
        &format!("Download {} update(s) for {folder}", wanted.len()),
        vec![format!("dest:{pid}")],
    );
    let job_id = job.id;
    let db = st.db.clone();
    st.jobs.spawn(job, move |job| async move {
        let (j, token) = (job.clone(), job.cancel_token());
        let root = std::path::PathBuf::from(&lp.path);
        let n = wanted.len();
        let saved = tokio::task::spawn_blocking(move || -> Result<Vec<String>, Error> {
            let cancelled = || token.is_cancelled();
            let mut saved = Vec::new();
            for (i, u) in wanted.iter().enumerate() {
                let progress = |f: f32, _: &str| j.progress((i as f32 + f) / n as f32 * 95.0);
                let rel = format!("{folder}/{tid}_TU{}_{}", u.version, u.tuid);
                let dest = crate::fsops::join_rel(&root, &rel)?;
                if let Some(p) = dest.parent() {
                    std::fs::create_dir_all(p)?;
                }
                unity::Client::new(mock).download(
                    &tid,
                    u,
                    &dest,
                    &Ctl {
                        cancelled: &cancelled,
                        progress: &progress,
                    },
                )?;
                saved.push(rel);
            }
            Ok(saved)
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
        for s in &saved {
            job.log(format!("Saved {s}"));
        }
        if let Ok(l) = db.run(move |c| library::get_library(c, lib)).await {
            job.step("Updating the library");
            library::scan::scan_library(db.clone(), job.clone(), l).await?;
        }
        Ok(())
    });
    Ok(Json(json!({"job": job_id})))
}

// ── Installing on the console ────────────────────────────────────────────────

#[derive(Deserialize, Clone)]
struct InstallReq {
    console_id: u32,
    files: Vec<FileRef>,
}

#[derive(Deserialize, Clone)]
struct FileRef {
    library_id: i64,
    path_id: i64,
    relpath: String,
}

struct Prepared {
    name: String,
    title_id: String,
    tu_version: u32,
    compat: Compat,
    size: u64,
    target: String,
    loc: Loc,
    rel: String,
    problems: Vec<String>,
}

async fn prepare(st: &AppState, req: &InstallReq) -> ApiResult<(console::Console, Vec<Prepared>)> {
    let c = st.consoles.get(req.console_id)?;
    let owned = owned_media(st).await?;
    let mut out = Vec::new();
    for f in &req.files {
        let (lib, pid) = (f.library_id, f.path_id);
        let lp = st
            .db
            .run(move |conn| library::get_path(conn, lib, pid))
            .await?;
        crate::fsops::join_rel(std::path::Path::new("/"), &f.relpath)?;
        let (loc, rel, c2) = (Loc::of(&lp), f.relpath.clone(), c.clone());
        let owned2 = owned.clone();
        let p = limited(60, move || {
            let head = loc.read(&rel, 0, (0x3A4 + 0x1400) as u64)?;
            let mut problems = Vec::new();
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            let size = loc.stat(&rel)?.map(|s| s.size).unwrap_or(0);
            let (title_id, tu_version, media, compat) = match stfs::parse(&head) {
                Ok(h) if h.content_type == updates::TU_CONTENT_TYPE => {
                    let v = updates::tu_number(&h.display_name, &name);
                    let compat = updates::compat(
                        &h.media_id,
                        &h.title_id,
                        owned2
                            .get(&h.title_id.to_uppercase())
                            .map(|v| v.as_slice())
                            .unwrap_or(&[]),
                    );
                    (h.title_id, v, h.media_id, compat)
                }
                Ok(_) => {
                    problems.push(
                        "This isn't a title update (its content type is something else)"
                            .to_string(),
                    );
                    (String::new(), 0, String::new(), Compat::Unknown)
                }
                Err(e) => {
                    problems.push(format!("Not a title update: {e}"));
                    (String::new(), 0, String::new(), Compat::Unknown)
                }
            };
            let _ = media;
            let target = if title_id.is_empty() {
                String::new()
            } else {
                updates::install_path(&c2, &title_id, &name)
            };
            Ok(Prepared {
                name,
                title_id,
                tu_version,
                compat,
                size,
                target,
                loc,
                rel,
                problems,
            })
        })
        .await?;
        out.push(p);
    }
    if out.is_empty() {
        return Err(ApiError::bad("Choose at least one update"));
    }
    Ok((c, out))
}

fn plan_json(c: &console::Console, p: &[Prepared]) -> Value {
    let ok = p.iter().all(|x| x.problems.is_empty());
    json!({
        "console": c.name,
        "ok": ok,
        "entries": p.iter().map(|x| json!({
            "name": x.name, "title_id": x.title_id, "tu_version": x.tu_version, "size": x.size,
            "game": crate::xbox::catalog::get().name(&x.title_id, None),
            "target": x.target, "compat": x.compat, "problems": x.problems,
            "warnings": match x.compat {
                Compat::Incompatible => vec!["This update is for a different release of the game than any RustyBox knows about: it may not apply to your copy.".to_string()],
                Compat::Unknown => vec!["RustyBox can't tell whether this update fits your copy of the game.".to_string()],
                Compat::Compatible => vec![],
            },
        })).collect::<Vec<_>>(),
    })
}

async fn install_plan(State(st): S, Json(req): Json<InstallReq>) -> ApiResult<Json<Value>> {
    let (c, p) = prepare(&st, &req).await?;
    Ok(Json(plan_json(&c, &p)))
}

async fn install_start(State(st): S, Json(req): Json<InstallReq>) -> ApiResult<Json<Value>> {
    let (c, prepared) = prepare(&st, &req).await?;
    if let Some(bad) = prepared.iter().find(|p| !p.problems.is_empty()) {
        return Err(ApiError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "PLAN_HAS_PROBLEMS",
            format!("{}: {}", bad.name, bad.problems.join("; ")),
            true,
        ));
    }
    let title = format!("Install {} update(s) on {}", prepared.len(), c.name);
    let job = st
        .jobs
        .create_queued("updates", &title, vec![format!("console:{}", c.id)]);
    let job_id = job.id;
    st.jobs.spawn(job, move |job| async move {
        let n = prepared.len();
        let (j, token) = (job.clone(), job.cancel_token());
        let notes = tokio::task::spawn_blocking(move || -> Result<Vec<String>, Error> {
            let mut f = c.connect()?;
            let cancelled = || token.is_cancelled();
            let mut notes = Vec::new();
            for (i, p) in prepared.iter().enumerate() {
                let progress = |frac: f32, _: &str| {
                    j.progress((i as f32 + frac.clamp(0.0, 1.0)) / n as f32 * 100.0)
                };
                j.step(format!("{} of {n}: {}", i + 1, p.name));
                let s = console::send_tree(
                    &mut f,
                    &p.loc,
                    &p.rel,
                    &p.target,
                    true,
                    &Ctl {
                        cancelled: &cancelled,
                        progress: &progress,
                    },
                )?;
                notes.push(format!(
                    "{} → {} ({} sent, {} already there)",
                    p.name, p.target, s.files, s.skipped
                ));
            }
            f.quit();
            Ok(notes)
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
        for note in notes {
            job.log(note);
        }
        Ok(())
    });
    Ok(Json(json!({"job": job_id})))
}

// ── What is installed on the console ─────────────────────────────────────────

#[derive(Deserialize)]
struct ConsoleQuery {
    console_id: u32,
}

/// The title updates already on the console, for the games found by its last scan.
async fn on_console(State(st): S, Query(q): Query<ConsoleQuery>) -> ApiResult<Json<Value>> {
    let c = st.consoles.get(q.console_id)?;
    let cache = st
        .cfg
        .config_dir
        .join(format!("console-{}-games.json", c.id));
    let games: Vec<console::ConsoleGame> = std::fs::read(&cache)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| serde_json::from_value(v["games"].clone()).ok())
        .ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::CONFLICT,
                "NOT_SCANNED",
                "Scan the console first (Console page).",
                true,
            )
        })?;
    let mut ids: Vec<String> = games.iter().map(|g| g.title_id.to_uppercase()).collect();
    ids.sort();
    ids.dedup();
    let c2 = c.clone();
    let found = limited(180, move || {
        let mut f = c2.connect()?;
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for t in ids {
            let dir = updates::install_path(&c2, &t, "");
            let dir = dir.trim_end_matches('/');
            let names: Vec<String> = match f.list(dir) {
                Ok(l) => l
                    .into_iter()
                    .filter(|e| !e.is_dir)
                    .map(|e| e.name)
                    .collect(),
                Err(Error::NotFound(_)) => vec![],
                Err(e) => return Err(e),
            };
            out.push((t, names));
        }
        f.quit();
        Ok(out)
    })
    .await?;
    let _ = ftp::ftp_path("/");
    let cat = crate::xbox::catalog::get();
    Ok(Json(
        json!({"games": found.iter().map(|(t, names)| json!({"title_id": t, "name": cat.name(t, None), "installed": names})).collect::<Vec<_>>()}),
    ))
}
