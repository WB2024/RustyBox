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
async fn usb_sticks_are_listed_safely_and_a_bad_avatar_stick_is_built_in_mock_mode() {
    let e = env("usb");
    let cfg = e.root.parent().unwrap().join("config");
    let (_, d) = call(&e.app, "GET", "/api/usb/devices", None).await;
    let devs = d["devices"].as_array().unwrap();
    assert!(
        devs.iter()
            .any(|x| x["path"] == "/mock/stick1" && x["eligible"] == true),
        "{d}"
    );
    let sys = devs.iter().find(|x| x["path"] == "/mock/disk").unwrap();
    assert_eq!(sys["eligible"], false);

    // The system disk is refused outright, whatever the request says.
    let (st, v) = call(
        &e.app,
        "POST",
        "/api/usb/format",
        Some(json!({"device": "/mock/disk", "confirm": "/mock/disk"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, _) = call(
        &e.app,
        "POST",
        "/api/usb/format",
        Some(json!({"device": "/dev/sda", "confirm": "/dev/sda"})),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "a device that isn't in the list is not touched"
    );

    // The sample exploit package is found (mock mode makes one).
    let (_, s) = call(&e.app, "GET", "/api/usb/badavatar/source", None).await;
    assert_eq!(s["ok"], true, "{s}");
    assert_eq!(s["has_aurora"], true);

    // Something already on the stick is kept unless it is formatted, and formatting needs the path typed back.
    let stick = cfg.join("mock-usb/stick1");
    fs::write(stick.join("holiday.jpg"), b"photo").unwrap();
    let (_, p) = call(
        &e.app,
        "POST",
        "/api/usb/badavatar/plan",
        Some(json!({"device": "/mock/stick1", "format": false, "set_default": true})),
    )
    .await;
    assert_eq!(p["plan"]["ok"], true, "{p}");
    assert!(
        p["plan"]["warnings"].to_string().contains("isn't empty"),
        "{p}"
    );
    let (st, v) = call(
        &e.app,
        "POST",
        "/api/usb/badavatar/start",
        Some(json!({"device": "/mock/stick1", "format": true})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        stick.join("holiday.jpg").exists(),
        "refused before anything was touched"
    );
    let (st, v) = call(&e.app, "POST", "/api/usb/badavatar/start", Some(json!({"device": "/mock/stick1", "format": true, "set_default": true, "label": "BADUPDATE", "confirm": "/mock/stick1"}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    assert!(
        !stick.join("holiday.jpg").exists(),
        "formatting erased the stick"
    );
    assert!(stick.join("BadUpdatePayload/default.xex").is_file());
    assert!(stick.join("Apps/Aurora/Aurora.xex").is_file());
    assert!(
        fs::read_to_string(stick.join("launch.ini"))
            .unwrap()
            .contains("Default = Usb:\\Apps\\Aurora\\Aurora.xex")
    );
    assert!(stick.join("info.txt").is_file());

    // A bad label is refused before a job exists.
    let (st, _) = call(&e.app, "POST", "/api/usb/format", Some(json!({"device": "/mock/stick2", "label": "not a valid label!", "confirm": "/mock/stick2"}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_stick_is_backed_up_restored_and_the_backup_deleted_in_mock_mode() {
    let e = env("usbbackup");
    let (_, b) = call(&e.app, "GET", "/api/usb/backups", None).await;
    if !b["tools_missing"].is_null() {
        eprintln!("partclone/zstd missing: skipping");
        return;
    }
    let (_, d) = call(&e.app, "GET", "/api/usb/devices", None).await;
    assert!(d["devices"][0]["fs_path"].is_string(), "{d}");
    let (st, v) = call(
        &e.app,
        "POST",
        "/api/usb/backup",
        Some(json!({"device": "/mock/stick1", "name": "Will's stick"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let (_, b) = call(&e.app, "GET", "/api/usb/backups", None).await;
    assert_eq!(b["backups"][0]["name"], "Will-s-stick", "{b}");
    let (st, _) = call(
        &e.app,
        "POST",
        "/api/usb/backup",
        Some(json!({"device": "/mock/stick1", "name": "Will's stick"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "a backup is never overwritten");
    let (st, _) = call(
        &e.app,
        "POST",
        "/api/usb/backup",
        Some(json!({"device": "/mock/disk", "name": "x"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // Restoring needs the device typed back.
    let body = json!({"backup": "Will-s-stick", "device": "/mock/stick2"});
    let (st, _) = call(&e.app, "POST", "/api/usb/restore", Some(body.clone())).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, v) = call(
        &e.app,
        "POST",
        "/api/usb/restore",
        Some(
            json!({"backup": "Will-s-stick", "device": "/mock/stick2", "confirm": "/mock/stick2"}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let (st, _) = call(&e.app, "DELETE", "/api/usb/backups?name=Will-s-stick", None).await;
    assert_eq!(st, StatusCode::OK);
    let (_, b) = call(&e.app, "GET", "/api/usb/backups", None).await;
    assert_eq!(b["backups"].as_array().unwrap().len(), 0);
}

async fn put(app: &Router, path: &str, body: &[u8]) -> StatusCode {
    let mut req = Request::builder().method("PUT").uri(format!(
        "/api/usb/badavatar/file?path={}",
        path.replace(' ', "%20")
    ));
    req = req.header(header::CONTENT_TYPE, "application/octet-stream");
    let mut req = req.body(Body::from(body.to_vec())).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
    app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn the_exploit_package_is_uploaded_from_the_browser_checked_and_built() {
    let e = env("usbupload");
    // The real package layout: no payload, no Aurora.
    for (p, d) in [
        ("BadUpdatePayload/update_data.bin", &b"u"[..]),
        ("BadUpdatePayload/xke_update.bin", b"x"),
        (
            "Content/E0002FF78DFBDE7B/FFFE07D1/00010000/E0002FF78DFBDE7B",
            b"profile",
        ),
    ] {
        assert_eq!(put(&e.app, p, d).await, StatusCode::OK, "{p}");
    }
    assert_eq!(
        put(&e.app, "../escape.txt", b"no").await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(put(&e.app, "a//b", b"no").await, StatusCode::BAD_REQUEST);
    let (_, s) = call(&e.app, "GET", "/api/usb/badavatar/source", None).await;
    assert_eq!(
        (
            s["ok"].as_bool(),
            s["has_payload"].as_bool(),
            s["has_content"].as_bool(),
            s["files"].as_u64()
        ),
        (Some(true), Some(false), Some(true), Some(3)),
        "{s}"
    );
    let cfg = e.root.parent().unwrap().join("config");
    assert!(!cfg.join("escape.txt").exists() && !e.root.join("escape.txt").exists());

    // Without a payload the plan says so; a stick is not built.
    let (_, p) = call(
        &e.app,
        "POST",
        "/api/usb/badavatar/plan",
        Some(json!({"device": "/mock/stick1"})),
    )
    .await;
    assert_eq!(p["plan"]["ok"], false, "{p}");
    assert!(
        p["plan"]["problems"].to_string().contains("default.xex"),
        "{p}"
    );
    let (st, v) = call(
        &e.app,
        "POST",
        "/api/usb/badavatar/start",
        Some(json!({"device": "/mock/stick1"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert!(!cfg.join("mock-usb/stick1/Content").exists());

    // Add the payload and Aurora; now it builds, and a minimal launch.ini is made.
    assert_eq!(
        put(&e.app, "BadUpdatePayload/default.xex", b"payload").await,
        StatusCode::OK
    );
    assert_eq!(
        put(&e.app, "Apps/Aurora/Aurora.xex", b"aurora").await,
        StatusCode::OK
    );
    let (_, p) = call(
        &e.app,
        "POST",
        "/api/usb/badavatar/plan",
        Some(json!({"device": "/mock/stick1", "set_default": true})),
    )
    .await;
    assert_eq!(p["plan"]["ok"], true, "{p}");
    let (_, v) = call(
        &e.app,
        "POST",
        "/api/usb/badavatar/start",
        Some(json!({"device": "/mock/stick1", "set_default": true})),
    )
    .await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let stick = cfg.join("mock-usb/stick1");
    assert!(
        stick.join("BadUpdatePayload/default.xex").is_file()
            && stick.join("Apps/Aurora/Aurora.xex").is_file()
    );
    assert!(
        fs::read_to_string(stick.join("launch.ini"))
            .unwrap()
            .contains("Default = Usb:")
    );

    // Removing the package needs a confirmation.
    let (st, _) = call(&e.app, "DELETE", "/api/usb/badavatar/package", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &e.app,
        "DELETE",
        "/api/usb/badavatar/package?confirm=true",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(!cfg.join("badavatar/BadUpdatePayload").exists());
}
