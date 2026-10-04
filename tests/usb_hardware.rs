#![cfg(feature = "hardware_tests")]

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

// Runs against a real USB stick: set RUSTYBOX_TEST_USB_MOUNT to its mount point and run
// `cargo test --features hardware_tests --test usb_hardware`. It never formats anything; it builds
// onto the (empty) stick with a pretend package and removes exactly what it created.

#[tokio::test]
async fn a_real_stick_is_detected_and_built_onto_without_being_formatted() {
    let Ok(mount) = std::env::var("RUSTYBOX_TEST_USB_MOUNT") else {
        eprintln!("RUSTYBOX_TEST_USB_MOUNT isn't set: skipping");
        return;
    };
    let mount = PathBuf::from(mount);
    let e = env("hw");
    let cfg = e.root.parent().unwrap().join("config");
    rustybox::usb::badavatar::write_sample_package(&cfg.join("badavatar")).unwrap();
    let (_, d) = call(&e.app, "GET", "/api/usb/devices", None).await;
    let dev = d["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| {
            x["mountpoints"]
                .to_string()
                .contains(mount.to_str().unwrap())
        })
        .unwrap_or_else(|| panic!("no device mounted at {}: {d}", mount.display()))
        .clone();
    assert_eq!(dev["eligible"], true, "{dev}");
    let before: Vec<_> = fs::read_dir(&mount)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(
        before.is_empty(),
        "the stick isn't empty, so this test won't touch it: {before:?}"
    );
    let path = dev["path"].as_str().unwrap();
    let (_, p) = call(
        &e.app,
        "POST",
        "/api/usb/badavatar/plan",
        Some(json!({"device": path, "format": false, "set_default": true})),
    )
    .await;
    assert_eq!(p["plan"]["ok"], true, "{p}");
    let (st, v) = call(
        &e.app,
        "POST",
        "/api/usb/badavatar/start",
        Some(json!({"device": path, "format": false, "set_default": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    let ok = j["job"]["status"] == "done";
    let present = mount.join("BadUpdatePayload/default.xex").is_file()
        && mount.join("Apps/Aurora/Aurora.xex").is_file()
        && mount.join("info.txt").is_file();
    // Clean up exactly what was made, whatever happened.
    for n in ["BadUpdatePayload", "Apps"] {
        let _ = fs::remove_dir_all(mount.join(n));
    }
    for n in ["launch.ini", "info.txt"] {
        let _ = fs::remove_file(mount.join(n));
    }
    assert!(ok, "{j}");
    assert!(present);
    assert!(
        fs::read_dir(&mount).unwrap().next().is_none(),
        "left the stick as it found it"
    );
}
