use std::net::SocketAddr;

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use rustybox::web::{self, Config};
use serde_json::{Value, json};
use tower::ServiceExt;

fn app(name: &str, mock: bool, auth: Option<(&str, &str)>) -> Router {
    let dir = std::env::temp_dir().join(format!("rustybox_web_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cfg = Config {
        config_dir: dir,
        roots: vec![],
        abgx360: None,
        dotenv: Default::default(),
        igdb_urls: None,
        mock,
        auth: auth.map(|(u, p)| (u.into(), p.into())),
    };
    web::router(web::build_state(cfg).unwrap())
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let mut req = req
        .body(
            body.map(|b| Body::from(b.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    (
        status,
        headers,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

fn json_of(b: &[u8]) -> Value {
    serde_json::from_slice(b).unwrap_or(Value::Null)
}

#[tokio::test]
async fn serves_the_ui_and_status() {
    let app = app("ui", true, None);
    let (s, h, body) = call(&app, "GET", "/", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("RustyBox"));
    let etag = h.get(header::ETAG).unwrap().to_str().unwrap().to_string();
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");

    // Revalidation with the ETag gives 304.
    let mut req = Request::builder()
        .uri("/")
        .header(header::IF_NONE_MATCH, etag)
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::NOT_MODIFIED
    );

    assert_eq!(
        call(&app, "GET", "/assets/js/app.js", None, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/assets/nope.js", None, None).await.0,
        StatusCode::NOT_FOUND
    );

    let (_, _, body) = call(&app, "GET", "/api/status", None, None).await;
    let st = json_of(&body);
    assert_eq!(st["mock"], true);
    assert!(st["version"].is_string());
}

#[tokio::test]
async fn login_protects_everything_but_the_login() {
    let app = app("auth", false, Some(("me", "secret pass")));

    let (s, _, body) = call(&app, "GET", "/api/jobs", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(json_of(&body)["error"], "AUTH_REQUIRED");
    // The root shows the login page, not the app.
    let (_, _, page) = call(&app, "GET", "/", None, None).await;
    assert!(String::from_utf8_lossy(&page).contains("Log in"));
    assert_eq!(
        call(&app, "GET", "/api/health", None, None).await.0,
        StatusCode::OK
    );

    let (s, _, _) = call(
        &app,
        "POST",
        "/api/login",
        Some(json!({"username": "me", "password": "wrong"})),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, h, _) = call(
        &app,
        "POST",
        "/api/login",
        Some(json!({"username": "me", "password": "secret pass"})),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let cookie = h
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert_eq!(
        call(&app, "GET", "/api/jobs", None, Some(&cookie)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn demo_job_runs_and_can_be_cancelled_but_only_in_mock() {
    let real = app("nomock", false, None);
    assert_eq!(
        call(&real, "POST", "/api/jobs/demo", None, None).await.0,
        StatusCode::BAD_REQUEST
    );

    let app = app("mock", true, None);
    let (s, _, body) = call(&app, "POST", "/api/jobs/demo", None, None).await;
    assert_eq!(s, StatusCode::OK);
    let id = json_of(&body)["job"].as_u64().unwrap();

    // A second demo job conflicts on the shared resource.
    assert_eq!(
        call(&app, "POST", "/api/jobs/demo", None, None).await.0,
        StatusCode::CONFLICT
    );

    assert_eq!(
        call(&app, "POST", &format!("/api/jobs/{id}/cancel"), None, None)
            .await
            .0,
        StatusCode::OK
    );
    for _ in 0..50 {
        let (_, _, body) = call(&app, "GET", &format!("/api/jobs/{id}"), None, None).await;
        if json_of(&body)["job"]["status"] == "cancelled" {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("job did not cancel");
}

/// An `on…` handler that returns `false` cancels the event, so `onkeydown = (e) => e.key === "Enter" && go()`
/// silently swallowed every other keystroke (typing into a search box did nothing). Handlers must use a
/// block body when they only want to act on some events.
#[test]
fn no_event_handler_in_the_ui_returns_false_by_accident() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "js") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/js"),
        &mut files,
    );
    assert!(files.len() > 10);
    let bad = regex_free_scan(&files);
    assert!(
        bad.is_empty(),
        "handlers that return false for some events: {bad:?}"
    );
}

fn regex_free_scan(files: &[std::path::PathBuf]) -> Vec<String> {
    let mut bad = Vec::new();
    for f in files {
        for (n, line) in std::fs::read_to_string(f).unwrap().lines().enumerate() {
            for h in [
                "onkeydown",
                "onkeyup",
                "onkeypress",
                "oninput",
                "onclick",
                "onchange",
                "onsubmit",
            ] {
                if let Some(i) = line.find(&format!(".{h} = (")) {
                    let rest = &line[i..];
                    // An expression body (no `{`) that uses `&&` can evaluate to false.
                    if let Some(arrow) = rest.find("=> ") {
                        let body = &rest[arrow + 3..];
                        let end = body.find(';').unwrap_or(body.len());
                        let body = &body[..end];
                        if !body.trim_start().starts_with('{')
                            && body.contains(" && ")
                            && !body.contains("(()")
                        {
                            bad.push(format!("{}:{}", f.display(), n + 1));
                        }
                    }
                }
            }
        }
    }
    bad
}

/// Every relative `import ... from "./x.js"` in the UI must point at a file that exists: a wrong
/// `../` in a shared module only shows up in the browser, as a page that won't load.
#[test]
fn every_import_in_the_ui_points_at_a_real_file() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "js") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/js"),
        &mut files,
    );
    let mut missing = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for part in text.split(" from \"").skip(1) {
            let Some(path) = part.split('"').next() else {
                continue;
            };
            if path.starts_with('.') && !f.parent().unwrap().join(path).is_file() {
                missing.push(format!("{} imports {path}", f.display()));
            }
        }
        for part in text.split("import(\"").skip(1) {
            let Some(path) = part.split('"').next() else {
                continue;
            };
            if path.starts_with('.')
                && !path.contains('$')
                && !f.parent().unwrap().join(path).is_file()
            {
                missing.push(format!("{} imports {path}", f.display()));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "imports that don't resolve: {missing:?}"
    );
}

/// A syntax error in any UI file only shows up in the browser, as a page that won't load. If Node
/// is installed, check every file parses as a module.
#[test]
fn every_ui_script_parses() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("node isn't installed: skipping the UI syntax check");
        return;
    }
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "js") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/js"),
        &mut files,
    );
    let mut bad = Vec::new();
    for f in &files {
        let mut child = Command::new("node")
            .args(["--input-type=module", "--check"])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&std::fs::read(f).unwrap())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        if !out.status.success() {
            bad.push(format!(
                "{}: {}",
                f.display(),
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
    }
    assert!(bad.is_empty(), "scripts that don't parse: {bad:?}");
}
