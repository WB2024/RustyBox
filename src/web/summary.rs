//! One small answer about the whole of RustyBox, for dashboards (Glance, Homepage, anything that
//! can read JSON). Cheap to ask: it only reads the database, never the disks.

use std::sync::Arc;

use axum::{Json, Router, extract::State, routing::get};
use serde_json::{Value, json};

use super::{ApiResult, AppState, S};
use crate::{grabber::store, library};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/summary", get(summary))
}

async fn summary(State(st): S) -> ApiResult<Json<Value>> {
    let (games, stats, grabs, wanted, recent) = st
        .db
        .run(|c| {
            let recent: Vec<String> = {
                let mut s = c.prepare(
                    "SELECT upper(title_id) FROM items WHERE kind IN ('iso', 'god') AND title_id IS NOT NULL
                       AND (content_type IS NULL OR content_type IN ('00007000', '00005000', '000D0000', '00004000'))
                     GROUP BY upper(title_id) ORDER BY max(mtime) DESC LIMIT 12",
                )?;
                s.query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<_, _>>()?
            };
            Ok((
                library::games(c, "")?,
                library::stats(c)?,
                store::list_grabs(c, 200)?,
                store::list_wanted(c)?,
                recent,
            ))
        })
        .await?;
    let now = crate::jobs::now();
    let jobs: Vec<_> = st.jobs.all().iter().map(|j| j.summary()).collect();
    let running: Vec<Value> = jobs
        .iter()
        .filter(|j| !j.status.is_terminal())
        .take(5)
        .map(|j| json!({"id": j.id, "title": j.title, "pct": j.pct, "step": j.step, "status": j.status}))
        .collect();
    let failed_24h = jobs
        .iter()
        .filter(|j| {
            matches!(j.status, crate::jobs::Status::Failed)
                && now.saturating_sub(j.finished.unwrap_or(j.started)) < 86_400
        })
        .count();
    let open = |g: &store::Grab, s: &[&str]| s.contains(&g.status.as_str());
    let latest: Vec<Value> = recent
        .iter()
        .filter_map(|t| games.iter().find(|g| &g.title_id == t))
        .map(|g| {
            json!({
                "title_id": g.title_id,
                "name": g.info.as_ref().and_then(|i| i.name.clone()).or_else(|| g.name.clone()),
                "cover": g.info.as_ref().and_then(|i| i.cover.clone()),
            })
        })
        .collect();
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "games": games.len(),
        "games_bytes": games.iter().map(|g| g.bytes).sum::<i64>(),
        "libraries": stats.len(),
        "items": stats.values().map(|s| s.items).sum::<i64>(),
        "duplicates": games.iter().filter(|g| g.duplicate).count(),
        "without_info": games.iter().filter(|g| g.info.is_none()).count(),
        "jobs": {"running": running, "failed_24h": failed_24h},
        "downloads": {
            "active": grabs.iter().filter(|g| open(g, &["queued", "downloading"])).count(),
            "importing": grabs.iter().filter(|g| open(g, &["completed", "unpacking", "importing"])).count(),
            "failed": grabs.iter().filter(|g| open(g, &["failed", "import_failed"])).count(),
            "imported": grabs.iter().filter(|g| open(g, &["imported"])).count(),
        },
        "wanted": {"total": wanted.len(), "waiting": wanted.iter().filter(|w| w.monitored && w.status == "wanted").count()},
        "latest": latest,
    })))
}
