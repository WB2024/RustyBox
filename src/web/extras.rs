//! Extras API: the notification test, and the background tasks (scheduled
//! scans and job notifications) that `serve` starts.

use std::{collections::HashSet, sync::Arc, time::Duration};

use axum::{Json, Router, extract::State, routing::post};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};
use crate::{error::Error, extras::notify, library};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/notify/test", post(test_notify))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::backend(e.to_string()))?
}

#[derive(Deserialize)]
struct TestNotify {
    /// Blank: use the saved address.
    notify_url: Option<String>,
}

async fn test_notify(State(st): S, Json(req): Json<TestNotify>) -> ApiResult<Json<Value>> {
    let url = req
        .notify_url
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| st.settings.get().notify_url);
    if url.is_empty() {
        return Err(ApiError::bad("Enter an address to send to first"));
    }
    blocking(move || {
        notify::send(
            &url,
            "RustyBox test",
            "Notifications are working.",
            "done",
            None,
        )
    })
    .await?;
    Ok(Json(json!({"ok": true})))
}

// ── Background tasks ─────────────────────────────────────────────────────────

/// Start the scheduled scans and the job notifier. Called by `serve`, not by tests.
pub fn spawn_background(st: Arc<AppState>) {
    // Notifications: look for jobs that have just finished.
    let s1 = st.clone();
    tokio::spawn(async move {
        let mut seen: HashSet<u64> = s1
            .jobs
            .all()
            .iter()
            .filter(|j| j.status().is_terminal())
            .map(|j| j.id)
            .collect();
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let settings = s1.settings.get();
            for j in s1.jobs.all() {
                if !j.status().is_terminal() || !seen.insert(j.id) {
                    continue;
                }
                let sum = j.summary();
                if settings.notify_url.is_empty() || !notify::wanted(&settings.notify_on, &sum) {
                    continue;
                }
                let (title, msg) = notify::describe(&sum);
                let (url, status, id) = (
                    settings.notify_url.clone(),
                    format!("{:?}", sum.status).to_lowercase(),
                    sum.id,
                );
                let _ = tokio::task::spawn_blocking(move || {
                    notify::send(&url, &title, &msg, &status, Some(id))
                })
                .await;
            }
        }
    });
    // Scheduled scans: every library, every N minutes.
    tokio::spawn(async move {
        let mut last = std::time::Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let every = st.settings.get().scan_interval_minutes as u64;
            if every == 0 || last.elapsed() < Duration::from_secs(every * 60) {
                continue;
            }
            last = std::time::Instant::now();
            if let Ok(libs) = st.db.run(|c| library::list_libraries(c)).await {
                for lib in libs {
                    // A library that is already being scanned is simply skipped this time.
                    let _ = super::libraries::start_scan(&st, lib);
                }
            }
        }
    });
}
