use std::{fs, io::Write, net::SocketAddr, path::PathBuf};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use rustybox::web::{self, Config};
use serde_json::Value;
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

fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        for (n, d) in files {
            z.start_file(*n, o).unwrap();
            z.write_all(d).unwrap();
        }
        z.finish().unwrap();
    }
    buf.into_inner()
}

#[tokio::test]
async fn the_xeunshackle_payload_is_fetched_unpacked_and_merged_into_the_package() {
    // A stand-in for the GitHub release, with the real zip's layout (one folder at the top).
    let zip = zip_of(&[
        (
            "XeUnshackle-BETA-v1_03/BadUpdatePayload/default.xex",
            b"payload-bytes",
        ),
        (
            "XeUnshackle-BETA-v1_03/BadUpdatePayload/BadStorage.xex.dll",
            b"dll",
        ),
        (
            "XeUnshackle-BETA-v1_03/launch.ini",
            b"[Paths]\r\nDefault = \r\n",
        ),
        ("XeUnshackle-BETA-v1_03/Xbdm.xex", b"xbdm"),
        ("../escape.txt", b"no"),
    ]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().route(
        "/xu.zip",
        axum::routing::get(move || {
            let z = zip.clone();
            async move { z }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    // SAFETY: the only test in this binary, so nothing else reads the environment meanwhile.
    unsafe { std::env::set_var("RUSTYBOX_XEUNSHACKLE_URL", format!("{base}/xu.zip")) };

    let e = env("payload");
    let cfg = e.root.parent().unwrap().join("config");
    // The real package first: no payload.
    for (p, d) in [
        ("BadUpdatePayload/update_data.bin", &b"u"[..]),
        (
            "Content/E0002FF78DFBDE7B/FFFE07D1/00010000/E0002FF78DFBDE7B",
            b"p",
        ),
    ] {
        let mut req = Request::builder()
            .method("PUT")
            .uri(format!("/api/usb/badavatar/file?path={p}"))
            .body(Body::from(d.to_vec()))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
        assert_eq!(
            e.app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::OK
        );
    }
    let (_, s) = call(&e.app, "GET", "/api/usb/badavatar/source", None).await;
    assert_eq!(s["has_payload"], false, "{s}");
    let (st, v) = call(&e.app, "POST", "/api/usb/badavatar/payload", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let pkg = cfg.join("badavatar");
    assert_eq!(
        fs::read(pkg.join("BadUpdatePayload/default.xex")).unwrap(),
        b"payload-bytes"
    );
    assert!(
        pkg.join("launch.ini").is_file()
            && pkg.join("Xbdm.xex").is_file()
            && pkg.join("BadUpdatePayload/BadStorage.xex.dll").is_file()
    );
    assert!(
        pkg.join("BadUpdatePayload/update_data.bin").is_file(),
        "what was there is kept"
    );
    assert!(
        !cfg.join("escape.txt").exists()
            && !pkg.join("escape.txt").exists()
            && !cfg.join("staging/payload").exists()
    );
    let (_, s) = call(&e.app, "GET", "/api/usb/badavatar/source", None).await;
    assert_eq!(
        (s["has_payload"].as_bool(), s["has_content"].as_bool()),
        (Some(true), Some(true)),
        "{s}"
    );
}
