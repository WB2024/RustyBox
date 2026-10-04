//! Web UI + JSON API for RustyBox.
//!
//! Read-only queries run in-process. Anything long (scans, conversions, transfers) is a job
//! (see `crate::jobs`) that browsers follow over Server-Sent Events.

mod assets;
mod auth;
mod console;
mod content;
mod convert;
mod discover;
mod extras;
mod grabber;
mod igdb;
mod import;
mod libraries;
mod settings_api;
mod summary;
mod torrents;
mod transfer;
mod updates;
mod usb;

use std::{
    convert::Infallible,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response, Sse, sse},
    routing::{get, post},
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    db::Db,
    error::{BoxError, Error},
    jobs::{Event, Jobs},
    settings,
};

pub struct Config {
    /// Where settings.json (and later the database) live. Mount a volume here in Docker.
    pub config_dir: PathBuf,
    /// Folders library paths may live under (container paths: mounted disks and shares).
    pub roots: Vec<PathBuf>,
    /// The abgx360 program, if not simply on the PATH.
    pub abgx360: Option<PathBuf>,
    /// Values from a `.env` file (development), checked after the real environment.
    pub dotenv: std::collections::HashMap<String, String>,
    /// Where IGDB and Twitch are (tests point this at a stand-in). `None` means the real ones.
    pub igdb_urls: Option<crate::igdb::Urls>,
    /// Simulate the console, USB drives and network, so everything works without hardware.
    pub mock: bool,
    /// Login from the command line / environment: (user, password).
    pub auth: Option<(String, String)>,
}

pub struct AppState {
    pub cfg: Config,
    pub jobs: Jobs,
    pub settings: settings::Store,
    pub auth: auth::Auth,
    pub db: Db,
    pub consoles: crate::console::Store,
    pub content: crate::content::Db,
    pub grabber: crate::grabber::config::Store,
    /// The last search results per wanted game, so a result can be grabbed by its id.
    pub grab_cache:
        std::sync::Mutex<std::collections::HashMap<i64, Vec<crate::grabber::engine::Judged>>>,
    pub igdb: crate::igdb::Client,
    /// Files with an upload in progress, so two requests never write the same one.
    pub uploading: crate::fsops::Locks,
}

pub type S = State<Arc<AppState>>;
pub type ApiResult<T> = Result<T, ApiError>;

// ── Errors ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ApiError(StatusCode, BoxError);

impl ApiError {
    /// The message, for logging into a job or a status line.
    pub fn message(&self) -> String {
        self.1.message.clone()
    }

    pub fn new(status: StatusCode, code: &str, msg: impl Into<String>, recoverable: bool) -> Self {
        ApiError(
            status,
            BoxError {
                error: code.into(),
                message: msg.into(),
                recoverable,
            },
        )
    }
    pub fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", msg, false)
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "NOT_FOUND", msg, false)
    }
    pub fn busy(job: &crate::jobs::Job) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "RESOURCE_BUSY",
            format!(
                "Busy: job #{} ({}) is using the same resource",
                job.id, job.title
            ),
            true,
        )
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        let status = match e {
            Error::Validation(_) | Error::Json(_) => StatusCode::BAD_REQUEST,
            Error::Backend(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::Coded { status, .. } => {
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST)
            }
            Error::Db(_) | Error::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(status, e.to_box_error())
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e).into()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Join a user-supplied relative path onto `root`, refusing `..` and absolute paths.
pub fn safe_join(root: &Path, rel: &str) -> ApiResult<PathBuf> {
    use std::path::Component;
    let rel = rel.trim_start_matches('/');
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err(ApiError::bad(format!("Invalid path: {rel}"))),
        }
    }
    Ok(root.join(rel))
}

fn find_in_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

// ── Router ───────────────────────────────────────────────────────────────────

pub fn build_state(mut cfg: Config) -> Result<Arc<AppState>, Error> {
    std::fs::create_dir_all(&cfg.config_dir)?;
    let db = Db::open(&cfg.config_dir.join("rustybox.db"))?;
    if cfg.mock {
        crate::mock::seed(&cfg.config_dir, &db)?;
        cfg.roots.push(crate::mock::data_dir(&cfg.config_dir));
    }
    let igdb = crate::igdb::Client::new(cfg.igdb_urls.clone().unwrap_or_default(), cfg.mock);
    let settings = settings::Store::load(&cfg.config_dir);
    let (env_user, env_pass) = cfg.auth.clone().unzip();
    let auth = auth::Auth::new(env_user, env_pass).map_err(Error::validation)?;
    let consoles = crate::console::Store::load(&cfg.config_dir);
    let content = crate::content::Db::new(&cfg.config_dir, cfg.mock);
    let grabber = crate::grabber::config::Store::load(&cfg.config_dir);
    let jobs = Jobs::default();
    jobs.attach(db.clone())?;
    let state = Arc::new(AppState {
        cfg,
        jobs,
        settings,
        auth,
        db,
        consoles,
        content,
        grabber,
        grab_cache: Default::default(),
        igdb,
        uploading: Default::default(),
    });
    if state.cfg.mock {
        console::start_mock(&state)?;
        if state.content.fetched().is_none() {
            let never = || false;
            let quiet = |_: f32, _: &str| {};
            state.content.refresh(&crate::convert::Ctl {
                cancelled: &never,
                progress: &quiet,
            })?;
        }
    }
    Ok(state)
}

/// Start the background tasks (scheduled scans, job notifications). `serve` calls this itself.
pub fn spawn_background(state: Arc<AppState>) {
    extras::spawn_background(state.clone());
    grabber::spawn_background(state)
}

/// Look at SABnzbd once and move downloads along (the background task does this every 15 seconds).
pub async fn poll_downloads(state: &Arc<AppState>) {
    grabber::poll_downloads(state).await
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(assets::index))
        .route("/assets/{*path}", get(assets::asset))
        .route("/api/health", get(|| async { Json(json!({"ok": true})) }))
        .route("/api/status", get(status))
        .route("/api/login", post(auth::login))
        .route("/api/logout", post(auth::logout))
        .route("/api/auth/status", get(auth::status))
        .route("/api/auth/config", post(auth::configure))
        .merge(libraries::routes())
        .merge(convert::routes())
        .merge(settings_api::routes())
        .merge(import::routes())
        .merge(igdb::routes())
        .merge(transfer::routes())
        .merge(console::routes())
        .merge(content::routes())
        .merge(updates::routes())
        .merge(usb::routes())
        .merge(extras::routes())
        .merge(grabber::routes())
        .merge(torrents::routes())
        .merge(summary::routes())
        .merge(discover::routes())
        .route("/api/jobs", get(list_jobs))
        .route("/api/jobs/demo", post(start_demo))
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/jobs/{id}/events", get(job_events))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::guard,
        ))
        .layer(axum::middleware::from_fn(safe_headers))
        .with_state(state)
}

/// Headers that cost nothing and close off some browser tricks: no guessing at content types, no
/// framing RustyBox inside another site (clickjacking), and no sending the address on to links.
async fn safe_headers(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    res
}

pub async fn serve(cfg: Config, bind: SocketAddr) -> Result<(), Error> {
    let state = build_state(cfg)?;
    spawn_background(state.clone());
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| Error::backend(format!("Cannot bind {bind}: {e}")))?;
    eprintln!("RustyBox web UI listening on http://{bind}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(|e| Error::backend(e.to_string()))
}

/// Wait for Ctrl-C or, as Docker sends on `docker stop` and `compose up -d`, SIGTERM.
pub async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let term = async {
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term => {}
    }
    // Open event streams would keep a graceful stop waiting for ever: give requests a few
    // seconds, then go.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(4));
        std::process::exit(0);
    });
}

// ── Status & settings ────────────────────────────────────────────────────────

async fn status(State(st): S) -> Json<Value> {
    let tools: Vec<Value> = [
        ("mkfs.vfat", "Format USB sticks (FAT32)"),
        ("partclone.vfat", "USB backup and restore"),
        ("zstd", "Compress USB backups"),
    ]
    .iter()
    .map(|(n, why)| json!({"name": n, "purpose": why, "found": find_in_path(n)}))
    .chain(std::iter::once(json!({"name": "abgx360", "purpose": "Check and fix ISOs", "found": crate::convert::verify::find(st.cfg.abgx360.as_deref()).is_some()})))
    .collect();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "mock": st.cfg.mock,
        "config_dir": st.cfg.config_dir.to_string_lossy(),
        "roots": st.cfg.roots.iter().map(|r| r.to_string_lossy()).collect::<Vec<_>>(),
        "tools": tools,
    }))
}

// ── Jobs ─────────────────────────────────────────────────────────────────────

fn find_job(st: &AppState, id: u64) -> ApiResult<Arc<crate::jobs::Job>> {
    st.jobs
        .get(id)
        .ok_or_else(|| ApiError::not_found(format!("No job #{id}")))
}

#[derive(Deserialize)]
struct JobsQuery {
    /// Only jobs that are queued or running (what the banner polls for).
    #[serde(default)]
    active: Option<String>,
}

async fn list_jobs(State(st): S, Query(q): Query<JobsQuery>) -> Json<Value> {
    let active = q
        .active
        .as_deref()
        .is_some_and(|v| !matches!(v, "" | "0" | "false"));
    Json(json!(
        st.jobs
            .all()
            .iter()
            .map(|j| j.summary())
            .filter(|j| !active || !j.status.is_terminal())
            .collect::<Vec<_>>()
    ))
}

async fn get_job(State(st): S, UrlPath(id): UrlPath<u64>) -> ApiResult<Json<Value>> {
    let job = find_job(&st, id)?;
    let (events, _) = job.events_since(0);
    Ok(Json(json!({"job": job.summary(), "events": events})))
}

async fn cancel_job(State(st): S, UrlPath(id): UrlPath<u64>) -> ApiResult<Json<Value>> {
    let job = find_job(&st, id)?;
    if job.status().is_terminal() {
        return Err(ApiError::bad("Job already finished"));
    }
    job.request_cancel();
    Ok(Json(json!({"ok": true})))
}

/// A harmless job that exercises progress, logging, results and cancel. Mock mode only.
async fn start_demo(State(st): S) -> ApiResult<Json<Value>> {
    if !st.cfg.mock {
        return Err(ApiError::bad("The demo job is only available with --mock"));
    }
    let job = st
        .jobs
        .create("demo", "Demo job", vec!["demo".into()])
        .map_err(|b| ApiError::busy(&b))?;
    let id = job.id;
    st.jobs.spawn(job, |job| async move {
        for i in 1..=20u32 {
            job.step(format!("Step {i} of 20"));
            job.log(format!("did some work ({i})"));
            job.progress(i as f32 * 5.0);
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }
        job.result("demo", json!({"message": "All done"}));
        Ok(())
    });
    Ok(Json(json!({"job": id})))
}

#[derive(Deserialize)]
struct EventsQuery {
    from: Option<usize>,
}

async fn job_events(
    State(st): S,
    UrlPath(id): UrlPath<u64>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<impl IntoResponse> {
    let job = find_job(&st, id)?;
    let rx = job.subscribe();
    let stream = futures_util::stream::unfold(
        (job, rx, q.from.unwrap_or(0), false),
        |(job, mut rx, mut idx, done)| async move {
            if done {
                return None;
            }
            loop {
                let (evs, terminal) = job.events_since(idx);
                if !evs.is_empty() || terminal {
                    idx += evs.len();
                    let mut out: Vec<Result<sse::Event, Infallible>> = evs
                        .iter()
                        .map(|e: &Event| {
                            Ok(sse::Event::default()
                                .data(serde_json::to_string(e).unwrap_or_default()))
                        })
                        .collect();
                    if terminal {
                        out.push(Ok(sse::Event::default().event("end").data("")));
                    }
                    return Some((out, (job, rx, idx, terminal)));
                }
                if rx.changed().await.is_err() {
                    return None;
                }
            }
        },
    )
    .flat_map(futures_util::stream::iter);
    // X-Accel-Buffering stops nginx-style reverse proxies from holding events back.
    Ok((
        [("x-accel-buffering", "no")],
        Sse::new(stream).keep_alive(sse::KeepAlive::default()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_refuses_escapes() {
        let root = Path::new("/data");
        assert_eq!(safe_join(root, "a/b").unwrap(), Path::new("/data/a/b"));
        assert_eq!(safe_join(root, "/a").unwrap(), Path::new("/data/a"));
        assert!(safe_join(root, "../etc").is_err());
        assert!(safe_join(root, "a/../../b").is_err());
    }
}
