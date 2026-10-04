//! Content API: the mods/homebrew/trainers/cheats/saves/patches database, installing to the
//! console, and keeping downloads in a library.

use std::{collections::HashSet, sync::Arc};

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};
use crate::{
    content::{self, install},
    convert::Ctl,
    error::Error,
    library,
    transfer::Loc,
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/content/status", get(status))
        .route("/api/content/refresh", post(refresh))
        .route("/api/content", get(list))
        .route("/api/content/item", get(item))
        .route("/api/content/install/plan", post(install_plan))
        .route("/api/content/install/start", post(install_start))
        .route("/api/content/download", post(download))
}

async fn status(State(st): S) -> Json<Value> {
    let items = st.content.items();
    let counts: serde_json::Map<String, Value> = content::KINDS
        .iter()
        .map(|k| {
            (
                (*k).to_string(),
                json!(items.iter().filter(|i| i.kind == *k).count()),
            )
        })
        .collect();
    Json(json!({"fetched": st.content.fetched(), "counts": counts, "mock": st.content.is_mock()}))
}

/// Fetch the database again (a job).
async fn refresh(State(st): S) -> ApiResult<Json<Value>> {
    let job = st
        .jobs
        .create(
            "content",
            "Update the mods and trainers database",
            vec!["content-db".into()],
        )
        .map_err(|b| ApiError::busy(&b))?;
    let id = job.id;
    let st2 = st.clone();
    st.jobs.spawn(job, move |job| async move {
        let (j, token) = (job.clone(), job.cancel_token());
        let n = tokio::task::spawn_blocking(move || {
            let cancelled = || token.is_cancelled();
            let progress = |f: f32, what: &str| {
                j.progress(f * 100.0);
                if !what.is_empty() {
                    j.step(what.to_string());
                }
            };
            st2.content.refresh(&Ctl {
                cancelled: &cancelled,
                progress: &progress,
            })
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
        job.log(format!("{n} items in the database"));
        job.result("content", json!({"items": n}));
        Ok(())
    });
    Ok(Json(json!({"job": id})))
}

#[derive(Deserialize)]
struct ListQuery {
    kind: String,
    q: Option<String>,
    /// Only items for games that are in a library.
    owned: Option<bool>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn list(State(st): S, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    if !content::KINDS.contains(&q.kind.as_str()) {
        return Err(ApiError::bad(format!("Unknown kind '{}'", q.kind)));
    }
    let owned: Option<HashSet<String>> = if q.owned.unwrap_or(false) {
        Some(
            st.db
                .run(|c| {
                    let mut s = c.prepare("SELECT DISTINCT upper(title_id) FROM items WHERE title_id IS NOT NULL AND kind IN ('iso', 'god')")?;
                    Ok(s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<HashSet<_>, _>>()?)
                })
                .await?,
        )
    } else {
        None
    };
    let items = st.content.items();
    let all = content::search(
        &items,
        &q.kind,
        q.q.as_deref().unwrap_or(""),
        owned.as_ref(),
    );
    let (limit, offset) = (q.limit.unwrap_or(100).min(500), q.offset.unwrap_or(0));
    Ok(Json(json!({
        "total": all.len(),
        "items": all.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct IdQuery {
    id: String,
}

async fn item(State(st): S, Query(q): Query<IdQuery>) -> ApiResult<Json<Value>> {
    st.content
        .get(&q.id)
        .map(|i| Json(json!(i)))
        .ok_or_else(|| ApiError::not_found(format!("No item {}", q.id)))
}

// ── Installing ───────────────────────────────────────────────────────────────

#[derive(Deserialize, Clone)]
struct InstallReq {
    console_id: u32,
    /// An item of the database...
    id: Option<String>,
    /// ...or a file or folder from a library, to a folder on the console.
    local: Option<LocalReq>,
}

#[derive(Deserialize, Clone)]
struct LocalReq {
    library_id: i64,
    item_id: i64,
    dest: Option<String>,
}

fn first_segment(rel: &str) -> &str {
    rel.split('/').next().unwrap_or("")
}

fn is_title_id(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Where a library item of this kind usually goes on the console.
fn suggest_dest(kind: &str, rel: &str, name: &str, aurora: &str) -> Option<String> {
    let seg = first_segment(rel);
    let tid = is_title_id(seg).then(|| seg.to_uppercase());
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    match (kind, tid) {
        ("trainers", Some(t)) => Some(format!("{aurora}/User/Trainers/{t}/{stem}")),
        ("saves", Some(t)) => Some(format!("Hdd1/Content/0000000000000000/{t}/00000001")),
        ("mods", _) => Some(format!(
            "Hdd1/Arisen Studio/Mods/{}",
            if seg.is_empty() || seg == name {
                stem
            } else {
                seg
            }
        )),
        ("homebrew", _) => Some(format!("Hdd1/Arisen Studio/Homebrew/{stem}")),
        _ => None,
    }
}

async fn spec_of(
    st: &AppState,
    req: &InstallReq,
) -> ApiResult<(crate::console::Console, install::Spec, Option<String>)> {
    let console = st.consoles.get(req.console_id)?;
    if let Some(id) = &req.id {
        let item = st
            .content
            .get(id)
            .ok_or_else(|| ApiError::not_found(format!("No item {id}")))?;
        let spec = install::spec_for(&item, &console).map_err(|p| ApiError::bad(p.join("\n")))?;
        return Ok((console, spec, None));
    }
    let Some(l) = &req.local else {
        return Err(ApiError::bad("Choose something to install"));
    };
    let (lib, iid) = (l.library_id, l.item_id);
    let (item, lp) = st.db.run(move |c| library::get_item(c, lib, iid)).await?;
    let kind = st
        .db
        .run(move |c| Ok(library::get_library(c, lib)?.kind))
        .await?;
    let dest = l
        .dest
        .clone()
        .filter(|d| !d.trim().is_empty())
        .or_else(|| suggest_dest(&kind, &item.relpath, &item.name, &console.aurora_path))
        .ok_or_else(|| ApiError::bad("Say which folder on the console it should go to"))?;
    let spec = install::spec_for_local(&item.name, Loc::of(&lp), &item.relpath, &dest, &console)
        .map_err(|p| ApiError::bad(p.join("\n")))?;
    Ok((console, spec, Some(dest)))
}

async fn install_plan(State(st): S, Json(req): Json<InstallReq>) -> ApiResult<Json<Value>> {
    let (console, spec, dest) = spec_of(&st, &req).await?;
    let plan = install::plan(&spec, content::DEFAULT_BASE);
    Ok(Json(
        json!({"console": console.name, "dest": dest, "plan": plan}),
    ))
}

async fn install_start(State(st): S, Json(req): Json<InstallReq>) -> ApiResult<Json<Value>> {
    let (console, spec, _) = spec_of(&st, &req).await?;
    let plan = install::plan(&spec, content::DEFAULT_BASE);
    if !plan.ok {
        return Err(ApiError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "PLAN_HAS_PROBLEMS",
            plan.problems.join("\n"),
            true,
        ));
    }
    let job = st.jobs.create_queued(
        "content",
        &format!("Install {} on {}", spec.title, console.name),
        vec![format!("console:{}", console.id)],
    );
    let job_id = job.id;
    let st2 = st.clone();
    st.jobs.spawn(job, move |job| async move {
        let staging = st2
            .cfg
            .config_dir
            .join("staging")
            .join(format!("content-{}", job.id));
        let (j, token) = (job.clone(), job.cancel_token());
        let st3 = st2.clone();
        let report = tokio::task::spawn_blocking(move || {
            let mut f = console.connect()?;
            let cancelled = || token.is_cancelled();
            let progress = |frac: f32, what: &str| {
                j.progress(frac * 100.0);
                if !what.is_empty() {
                    j.step(what.to_string());
                }
            };
            let r = install::run(
                &st3.content,
                &mut f,
                &spec,
                &staging,
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
        for n in &report.notes {
            job.log(n.clone());
        }
        job.result("install", json!({"files": report.files}));
        Ok(())
    });
    Ok(Json(json!({"job": job_id})))
}

// ── Keeping a download in a library ──────────────────────────────────────────

#[derive(Deserialize)]
struct DownloadReq {
    id: String,
    library_id: i64,
    path_id: i64,
}

/// Download an item's files into a library folder here, as `TitleID/Name/file` (or `Category/Name/file`).
async fn download(State(st): S, Json(req): Json<DownloadReq>) -> ApiResult<Json<Value>> {
    let item = st
        .content
        .get(&req.id)
        .ok_or_else(|| ApiError::not_found(format!("No item {}", req.id)))?;
    let (lib, pid) = (req.library_id, req.path_id);
    let lp = st.db.run(move |c| library::get_path(c, lib, pid)).await?;
    if lp.is_remote() || !lp.writable {
        return Err(ApiError::bad(
            "Choose a writable library folder on this server",
        ));
    }
    if item.files.is_empty() {
        return Err(ApiError::bad("This item has nothing to download"));
    }
    let job = st.jobs.create_queued(
        "content",
        &format!("Download {}", item.name),
        vec![format!("dest:{pid}")],
    );
    let job_id = job.id;
    let (st2, db) = (st.clone(), st.db.clone());
    st.jobs.spawn(job, move |job| async move {
        let (j, token) = (job.clone(), job.cancel_token());
        let root = std::path::PathBuf::from(&lp.path);
        let it = item.clone();
        let saved = tokio::task::spawn_blocking(move || -> Result<Vec<String>, Error> {
            let cancelled = || token.is_cancelled();
            let progress = |f: f32, _: &str| j.progress(f * 95.0);
            let ctl = Ctl {
                cancelled: &cancelled,
                progress: &progress,
            };
            let group = it
                .title_id
                .clone()
                .unwrap_or_else(|| crate::convert::plan::safe_name(&it.category));
            let mut saved = Vec::new();
            for f in &it.files {
                let file = f
                    .url
                    .rsplit('/')
                    .next()
                    .filter(|n| !n.is_empty())
                    .map(|n| n.split(['?', '#']).next().unwrap_or(n).to_string())
                    .unwrap_or_else(|| f.name.clone());
                let rel = format!(
                    "{group}/{}/{}",
                    crate::convert::plan::safe_name(&it.name),
                    crate::convert::plan::safe_name(&file)
                );
                let dest = crate::fsops::join_rel(&root, &rel)?;
                if let Some(p) = dest.parent() {
                    std::fs::create_dir_all(p)?;
                }
                st2.content.download(&f.url, &dest, &ctl)?;
                saved.push(rel);
            }
            Ok(saved)
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
        for s in &saved {
            job.log(format!("Saved {s}"));
        }
        if let Ok(lib) = db.run(move |c| library::get_library(c, lib)).await {
            job.step("Updating the library");
            library::scan::scan_library(db.clone(), job.clone(), lib).await?;
        }
        Ok(())
    });
    Ok(Json(json!({"job": job_id})))
}
