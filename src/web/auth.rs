//! An optional login for the web UI and API.
//!
//! When enabled, everything except the login itself needs a session cookie. Passwords are
//! stored only as Argon2 hashes. Credentials come from the settings page, or from
//! `--auth-user` / `--auth-password` (`RUSTYBOX_AUTH_USER` / `RUSTYBOX_AUTH_PASSWORD`), which
//! take priority and can't be changed from the UI.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use argon2::{
    Argon2,
    password_hash::{
        PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
        rand_core::{OsRng, RngCore},
    },
};
use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};

const COOKIE: &str = "rustybox_session";
const SESSION_LIFETIME: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_SESSIONS: usize = 200;
const MAX_FAILURES: u32 = 8;
const LOCKOUT: Duration = Duration::from_secs(300);

pub const LOGIN_HTML: &str = include_str!("login.html");

#[derive(Default)]
pub struct Auth {
    sessions: Mutex<HashMap<String, Instant>>,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    /// Credentials given on the command line or in the environment.
    pub env: Option<(String, String)>, // (user, argon2 hash)
}

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
        .unwrap_or(false)
}

impl Auth {
    pub fn new(env_user: Option<String>, env_password: Option<String>) -> Result<Auth, String> {
        let env = match (env_user.filter(|u| !u.trim().is_empty()), env_password.filter(|p| !p.is_empty())) {
            (Some(u), Some(p)) => Some((u.trim().to_string(), hash_password(&p)?)),
            (None, None) => None,
            _ => return Err("Set both a login user and a password (RUSTYBOX_AUTH_USER and RUSTYBOX_AUTH_PASSWORD), or neither.".into()),
        };
        Ok(Auth {
            env,
            ..Default::default()
        })
    }

    /// (user, hash, set-by-environment) when a login is required.
    pub fn credentials(&self, st: &AppState) -> Option<(String, String, bool)> {
        if let Some((u, h)) = &self.env {
            return Some((u.clone(), h.clone(), true));
        }
        let s = st.settings.get();
        (!s.auth_user.is_empty() && !s.auth_password_hash.is_empty()).then_some((
            s.auth_user,
            s.auth_password_hash,
            false,
        ))
    }

    fn new_session(&self) -> String {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|_, seen| seen.elapsed() < SESSION_LIFETIME);
        if sessions.len() >= MAX_SESSIONS
            && let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, t)| **t)
                .map(|(k, _)| k.clone())
        {
            sessions.remove(&oldest);
        }
        sessions.insert(token.clone(), Instant::now());
        token
    }

    fn valid(&self, token: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        match sessions.get_mut(token) {
            Some(seen) if seen.elapsed() < SESSION_LIFETIME => {
                *seen = Instant::now();
                true
            }
            Some(_) => {
                sessions.remove(token);
                false
            }
            None => false,
        }
    }

    fn end_session(&self, token: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token);
    }

    /// Log everyone out (used when the password changes).
    fn end_all_sessions(&self) {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

fn session_cookie(token: &str) -> String {
    format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}",
        SESSION_LIFETIME.as_secs()
    )
}

fn cleared_cookie() -> String {
    format!("{COOKIE}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0")
}

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
}

fn is_public(path: &str) -> bool {
    matches!(path, "/api/login" | "/api/auth/status" | "/api/health")
}

/// Everything needs a session once a login is set, except the login itself.
pub async fn guard(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if st.auth.credentials(&st).is_none() || is_public(req.uri().path()) {
        return next.run(req).await;
    }
    if cookie_value(req.headers()).is_some_and(|t| st.auth.valid(&t)) {
        return next.run(req).await;
    }
    let path = req.uri().path();
    if path == "/" {
        return Html(LOGIN_HTML).into_response();
    }
    if path.starts_with("/api/") || path.starts_with("/files/") || path.starts_with("/assets/") {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "AUTH_REQUIRED",
            "Log in first",
            true,
        )
        .into_response();
    }
    Redirect::to("/").into_response()
}

pub async fn status(State(st): S, headers: HeaderMap) -> Json<Value> {
    let creds = st.auth.credentials(&st);
    let logged_in = creds.is_none() || cookie_value(&headers).is_some_and(|t| st.auth.valid(&t));
    Json(json!({
        "enabled": creds.is_some(),
        "logged_in": logged_in,
        "user": creds.as_ref().map(|c| c.0.clone()),
        "from_environment": creds.as_ref().map(|c| c.2).unwrap_or(false),
    }))
}

#[derive(Deserialize)]
pub struct LoginReq {
    username: String,
    password: String,
}

pub async fn login(
    State(st): S,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<LoginReq>,
) -> Response {
    let Some((user, hash, _)) = st.auth.credentials(&st) else {
        return Json(json!({"ok": true, "enabled": false})).into_response();
    };
    let ip = addr.ip();

    {
        let fails = st.auth.failures.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((n, since)) = fails.get(&ip)
            && *n >= MAX_FAILURES
            && since.elapsed() < LOCKOUT
        {
            let wait = (LOCKOUT - since.elapsed()).as_secs();
            return ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "TOO_MANY_ATTEMPTS",
                format!(
                    "Too many wrong attempts. Try again in {} minute(s).",
                    wait / 60 + 1
                ),
                true,
            )
            .into_response();
        }
    }

    // Always do the expensive check so a wrong user name takes as long as a wrong password.
    let user_ok = req.username.trim() == user;
    let (password, hash) = (req.password, hash);
    let pass_ok = tokio::task::spawn_blocking(move || verify_password(&password, &hash))
        .await
        .unwrap_or(false);

    if user_ok && pass_ok {
        st.auth
            .failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ip);
        let token = st.auth.new_session();
        return (
            [(header::SET_COOKIE, session_cookie(&token))],
            Json(json!({"ok": true})),
        )
            .into_response();
    }

    {
        let mut fails = st.auth.failures.lock().unwrap_or_else(|e| e.into_inner());
        let entry = fails.entry(ip).or_insert((0, Instant::now()));
        if entry.1.elapsed() > LOCKOUT {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
        entry.1 = Instant::now();
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "BAD_LOGIN",
        "That user name or password isn't right.",
        true,
    )
    .into_response()
}

pub async fn logout(State(st): S, headers: HeaderMap) -> Response {
    if let Some(t) = cookie_value(&headers) {
        st.auth.end_session(&t);
    }
    (
        [(header::SET_COOKIE, cleared_cookie())],
        Json(json!({"ok": true})),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct ConfigReq {
    enabled: bool,
    username: Option<String>,
    /// Omit to keep the current password.
    password: Option<String>,
}

/// Turn the login on or off, or change it. Only reachable when logged in (or when no login exists yet).
pub async fn configure(State(st): S, Json(req): Json<ConfigReq>) -> ApiResult<Response> {
    if st.auth.env.is_some() {
        return Err(ApiError::bad(
            "The login is set by the RUSTYBOX_AUTH_USER / RUSTYBOX_AUTH_PASSWORD environment variables; change it there.",
        ));
    }
    let mut s = st.settings.get();
    if req.enabled {
        let user = req
            .username
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .unwrap_or(&s.auth_user)
            .to_string();
        if user.is_empty() {
            return Err(ApiError::bad("Choose a user name"));
        }
        if user.chars().any(|c| c.is_control()) || user.len() > 64 {
            return Err(ApiError::bad("That user name isn't allowed"));
        }
        s.auth_user = user;
        match req.password.as_deref().filter(|p| !p.is_empty()) {
            Some(p) => {
                if p.chars().count() < 8 {
                    return Err(ApiError::bad("Use a password of at least 8 characters"));
                }
                s.auth_password_hash = tokio::task::spawn_blocking({
                    let p = p.to_string();
                    move || hash_password(&p)
                })
                .await
                .map_err(|e| ApiError::bad(e.to_string()))?
                .map_err(ApiError::bad)?;
            }
            None if s.auth_password_hash.is_empty() => {
                return Err(ApiError::bad("Choose a password"));
            }
            None => {}
        }
    } else {
        s.auth_password_hash.clear();
    }
    st.settings
        .set(s.clone())
        .map_err(|e| ApiError::bad(format!("Could not save: {e}")))?;

    // The password changed or the login was switched: everyone else is logged out, this browser stays in.
    st.auth.end_all_sessions();
    let enabled = st.auth.credentials(&st).is_some();
    let body = Json(json!({"enabled": enabled, "user": s.auth_user}));
    if enabled {
        let token = st.auth.new_session();
        Ok(([(header::SET_COOKIE, session_cookie(&token))], body).into_response())
    } else {
        Ok(([(header::SET_COOKIE, cleared_cookie())], body).into_response())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_hashed_and_verified() {
        let h = hash_password("correct horse").unwrap();
        assert!(h.starts_with("$argon2"));
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("x", "not a hash"));
        // two hashes of the same password differ (random salt)
        assert_ne!(h, hash_password("correct horse").unwrap());
    }

    #[test]
    fn reads_the_session_cookie_among_others() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            "theme=dark; rustybox_session=abc123; other=1"
                .parse()
                .unwrap(),
        );
        assert_eq!(cookie_value(&h).as_deref(), Some("abc123"));
        assert_eq!(cookie_value(&HeaderMap::new()), None);
    }

    #[test]
    fn sessions_expire_and_can_be_ended() {
        let a = Auth::default();
        let t = a.new_session();
        assert!(a.valid(&t) && !a.valid("nope"));
        a.end_session(&t);
        assert!(!a.valid(&t));
        let t2 = a.new_session();
        a.end_all_sessions();
        assert!(!a.valid(&t2));
        // an old session is refused
        let old = a.new_session();
        a.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(
            old.clone(),
            Instant::now() - SESSION_LIFETIME - Duration::from_secs(1),
        );
        assert!(!a.valid(&old));
    }

    #[test]
    fn environment_credentials_need_both_parts() {
        assert!(Auth::new(Some("me".into()), None).is_err());
        assert!(Auth::new(None, Some("secret".into())).is_err());
        assert!(Auth::new(None, None).unwrap().env.is_none());
        let a = Auth::new(Some(" me ".into()), Some("secret pass".into())).unwrap();
        let (u, h) = a.env.unwrap();
        assert_eq!(u, "me");
        assert!(verify_password("secret pass", &h));
    }
}
