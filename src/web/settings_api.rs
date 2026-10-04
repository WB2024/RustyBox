//! Settings API. Secrets are never sent back: only whether one is set, and its last four characters.

use std::sync::Arc;

use axum::{Json, Router, extract::State, routing::get};
use serde::Deserialize;
use serde_json::Value;

use super::{ApiError, ApiResult, AppState, S, igdb};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/settings", get(get_settings).put(put_settings))
}

fn view(st: &AppState) -> Value {
    let mut v = st.settings.get().public();
    v["igdb"] = igdb::status_json(st);
    v
}

async fn get_settings(State(st): S) -> Json<Value> {
    Json(view(&st))
}

#[derive(Deserialize)]
struct SettingsReq {
    convert_threads: Option<u32>,
    god_layout: Option<String>,
    igdb_client_id: Option<String>,
    /// Leave out to keep the saved secret.
    igdb_client_secret: Option<String>,
    igdb_auto: Option<bool>,
    /// Forget the saved IGDB credentials.
    #[serde(default)]
    igdb_remove: bool,
    scan_interval_minutes: Option<u32>,
    /// Leave out or blank to keep the saved address (it can hold a secret).
    notify_url: Option<String>,
    #[serde(default)]
    notify_remove: bool,
    notify_on: Option<String>,
}

async fn put_settings(State(st): S, Json(req): Json<SettingsReq>) -> ApiResult<Json<Value>> {
    let mut s = st.settings.get();
    if let Some(t) = req.convert_threads {
        s.convert_threads = t;
    }
    if let Some(l) = req.god_layout {
        s.god_layout = l;
    }
    if let Some(a) = req.igdb_auto {
        s.igdb_auto = a;
    }
    if req.igdb_remove {
        s.igdb_client_id.clear();
        s.igdb_client_secret.clear();
    } else {
        if let Some(id) = req.igdb_client_id {
            s.igdb_client_id = id;
        }
        if let Some(secret) = req.igdb_client_secret.filter(|k| !k.trim().is_empty()) {
            s.igdb_client_secret = secret;
        }
    }
    if let Some(m) = req.scan_interval_minutes {
        s.scan_interval_minutes = m;
    }
    if req.notify_remove {
        s.notify_url.clear();
    } else if let Some(u) = req.notify_url.filter(|u| !u.trim().is_empty()) {
        s.notify_url = u;
    }
    if let Some(m) = req.notify_on {
        s.notify_on = m;
    }
    let s = s.validate().map_err(ApiError::bad)?;
    st.settings
        .set(s)
        .map_err(|e| ApiError::bad(format!("Could not save: {e}")))?;
    Ok(Json(view(&st)))
}
