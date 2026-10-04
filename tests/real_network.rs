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
        mock: false,
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

// These use the REAL internet (Arisen Studio, XboxUnity, GitHub) with a pretend Aurora console.
// Run them with: cargo test --test real_network -- --ignored --test-threads=1

#[tokio::test]
#[ignore = "uses the real internet"]
async fn real_trainer_is_downloaded_unzipped_and_installed_on_a_pretend_console() {
    let e = env("realtrainer");
    let srv_root = e.root.parent().unwrap().join("fakeconsole");
    rustybox::console::seed_mock(&srv_root).unwrap();
    let srv = rustybox::console::fake::start(&srv_root, "xbox", "xbox").unwrap();
    let (_, c) = call(
        &e.app,
        "POST",
        "/api/consoles",
        Some(json!({"name": "X", "host": "127.0.0.1", "port": srv.addr.port()})),
    )
    .await;
    let cid = c["id"].as_u64().unwrap();
    let (_, v) = call(&e.app, "POST", "/api/content/refresh", None).await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let (_, s) = call(&e.app, "GET", "/api/content/status", None).await;
    assert!(
        s["counts"]["trainers"].as_u64().unwrap() > 100
            && s["counts"]["mods"].as_u64().unwrap() > 20,
        "{s}"
    );
    let (_, l) = call(&e.app, "GET", "/api/content?kind=trainers&q=4D530805", None).await;
    let id = l["items"][0]["id"].clone();
    let (st, p) = call(
        &e.app,
        "POST",
        "/api/content/install/plan",
        Some(json!({"console_id": cid, "id": id})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    let (_, v) = call(
        &e.app,
        "POST",
        "/api/content/install/start",
        Some(json!({"console_id": cid, "id": id})),
    )
    .await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let on = srv_root
        .join("Hdd1/Aurora/User/Trainers/4D530805/Trainer(RETROBYTE)/Trainer(RETROBYTE).xex");
    assert_eq!(
        fs::read(&on).unwrap()[..4],
        *b"XEX2",
        "a real xex arrived: {}",
        on.display()
    );
    // Every mod/trainer/homebrew entry in the real database resolves to a safe console path.
    let items = rustybox::content::Db::new(&e.root.parent().unwrap().join("config"), false).items();
    let console = rustybox::console::Console {
        host: "x".into(),
        ..Default::default()
    };
    let (mut ok, mut bad) = (0, 0);
    for i in items
        .iter()
        .filter(|i| ["mods", "homebrew", "trainers"].contains(&i.kind.as_str()))
    {
        match rustybox::content::install::spec_for(i, &console) {
            Ok(spec) => {
                ok += 1;
                assert!(
                    spec.files
                        .iter()
                        .flat_map(|f| &f.targets)
                        .all(|t| t.starts_with("/Hdd1/") || t.starts_with("/Usb")),
                    "{}",
                    i.name
                );
            }
            Err(_) => bad += 1,
        }
    }
    eprintln!("real database: {ok} items resolve to console paths, {bad} are refused");
    assert!(ok > 400);
}

#[tokio::test]
#[ignore = "uses the real internet"]
async fn real_title_update_is_downloaded_read_and_installed_on_a_pretend_console() {
    let e = env("realtu");
    let srv_root = e.root.parent().unwrap().join("fakeconsole");
    rustybox::console::seed_mock(&srv_root).unwrap();
    let srv = rustybox::console::fake::start(&srv_root, "xbox", "xbox").unwrap();
    let (_, c) = call(
        &e.app,
        "POST",
        "/api/consoles",
        Some(json!({"name": "X", "host": "127.0.0.1", "port": srv.addr.port()})),
    )
    .await;
    let cid = c["id"].as_u64().unwrap();
    fs::create_dir_all(e.root.join("tus")).unwrap();
    let (_, lb) = call(&e.app, "POST", "/api/libraries", Some(json!({"name": "TUs", "kind": "patches", "paths": [{"path": e.root.join("tus").to_string_lossy(), "label": "t", "writable": true}]}))).await;
    let lid = lb["id"].as_i64().unwrap();
    let (_, lib) = call(&e.app, "GET", &format!("/api/libraries/{lid}"), None).await;
    let pid = lib["paths"][0]["id"].as_i64().unwrap();
    let (_, t) = call(
        &e.app,
        "GET",
        "/api/updates/unity/title?titleid=4D5307E6",
        None,
    )
    .await;
    assert!(t["updates"].as_array().unwrap().len() >= 5, "{t}");
    let (st, v) = call(
        &e.app,
        "POST",
        "/api/updates/unity/download",
        Some(
            json!({"title_id": "4D5307E6", "tuids": ["21581"], "library_id": lid, "path_id": pid}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let (_, l) = call(
        &e.app,
        "GET",
        &format!("/api/updates/local?library_id={lid}"),
        None,
    )
    .await;
    let tu = &l["updates"][0]["tu"];
    assert_eq!(
        (
            tu["title_id"].as_str(),
            tu["media_id"].as_str(),
            tu["base_version"].as_str(),
            tu["tu_version"].as_u64()
        ),
        (
            Some("4D5307E6"),
            Some("1CDE207A"),
            Some("0000002C"),
            Some(3)
        ),
        "XboxUnity says media 1CDE207A, base 0000002C, TU3: {l}"
    );
    assert_eq!(l["updates"][0]["compat"], "compatible");
    let body = json!({"console_id": cid, "files": [{"library_id": lid, "path_id": pid, "relpath": tu["relpath"]}]});
    let (_, v) = call(&e.app, "POST", "/api/updates/install/start", Some(body)).await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    assert!(
        srv_root
            .join("Hdd1/Content/0000000000000000/4D5307E6/000B0000/4D5307E6_TU3_21581")
            .is_file()
    );
}
