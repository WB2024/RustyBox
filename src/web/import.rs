//! Import API: look inside a source folder, plan an import, then run it as a job.

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};
use crate::{
    error::Error,
    library,
    transfer::{
        Loc,
        plan::{self, Plan, Request, Source},
        run::{self, RunOpts},
    },
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/import/scan", post(scan))
        .route("/api/import/plan", post(plan_only))
        .route("/api/import/start", post(start))
        .route("/api/libraries/{id}/paths/{pid}/dirs", get(dirs))
}

/// Anything slow (scanning a big drive over the network) runs off the async threads, with a limit.
async fn limited<T: Send + 'static>(
    secs: u64,
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    match tokio::time::timeout(Duration::from_secs(secs), tokio::task::spawn_blocking(f)).await {
        Ok(r) => r.map_err(|e| Error::backend(e.to_string()))?,
        Err(_) => Err(Error::backend(
            "That took too long (a network share or drive may not be responding)",
        )),
    }
}

#[derive(Deserialize)]
struct ScanReq {
    source: Source,
}

/// What is in a source folder that can be imported.
async fn scan(State(st): S, Json(req): Json<ScanReq>) -> ApiResult<Json<Value>> {
    let (roots, source) = (st.cfg.roots.clone(), req.source.clone());
    let resolved = st
        .db
        .run(move |c| plan::resolve(c, &source, &roots))
        .await?;
    let label = resolved.label.clone();
    let candidates = limited(300, move || plan::discover(&resolved)).await?;
    Ok(Json(json!({"label": label, "candidates": candidates})))
}

async fn make_plan(st: &AppState, req: &Request) -> ApiResult<Plan> {
    let (r, roots) = (req.clone(), st.cfg.roots.clone());
    let gathered = st.db.run(move |c| plan::gather(c, &r, &roots)).await?;
    let layout = st.settings.get().god_layout;
    let staging_free = crate::library::health::space(&st.cfg.config_dir).map(|s| s.1);
    let r2 = req.clone();
    Ok(limited(300, move || {
        plan::build(&gathered, &r2, &layout, staging_free)
    })
    .await?)
}

async fn plan_only(State(st): S, Json(req): Json<Request>) -> ApiResult<Json<Plan>> {
    Ok(Json(make_plan(&st, &req).await?))
}

/// Plan an import again and, if it is sound, run it as a job. Returns the job's id.
pub(super) async fn start_import(st: &Arc<AppState>, req: Request) -> ApiResult<u64> {
    // Planned again here, so what runs is what is checked now, not what was shown earlier.
    let plan = make_plan(st, &req).await?;
    if !plan.ok {
        let mut why: Vec<String> = plan.problems.clone();
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
    let mut resources = vec![format!("dest:{}", req.dest.path_id)];
    if let Some(a) = req.also_to {
        resources.push(format!("dest:{}", a.path_id));
    }
    match &req.source {
        Source::Folder { path } => resources.push(format!("src:{path}")),
        Source::Drive { path_id, rel, .. } => resources.push(format!("src:{path_id}:{rel}")),
        Source::Items { items } => resources.extend(
            items
                .iter()
                .map(|i| format!("item:{}:{}", i.library_id, i.item_id)),
        ),
    }
    let title = match plan.entries.as_slice() {
        [one] => format!("{}: {}", plan.mode.label(), one.name),
        many => format!("{}: {} games", plan.mode.label(), many.len()),
    };
    let job = st.jobs.create_queued("import", &title, resources);
    let id = job.id;
    let opts = RunOpts {
        threads: st.settings.get().convert_threads as usize,
        overwrite: req.overwrite || req.replace,
        stage_root: st.cfg.config_dir.join("staging"),
    };
    let (db, st2) = (st.db.clone(), st.clone());
    st.jobs.spawn(job, move |job| async move {
        let result = run::run(db, job, plan, opts).await;
        super::igdb::kick(&st2); // look up the new games
        result
    });
    Ok(id)
}

async fn start(State(st): S, Json(req): Json<Request>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"job": start_import(&st, req).await?})))
}

#[derive(Deserialize)]
struct DirsQuery {
    path: Option<String>,
}

/// Sub-folders inside a library folder (here or on a drive), for choosing where to import from.
async fn dirs(
    State(st): S,
    UrlPath((id, pid)): UrlPath<(i64, i64)>,
    Query(q): Query<DirsQuery>,
) -> ApiResult<Json<Value>> {
    let lp = st.db.run(move |c| library::get_path(c, id, pid)).await?;
    let rel = q.path.unwrap_or_default().trim_matches('/').to_string();
    crate::fsops::join_rel(std::path::Path::new("/"), &rel)?;
    let (loc, rel2) = (Loc::of(&lp), rel.clone());
    let entries = limited(20, move || -> Result<Vec<String>, Error> {
        Ok(match &loc {
            Loc::Local(root) => crate::fsops::list_dir(root, &rel2)?
                .into_iter()
                .filter(|e| e.is_dir)
                .map(|e| e.name)
                .collect(),
            Loc::Remote(r) => r
                .list(&rel2)?
                .into_iter()
                .filter(|e| e.is_dir)
                .map(|e| e.name)
                .collect(),
        })
    })
    .await?;
    let parent = rel
        .rsplit_once('/')
        .map(|(p, _)| p.to_string())
        .or_else(|| (!rel.is_empty()).then(String::new));
    Ok(Json(
        json!({"path": rel, "parent": parent, "dirs": entries}),
    ))
}
