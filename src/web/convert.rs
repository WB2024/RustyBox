//! Conversion API: plan a conversion (nothing is written), then start it as a job.

use std::{sync::Arc, time::Duration};

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};
use crate::{
    convert::{
        Ctl,
        plan::{self, Op, Plan, Request},
        run::{self, RunOpts},
        verify,
    },
    error::Error,
    library::{self, health},
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/iso/check", post(check_iso))
        .route("/api/convert/plan", post(plan_only))
        .route("/api/convert/start", post(start))
}

async fn make_plan(st: &AppState, req: &Request) -> ApiResult<Plan> {
    let r = req.clone();
    let gathered = st.db.run(move |c| plan::gather(c, &r)).await?;
    let (req2, roots, layout) = (
        req.clone(),
        st.cfg.roots.clone(),
        st.settings.get().god_layout,
    );
    // Inspecting files can be slow on a network share, so it runs off the async threads, with a limit.
    let work = tokio::task::spawn_blocking(move || plan::build(&gathered, &req2, &roots, &layout));
    match tokio::time::timeout(Duration::from_secs(60), work).await {
        Ok(r) => Ok(r.map_err(|e| Error::backend(e.to_string()))??),
        Err(_) => Err(Error::backend(
            "Checking the files took too long (a network share may be down)",
        )
        .into()),
    }
}

async fn plan_only(State(st): S, Json(req): Json<Request>) -> ApiResult<Json<Plan>> {
    Ok(Json(make_plan(&st, &req).await?))
}

async fn start(State(st): S, Json(req): Json<Request>) -> ApiResult<Json<Value>> {
    // The plan is made again here, so what runs is what is checked now, not what was shown earlier.
    let plan = make_plan(&st, &req).await?;
    if !plan.ok {
        let mut why: Vec<String> = plan.problems.clone();
        for e in &plan.entries {
            why.extend(e.problems.iter().map(|p| format!("{}: {p}", e.name)));
        }
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PLAN_HAS_PROBLEMS",
            why.join("\n"),
            true,
        ));
    }
    let mut resources = vec![format!("path:{}", plan.dest_root.display())];
    if req.op != Op::Create {
        resources.extend(
            req.items
                .iter()
                .map(|i| format!("item:{}:{}", i.library_id, i.item_id)),
        );
    }
    let title = match plan.entries.as_slice() {
        [one] => format!("{}: {}", plan.op_label, one.name),
        many => format!("{}: {} games", plan.op_label, many.len()),
    };
    let job = st.jobs.create_queued("convert", &title, resources);
    let id = job.id;
    let (db, lib_id, threads) = (
        st.db.clone(),
        req.dest_library_id,
        st.settings.get().convert_threads as usize,
    );
    let st2 = st.clone();
    st.jobs.spawn(job, move |job| async move {
        let result = run::run(
            db,
            job,
            plan,
            lib_id,
            RunOpts {
                threads,
                overwrite: req.overwrite,
            },
        )
        .await;
        super::igdb::kick(&st2); // look up the new games
        result
    });
    Ok(Json(json!({"job": id})))
}

// ── Check / fix an ISO with abgx360 ──────────────────────────────────────────

#[derive(serde::Deserialize)]
struct CheckReq {
    library_id: i64,
    item_id: i64,
    /// Let abgx360 repair the image in place. Without this the check cannot write at all.
    #[serde(default)]
    fix: bool,
    /// Keep a copy of the image before fixing it (needs the free space).
    #[serde(default = "yes")]
    backup: bool,
}

fn yes() -> bool {
    true
}

async fn check_iso(State(st): S, Json(req): Json<CheckReq>) -> ApiResult<Json<Value>> {
    let program = verify::find(st.cfg.abgx360.as_deref())
        .ok_or_else(|| ApiError::bad("abgx360 isn't installed here, so ISOs can't be checked."))?;
    let (lib_id, item_id) = (req.library_id, req.item_id);
    let (item, lp) = st
        .db
        .run(move |c| library::get_item(c, lib_id, item_id))
        .await?;
    if item.kind != "iso" {
        return Err(ApiError::bad("Only ISO images can be checked."));
    }
    if lp.is_remote() {
        return Err(ApiError::bad(
            "That ISO is on a remote drive. Import it into a local library to check it.",
        ));
    }
    let path = std::path::Path::new(&lp.path).join(&item.relpath);
    let meta = tokio::fs::metadata(&path).await.map_err(|_| {
        ApiError::bad("The ISO isn't available right now (is its disk or share mounted?).")
    })?;
    let backup = verify::backup_path(&path);
    if req.fix {
        if !lp.writable {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "READ_ONLY",
                "Fixing changes the ISO, so its folder must be writable. Turn on writing for it first.",
                true,
            ));
        }
        if req.backup {
            if backup.exists() {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "BACKUP_EXISTS",
                    format!(
                        "A backup already exists: {}. Remove it first.",
                        backup.display()
                    ),
                    true,
                ));
            }
            let free = health::space(std::path::Path::new(&lp.path))
                .map(|s| s.1)
                .unwrap_or(0);
            if free < meta.len() + 256 * 1024 * 1024 {
                return Err(ApiError::new(
                    StatusCode::INSUFFICIENT_STORAGE,
                    "NO_SPACE",
                    "Not enough free space for a backup copy. Free some space, or fix without a backup (risky).",
                    true,
                ));
            }
        }
    }
    let name = item.game_name.clone().unwrap_or(item.name.clone());
    let title = format!(
        "{}: {name}",
        if req.fix {
            "Check and fix ISO"
        } else {
            "Check ISO"
        }
    );
    let job = st
        .jobs
        .create_queued("verify", &title, vec![format!("item:{lib_id}:{item_id}")]);
    let id = job.id;
    let (db, fix, make_backup) = (st.db.clone(), req.fix, req.backup);
    st.jobs.spawn(job, move |job| async move {
        if fix && make_backup {
            job.step("Making a backup copy first");
            let (j, token, from, to) = (
                job.clone(),
                job.cancel_token(),
                path.clone(),
                backup.clone(),
            );
            tokio::task::spawn_blocking(move || {
                let cancelled = || token.is_cancelled();
                let progress = |f: f32, _: &str| j.progress(f * 50.0);
                verify::copy_backup(
                    &from,
                    &to,
                    &Ctl {
                        cancelled: &cancelled,
                        progress: &progress,
                    },
                )
            })
            .await
            .map_err(|e| Error::backend(e.to_string()))??;
            job.log(format!("Backup saved as {}", backup.display()));
        }
        job.step(if fix {
            "Checking and fixing with abgx360"
        } else {
            "Checking with abgx360"
        });
        verify::run(job.clone(), program, path, fix).await?;
        if fix {
            // The image changed, so read its header again.
            let lib = db.run(move |c| library::get_library(c, lib_id)).await?;
            library::scan::scan_library(db, job, lib).await?;
        }
        Ok(())
    });
    Ok(Json(json!({"job": id})))
}
