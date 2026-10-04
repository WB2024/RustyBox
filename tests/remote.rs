use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use rustybox::{
    agent::{self, AgentState},
    web::{self, Config},
    xbox::stfs,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

struct Agent {
    root: PathBuf,
    url: String,
    task: tokio::task::JoinHandle<()>,
}

async fn start_agent(name: &str, read_only: bool) -> Agent {
    let root = std::env::temp_dir().join(format!("rustybox_remote_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let state = AgentState::new(&root, TOKEN.into(), "Will's PC".into(), read_only).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = agent::router(Arc::new(state));
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Agent { root, url, task }
}

fn main_app(name: &str) -> Router {
    let base = std::env::temp_dir().join(format!(
        "rustybox_remote_main_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let cfg = Config {
        config_dir: base.join("config"),
        roots: vec![],
        abgx360: None,
        dotenv: Default::default(),
        igdb_urls: None,
        mock: false,
        auth: None,
    };
    web::router(web::build_state(cfg).unwrap())
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    json_body: Option<Value>,
    raw: Option<Vec<u8>>,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
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

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (s, _, b) = send(app, method, uri, body, None, &[]).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

async fn wait_job(app: &Router, id: u64) -> Value {
    for _ in 0..400 {
        let (_, j) = call(app, "GET", &format!("/api/jobs/{id}"), None).await;
        if matches!(
            j["job"]["status"].as_str().unwrap(),
            "done" | "failed" | "cancelled"
        ) {
            return j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("job {id} did not finish");
}

async fn scan(app: &Router, lib: i64) -> Value {
    let (_, v) = call(app, "POST", &format!("/api/libraries/{lib}/scan"), None).await;
    wait_job(app, v["job"].as_u64().unwrap()).await
}

fn make_god(dir: &Path, title: u32, name: &str, parts: u32, data_files: u32) {
    let ct = dir.join(format!("{title:08X}")).join("00007000");
    std::fs::create_dir_all(ct.join("ABCDEF01.data")).unwrap();
    let mut c = stfs::build(b"LIVE", 0x7000, title, 1, name, name);
    c[0x3A0..0x3A4].copy_from_slice(&parts.to_le_bytes());
    std::fs::write(ct.join("ABCDEF01"), c).unwrap();
    for i in 0..data_files {
        std::fs::write(ct.join(format!("ABCDEF01.data/Data{i:04}")), b"data").unwrap();
    }
}

fn remote_path(a: &Agent, token: &str, writable: bool) -> Value {
    json!({"remote": {"url": a.url, "token": token}, "writable": writable})
}

#[tokio::test]
async fn a_drive_on_another_machine_is_added_scanned_and_shown_like_any_other() {
    let a = start_agent("add", false).await;
    make_god(
        &a.root.join("Games/Alan Wake"),
        0x4D53_0805,
        "Alan Wake",
        2,
        2,
    );
    make_god(
        &a.root.join("Games/Viva Pinata"),
        0x4D53_07F2,
        "Viva Pinata",
        5,
        2,
    ); // an incomplete copy
    let app = main_app("add");

    // The wrong token is refused clearly and nothing is saved.
    let (s, v) = call(&app, "POST", "/api/libraries", Some(json!({"name": "Xbox drive", "kind": "god", "paths": [remote_path(&a, "wrong-token-wrong-token", true)]}))).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("AGENT_AUTH")),
        "{v}"
    );
    assert_eq!(
        call(&app, "GET", "/api/libraries", None)
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // So is an address that makes no sense.
    let (s, _) = call(
        &app,
        "POST",
        "/api/remote/test",
        Some(json!({"url": "192.168.1.5:8099", "token": TOKEN})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // The connection can be tried first.
    let (s, t) = call(
        &app,
        "POST",
        "/api/remote/test",
        Some(json!({"url": a.url, "token": TOKEN})),
    )
    .await;
    assert_eq!(
        (s, t["name"].as_str()),
        (StatusCode::OK, Some("Will's PC")),
        "{t}"
    );

    let (s, v) = call(
        &app,
        "POST",
        "/api/libraries",
        Some(json!({"name": "Xbox drive", "kind": "god", "paths": [remote_path(&a, TOKEN, true)]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let lib = v["id"].as_i64().unwrap();

    // It looks like a folder, labelled with the agent's name, and the token never comes back.
    let (_, raw) = {
        let (_, _, b) = send(&app, "GET", "/api/libraries", None, None, &[]).await;
        ((), String::from_utf8(b).unwrap())
    };
    assert!(
        !raw.contains(TOKEN),
        "the agent token must not appear in any response"
    );
    let libs: Value = serde_json::from_str(&raw).unwrap();
    let p = &libs[0]["paths"][0];
    assert_eq!(
        (
            p["label"].as_str(),
            p["writable"].as_bool(),
            p["health"]["online"].as_bool()
        ),
        (Some("Will's PC"), Some(true), Some(true))
    );
    assert_eq!(p["health"]["remote_name"], "Will's PC");
    assert!(p["health"]["total"].as_u64().unwrap() > 0 && p["health"]["fs_type"].is_string());
    assert_eq!(p["remote_url"].as_str(), Some(a.url.as_str()));

    // Scanning happens on the drive's side, and the results are the usual items.
    assert_eq!(scan(&app, lib).await["job"]["status"], "done");
    let (_, items) = call(&app, "GET", &format!("/api/libraries/{lib}/items"), None).await;
    assert_eq!(items["total"], 2);
    let wake = items["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["title_id"] == "4D530805")
        .unwrap();
    assert_eq!(
        (wake["game_name"].as_str(), wake["health"].is_null()),
        (Some("Alan Wake"), true)
    );
    let viva = items["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["title_id"] == "4D5307F2")
        .unwrap();
    assert!(
        viva["health"]
            .as_str()
            .unwrap()
            .contains("3 of 5 data files are missing"),
        "{viva}"
    );

    // They appear in the cross-library Games view, with the incomplete copy marked.
    let (_, g) = call(&app, "GET", "/api/games", None).await;
    assert_eq!(g["games"].as_array().unwrap().len(), 2);
    let copy = &g["games"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["title_id"] == "4D5307F2")
        .unwrap()["copies"][0];
    assert!(copy["health"].as_str().unwrap().contains("missing"));

    // A second scan reads no headers again for unchanged games (the agent is told what is known).
    let j = scan(&app, lib).await;
    let log: String = j["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "log")
        .map(|e| e["msg"].as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(log.contains("0 new, 0 changed, 2 unchanged"), "{log}");
}

#[tokio::test]
async fn files_go_to_and_come_from_a_remote_drive_through_the_normal_endpoints() {
    let a = start_agent("transfer", false).await;
    std::fs::write(a.root.join("notes.txt"), b"0123456789").unwrap();
    let app = main_app("transfer");
    let (_, v) = call(&app, "POST", "/api/libraries", Some(json!({"name": "Drive files", "kind": "custom", "paths": [remote_path(&a, TOKEN, true)]}))).await;
    let lib = v["id"].as_i64().unwrap();
    let (_, l) = call(&app, "GET", &format!("/api/libraries/{lib}"), None).await;
    let path_id = l["paths"][0]["id"].as_i64().unwrap();
    scan(&app, lib).await;

    // Browser upload, in two pieces with a resume in between, lands on the drive.
    let url = |q: &str| {
        format!("/api/libraries/{lib}/upload?path_id={path_id}&rel=Mods%2Fnew.bin&total=10{q}")
    };
    let (s, _, b) = send(
        &app,
        "PUT",
        &url("&offset=0"),
        None,
        Some(b"01234".to_vec()),
        &[],
    )
    .await;
    assert_eq!(
        (s, serde_json::from_slice::<Value>(&b).unwrap()),
        (StatusCode::OK, json!({"done": false, "offset": 5}))
    );
    let (_, st) = call(
        &app,
        "GET",
        &format!("/api/libraries/{lib}/upload?path_id={path_id}&rel=Mods%2Fnew.bin"),
        None,
    )
    .await;
    assert_eq!(st["offset"], 5);
    let (s, _, _) = send(
        &app,
        "PUT",
        &url("&offset=5"),
        None,
        Some(b"56789".to_vec()),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        std::fs::read(a.root.join("Mods/new.bin")).unwrap(),
        b"0123456789"
    );
    // The agent's own refusals come through with their codes.
    let (s, v) = call(&app, "PUT", &url("&offset=0"), None).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::CONFLICT, Some("EXISTS")),
        "{v}"
    );

    // Download whole, and in a range for resuming.
    scan(&app, lib).await;
    let (_, items) = call(
        &app,
        "GET",
        &format!("/api/libraries/{lib}/items?q=notes"),
        None,
    )
    .await;
    let item = items["items"][0]["id"].as_i64().unwrap();
    let dl = format!("/api/libraries/{lib}/items/{item}/download");
    let (s, h, b) = send(&app, "GET", &dl, None, None, &[]).await;
    assert_eq!(
        (s, b.as_slice(), h[header::CONTENT_LENGTH].to_str().unwrap()),
        (StatusCode::OK, b"0123456789".as_slice(), "10")
    );
    assert!(
        h[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("notes.txt")
    );
    let (s, h, b) = send(&app, "GET", &dl, None, None, &[("range", "bytes=3-6")]).await;
    assert_eq!(
        (s, b.as_slice(), h[header::CONTENT_RANGE].to_str().unwrap()),
        (
            StatusCode::PARTIAL_CONTENT,
            b"3456".as_slice(),
            "bytes 3-6/10"
        )
    );
    let (s, _, _) = send(&app, "GET", &dl, None, None, &[("range", "bytes=50-60")]).await;
    assert_eq!(s, StatusCode::RANGE_NOT_SATISFIABLE);
}

#[tokio::test]
async fn a_read_only_share_stays_read_only() {
    let a = start_agent("ro", true).await;
    let app = main_app("ro");
    // Asking for writing on a drive shared read-only is quietly refused.
    let (s, v) = call(
        &app,
        "POST",
        "/api/libraries",
        Some(
            json!({"name": "RO drive", "kind": "custom", "paths": [remote_path(&a, TOKEN, true)]}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let lib = v["id"].as_i64().unwrap();
    let (_, l) = call(&app, "GET", &format!("/api/libraries/{lib}"), None).await;
    assert_eq!(l["paths"][0]["writable"], false);
    assert_eq!(l["paths"][0]["health"]["share_read_only"], true);
    let path_id = l["paths"][0]["id"].as_i64().unwrap();
    let (s, _, _) = send(
        &app,
        "PUT",
        &format!("/api/libraries/{lib}/upload?path_id={path_id}&rel=x.bin&total=1&offset=0"),
        None,
        Some(b"x".to_vec()),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_offline_drive_is_reported_and_keeps_its_items() {
    let a = start_agent("offline", false).await;
    make_god(
        &a.root.join("Games/Alan Wake"),
        0x4D53_0805,
        "Alan Wake",
        1,
        1,
    );
    let app = main_app("offline");
    let (_, v) = call(
        &app,
        "POST",
        "/api/libraries",
        Some(json!({"name": "Xbox drive", "kind": "god", "paths": [remote_path(&a, TOKEN, true)]})),
    )
    .await;
    let lib = v["id"].as_i64().unwrap();
    scan(&app, lib).await;

    // The PC is switched off (or the drive unplugged).
    a.task.abort();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let (_, l) = call(&app, "GET", &format!("/api/libraries/{lib}"), None).await;
    let h = &l["paths"][0]["health"];
    assert_eq!(h["online"], false);
    assert!(
        h["problem"]
            .as_str()
            .unwrap()
            .contains("Can't reach the drive agent"),
        "{h}"
    );
    let j = scan(&app, lib).await;
    assert_eq!(j["job"]["status"], "done");
    let (_, items) = call(&app, "GET", &format!("/api/libraries/{lib}/items"), None).await;
    assert_eq!(
        (
            items["total"].clone(),
            items["items"][0]["available"].clone()
        ),
        (json!(1), json!(false)),
        "kept, but marked unavailable"
    );
}

#[tokio::test]
async fn a_library_can_start_inside_a_folder_of_the_drive() {
    let a = start_agent("subdir", false).await;
    make_god(
        &a.root.join("Games/Alan Wake"),
        0x4D53_0805,
        "Alan Wake",
        1,
        1,
    );
    // Profile content next to the games, as on a real console drive.
    make_god(
        &a.root.join("Content/E000FF/Saves"),
        0x4D53_0805,
        "Alan Wake save",
        1,
        1,
    );
    let app = main_app("subdir");

    let with = |sub: &str| json!({"remote": {"url": a.url, "token": TOKEN, "subdir": sub}, "writable": true});
    let (s, v) = call(
        &app,
        "POST",
        "/api/libraries",
        Some(json!({"name": "X", "kind": "god", "paths": [with("Nope")]})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["message"].as_str().unwrap().contains("no folder called"));
    let (s, _) = call(
        &app,
        "POST",
        "/api/libraries",
        Some(json!({"name": "X", "kind": "god", "paths": [with("../etc")]})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, v) = call(
        &app,
        "POST",
        "/api/libraries",
        Some(json!({"name": "Games on the drive", "kind": "god", "paths": [with("Games")]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let lib = v["id"].as_i64().unwrap();
    assert_eq!(scan(&app, lib).await["job"]["status"], "done");
    let (_, items) = call(&app, "GET", &format!("/api/libraries/{lib}/items"), None).await;
    assert_eq!(items["total"], 1, "only what is inside Games");
    assert_eq!(
        items["items"][0]["relpath"], "Alan Wake/4D530805",
        "paths are relative to the chosen folder"
    );

    // Uploads land inside it too.
    let (_, l) = call(&app, "GET", &format!("/api/libraries/{lib}"), None).await;
    let pid = l["paths"][0]["id"].as_i64().unwrap();
    let (s, _, _) = send(
        &app,
        "PUT",
        &format!("/api/libraries/{lib}/upload?path_id={pid}&rel=New%2Ffile.bin&total=2&offset=0"),
        None,
        Some(b"hi".to_vec()),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        std::fs::read(a.root.join("Games/New/file.bin")).unwrap(),
        b"hi"
    );
    assert!(!a.root.join("New").exists());
}

/// Game and folder names are full of characters that mean something in a web address: `&`, `#`,
/// `?`, `%`, `+`, spaces, accents. Every operation must carry them through untouched.
#[tokio::test(flavor = "multi_thread")]
async fn awkward_names_survive_the_trip_to_a_drive() {
    let a = start_agent("awkward", false).await;
    let names = [
        "A&B #1 ?x=y",
        "100% Fun + More",
        "Tom Clancy's H.A.W.X. 2",
        "Pokémon – Ünï 日本語",
        "semi;colon, comma",
    ];
    for n in names {
        std::fs::create_dir_all(a.root.join(n).join("sub")).unwrap();
        std::fs::write(a.root.join(n).join("file #1.bin"), b"data").unwrap();
    }
    let url = a.url.clone();
    tokio::task::spawn_blocking(move || {
        let r = rustybox::remote::Remote::new(&url, TOKEN);
        let top = r.list("").unwrap();
        for n in names {
            assert!(top.iter().any(|e| e.name == n && e.is_dir), "listed {n}");
            let inner = r.list(n).unwrap();
            assert!(
                inner.iter().any(|e| e.name == "file #1.bin" && e.size == 4),
                "inside {n}"
            );
            let st = r.stat(&format!("{n}/file #1.bin")).unwrap();
            assert!(st.exists && !st.is_dir, "stat {n}");
        }
        // Make, rename and delete with such names.
        r.mkdir("New &folder #2").unwrap();
        r.rename("New &folder #2", "Renamed ?folder + 3").unwrap();
        assert!(r.stat("Renamed ?folder + 3").unwrap().exists);
        r.delete("Renamed ?folder + 3").unwrap();
        assert!(!r.stat("Renamed ?folder + 3").unwrap().exists);
    })
    .await
    .unwrap();
    a.task.abort();
    let _ = std::fs::remove_dir_all(&a.root);
}
