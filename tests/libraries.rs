use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
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
    let base = std::env::temp_dir().join(format!("rustybox_lib_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("data");
    std::fs::create_dir_all(root.join("disk")).unwrap();
    std::fs::write(root.join("disk/a.iso"), b"AAAAA").unwrap();
    std::fs::create_dir_all(root.join("disk/sub")).unwrap();
    std::fs::write(root.join("disk/sub/b.ISO"), b"BBBBBBBB").unwrap();
    std::fs::write(root.join("disk/readme.txt"), b"x").unwrap();
    std::fs::create_dir_all(root.join("ro")).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
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

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    json_body: Option<Value>,
    raw: Option<Vec<u8>>,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let body = match (json_body, raw) {
        (Some(j), _) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(j.to_string())
        }
        (None, Some(b)) => Body::from(b),
        _ => Body::empty(),
    };
    let mut req = req.body(body).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
    let res = app.clone().oneshot(req).await.unwrap();
    let (s, h) = (res.status(), res.headers().clone());
    (
        s,
        h,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

async fn json_call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, method, uri, body, None, &[]).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

fn p(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// Create the standard test library: a writable disk and a read-only one.
async fn make_library(e: &Env) -> (i64, i64, i64) {
    let (s, v) = json_call(
        &e.app,
        "POST",
        "/api/libraries",
        Some(json!({"name": "ISOs", "kind": "iso", "paths": [
            {"path": p(&e.root.join("disk")), "label": "Disk", "writable": true},
            {"path": p(&e.root.join("ro")), "label": "Read only", "writable": false}]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["id"].as_i64().unwrap();
    let (_, lib) = json_call(&e.app, "GET", &format!("/api/libraries/{id}"), None).await;
    let paths = lib["paths"].as_array().unwrap();
    (
        id,
        paths[0]["id"].as_i64().unwrap(),
        paths[1]["id"].as_i64().unwrap(),
    )
}

async fn scan_and_wait(e: &Env, id: i64) {
    let (s, v) = json_call(&e.app, "POST", &format!("/api/libraries/{id}/scan"), None).await;
    assert_eq!(s, StatusCode::OK);
    let job = v["job"].as_u64().unwrap();
    for _ in 0..100 {
        let (_, j) = json_call(&e.app, "GET", &format!("/api/jobs/{job}"), None).await;
        match j["job"]["status"].as_str().unwrap() {
            "done" => return,
            "failed" | "cancelled" => panic!("scan failed: {j}"),
            _ => tokio::time::sleep(std::time::Duration::from_millis(30)).await,
        }
    }
    panic!("scan did not finish");
}

#[tokio::test]
async fn library_paths_must_be_absolute_existing_folders_inside_the_roots() {
    let e = env("validate");
    let bad = |path: String| json!({"name": "X", "kind": "iso", "paths": [{"path": path}]});
    for path in [
        "relative/dir".to_string(),
        p(&e.root.join("nope")),
        p(&e.root.join("disk/a.iso")),
        "/etc".into(),
        format!("{}/../../..", p(&e.root)),
    ] {
        let (s, v) = json_call(&e.app, "POST", "/api/libraries", Some(bad(path.clone()))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{path}: {v}");
    }
    let (s, _) = json_call(
        &e.app,
        "POST",
        "/api/libraries",
        Some(json!({"name": "X", "kind": "bogus"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    make_library(&e).await;
    let (s, v) = json_call(
        &e.app,
        "POST",
        "/api/libraries",
        Some(json!({"name": "isos", "kind": "iso"})),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "names are unique, ignoring case: {v}"
    );
}

#[tokio::test]
async fn scan_fills_the_index_and_items_can_be_searched() {
    let e = env("scan");
    let (id, _, _) = make_library(&e).await;
    scan_and_wait(&e, id).await;

    let (_, v) = json_call(&e.app, "GET", &format!("/api/libraries/{id}/items"), None).await;
    assert_eq!(v["total"], 2);
    let names: Vec<_> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["a", "b"]);

    let (_, v) = json_call(
        &e.app,
        "GET",
        &format!("/api/libraries/{id}/items?q=b"),
        None,
    )
    .await;
    assert_eq!(v["total"], 1);

    let (_, libs) = json_call(&e.app, "GET", "/api/libraries", None).await;
    assert_eq!(
        (libs[0]["items"].as_i64(), libs[0]["bytes"].as_i64()),
        (Some(2), Some(13))
    );
    assert_eq!(libs[0]["paths"][0]["health"]["online"], true);

    // A file removed from disk disappears on the next scan.
    std::fs::remove_file(e.root.join("disk/a.iso")).unwrap();
    scan_and_wait(&e, id).await;
    let (_, v) = json_call(&e.app, "GET", &format!("/api/libraries/{id}/items"), None).await;
    assert_eq!(v["total"], 1);
}

#[tokio::test]
async fn deleting_a_library_never_touches_the_files() {
    let e = env("delete");
    let (id, _, _) = make_library(&e).await;
    scan_and_wait(&e, id).await;
    let (s, _) = json_call(&e.app, "DELETE", &format!("/api/libraries/{id}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(e.root.join("disk/a.iso").exists());
    let (s, _) = json_call(&e.app, "GET", &format!("/api/libraries/{id}"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn uploads_are_chunked_resumable_and_safe() {
    let e = env("upload");
    let (id, w, ro) = make_library(&e).await;
    let url =
        |q: &str| format!("/api/libraries/{id}/upload?path_id={w}&rel=new%2FGame.iso&total=10{q}");

    // First half.
    let (s, _, b) = send(
        &e.app,
        "PUT",
        &url("&offset=0"),
        None,
        Some(b"01234".to_vec()),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&b).unwrap(),
        json!({"done": false, "offset": 5})
    );
    assert!(
        e.root.join("disk/new/Game.iso.part").exists()
            && !e.root.join("disk/new/Game.iso").exists()
    );

    // The browser reconnects and asks where to continue.
    let (_, v) = json_call(
        &e.app,
        "GET",
        &format!("/api/libraries/{id}/upload?path_id={w}&rel=new%2FGame.iso"),
        None,
    )
    .await;
    assert_eq!(v["offset"], 5);

    // Wrong offset is refused; the right one finishes the file.
    let (s, _, _) = send(
        &e.app,
        "PUT",
        &url("&offset=3"),
        None,
        Some(b"xx".to_vec()),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _, b) = send(
        &e.app,
        "PUT",
        &url("&offset=5"),
        None,
        Some(b"56789".to_vec()),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(serde_json::from_slice::<Value>(&b).unwrap()["done"], true);
    assert_eq!(
        std::fs::read(e.root.join("disk/new/Game.iso")).unwrap(),
        b"0123456789"
    );
    assert!(!e.root.join("disk/new/Game.iso.part").exists());

    // Uploading over an existing file needs an explicit overwrite.
    let (s, _, _) = send(
        &e.app,
        "PUT",
        &url("&offset=0"),
        None,
        Some(b"0123456789".to_vec()),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _, _) = send(
        &e.app,
        "PUT",
        &url("&offset=0&overwrite=true"),
        None,
        Some(b"abcdefghij".to_vec()),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        std::fs::read(e.root.join("disk/new/Game.iso")).unwrap(),
        b"abcdefghij"
    );

    // More data than declared is refused and leaves nothing wrong behind.
    let (s, _, _) = send(
        &e.app,
        "PUT",
        &format!("/api/libraries/{id}/upload?path_id={w}&rel=big.iso&total=3&offset=0"),
        None,
        Some(b"toolong".to_vec()),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Read-only folders refuse writes; unsafe names are refused.
    let (s, _, _) = send(
        &e.app,
        "PUT",
        &format!("/api/libraries/{id}/upload?path_id={ro}&rel=a.iso&total=1&offset=0"),
        None,
        Some(b"x".to_vec()),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    for rel in [
        "..%2Fescape.iso",
        "a%2F..%2Fb.iso",
        ".hidden.iso",
        "x.part",
        "a%2F%2Fb.iso",
    ] {
        let (s, _, _) = send(
            &e.app,
            "PUT",
            &format!("/api/libraries/{id}/upload?path_id={w}&rel={rel}&total=1&offset=0"),
            None,
            Some(b"x".to_vec()),
            &[],
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{rel}");
    }
    assert!(!e.root.join("escape.iso").exists());

    // A leading slash is not an absolute path: it is read as relative to the library folder.
    let (s, _, _) = send(
        &e.app,
        "PUT",
        &format!("/api/libraries/{id}/upload?path_id={w}&rel=%2Fetc%2Fpasswd&total=1&offset=0"),
        None,
        Some(b"x".to_vec()),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(e.root.join("disk/etc/passwd").exists());

    // After a scan the upload is in the index.
    scan_and_wait(&e, id).await;
    let (_, v) = json_call(
        &e.app,
        "GET",
        &format!("/api/libraries/{id}/items?q=Game"),
        None,
    )
    .await;
    assert_eq!(v["total"], 1);
}

#[tokio::test]
async fn downloads_support_ranges_and_refuse_leaving_the_folder() {
    let e = env("download");
    let (id, w, _) = make_library(&e).await;
    scan_and_wait(&e, id).await;
    let (_, v) = json_call(
        &e.app,
        "GET",
        &format!("/api/libraries/{id}/items?q=b"),
        None,
    )
    .await;
    let item = v["items"][0]["id"].as_i64().unwrap();
    let url = format!("/api/libraries/{id}/items/{item}/download");

    let (s, h, b) = send(&e.app, "GET", &url, None, None, &[]).await;
    assert_eq!((s, b.as_slice()), (StatusCode::OK, b"BBBBBBBB".as_slice()));
    assert!(
        h[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("b.ISO")
    );

    let (s, h, b) = send(&e.app, "GET", &url, None, None, &[("range", "bytes=2-4")]).await;
    assert_eq!(
        (s, b.as_slice()),
        (StatusCode::PARTIAL_CONTENT, b"BBB".as_slice())
    );
    assert_eq!(h[header::CONTENT_RANGE], "bytes 2-4/8");

    // A symlink inside the library that points outside everywhere RustyBox may use must not be served.
    std::fs::write(e.root.parent().unwrap().join("secret.txt"), b"secret").unwrap();
    std::os::unix::fs::symlink(
        e.root.parent().unwrap().join("secret.txt"),
        e.root.join("disk/link.iso"),
    )
    .unwrap();
    scan_and_wait(&e, id).await;
    let (_, v) = json_call(
        &e.app,
        "GET",
        &format!("/api/libraries/{id}/items?q=link"),
        None,
    )
    .await;
    let link = v["items"][0]["id"].as_i64().unwrap();
    let (s, _, _) = send(
        &e.app,
        "GET",
        &format!("/api/libraries/{id}/items/{link}/download"),
        None,
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let _ = w;
}

#[tokio::test]
async fn the_folder_picker_stays_inside_the_roots() {
    let e = env("browse");
    let (_, v) = json_call(&e.app, "GET", "/api/fs/browse", None).await;
    assert_eq!(v["dirs"][0]["path"], p(&e.root));

    let (s, v) = json_call(
        &e.app,
        "GET",
        &format!("/api/fs/browse?path={}", p(&e.root.join("disk"))),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v["dirs"].as_array().unwrap().len(),
        1,
        "folders only, no files"
    );
    assert_eq!(v["parent"], p(&e.root));

    // The top of a root has no parent to go up to.
    let (_, v) = json_call(
        &e.app,
        "GET",
        &format!("/api/fs/browse?path={}", p(&e.root)),
        None,
    )
    .await;
    assert!(v["parent"].is_null());

    let (s, _) = json_call(&e.app, "GET", "/api/fs/browse?path=/etc", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn games_are_named_grouped_and_flagged_across_libraries() {
    let base = std::env::temp_dir().join(format!("rustybox_lib_games_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let cfg = Config {
        config_dir: base.join("config"),
        roots: vec![],
        abgx360: None,
        dotenv: Default::default(),
        igdb_urls: None,
        mock: true,
        auth: None,
    };
    let e = Env {
        app: web::router(web::build_state(cfg).unwrap()),
        root: base,
    };

    let (_, libs) = json_call(&e.app, "GET", "/api/libraries", None).await;
    for l in libs.as_array().unwrap() {
        scan_and_wait(&e, l["id"].as_i64().unwrap()).await;
    }

    let (_, v) = json_call(&e.app, "GET", "/api/games", None).await;
    let games = v["games"].as_array().unwrap();
    let find = |tid: &str| {
        games
            .iter()
            .find(|g| g["title_id"] == tid)
            .unwrap_or_else(|| panic!("no game {tid}: {v}"))
    };

    let gears = find("4D5307D5");
    assert_eq!(gears["name"], "Gears of War");
    assert_eq!(gears["duplicate"], true, "the same ISO on two disks");
    assert_eq!(gears["copies"].as_array().unwrap().len(), 2);

    let fable = find("4D5307F1");
    assert_eq!(
        (
            fable["has_iso"].as_bool(),
            fable["has_god"].as_bool(),
            fable["duplicate"].as_bool()
        ),
        (Some(true), Some(true), Some(false))
    );

    let forza = find("4D530910");
    assert_eq!(forza["copies"][0]["discs"], 2);

    assert!(find("ABCDEF12")["name"].is_null(), "not in the title list");

    // The file that isn't a game isn't a game, and says why.
    let iso_lib = libs
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "iso")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let (_, items) = json_call(
        &e.app,
        "GET",
        &format!("/api/libraries/{iso_lib}/items?q=not-a-game"),
        None,
    )
    .await;
    let bad = &items["items"][0];
    assert_eq!(bad["meta"], "error");
    assert!(
        bad["meta_error"]
            .as_str()
            .unwrap()
            .contains("Not an Xbox disc image")
    );
    assert!(bad["title_id"].is_null());

    // Filters.
    let (_, v) = json_call(&e.app, "GET", "/api/games?only=duplicates", None).await;
    assert_eq!(v["games"].as_array().unwrap().len(), 1);
    let (_, v) = json_call(&e.app, "GET", "/api/games?only=unknown", None).await;
    assert_eq!(v["games"].as_array().unwrap().len(), 1);
    let (_, v) = json_call(&e.app, "GET", "/api/games?q=fable", None).await;
    assert_eq!(v["games"].as_array().unwrap().len(), 1);
}
