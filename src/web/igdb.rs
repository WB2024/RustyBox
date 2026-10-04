//! IGDB: connection test, looking games up (all pending ones as a job, or one at a time), choosing a
//! match by hand, and serving the saved covers.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path as UrlPath, State},
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};
use crate::{
    error::Error,
    igdb::{self, Credentials, store},
    jobs::Job,
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/igdb/status", get(status))
        .route("/api/igdb/test", post(test))
        .route("/api/igdb/fetch", post(fetch))
        .route("/api/igdb/search", post(search))
        .route(
            "/api/games/{title_id}/info",
            get(get_info).put(set_info).delete(clear_info),
        )
        .route("/api/games/{title_id}/info/refresh", post(refresh))
        .route("/api/covers/{file}", get(cover))
}

fn from_environment(st: &AppState, names: &[&str], dot_names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|n| std::env::var(n).ok())
        .or_else(|| {
            dot_names
                .iter()
                .find_map(|n| st.cfg.dotenv.get(*n).cloned())
        })
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The credentials in use and where they came from. The environment (or a `.env` file) wins over
/// what was saved in Settings, like the login does.
pub fn resolve(st: &AppState) -> Option<(Credentials, &'static str)> {
    let id = from_environment(
        st,
        &["RUSTYBOX_IGDB_CLIENT_ID", "IGDB_CLIENT_ID"],
        &["RUSTYBOX_IGDB_CLIENT_ID", "IGDB_CLIENT_ID", "clientid"],
    );
    let secret = from_environment(
        st,
        &["RUSTYBOX_IGDB_CLIENT_SECRET", "IGDB_CLIENT_SECRET"],
        &[
            "RUSTYBOX_IGDB_CLIENT_SECRET",
            "IGDB_CLIENT_SECRET",
            "clientsecret",
        ],
    );
    if let (Some(client_id), Some(client_secret)) = (id, secret) {
        return Some((
            Credentials {
                client_id,
                client_secret,
            },
            "environment",
        ));
    }
    let s = st.settings.get();
    (!s.igdb_client_id.is_empty() && !s.igdb_client_secret.is_empty()).then_some({
        (
            Credentials {
                client_id: s.igdb_client_id,
                client_secret: s.igdb_client_secret,
            },
            "settings",
        )
    })
}

fn credentials(st: &AppState) -> Result<Credentials, ApiError> {
    if st.igdb.is_mock() {
        return Ok(Credentials {
            client_id: "mock".into(),
            client_secret: "mock".into(),
        });
    }
    resolve(st).map(|(c, _)| c).ok_or_else(|| {
        ApiError::bad("IGDB isn't set up yet. Add your Twitch Client ID and Secret in Settings.")
    })
}

fn hint(v: &str) -> String {
    crate::settings::tail_hint(v)
}

pub fn status_json(st: &AppState) -> Value {
    if st.igdb.is_mock() {
        return json!({"configured": true, "source": "mock", "client_id_hint": null, "mock": true});
    }
    match resolve(st) {
        Some((c, source)) => {
            json!({"configured": true, "source": source, "client_id_hint": hint(&c.client_id), "mock": false})
        }
        None => json!({"configured": false, "source": null, "client_id_hint": null, "mock": false}),
    }
}

async fn status(State(st): S) -> Json<Value> {
    Json(status_json(&st))
}

#[derive(Deserialize, Default)]
struct TestReq {
    client_id: Option<String>,
    client_secret: Option<String>,
}

/// Check credentials: the ones typed in the form if given (so they can be tried before saving), else the saved ones.
async fn test(State(st): S, Json(req): Json<TestReq>) -> ApiResult<Json<Value>> {
    let typed = match (
        req.client_id.as_deref().map(str::trim),
        req.client_secret.as_deref().map(str::trim),
    ) {
        (Some(id), Some(secret)) if !id.is_empty() && !secret.is_empty() => Some(Credentials {
            client_id: id.into(),
            client_secret: secret.into(),
        }),
        _ => None,
    };
    let creds = match typed {
        Some(c) => c,
        None => credentials(&st)?,
    };
    let st2 = st.clone();
    let days = tokio::task::spawn_blocking(move || st2.igdb.test(&creds))
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
    Ok(Json(json!({"ok": true, "token_days": days})))
}

// ── Looking games up ─────────────────────────────────────────────────────────

pub fn start_fetch(st: &Arc<AppState>, retry_unmatched: bool) -> Result<u64, ApiError> {
    let creds = credentials(st)?;
    let job = st
        .jobs
        .create("igdb", "Fetch game information", vec!["igdb".into()])
        .map_err(|b| ApiError::busy(&b))?;
    let id = job.id;
    let st2 = st.clone();
    st.jobs
        .spawn(job, move |job| fetch_all(st2, job, creds, retry_unmatched));
    Ok(id)
}

/// After a scan or conversion: look up any new games, if IGDB is set up and automatic lookups are on.
pub fn kick(st: &Arc<AppState>) {
    if st.settings.get().igdb_auto {
        let _ = start_fetch(st, false); // not set up, or already running: nothing to do
    }
}

async fn fetch_all(
    st: Arc<AppState>,
    job: Arc<Job>,
    creds: Credentials,
    retry: bool,
) -> Result<(), Error> {
    let pending = st.db.run(move |c| store::pending(c, retry)).await?;
    let n = pending.len();
    if n == 0 {
        job.log("Every game already has its information.");
        return Ok(());
    }
    let dir = store::covers_dir(&st.cfg.config_dir);
    let (mut matched, mut unmatched, mut failed, mut in_a_row) = (0, 0, 0, 0);
    for (i, (title_id, name)) in pending.into_iter().enumerate() {
        if job.is_cancelled() {
            return Ok(());
        }
        job.step(format!("{} of {}: {name}", i + 1, n));
        job.progress(i as f32 / n as f32 * 100.0);
        let (st2, c2, d2, nm) = (st.clone(), creds.clone(), dir.clone(), name.clone());
        let found = tokio::task::spawn_blocking(
            move || -> Result<Option<(igdb::Candidate, Option<String>)>, Error> {
                Ok(igdb::lookup(&st2.igdb, &c2, &nm)?.map(|(c, _)| {
                    let cover = c
                        .cover
                        .as_deref()
                        .and_then(|id| igdb::save_cover(&st2.igdb, &d2, id).ok());
                    (c, cover)
                }))
            },
        )
        .await
        .map_err(|e| Error::backend(e.to_string()))?;
        match found {
            Ok(Some((c, cover))) => {
                let tid = title_id.clone();
                st.db
                    .run(move |conn| store::put_match(conn, &tid, &c, cover.as_deref(), false))
                    .await?;
                matched += 1;
                in_a_row = 0;
            }
            Ok(None) => {
                let tid = title_id.clone();
                st.db.run(move |conn| store::put_none(conn, &tid)).await?;
                job.log(format!("No match for {name} ({title_id})"));
                unmatched += 1;
                in_a_row = 0;
            }
            // Bad credentials won't get better by trying the next game.
            Err(e @ Error::Validation(_)) => return Err(e),
            Err(e) => {
                job.log(format!("{name}: {e}"));
                failed += 1;
                in_a_row += 1;
                if in_a_row >= 3 {
                    return Err(Error::backend(format!(
                        "Stopped after repeated errors: {e}"
                    )));
                }
            }
        }
    }
    job.result(
        "igdb",
        json!({"matched": matched, "unmatched": unmatched, "failed": failed}),
    );
    Ok(())
}

#[derive(Deserialize)]
struct FetchReq {
    #[serde(default)]
    retry_unmatched: bool,
}

async fn fetch(State(st): S, Json(req): Json<FetchReq>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"job": start_fetch(&st, req.retry_unmatched)?})))
}

// ── One game ─────────────────────────────────────────────────────────────────

fn check_title_id(t: &str) -> ApiResult<String> {
    if t.len() == 8 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(t.to_uppercase())
    } else {
        Err(ApiError::bad("That isn't a title ID"))
    }
}

async fn get_info(State(st): S, UrlPath(title_id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let tid = check_title_id(&title_id)?;
    let meta = st.db.run(move |c| store::get(c, &tid)).await?;
    Ok(Json(json!(meta)))
}

async fn title_name(st: &AppState, tid: &str) -> Result<String, Error> {
    let t = tid.to_string();
    st.db
        .run(move |c| {
            use rusqlite::OptionalExtension;
            let name: Option<String> = c
                .query_row("SELECT COALESCE(MAX(game_name), MIN(name)) FROM items WHERE title_id = ?1 AND kind IN ('iso', 'god')", [&t], |r| r.get(0))
                .optional()?
                .flatten();
            name.ok_or_else(|| Error::not_found("No game with that title ID"))
        })
        .await
}

/// Look this game up again, replacing automatic results (but never a match chosen by hand).
async fn refresh(State(st): S, UrlPath(title_id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let tid = check_title_id(&title_id)?;
    let creds = credentials(&st)?;
    let name = title_name(&st, &tid).await?;
    let (st2, dir) = (st.clone(), store::covers_dir(&st.cfg.config_dir));
    let found = tokio::task::spawn_blocking(
        move || -> Result<Option<(igdb::Candidate, Option<String>)>, Error> {
            Ok(igdb::lookup(&st2.igdb, &creds, &name)?.map(|(c, _)| {
                let cover = c
                    .cover
                    .as_deref()
                    .and_then(|id| igdb::save_cover(&st2.igdb, &dir, id).ok());
                (c, cover)
            }))
        },
    )
    .await
    .map_err(|e| Error::backend(e.to_string()))??;
    let t2 = tid.clone();
    let meta = st
        .db
        .run(move |c| {
            if store::get(c, &t2)?.is_some_and(|m| m.status == "manual") {
                return store::get(c, &t2); // a human's choice stays
            }
            match &found {
                Some((cand, cover)) => store::put_match(c, &t2, cand, cover.as_deref(), false)?,
                None => store::put_none(c, &t2)?,
            }
            store::get(c, &t2)
        })
        .await?;
    Ok(Json(json!(meta)))
}

#[derive(Deserialize)]
struct SearchReq {
    q: String,
}

/// Candidates for choosing a match by hand.
async fn search(State(st): S, Json(req): Json<SearchReq>) -> ApiResult<Json<Value>> {
    let q = req.q.trim().to_string();
    if q.is_empty() || q.len() > 200 {
        return Err(ApiError::bad("Type a game name to search for"));
    }
    let creds = credentials(&st)?;
    let st2 = st.clone();
    let found = tokio::task::spawn_blocking(move || st2.igdb.search(&creds, &q))
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
    let out: Vec<Value> = found
        .iter()
        .map(|c| {
            json!({
                "id": c.id, "name": c.name, "release": c.release, "developers": c.developers, "genres": c.genres,
                "cover_url": c.cover.as_deref().and_then(|id| st.igdb.thumb_url(id)),
            })
        })
        .collect();
    Ok(Json(json!(out)))
}

#[derive(Deserialize)]
struct ChooseReq {
    igdb_id: i64,
}

async fn set_info(
    State(st): S,
    UrlPath(title_id): UrlPath<String>,
    Json(req): Json<ChooseReq>,
) -> ApiResult<Json<Value>> {
    let tid = check_title_id(&title_id)?;
    if req.igdb_id <= 0 {
        return Err(ApiError::bad("That isn't an IGDB game ID"));
    }
    let creds = credentials(&st)?;
    let (st2, dir) = (st.clone(), store::covers_dir(&st.cfg.config_dir));
    let found = tokio::task::spawn_blocking(
        move || -> Result<Option<(igdb::Candidate, Option<String>)>, Error> {
            Ok(st2.igdb.by_id(&creds, req.igdb_id)?.map(|c| {
                let cover = c
                    .cover
                    .as_deref()
                    .and_then(|id| igdb::save_cover(&st2.igdb, &dir, id).ok());
                (c, cover)
            }))
        },
    )
    .await
    .map_err(|e| Error::backend(e.to_string()))??;
    let (cand, cover) = found.ok_or_else(|| ApiError::bad("IGDB has no game with that ID"))?;
    let t2 = tid.clone();
    let meta = st
        .db
        .run(move |c| {
            store::put_match(c, &t2, &cand, cover.as_deref(), true)?;
            store::get(c, &t2)
        })
        .await?;
    Ok(Json(json!(meta)))
}

async fn clear_info(State(st): S, UrlPath(title_id): UrlPath<String>) -> ApiResult<Json<Value>> {
    let tid = check_title_id(&title_id)?;
    st.db.run(move |c| store::remove(c, &tid)).await?;
    Ok(Json(json!({"ok": true})))
}

// ── Covers ───────────────────────────────────────────────────────────────────

async fn cover(State(st): S, UrlPath(file): UrlPath<String>) -> ApiResult<Response> {
    let ext = file.rsplit_once('.').map(|(_, e)| e);
    let stem_ok = file
        .rsplit_once('.')
        .is_some_and(|(s, _)| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()));
    let mime = match ext {
        Some("jpg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        _ => return Err(ApiError::not_found("No such cover")),
    };
    if !stem_ok {
        return Err(ApiError::not_found("No such cover"));
    }
    let bytes = tokio::fs::read(store::covers_dir(&st.cfg.config_dir).join(&file))
        .await
        .map_err(|_| ApiError::not_found("No such cover"))?;
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "private, max-age=86400"),
        ],
        bytes,
    )
        .into_response())
}
