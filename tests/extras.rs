use std::{fs, net::SocketAddr};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use rustybox::web::{self, Config};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

struct Env {
    app: Router,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("rustybox_content_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("data/trainers")).unwrap();
    let root = fs::canonicalize(base.join("data")).unwrap();
    let cfg = Config {
        config_dir: base.join("config"),
        roots: vec![root.clone()],
        abgx360: None,
        dotenv: Default::default(),
        igdb_urls: None,
        mock: false,
        auth: None,
    };
    let _ = root;
    Env {
        app: web::router(web::build_state(cfg).unwrap()),
    }
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    let b = match body {
        Some(j) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(j.to_string())
        }
        None => Body::empty(),
    };
    let mut req = req.body(b).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
    let res = app.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (s, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn wait_job(app: &Router, id: u64) -> Value {
    for _ in 0..600 {
        let (_, j) = call(app, "GET", &format!("/api/jobs/{id}"), None).await;
        if matches!(
            j["job"]["status"].as_str().unwrap(),
            "done" | "failed" | "cancelled"
        ) {
            return j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    panic!("job {id} did not finish");
}

type Seen = Arc<Mutex<Vec<Value>>>;

/// A stand-in for a notification receiver.
async fn fake_services() -> (String, Seen) {
    use axum::{
        Form,
        extract::State,
        response::IntoResponse,
        routing::{get, post},
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Default::default();
    let torrents = Arc::new(Mutex::new(vec![
        json!({"hash": "abcdef0123", "name": "Some Game", "state": "stalledUP", "progress": 1.0, "size": 1000, "dlspeed": 0, "eta": 8640000, "content_path": "/downloads/Some Game", "save_path": "/downloads"}),
    ]));
    let t1 = torrents.clone();
    let t2 = torrents.clone();
    let t3 = torrents.clone();
    let s2 = seen.clone();
    let app = Router::new()
        .route("/api/v2/auth/login", post(|Form(f): Form<std::collections::HashMap<String, String>>| async move {
            if f.get("username").map(String::as_str) == Some("admin") && f.get("password").map(String::as_str) == Some("secret") {
                ([("set-cookie", "SID=abc123; HttpOnly; path=/")], "Ok.").into_response()
            } else {
                ([("x", "y")], "Fails.").into_response()
            }
        }))
        .route("/api/v2/app/version", get(|headers: axum::http::HeaderMap| async move {
            if headers.get("cookie").and_then(|c| c.to_str().ok()) == Some("SID=abc123") { (StatusCode::OK, "v5.0.0").into_response() } else { StatusCode::FORBIDDEN.into_response() }
        }))
        .route("/api/v2/torrents/info", get(move || { let t = t1.clone(); async move { axum::Json(t.lock().unwrap().clone()) } }))
        .route("/api/v2/torrents/add", post(move |Form(f): Form<std::collections::HashMap<String, String>>| { let t = t2.clone(); async move {
            t.lock().unwrap().push(json!({"hash": "feed0001", "name": f["urls"], "state": "downloading", "progress": 0.1, "size": 5000, "dlspeed": 100, "eta": 60, "content_path": f.get("savepath").cloned().unwrap_or_default(), "save_path": ""}));
            "Ok."
        }}))
        .route("/api/v2/torrents/delete", post(move |Form(f): Form<std::collections::HashMap<String, String>>| { let t = t3.clone(); async move {
            t.lock().unwrap().retain(|x| x["hash"] != f["hashes"].as_str());
            "Ok."
        }}))
        .route("/notify", post(|State(s): State<Seen>, axum::Json(v): axum::Json<Value>| async move { s.lock().unwrap().push(v); "ok" }))
        .with_state(s2);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, seen)
}

#[tokio::test]
async fn notifications_and_secret_settings() {
    let (base, seen) = fake_services().await;
    let e = env("extras");
    // Settings: validated, and secrets never come back.
    let (st, v) = call(
        &e.app,
        "PUT",
        "/api/settings",
        Some(json!({"scan_interval_minutes": 2})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v) = call(
        &e.app,
        "PUT",
        "/api/settings",
        Some(json!({"notify_url": "ftp://nope"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v) = call(&e.app, "PUT", "/api/settings", Some(json!({"scan_interval_minutes": 60, "notify_url": format!("{base}/notify"), "notify_on": "all"}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let text = v.to_string();
    assert!(
        v.get("qbit_password").is_none()
            && v.get("notify_url").is_none()
            && !text.contains("/notify")
            && !text.contains("\"secret\""),
        "secrets stay on the server: {text}"
    );
    assert_eq!(
        (
            v["notify_url_set"].as_bool(),
            v["scan_interval_minutes"].as_u64()
        ),
        (Some(true), Some(60))
    );
    // Saving something else keeps them.
    let (_, v) = call(
        &e.app,
        "PUT",
        "/api/settings",
        Some(json!({"god_layout": "titleid"})),
    )
    .await;
    assert_eq!(v["notify_url_set"], true);

    // The notification test reaches the receiver.
    let (st, v) = call(&e.app, "POST", "/api/notify/test", Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(seen.lock().unwrap()[0]["title"], "RustyBox test");
}

#[tokio::test]
async fn finished_jobs_are_announced_by_the_background_notifier() {
    let (base, seen) = fake_services().await;
    let base_dir =
        std::env::temp_dir().join(format!("rustybox_content_notify_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base_dir);
    fs::create_dir_all(base_dir.join("data/lib")).unwrap();
    let root = fs::canonicalize(base_dir.join("data")).unwrap();
    let cfg = Config {
        config_dir: base_dir.join("config"),
        roots: vec![root.clone()],
        abgx360: None,
        dotenv: Default::default(),
        igdb_urls: None,
        mock: false,
        auth: None,
    };
    let state = web::build_state(cfg).unwrap();
    let app = web::router(state.clone());
    web::spawn_background(state);
    call(
        &app,
        "PUT",
        "/api/settings",
        Some(json!({"notify_url": format!("{base}/notify"), "notify_on": "failures"})),
    )
    .await;
    let (_, lb) = call(&app, "POST", "/api/libraries", Some(json!({"name": "Extras", "kind": "custom", "paths": [{"path": root.join("lib").to_string_lossy(), "label": "x", "writable": true}]}))).await;
    // A job that fails (a scan can't, so ask for something impossible): removing a game that isn't there is a 404, not a job, so use a failing console install instead.
    let (_, c) = call(
        &app,
        "POST",
        "/api/consoles",
        Some(json!({"name": "Nowhere", "host": "127.0.0.1", "port": 1})),
    )
    .await;
    let (_, v) = call(
        &app,
        "POST",
        &format!("/api/consoles/{}/scan", c["id"]),
        None,
    )
    .await;
    let _ = lb;
    wait_job(&app, v["job"].as_u64().unwrap()).await;
    for _ in 0..40 {
        if !seen.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let got = seen.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["status"], "failed");
    assert!(
        got[0]["title"].as_str().unwrap().contains("Scan Nowhere"),
        "{got:?}"
    );
}
