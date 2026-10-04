//! A drive shared from a web browser instead of from a program.
//!
//! The browser (Chrome, Edge or Brave, on a secure page) lets the user pick a folder on their own
//! computer, such as the Xbox's hard drive, and the page then does the file work. RustyBox on the
//! server can't open a connection to a browser, so it works the other way round: the page asks
//! "is there anything to do?" (a long poll), does it, and posts the answer back.
//!
//! The rest of RustyBox doesn't know the difference. It talks to the same `/agent/v1/...` calls it
//! uses for a drive agent, at `http://127.0.0.1:<port>/browser-agent/<id>`; this module turns each
//! call into a job for the browser and waits for the answer.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use argon2::password_hash::rand_core::{OsRng, RngCore};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path as UrlPath, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

use super::{ApiError, ApiResult, AppState, S};
use crate::fsops::MAX_CHUNK;

/// How long the browser can go without asking for work before the drive counts as disconnected.
const ONLINE_FOR: Duration = Duration::from_secs(45);
/// How long one question to the browser may take (a big read or write chunk over a slow disk).
const ANSWER_WITHIN: Duration = Duration::from_secs(110);
/// How long the browser's poll waits for work before answering "nothing yet".
const POLL_FOR: Duration = Duration::from_secs(25);
const FILE: &str = "browser-agents.json";

#[derive(Serialize, Deserialize, Clone)]
struct Known {
    token: String,
    name: String,
}

struct Job {
    id: u64,
    method: String,
    /// What follows `/agent/v1/`, such as `list`.
    path: String,
    query: String,
    range: Option<String>,
    body: Bytes,
    reply: Option<oneshot::Sender<Answer>>,
}

pub struct Answer {
    status: u16,
    content_type: String,
    content_range: Option<String>,
    body: Bytes,
}

#[derive(Default)]
struct Agent {
    last_seen: Option<Instant>,
    queue: VecDeque<u64>,
    /// Jobs waiting to be fetched or answered, by number.
    jobs: HashMap<u64, Job>,
}

#[derive(Default)]
pub struct Relay {
    known: Mutex<HashMap<String, Known>>,
    agents: Mutex<HashMap<String, Agent>>,
    wake: Notify,
    next: std::sync::atomic::AtomicU64,
    dir: Mutex<Option<PathBuf>>,
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    OsRng.fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Relay {
    pub fn load(dir: &Path) -> Relay {
        let r = Relay::default();
        if let Ok(s) = std::fs::read_to_string(dir.join(FILE))
            && let Ok(m) = serde_json::from_str::<HashMap<String, Known>>(&s)
        {
            *r.known.lock().unwrap_or_else(|e| e.into_inner()) = m;
        }
        *r.dir.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.to_path_buf());
        r
    }

    fn save(&self) {
        let dir = self.dir.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(dir) = dir else { return };
        let k = self.known.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(s) = serde_json::to_string_pretty(&*k) {
            let tmp = dir.join(format!("{FILE}.part"));
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, dir.join(FILE));
            }
        }
    }

    fn token_ok(&self, id: &str, given: &str) -> bool {
        let k = self.known.lock().unwrap_or_else(|e| e.into_inner());
        k.get(id)
            .is_some_and(|k| crate::agent::same(&k.token, given))
    }

    /// Is the browser asking for work? Used to say "not connected" at once instead of waiting.
    pub fn online(&self, id: &str) -> bool {
        let a = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        a.get(id)
            .and_then(|a| a.last_seen)
            .is_some_and(|t| t.elapsed() < ONLINE_FOR)
    }

    fn touch(&self, id: &str) {
        let mut a = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        a.entry(id.to_string()).or_default().last_seen = Some(Instant::now());
    }
}

fn bearer(h: &HeaderMap) -> &str {
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("")
}

fn agent_error(status: StatusCode, code: &str, msg: &str) -> Response {
    (
        status,
        Json(json!({"error": code, "message": msg, "recoverable": true})),
    )
        .into_response()
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/browser-agent/register", post(register))
        .route("/api/browser-agent/{id}/poll", get(poll))
        .route("/api/browser-agent/{id}/job/{job}/body", get(job_body))
        .route(
            "/api/browser-agent/{id}/job/{job}/answer",
            post(job_answer).layer(DefaultBodyLimit::max(MAX_CHUNK + 1024 * 1024)),
        )
        // What RustyBox itself calls (it sends the agent token, so no login is needed here).
        .route(
            "/browser-agent/{id}/agent/v1/{*rest}",
            axum::routing::any(relay).layer(DefaultBodyLimit::max(MAX_CHUNK + 1024 * 1024)),
        )
}

#[derive(Deserialize)]
struct RegisterReq {
    name: String,
    /// What this browser was given last time, to keep the same drive.
    id: Option<String>,
    token: Option<String>,
}

/// A browser says it wants to share a drive. It keeps the id and token and shows them back later.
async fn register(State(st): S, Json(r): Json<RegisterReq>) -> ApiResult<Json<Value>> {
    let name: String = r.name.trim().chars().take(60).collect();
    if name.is_empty() {
        return Err(ApiError::bad("Give the drive a name"));
    }
    let (id, token) = match (r.id, r.token) {
        (Some(id), Some(t)) if st.relay.token_ok(&id, &t) => (id, t),
        _ => (random_hex(8), random_hex(24)),
    };
    st.relay
        .known
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            id.clone(),
            Known {
                token: token.clone(),
                name: name.clone(),
            },
        );
    st.relay.save();
    let port = st.listen_port.load(std::sync::atomic::Ordering::Relaxed);
    Ok(Json(json!({
        "id": id, "token": token, "name": name,
        "url": format!("http://127.0.0.1:{port}/browser-agent/{id}"),
    })))
}

/// The browser asks for its next job. Waits a while if there is none.
async fn poll(State(st): S, UrlPath(id): UrlPath<String>, headers: HeaderMap) -> Response {
    if !st.relay.token_ok(&id, bearer(&headers)) {
        return agent_error(
            StatusCode::UNAUTHORIZED,
            "AGENT_AUTH",
            "That drive isn't known (connect it again)",
        );
    }
    let deadline = Instant::now() + POLL_FOR;
    loop {
        // Ask to be woken before looking, so a job that arrives in between isn't missed.
        let woken = st.relay.wake.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        st.relay.touch(&id);
        let next = {
            let mut a = st.relay.agents.lock().unwrap_or_else(|e| e.into_inner());
            let ag = a.entry(id.clone()).or_default();
            ag.queue.pop_front().and_then(|j| {
                ag.jobs.get(&j).map(|job| {
                    json!({
                        "job": job.id, "method": job.method, "path": job.path,
                        "query": job.query, "range": job.range, "body_len": job.body.len(),
                    })
                })
            })
        };
        if let Some(job) = next {
            return Json(job).into_response();
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            st.relay.touch(&id);
            return StatusCode::NO_CONTENT.into_response();
        }
        let _ = tokio::time::timeout(left, woken).await;
    }
}

async fn job_body(
    State(st): S,
    UrlPath((id, job)): UrlPath<(String, u64)>,
    headers: HeaderMap,
) -> Response {
    if !st.relay.token_ok(&id, bearer(&headers)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let a = st.relay.agents.lock().unwrap_or_else(|e| e.into_inner());
    match a.get(&id).and_then(|a| a.jobs.get(&job)) {
        Some(j) => (
            [(header::CONTENT_TYPE, "application/octet-stream")],
            j.body.clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The browser's answer to a job: the status in `x-agent-status`, the body as it is.
async fn job_answer(
    State(st): S,
    UrlPath((id, job)): UrlPath<(String, u64)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !st.relay.token_ok(&id, bearer(&headers)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let status = headers
        .get("x-agent-status")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(200);
    let h = |n: &str| {
        headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    };
    let reply = {
        let mut a = st.relay.agents.lock().unwrap_or_else(|e| e.into_inner());
        a.get_mut(&id)
            .and_then(|a| a.jobs.remove(&job))
            .and_then(|mut j| j.reply.take())
    };
    match reply {
        Some(tx) => {
            let _ = tx.send(Answer {
                status,
                content_type: h("x-agent-content-type")
                    .unwrap_or_else(|| "application/json".into()),
                content_range: h("x-agent-content-range"),
                body,
            });
            StatusCode::NO_CONTENT.into_response()
        }
        // Nobody is waiting any more (it timed out): not the browser's problem.
        None => StatusCode::GONE.into_response(),
    }
}

/// RustyBox's own call for a drive that lives in a browser: queue it, wait for the answer.
async fn relay(
    State(st): S,
    UrlPath((id, rest)): UrlPath<(String, String)>,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    if !st.relay.token_ok(&id, bearer(&parts.headers)) {
        tokio::time::sleep(Duration::from_millis(400)).await;
        return agent_error(
            StatusCode::UNAUTHORIZED,
            "AGENT_AUTH",
            "The token isn't right",
        );
    }
    // The browser isn't asking for work: say so at once.
    if !st.relay.online(&id) {
        return agent_error(
            StatusCode::BAD_GATEWAY,
            "BROWSER_OFFLINE",
            "The browser sharing this drive isn't connected. Open RustyBox in it and keep the Drive page open.",
        );
    }
    let body = match axum::body::to_bytes(body, MAX_CHUNK + 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return agent_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "TOO_BIG",
                "That request is too big",
            );
        }
    };
    let jid = st
        .relay
        .next
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    let (tx, rx) = oneshot::channel();
    {
        let mut a = st.relay.agents.lock().unwrap_or_else(|e| e.into_inner());
        let ag = a.entry(id.clone()).or_default();
        ag.jobs.insert(
            jid,
            Job {
                id: jid,
                method: parts.method.to_string(),
                path: rest,
                query: parts.uri.query().unwrap_or("").to_string(),
                range: parts
                    .headers
                    .get(header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .map(String::from),
                body,
                reply: Some(tx),
            },
        );
        ag.queue.push_back(jid);
    }
    st.relay.wake.notify_waiters();
    let answer = tokio::time::timeout(ANSWER_WITHIN, rx).await;
    // Whatever happened, the job is finished with.
    {
        let mut a = st.relay.agents.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ag) = a.get_mut(&id) {
            ag.jobs.remove(&jid);
            ag.queue.retain(|j| *j != jid);
        }
    }
    match answer {
        Ok(Ok(a)) => {
            let mut res = Response::new(axum::body::Body::from(a.body));
            *res.status_mut() = StatusCode::from_u16(a.status).unwrap_or(StatusCode::OK);
            if let Ok(v) = a.content_type.parse() {
                res.headers_mut().insert(header::CONTENT_TYPE, v);
            }
            if let Some(r) = a.content_range.and_then(|r| r.parse().ok()) {
                res.headers_mut().insert(header::CONTENT_RANGE, r);
            }
            res
        }
        _ => agent_error(
            StatusCode::GATEWAY_TIMEOUT,
            "BROWSER_TIMEOUT",
            "The browser didn't answer in time. Is the tab still open?",
        ),
    }
}
