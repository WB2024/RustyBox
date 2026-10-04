use std::{fs, net::SocketAddr, path::PathBuf};

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

struct Env {
    app: Router,
    root: PathBuf,
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
        mock: true,
        auth: None,
    };
    Env {
        app: web::router(web::build_state(cfg).unwrap()),
        root,
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

#[tokio::test]
async fn title_updates_are_found_downloaded_checked_and_installed_in_mock_mode() {
    let e = env("tu");
    // Mock mode brings its own pretend console and pretend XboxUnity.
    let (_, cons) = call(&e.app, "GET", "/api/consoles", None).await;
    let cid = cons[0]["id"].as_u64().unwrap();
    let (_, v) = call(&e.app, "POST", &format!("/api/consoles/{cid}/scan"), None).await;
    wait_job(&e.app, v["job"].as_u64().unwrap()).await;

    let (st, s) = call(&e.app, "GET", "/api/updates/unity/search?q=alan", None).await;
    assert_eq!(st, StatusCode::OK, "{s}");
    assert_eq!(s["titles"][0]["title_id"], "4D530805");
    let (st, _) = call(&e.app, "GET", "/api/updates/unity/search?q=a", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &e.app,
        "GET",
        "/api/updates/unity/title?titleid=nothex!!",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (_, t) = call(
        &e.app,
        "GET",
        "/api/updates/unity/title?titleid=4D530805",
        None,
    )
    .await;
    assert_eq!(t["updates"].as_array().unwrap().len(), 2, "{t}");
    assert_eq!(
        t["updates"][0]["compat"], "compatible",
        "that media ID is one of Alan Wake's known discs: {t}"
    );

    fs::create_dir_all(e.root.join("tus")).unwrap();
    // A title-updates library to keep them in, and a download into it.
    let (_, lb) = call(&e.app, "POST", "/api/libraries", Some(json!({"name": "TUs", "kind": "patches", "paths": [{"path": e.root.join("tus").to_string_lossy(), "label": "tus", "writable": true}]}))).await;
    let lid = lb["id"].as_i64().unwrap();
    let (_, lib) = call(&e.app, "GET", &format!("/api/libraries/{lid}"), None).await;
    let pid = lib["paths"][0]["id"].as_i64().unwrap();
    let (st, v) = call(&e.app, "POST", "/api/updates/unity/download", Some(json!({"title_id": "4D530805", "tuids": ["1002", "9999"], "library_id": lid, "path_id": pid}))).await;
    assert_eq!(
        st,
        StatusCode::BAD_REQUEST,
        "an update XboxUnity doesn't list is refused: {v}"
    );
    let (st, v) = call(&e.app, "POST", "/api/updates/unity/download", Some(json!({"title_id": "4D530805", "tuids": ["1001", "1002"], "library_id": lid, "path_id": pid}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");

    // They are read back from their headers.
    let (_, l) = call(
        &e.app,
        "GET",
        &format!("/api/updates/local?library_id={lid}"),
        None,
    )
    .await;
    let ups = l["updates"].as_array().unwrap();
    assert_eq!(ups.len(), 2, "{l}");
    assert_eq!(ups[0]["tu"]["title_id"], "4D530805");
    assert_eq!(ups[1]["tu"]["tu_version"], 2);

    // Install TU2 on the console; a non-update file is refused in the plan.
    let tu2 = &ups[1]["tu"]["relpath"];
    let body =
        json!({"console_id": cid, "files": [{"library_id": lid, "path_id": pid, "relpath": tu2}]});
    let (st, p) = call(
        &e.app,
        "POST",
        "/api/updates/install/plan",
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["ok"], true, "{p}");
    let target = p["entries"][0]["target"].as_str().unwrap().to_string();
    assert!(
        target.starts_with("/Hdd1/Content/0000000000000000/4D530805/000B0000/"),
        "{target}"
    );
    let (_, v) = call(&e.app, "POST", "/api/updates/install/start", Some(body)).await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let on = e
        .root
        .parent()
        .unwrap()
        .join("config/mock-console")
        .join(target.trim_start_matches('/'));
    assert!(on.is_file(), "{}", on.display());
    fs::write(e.root.join("tus/notes.txt"), vec![b'x'; 6000]).unwrap();
    let (_, p) = call(&e.app, "POST", "/api/updates/install/plan", Some(json!({"console_id": cid, "files": [{"library_id": lid, "path_id": pid, "relpath": "notes.txt"}]}))).await;
    assert_eq!(p["ok"], false, "{p}");

    // What is installed on the console shows up per game.
    let (_, on_c) = call(
        &e.app,
        "GET",
        &format!("/api/updates/console?console_id={cid}"),
        None,
    )
    .await;
    let aw = on_c["games"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["title_id"] == "4D530805")
        .unwrap();
    assert_eq!(aw["installed"].as_array().unwrap().len(), 1, "{on_c}");
}
