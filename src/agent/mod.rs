//! The drive agent: a small authenticated web service that exposes one folder (for example an
//! Xbox hard drive plugged into your PC) so RustyBox, running elsewhere, can read and write it.
//!
//! Run it on the machine the drive is attached to: `rustybox agent --root /media/you/DRIVE`.
//! It listens on a port, wants a secret token with every request, and only ever touches files
//! inside the root folder.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Query, Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::{
    error::Error,
    fsops::{self, Locks},
    library::scan::{self, Existing, Scanner, Walk},
};

pub struct AgentState {
    pub root: PathBuf,
    pub token: String,
    pub name: String,
    pub read_only: bool,
    locks: Locks,
}

impl AgentState {
    pub fn new(
        root: &Path,
        token: String,
        name: String,
        read_only: bool,
    ) -> Result<AgentState, Error> {
        let root = std::fs::canonicalize(root).map_err(|e| {
            Error::validation(format!("The folder {} can't be used: {e}", root.display()))
        })?;
        if !root.is_dir() {
            return Err(Error::validation(format!(
                "{} isn't a folder",
                root.display()
            )));
        }
        if token.len() < 16 {
            return Err(Error::validation(
                "The token is too short (use at least 16 characters)",
            ));
        }
        Ok(AgentState {
            root,
            token,
            name,
            read_only,
            locks: Locks::default(),
        })
    }
}

type St = State<Arc<AgentState>>;

/// A failure answered the same way as RustyBox's own API: `{error, message, recoverable}`.
struct Fail(Error);

impl From<Error> for Fail {
    fn from(e: Error) -> Self {
        Fail(e)
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        Fail(Error::Io(e))
    }
}

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            Error::Validation(_) | Error::Json(_) => StatusCode::BAD_REQUEST,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::Coded { status, .. } => {
                StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_REQUEST)
            }
            Error::Backend(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Error::Db(_) | Error::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self.0.to_box_error())).into_response()
    }
}

type R<T> = Result<T, Fail>;

pub fn router(state: Arc<AgentState>) -> Router {
    Router::new()
        .route("/agent/v1/info", get(info))
        .route("/agent/v1/list", get(list))
        .route("/agent/v1/stat", get(stat))
        .route("/agent/v1/file", get(file))
        .route(
            "/agent/v1/write",
            get(write_status)
                .put(write_chunk)
                .layer(axum::extract::DefaultBodyLimit::max(fsops::MAX_CHUNK)),
        )
        .route("/agent/v1/mkdir", post(mkdir))
        .route("/agent/v1/rename", post(rename))
        .route("/agent/v1/delete", post(delete))
        .route(
            "/agent/v1/scan",
            post(scan_route).layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state)
}

/// Constant-time comparison, so the token can't be found out by timing.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

async fn auth(State(st): St, req: Request, next: Next) -> Response {
    let given = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if same(given, &st.token) {
        return next.run(req).await;
    }
    // Slow wrong guesses down.
    tokio::time::sleep(Duration::from_millis(400)).await;
    (
        StatusCode::UNAUTHORIZED,
        Json(
            json!({"error": "AGENT_AUTH", "message": "The token isn't right", "recoverable": true}),
        ),
    )
        .into_response()
}

fn writable(st: &AgentState) -> R<()> {
    if st.read_only {
        return Err(Fail(Error::coded(
            403,
            "READ_ONLY",
            "This drive was shared read-only",
        )));
    }
    Ok(())
}

async fn info(State(st): St) -> Json<Value> {
    let root = st.root.clone();
    let fs = tokio::task::spawn_blocking(move || fsops::fs_info(&root))
        .await
        .unwrap_or(fsops::FsInfo {
            fs_type: "unknown".into(),
            total: 0,
            free: 0,
            max_file: None,
        });
    Json(json!({
        "name": st.name, "version": env!("CARGO_PKG_VERSION"), "read_only": st.read_only,
        "root": st.root.to_string_lossy(), "fs": fs,
    }))
}

#[derive(Deserialize)]
struct PathQ {
    #[serde(default)]
    path: String,
}

async fn list(State(st): St, Query(q): Query<PathQ>) -> R<Json<Value>> {
    let root = st.root.clone();
    let entries = tokio::task::spawn_blocking(move || fsops::list_dir(&root, &q.path))
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
    Ok(Json(json!(entries)))
}

async fn stat(State(st): St, Query(q): Query<PathQ>) -> R<Json<Value>> {
    let root = st.root.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, Error> {
        Ok(match fsops::contained(&root, &q.path) {
            Ok(p) => {
                let m = std::fs::metadata(&p)?;
                json!({"exists": true, "is_dir": m.is_dir(), "size": if m.is_dir() { 0 } else { m.len() }})
            }
            Err(Error::NotFound(_)) => json!({"exists": false, "is_dir": false, "size": 0}),
            Err(e) => return Err(e),
        })
    })
    .await
    .map_err(|e| Error::backend(e.to_string()))??;
    Ok(Json(out))
}

/// A file's contents, with HTTP Range so big files can be read in pieces and resumed.
async fn file(State(st): St, Query(q): Query<PathQ>, req: Request) -> R<Response> {
    let root = st.root.clone();
    let path = tokio::task::spawn_blocking(move || fsops::contained(&root, &q.path))
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
    if !path.is_file() {
        return Err(Fail(Error::validation("That isn't a file")));
    }
    Ok(ServeFile::new(path)
        .oneshot(req)
        .await
        .unwrap_or_else(|e| match e {})
        .into_response())
}

#[derive(Deserialize)]
struct WriteQ {
    path: String,
    offset: Option<u64>,
    total: Option<u64>,
    overwrite: Option<bool>,
}

async fn write_status(State(st): St, Query(q): Query<WriteQ>) -> R<Json<Value>> {
    Ok(Json(json!(fsops::write_status(&st.root, &q.path).await?)))
}

async fn write_chunk(State(st): St, Query(q): Query<WriteQ>, body: Body) -> R<Json<Value>> {
    writable(&st)?;
    let total = q
        .total
        .ok_or_else(|| Error::validation("Missing total (the file's full size)"))?;
    let r = fsops::write_chunk(
        &st.locks,
        &st.root,
        &q.path,
        q.offset.unwrap_or(0),
        total,
        q.overwrite.unwrap_or(false),
        body.into_data_stream(),
    )
    .await?;
    Ok(Json(json!({"done": r.done, "offset": r.offset})))
}

async fn mkdir(State(st): St, Json(q): Json<PathQ>) -> R<Json<Value>> {
    writable(&st)?;
    let root = st.root.clone();
    tokio::task::spawn_blocking(move || fsops::make_dir(&root, &q.path))
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct RenameQ {
    from: String,
    to: String,
}

async fn rename(State(st): St, Json(q): Json<RenameQ>) -> R<Json<Value>> {
    writable(&st)?;
    let root = st.root.clone();
    tokio::task::spawn_blocking(move || fsops::rename(&root, &q.from, &q.to))
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
    Ok(Json(json!({"ok": true})))
}

async fn delete(State(st): St, Json(q): Json<PathQ>) -> R<Json<Value>> {
    writable(&st)?;
    let root = st.root.clone();
    tokio::task::spawn_blocking(move || fsops::remove(&root, &q.path))
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct ScanReq {
    /// `iso`, `god` or `files`.
    scanner: String,
    /// What RustyBox already knows, so unchanged games aren't read again.
    #[serde(default)]
    existing: HashMap<String, Existing>,
    /// A folder inside the root to scan (default: all of it).
    #[serde(default)]
    path: String,
}

/// Scan the drive here, where the files are, and send back only the results.
async fn scan_route(State(st): St, Json(req): Json<ScanReq>) -> R<Json<Value>> {
    let scanner = match req.scanner.as_str() {
        "iso" => Scanner::Iso,
        "god" => Scanner::God,
        "files" => Scanner::Files,
        other => {
            return Err(Fail(Error::validation(format!(
                "Unknown scanner '{other}'"
            ))));
        }
    };
    let root = st.root.clone();
    let found = tokio::task::spawn_blocking(move || -> Result<Vec<scan::Found>, Error> {
        let base = if req.path.trim_matches('/').is_empty() {
            root.clone()
        } else {
            fsops::contained(&root, &req.path)?
        };
        let (never, quiet) = (|| false, |_: usize| {});
        let w = Walk {
            scanner,
            cancelled: &never,
            progress: &quiet,
        };
        let (mut found, _skipped) = scan::walk(&base, &w)?;
        scan::enrich(&base, scanner, &mut found, &req.existing, &w)?;
        Ok(found)
    })
    .await
    .map_err(|e| Error::backend(e.to_string()))??;
    Ok(Json(json!({"items": found})))
}

pub async fn serve(state: AgentState, bind: std::net::SocketAddr) -> Result<(), Error> {
    let app = router(Arc::new(state));
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| Error::backend(format!("Cannot listen on {bind}: {e}")))?;
    axum::serve(listener, app)
        .with_graceful_shutdown(crate::web::shutdown_signal())
        .await
        .map_err(|e| Error::backend(e.to_string()))
}
