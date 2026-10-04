//! Torrents: from an indexer through qBittorrent into a library, and from a folder of `.torrent`
//! files with a choice of which files to fetch. qBittorrent and the indexer are pretend servers
//! that behave as the real ones do (login cookie, stopped adds, file priorities).

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use axum::{
    Form, Router,
    body::{Body, Bytes},
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, Request, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use http_body_util::BodyExt;
use rustybox::{
    grabber::torrent,
    web::{self, Config},
    xbox::{xex, xiso},
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Env {
    app: Router,
    state: Arc<web::AppState>,
    root: PathBuf,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("rustybox_tor_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    for d in ["downloads", "isos", "torrents"] {
        fs::create_dir_all(base.join("data").join(d)).unwrap();
    }
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
    let state = web::build_state(cfg).unwrap();
    Env {
        app: web::router(state.clone()),
        state,
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

fn write_iso(path: &Path, title: u32, media: u32) {
    let mut bytes = xiso::build_simple_disc(&xex::build(media, title, 1, 1));
    bytes.extend((0..1_500_000u32).map(|i| (i % 253) as u8));
    while !bytes.len().is_multiple_of(2048) {
        bytes.push(0);
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn make_zip(path: &Path, name: &str, data: &[u8]) {
    use std::io::Write;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut w = zip::ZipWriter::new(fs::File::create(path).unwrap());
    w.start_file(name, zip::write::SimpleFileOptions::default())
        .unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap();
}

/// Build a multi-file `.torrent`: (path parts joined by '/', size).
fn make_torrent(name: &str, files: &[(&str, u64)]) -> Vec<u8> {
    let mut b = b"d8:announce20:udp://t.invalid:1/an4:infod5:filesl".to_vec();
    for (p, size) in files {
        b.extend(format!("d6:lengthi{size}e4:pathl").as_bytes());
        for part in p.split('/') {
            b.extend(format!("{}:{part}", part.len()).as_bytes());
        }
        b.extend(b"ee");
    }
    b.extend(
        format!(
            "e4:name{}:{name}12:piece lengthi16384e6:pieces20:",
            name.len()
        )
        .as_bytes(),
    );
    b.extend([9u8; 20]);
    b.extend(b"ee");
    b
}

/// A one-file `.torrent`.
fn make_single(name: &str, size: u64) -> Vec<u8> {
    let mut b = format!(
        "d4:infod6:lengthi{size}e4:name{}:{name}12:piece lengthi16384e6:pieces20:",
        name.len()
    )
    .into_bytes();
    b.extend([5u8; 20]);
    b.extend(b"ee");
    b
}

// ── A pretend qBittorrent ────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct Tor {
    name: String,
    state: String,
    progress: f64,
    stopped: bool,
    files: Vec<(String, u64, u32)>,
    content_path: String,
}

#[derive(Default)]
struct Qb {
    torrents: BTreeMap<String, Tor>,
    cats: Vec<String>,
    /// What it was asked, in order: for the test to check.
    log: Vec<String>,
}

type Shared = Arc<Mutex<Qb>>;

fn authed(h: &HeaderMap) -> bool {
    h.get(header::COOKIE).and_then(|c| c.to_str().ok()) == Some("QBT_SID_8082=sess123")
}

/// The file part and the other fields of a multipart body.
fn multipart(body: &[u8], content_type: &str) -> (HashMap<String, String>, Option<Vec<u8>>) {
    let boundary = content_type.split("boundary=").nth(1).unwrap();
    let delim = format!("--{boundary}");
    let text_pieces: Vec<&[u8]> = {
        let mut out = vec![];
        let mut rest = body;
        let d = delim.as_bytes();
        while let Some(i) = rest.windows(d.len()).position(|w| w == d) {
            out.push(&rest[..i]);
            rest = &rest[i + d.len()..];
        }
        out.push(rest);
        out
    };
    let (mut fields, mut file) = (HashMap::new(), None);
    for p in text_pieces {
        let Some(split) = p.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&p[..split]).to_string();
        let mut data = &p[split + 4..];
        if data.ends_with(b"\r\n") {
            data = &data[..data.len() - 2];
        }
        let name = head
            .split("name=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("")
            .to_string();
        if head.contains("filename=") {
            file = Some(data.to_vec());
        } else {
            fields.insert(name, String::from_utf8_lossy(data).to_string());
        }
    }
    (fields, file)
}

async fn fake_qbit() -> (String, Shared) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let qb: Shared = Arc::default();
    let forbidden = || (StatusCode::FORBIDDEN, "Forbidden").into_response();
    let app = Router::new()
        .route("/api/v2/auth/login", post(|State(q): State<Shared>, headers: HeaderMap, Form(f): Form<HashMap<String, String>>| async move {
            q.lock().unwrap().log.push(format!("login referer={}", headers.get("referer").is_some()));
            if f.get("username").map(String::as_str) == Some("admin") && f.get("password").map(String::as_str) == Some("adminpass") {
                ([(header::SET_COOKIE, "QBT_SID_8082=sess123; HttpOnly; SameSite=Lax; path=/")], StatusCode::NO_CONTENT).into_response()
            } else {
                "Fails.".into_response()
            }
        }))
        .route("/api/v2/app/version", get(move |headers: HeaderMap| async move {
            if authed(&headers) { "v5.0.0".into_response() } else { forbidden() }
        }))
        .route("/api/v2/torrents/categories", get(move |State(q): State<Shared>, headers: HeaderMap| async move {
            if !authed(&headers) { return forbidden(); }
            let cats: serde_json::Map<String, Value> = q.lock().unwrap().cats.iter().map(|c| (c.clone(), json!({"name": c, "savePath": ""}))).collect();
            axum::Json(Value::Object(cats)).into_response()
        }))
        .route("/api/v2/torrents/createCategory", post(move |State(q): State<Shared>, headers: HeaderMap, Form(f): Form<HashMap<String, String>>| async move {
            if !authed(&headers) { return forbidden(); }
            q.lock().unwrap().cats.push(f["category"].clone());
            "".into_response()
        }))
        .route("/api/v2/torrents/add", post(move |State(q): State<Shared>, headers: HeaderMap, body: Bytes| async move {
            if !authed(&headers) { return forbidden(); }
            let ct = headers.get(header::CONTENT_TYPE).unwrap().to_str().unwrap().to_string();
            let (fields, file) = multipart(&body, &ct);
            let mut g = q.lock().unwrap();
            let stopped = fields.get("paused").map(String::as_str) == Some("true") || fields.get("stopped").map(String::as_str) == Some("true");
            let (hash, name, files) = if let Some(bytes) = file {
                let t = torrent::parse(&bytes).unwrap();
                (t.info_hash.clone(), t.name.clone(), t.files.iter().map(|f| (format!("{}/{}", t.name, f.path), f.size, 1u32)).collect())
            } else {
                let (h, dn) = torrent::magnet(fields.get("urls").unwrap()).unwrap();
                (h, dn.unwrap_or_default(), vec![])
            };
            g.log.push(format!("add {hash} stopped={stopped} category={}", fields.get("category").cloned().unwrap_or_default()));
            if g.torrents.contains_key(&hash) {
                return "Fails.".into_response();
            }
            g.torrents.insert(hash, Tor { name, state: if stopped { "stoppedDL".into() } else { "metaDL".into() }, stopped, files, ..Default::default() });
            "Ok.".into_response()
        }))
        .route("/api/v2/torrents/info", get(move |State(q): State<Shared>, headers: HeaderMap, Query(f): Query<HashMap<String, String>>| async move {
            if !authed(&headers) { return forbidden(); }
            let g = q.lock().unwrap();
            let want: Option<Vec<&str>> = f.get("hashes").map(|h| h.split('|').collect());
            let list: Vec<Value> = g.torrents.iter().filter(|(h, _)| want.as_ref().is_none_or(|w| w.contains(&h.as_str()))).map(|(h, t)| {
                json!({"hash": h, "name": t.name, "state": t.state, "progress": t.progress,
                       "amount_left": if t.progress >= 1.0 { 0 } else { 1000 }, "content_path": t.content_path,
                       "save_path": "/downloads", "ratio": 0.0})
            }).collect();
            axum::Json(Value::Array(list)).into_response()
        }))
        .route("/api/v2/transfer/info", get(move |headers: HeaderMap| async move {
            if !authed(&headers) { return forbidden(); }
            axum::Json(json!({"dl_info_speed": 2_000_000, "up_info_speed": 500_000, "connection_status": "connected"})).into_response()
        }))
        .route("/api/v2/sync/maindata", get(move |headers: HeaderMap| async move {
            if !authed(&headers) { return forbidden(); }
            axum::Json(json!({"rid": 1, "server_state": {"free_space_on_disk": 123_456_789_u64}})).into_response()
        }))
        .route("/api/v2/torrents/files", get(move |State(q): State<Shared>, headers: HeaderMap, Query(f): Query<HashMap<String, String>>| async move {
            if !authed(&headers) { return forbidden(); }
            let g = q.lock().unwrap();
            let t = g.torrents.get(&f["hash"]).cloned().unwrap_or_default();
            axum::Json(Value::Array(t.files.iter().enumerate().map(|(i, (n, s, p))| json!({"index": i, "name": n, "size": s, "priority": p})).collect())).into_response()
        }))
        .route("/api/v2/torrents/filePrio", post(move |State(q): State<Shared>, headers: HeaderMap, Form(f): Form<HashMap<String, String>>| async move {
            if !authed(&headers) { return forbidden(); }
            let mut g = q.lock().unwrap();
            g.log.push(format!("filePrio id={} priority={}", f["id"], f["priority"]));
            let prio: u32 = f["priority"].parse().unwrap();
            if let Some(t) = g.torrents.get_mut(&f["hash"]) {
                for i in f["id"].split('|') {
                    t.files[i.parse::<usize>().unwrap()].2 = prio;
                }
            }
            "".into_response()
        }))
        .route("/api/v2/torrents/start", post(move |State(q): State<Shared>, headers: HeaderMap, Form(f): Form<HashMap<String, String>>| async move {
            if !authed(&headers) { return forbidden(); }
            let mut g = q.lock().unwrap();
            g.log.push("start".into());
            if let Some(t) = g.torrents.get_mut(&f["hashes"]) {
                t.stopped = false;
                t.state = "metaDL".into();
            }
            "".into_response()
        }))
        .route("/api/v2/torrents/delete", post(move |State(q): State<Shared>, headers: HeaderMap, Form(f): Form<HashMap<String, String>>| async move {
            if !authed(&headers) { return forbidden(); }
            let mut g = q.lock().unwrap();
            g.log.push(format!("delete files={}", f["deleteFiles"]));
            g.torrents.remove(&f["hashes"]);
            "".into_response()
        }))
        .with_state(qb.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, qb)
}

fn set_state(qb: &Shared, hash: &str, state: &str, progress: f64, content: &str) {
    let mut g = qb.lock().unwrap();
    let t = g.torrents.get_mut(hash).unwrap();
    t.state = state.into();
    t.progress = progress;
    t.content_path = content.into();
}

async fn setup(e: &Env, qb_url: &str, extra: Value) -> (i64, i64) {
    let app = &e.app;
    let (st, lb) = call(app, "POST", "/api/libraries", Some(json!({"name": "Torrent ISOs", "kind": "iso", "paths": [{"path": e.root.join("isos").to_string_lossy(), "label": "isos", "writable": true}]}))).await;
    assert_eq!(st, StatusCode::OK, "{lb}");
    let lid = lb["id"].as_i64().unwrap();
    let (_, l) = call(app, "GET", &format!("/api/libraries/{lid}"), None).await;
    let pid = l["paths"][0]["id"].as_i64().unwrap();
    let mut cfg = json!({"qbit": {"url": qb_url, "username": "admin", "password": "adminpass", "category": "xbox360"}, "import_library": [lid, pid], "auto_import": true, "convert_iso": false});
    for (k, v) in extra.as_object().unwrap() {
        cfg[k] = v.clone();
    }
    let (st, c) = call(app, "PUT", "/api/grab/config", Some(cfg)).await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert!(
        !c.to_string().contains("adminpass"),
        "the password never reaches the browser: {c}"
    );
    assert_eq!(c["qbit"]["password_set"], true);
    (lid, pid)
}

#[tokio::test]
async fn qbittorrent_is_tested_with_a_wrong_password_and_a_right_one_and_the_category_is_made() {
    let (base, qb) = fake_qbit().await;
    let e = env("qbtest");
    let (st, v) = call(
        &e.app,
        "POST",
        "/api/grab/qbit/test",
        Some(json!({"url": base, "username": "admin", "password": "nope"})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{v}");
    assert_eq!(v["error"], "QBIT_LOGIN", "{v}");
    assert!(!v.to_string().contains("nope"));
    let (st, v) = call(
        &e.app,
        "POST",
        "/api/grab/qbit/test",
        Some(json!({"url": base, "username": "admin", "password": "adminpass"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        (v["version"].as_str(), v["category_exists"].as_bool()),
        (Some("v5.0.0"), Some(false))
    );
    // An address that nobody answers is explained, not a crash.
    let (st, v) = call(
        &e.app,
        "POST",
        "/api/grab/qbit/test",
        Some(json!({"url": "http://127.0.0.1:1", "username": "a", "password": "b"})),
    )
    .await;
    assert_ne!(st, StatusCode::OK);
    assert_eq!(v["error"], "QBIT_UNREACHABLE", "{v}");
    setup(&e, &base, json!({})).await;
    let (st, _) = call(
        &e.app,
        "POST",
        "/api/grab/qbit/category",
        Some(json!({"name": "xbox360"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(qb.lock().unwrap().cats.contains(&"xbox360".to_string()));
    // The login is reused: asking again doesn't log in again.
    let before = qb
        .lock()
        .unwrap()
        .log
        .iter()
        .filter(|l| l.starts_with("login"))
        .count();
    call(&e.app, "POST", "/api/grab/qbit/test", Some(json!({}))).await;
    let after = qb
        .lock()
        .unwrap()
        .log
        .iter()
        .filter(|l| l.starts_with("login"))
        .count();
    assert_eq!(before, after, "the session cookie is kept");
}

#[tokio::test]
async fn files_are_chosen_from_a_torrent_in_a_folder_then_fetched_unpacked_and_imported() {
    let (base, qb) = fake_qbit().await;
    let e = env("folder");
    let app = &e.app;
    let (lid, _) = setup(
        &e,
        &base,
        json!({"torrent_import_mode": "copy", "torrent_after_import": "remove"}),
    )
    .await;

    // A folder of torrent files, as Minerva's collections are: one torrent, many zipped games.
    let t = make_torrent(
        "Minerva_Myrient",
        &[
            ("Redump/Microsoft - Xbox 360/Game One (USA).zip", 1_000_000),
            (
                "Redump/Microsoft - Xbox 360/Game Two (Europe).zip",
                2_000_000,
            ),
            ("Redump/Microsoft - Xbox 360/readme.txt", 10),
        ],
    );
    fs::write(
        e.root.join("torrents/Minerva - Redump - Xbox 360.torrent"),
        &t,
    )
    .unwrap();
    fs::write(e.root.join("torrents/notes.txt"), b"not a torrent").unwrap();
    fs::write(e.root.join("torrents/broken.torrent"), b"junk").unwrap();
    let hash = torrent::parse(&t).unwrap().info_hash;

    // Only folders inside the allowed roots can be added; the list is saved.
    let (st, _) = call(
        app,
        "PUT",
        "/api/torrents/dirs",
        Some(json!({"dirs": ["/etc"]})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "outside the allowed folders");
    let (st, v) = call(
        app,
        "PUT",
        "/api/torrents/dirs",
        Some(json!({"dirs": [e.root.join("torrents").to_string_lossy()]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        (
            v["dirs"][0]["online"].as_bool(),
            v["dirs"][0]["count"].as_u64()
        ),
        (Some(true), Some(2)),
        "{v}"
    );
    let (_, f) = call(
        app,
        "GET",
        "/api/torrents/files?dir=0&q=xbox%20redump",
        None,
    )
    .await;
    assert_eq!(
        (f["total"].as_u64(), f["all"].as_u64()),
        (Some(1), Some(2)),
        "search is by words, in any order: {f}"
    );

    // Look inside it: every file, with its number and size.
    let file = "Minerva - Redump - Xbox 360.torrent";
    let (st, i) = call(
        app,
        "GET",
        &format!(
            "/api/torrents/inspect?dir=0&file={}",
            file.replace(' ', "%20")
        ),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{i}");
    assert_eq!(i["torrent"]["info_hash"], hash);
    assert_eq!(i["torrent"]["files"].as_array().unwrap().len(), 3);
    assert_eq!(
        i["torrent"]["files"][1]["path"],
        "Redump/Microsoft - Xbox 360/Game Two (Europe).zip"
    );
    assert_eq!(i["already_in_client"], false);
    // A name can't reach outside the folder or be anything but a torrent.
    for bad in [
        "../x.torrent",
        "notes.txt",
        "broken.torrent",
        "missing.torrent",
    ] {
        let (st, _) = call(
            app,
            "GET",
            &format!("/api/torrents/inspect?dir=0&file={bad}"),
            None,
        )
        .await;
        assert_ne!(st, StatusCode::OK, "{bad}");
    }

    // Choosing nothing, or a file that isn't there, is refused before qBittorrent hears of it.
    let send = |sel: Value| json!({"dir": 0, "file": file, "select": sel});
    for sel in [json!([]), json!([7])] {
        let (st, _) = call(app, "POST", "/api/torrents/send", Some(send(sel))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }
    assert!(qb.lock().unwrap().torrents.is_empty());

    // Send just the first game: added stopped, the other two switched off, then started.
    let (st, v) = call(app, "POST", "/api/torrents/send", Some(send(json!([0])))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["title"], "Game One (USA).zip");
    {
        let g = qb.lock().unwrap();
        assert_eq!(
            g.log
                .iter()
                .filter(|l| !l.starts_with("login"))
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                format!("add {hash} stopped=true category=xbox360"),
                "filePrio id=1|2 priority=0".to_string(),
                "start".to_string()
            ]
        );
        let prios: Vec<u32> = g.torrents[&hash].files.iter().map(|f| f.2).collect();
        assert_eq!(prios, vec![1, 0, 0]);
        assert!(!g.torrents[&hash].stopped);
    }
    // Sending it again doesn't fail or make a second grab: the chosen file is switched on in the
    // torrent that is already there.
    let (st, v2) = call(app, "POST", "/api/torrents/send", Some(send(json!([0, 2])))).await;
    assert_eq!(st, StatusCode::OK, "{v2}");
    assert_eq!(v2["existed"], true);
    assert_eq!(v2["grab"], v["grab"]);
    {
        let g = qb.lock().unwrap();
        let prios: Vec<u32> = g.torrents[&hash].files.iter().map(|f| f.2).collect();
        assert_eq!(prios, vec![1, 0, 1]);
    }

    // It follows qBittorrent: waiting, then 40%, then done with the folder it made.
    let grab = |a: &Value| a["grabs"][0]["grab"].clone();
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(
        (grab(&a)["protocol"].as_str(), grab(&a)["status"].as_str()),
        (Some("torrent"), Some("queued")),
        "{a}"
    );
    set_state(&qb, &hash, "downloading", 0.4, "");
    web::poll_downloads(&e.state).await;
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(
        (grab(&a)["status"].as_str(), grab(&a)["progress"].as_f64()),
        (Some("downloading"), Some(40.0)),
        "{a}"
    );

    // The download: only the chosen zip exists, with a disc image inside it.
    let content = e.root.join("downloads/Minerva_Myrient");
    let mut iso = Vec::new();
    {
        write_iso(&e.root.join("downloads/tmp.iso"), 0x4D53_07E6, 0x1CDE_207A);
        iso.extend(fs::read(e.root.join("downloads/tmp.iso")).unwrap());
        fs::remove_file(e.root.join("downloads/tmp.iso")).unwrap();
    }
    make_zip(
        &content.join("Redump/Microsoft - Xbox 360/Game One (USA).zip"),
        "Game One (USA).iso",
        &iso,
    );
    set_state(&qb, &hash, "stalledUP", 1.0, &content.to_string_lossy());
    web::poll_downloads(&e.state).await;
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(
        grab(&a)["status"],
        "unpacking",
        "a zip of a disc image is opened first: {a}"
    );
    let unpack_job = grab(&a)["import_job"].as_u64().unwrap();
    assert_eq!(wait_job(app, unpack_job).await["job"]["status"], "done");

    web::poll_downloads(&e.state).await;
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(grab(&a)["status"], "importing", "{a}");
    let import_job = grab(&a)["import_job"].as_u64().unwrap();
    assert_ne!(import_job, unpack_job);
    assert_eq!(wait_job(app, import_job).await["job"]["status"], "done");
    web::poll_downloads(&e.state).await;
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(grab(&a)["status"], "imported", "{a}");

    // The disc image is in the library; the zip is untouched; our unpacked copy is gone; and the
    // torrent (with its files) was taken out of qBittorrent as asked.
    assert!(e.root.join("isos/Game One (USA).iso").is_file());
    assert!(
        content
            .join("Redump/Microsoft - Xbox 360/Game One (USA).zip")
            .is_file()
            || qb
                .lock()
                .unwrap()
                .log
                .contains(&"delete files=true".to_string())
    );
    assert!(
        !e.root.join("downloads/.rustybox-unpack-1").exists(),
        "the unpacked copy was cleaned up"
    );
    assert!(
        qb.lock()
            .unwrap()
            .log
            .contains(&"delete files=true".to_string())
    );
    assert!(qb.lock().unwrap().torrents.is_empty());
    let _ = lid;
}

fn rss_torznab(items: &[(&str, &str, u32, &str)]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0"?><rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><title>x</title>"#,
    );
    for (title, guid, seeders, link) in items {
        let link = &link.replace('&', "&amp;");
        s.push_str(&format!(r#"<item><title>{title}</title><guid>{guid}</guid><link>{link}</link><pubDate>Sat, 19 Sep 2026 12:00:00 +0000</pubDate><enclosure url="{link}" length="7000000000" type="application/x-bittorrent"/><torznab:attr name="category" value="1050"/><torznab:attr name="size" value="7000000000"/><torznab:attr name="seeders" value="{seeders}"/><torznab:attr name="peers" value="{}"/></item>"#, seeders + 2));
    }
    s + "</channel></rss>"
}

#[tokio::test]
async fn torrent_indexers_are_searched_graded_by_seeders_grabbed_and_imported_by_hardlink() {
    let (qb_url, qb) = fake_qbit().await;
    let e = env("indexer");
    let app = &e.app;
    // The pretend torznab indexer: one release with seeders, one dead, one that redirects to a magnet.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ix = format!("http://{}", listener.local_addr().unwrap());
    let good = make_torrent(
        "Halo.3.USA.XBOX360-GRP",
        &[("halo3.iso", 7_000_000_000), ("halo3.nfo", 100)],
    );
    let good2 = good.clone();
    let ix2 = ix.clone();
    let magnet = format!(
        "magnet:?xt=urn:btih:{}&dn=Halo.3.PAL.XBOX360-MAG",
        "ab".repeat(20)
    );
    let m2 = magnet.clone();
    let m3 = magnet.clone();
    let tz = Router::new()
        .route("/tz/api", get(move |Query(q): Query<HashMap<String, String>>| {
            let (ix, m) = (ix2.clone(), m2.clone());
            async move {
                if q.get("apikey").map(String::as_str) != Some("TZKEY99") {
                    return r#"<?xml version="1.0"?><error code="100" description="bad key"/>"#.to_string();
                }
                if q.get("t").map(String::as_str) == Some("caps") {
                    return r#"<caps><categories><category id="1000" name="Console"><subcat id="1050" name="Xbox 360"/></category></categories></caps>"#.into();
                }
                rss_torznab(&[
                    ("Halo.3.USA.XBOX360-GRP", "t1", 25, &format!("{ix}/tz/dl/halo3.torrent?apikey=TZKEY99")),
                    ("Halo.3.PAL.XBOX360-DEAD", "t2", 0, &format!("{ix}/tz/dl/dead.torrent")),
                    ("Halo.3.PAL.XBOX360-MAG", "t3", 8, &format!("{ix}/tz/dl/viamagnet")),
                    ("Halo.3.NTSC.XBOX360-MGO", "t4", 3, &m),
                ])
            }
        }))
        .route("/tz/dl/halo3.torrent", get(move || { let b = good2.clone(); async move { ([(header::CONTENT_TYPE, "application/x-bittorrent")], b) } }))
        .route("/tz/dl/viamagnet", get(move || { let m = m3.clone(); async move { (StatusCode::FOUND, [(header::LOCATION, m)], "").into_response() } }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, tz).await;
    });

    // With qBittorrent not set up, the torrent indexer is left out and the search says so.
    let (st, v) = call(app, "POST", "/api/grab/indexers", Some(json!({"name": "Tz", "url": format!("{ix}/tz"), "api_key": "TZKEY99", "protocol": "torrent"}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["protocol"], "torrent");
    let (st, _) = call(
        app,
        "POST",
        "/api/grab/indexers",
        Some(json!({"name": "Bad", "url": ix, "api_key": "K", "protocol": "carrier-pigeon"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (_, found) = call(app, "GET", "/api/wanted/lookup?q=Halo%203", None).await;
    let igdb_id = found["games"][0]["candidate"]["id"].as_i64().unwrap();
    let (_, w) = call(
        app,
        "POST",
        "/api/wanted",
        Some(json!({"igdb_id": igdb_id, "candidate": found["games"][0]["candidate"]})),
    )
    .await;
    let wid = w["id"].as_i64().unwrap();
    let (st, v) = call(app, "POST", &format!("/api/wanted/{wid}/search"), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "no usable indexer yet: {v}");

    let (lid, pid) = setup(&e, &qb_url, json!({})).await;
    let (st, s) = call(app, "POST", &format!("/api/wanted/{wid}/search"), None).await;
    assert_eq!(st, StatusCode::OK, "{s}");
    let results = s["results"].as_array().unwrap();
    assert_eq!(results.len(), 4, "{s}");
    let by = |t: &str| results.iter().find(|r| r["title"] == t).unwrap().clone();
    assert_eq!(by("Halo.3.USA.XBOX360-GRP")["protocol"], "torrent");
    assert_eq!(by("Halo.3.USA.XBOX360-GRP")["seeders"], 25);
    let dead = by("Halo.3.PAL.XBOX360-DEAD")["verdict"]["rejected"].to_string();
    assert!(
        dead.contains("0 seeders"),
        "a torrent nobody shares is refused: {dead}"
    );
    assert!(
        !s.to_string().contains("TZKEY99") && !s.to_string().contains("magnet:"),
        "no key or magnet link reaches the browser"
    );

    // Grab the best: the .torrent is fetched by RustyBox and handed over (not left to qBittorrent).
    let (st, v) = call(app, "POST", &format!("/api/wanted/{wid}/auto"), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["grabbed"], "Halo.3.USA.XBOX360-GRP");
    let hash = torrent::parse(&good).unwrap().info_hash;
    assert!(
        qb.lock()
            .unwrap()
            .log
            .contains(&format!("add {hash} stopped=false category=xbox360")),
        "{:?}",
        qb.lock().unwrap().log
    );
    // A link that answers with a redirect to a magnet is followed to it (by hand: it isn't a web address).
    let url = format!("{ix}/tz/dl/viamagnet");
    let f = tokio::task::spawn_blocking(move || torrent::fetch(&url))
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&f, torrent::Fetched::Magnet(m) if m == &magnet),
        "{f:?}"
    );
    // And the same release can't be grabbed while another download of the game is open.
    let (st, v) = call(
        app,
        "POST",
        &format!("/api/wanted/{wid}/grab"),
        Some(json!({"guid": "t3", "indexer_id": by("Halo.3.PAL.XBOX360-MAG")["indexer_id"]})),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "one download per game at a time: {v}"
    );

    // Done: imported by hardlink into the library on the same disk, so the torrent keeps seeding.
    let content = e.root.join("downloads/Halo.3.USA.XBOX360-GRP");
    write_iso(&content.join("halo3.iso"), 0x4D53_07E6, 0x1CDE_207A);
    set_state(&qb, &hash, "uploading", 1.0, &content.to_string_lossy());
    web::poll_downloads(&e.state).await;
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(a["grabs"][0]["grab"]["status"], "importing", "{a}");
    assert_eq!(
        wait_job(app, a["grabs"][0]["grab"]["import_job"].as_u64().unwrap()).await["job"]["status"],
        "done"
    );
    web::poll_downloads(&e.state).await;
    use std::os::unix::fs::MetadataExt;
    let (a_meta, b_meta) = (
        fs::metadata(content.join("halo3.iso")).unwrap(),
        fs::metadata(e.root.join("isos/halo3.iso")).unwrap(),
    );
    assert_eq!(
        a_meta.ino(),
        b_meta.ino(),
        "a hardlink: one file, two names"
    );
    assert!(
        qb.lock().unwrap().torrents.contains_key(&hash),
        "kept in qBittorrent by default"
    );
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(a["grabs"][0]["grab"]["status"], "imported");
    let _ = (lid, pid);
}

#[tokio::test]
async fn a_torrent_link_that_is_a_magnet_or_a_redirect_is_understood_and_keys_are_hidden() {
    let t = make_torrent("X", &[("a.iso", 5)]);
    // Fetching straight from a magnet link needs no network.
    let m = format!("magnet:?xt=urn:btih:{}", "cd".repeat(20));
    assert!(matches!(torrent::fetch(&m).unwrap(), torrent::Fetched::Magnet(x) if x == m));
    assert!(torrent::fetch("ftp://x/y.torrent").is_err());
    assert!(
        torrent::fetch("http://127.0.0.1:1/x.torrent?apikey=SECRET1234")
            .unwrap_err()
            .to_string()
            .find("SECRET1234")
            .is_none()
    );
    let _ = t;
}

#[tokio::test]
async fn a_one_file_torrent_is_imported_from_a_folder_of_its_own_and_keeps_seeding() {
    let (base, qb) = fake_qbit().await;
    let e = env("single");
    let app = &e.app;
    setup(&e, &base, json!({})).await;
    // The download folder holds this game's disc image and an unrelated one that must not be swept up.
    let dl = e.root.join("downloads");
    write_iso(&dl.join("Fable II (USA).iso"), 0x4D53_07F1, 0x6339_E4E9);
    write_iso(&dl.join("Someone Elses Game.iso"), 0x4D53_0805, 0x54E3_4DF4);
    let t = make_single(
        "Fable II (USA).iso",
        fs::metadata(dl.join("Fable II (USA).iso")).unwrap().len(),
    );
    fs::write(e.root.join("torrents/fable.torrent"), &t).unwrap();
    let (st, _) = call(
        app,
        "PUT",
        "/api/torrents/dirs",
        Some(json!({"dirs": [e.root.join("torrents").to_string_lossy()]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // No choice of files means everything: added running, no priorities touched.
    let (st, v) = call(
        app,
        "POST",
        "/api/torrents/send",
        Some(json!({"dir": 0, "file": "fable.torrent"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let hash = torrent::parse(&t).unwrap().info_hash;
    {
        let g = qb.lock().unwrap();
        let log: Vec<&String> = g.log.iter().filter(|l| !l.starts_with("login")).collect();
        assert_eq!(
            log,
            vec![&format!("add {hash} stopped=false category=xbox360")],
            "{log:?}"
        );
    }
    set_state(
        &qb,
        &hash,
        "stalledUP",
        1.0,
        &dl.join("Fable II (USA).iso").to_string_lossy(),
    );
    web::poll_downloads(&e.state).await;
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(a["grabs"][0]["grab"]["status"], "importing", "{a}");
    assert_eq!(
        wait_job(app, a["grabs"][0]["grab"]["import_job"].as_u64().unwrap()).await["job"]["status"],
        "done"
    );
    web::poll_downloads(&e.state).await;
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(a["grabs"][0]["grab"]["status"], "imported", "{a}");
    assert!(e.root.join("isos/Fable II (USA).iso").is_file());
    assert!(
        dl.join("Fable II (USA).iso").is_file(),
        "the seeded file stays"
    );
    assert!(
        !e.root.join("isos/Someone Elses Game.iso").exists(),
        "only this download was imported"
    );
    assert!(dl.join("Someone Elses Game.iso").is_file());
    assert!(
        !dl.join(".rustybox-unpack-1").exists(),
        "the staging folder is gone"
    );
    assert!(
        qb.lock().unwrap().torrents.contains_key(&hash),
        "left in qBittorrent"
    );
}

#[tokio::test]
async fn a_long_list_of_torrent_files_can_be_shown_past_five_hundred_and_the_summary_answers() {
    let e = env("many");
    let dir = e.root.join("torrents");
    for i in 0..620 {
        fs::write(
            dir.join(format!("Collection {i:04}.torrent")),
            make_single(&format!("g{i}.iso"), 10),
        )
        .unwrap();
    }
    let (st, _) = call(
        &e.app,
        "PUT",
        "/api/torrents/dirs",
        Some(json!({"dirs": [dir.to_string_lossy()]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, f) = call(&e.app, "GET", "/api/torrents/files?dir=0&limit=1000", None).await;
    assert_eq!(
        f["files"].as_array().unwrap().len(),
        620,
        "not cut off at 500"
    );
    assert_eq!(f["total"], 620);
    // A page further on.
    let (_, f) = call(
        &e.app,
        "GET",
        "/api/torrents/files?dir=0&limit=200&offset=600",
        None,
    )
    .await;
    assert_eq!(f["files"].as_array().unwrap().len(), 20);

    // qBittorrent not set up: the live view says so (and still answers).
    let (st, v) = call(&e.app, "GET", "/api/torrents/live", None).await;
    assert_eq!(
        (st, v["configured"].as_bool(), v["online"].as_bool()),
        (StatusCode::OK, Some(false), Some(false))
    );
    // The summary for dashboards.
    let (st, v) = call(&e.app, "GET", "/api/summary", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        v["games"].is_number()
            && v["libraries"].is_number()
            && v["jobs"]["running"].is_array()
            && v["latest"].is_array(),
        "{v}"
    );
    assert!(v["version"].is_string());
}

#[tokio::test]
async fn the_live_view_shows_speeds_counts_and_the_busiest_torrents() {
    let (base, qb) = fake_qbit().await;
    let e = env("live");
    setup(&e, &base, json!({})).await;
    {
        let mut g = qb.lock().unwrap();
        for (h, state, p) in [
            ("a", "downloading", 0.5),
            ("b", "stalledUP", 1.0),
            ("c", "error", 0.2),
            ("d", "pausedDL", 0.1),
        ] {
            g.torrents.insert(
                h.repeat(40),
                Tor {
                    name: format!("T {h}"),
                    state: state.into(),
                    progress: p,
                    ..Default::default()
                },
            );
        }
    }
    let (st, v) = call(&e.app, "GET", "/api/torrents/live", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["online"], true, "{v}");
    assert_eq!(
        (
            v["counts"]["total"].as_u64(),
            v["counts"]["downloading"].as_u64(),
            v["counts"]["seeding"].as_u64(),
            v["counts"]["errored"].as_u64(),
            v["counts"]["paused"].as_u64()
        ),
        (Some(4), Some(1), Some(1), Some(1), Some(1))
    );
    assert_eq!(v["torrents"].as_array().unwrap().len(), 4);
    assert_eq!(
        (v["down"].as_u64(), v["up"].as_u64(), v["free"].as_u64()),
        (Some(2_000_000), Some(500_000), Some(123_456_789))
    );
    assert!(!v.to_string().contains("adminpass"));
}

#[tokio::test]
async fn a_wanted_game_is_searched_for_inside_the_torrent_files_and_one_file_is_grabbed() {
    let (base, qb) = fake_qbit().await;
    let e = env("filesearch");
    let app = &e.app;
    setup(&e, &base, json!({})).await;
    let t = make_torrent(
        "Redump",
        &[
            (
                "Microsoft - Xbox 360/Halo 3 (USA) (En,Ja,Fr,De,Es,It,Pt,Zh,Ko).zip",
                7_000_000_000,
            ),
            (
                "Microsoft - Xbox 360/Halo 3 - ODST (USA) (En,Fr,Es).zip",
                7_000_000_000,
            ),
            (
                "Microsoft - Xbox 360/Halo 3 (Japan) (Ja).zip",
                7_000_000_000,
            ),
            (
                "Microsoft - Xbox 360/Halo Wars (USA) (En,Fr).zip",
                7_000_000_000,
            ),
            (
                "Microsoft - Xbox 360/Fable II (USA) (En).zip",
                7_000_000_000,
            ),
        ],
    );
    fs::write(
        e.root
            .join("torrents/Redump - Microsoft - Xbox 360.torrent"),
        &t,
    )
    .unwrap();
    // Another torrent that is not Xbox 360 at all.
    fs::write(
        e.root.join("torrents/Sega Dreamcast.torrent"),
        make_torrent("Dreamcast", &[("Halo 3 (USA).zip", 1000)]),
    )
    .unwrap();
    let (st, _) = call(
        app,
        "PUT",
        "/api/torrents/dirs",
        Some(json!({"dirs": [e.root.join("torrents").to_string_lossy()]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, found) = call(app, "GET", "/api/wanted/lookup?q=Halo%203", None).await;
    let cand = found["games"][0]["candidate"].clone();
    let (st, w) = call(
        app,
        "POST",
        "/api/wanted",
        Some(json!({"igdb_id": cand["id"], "candidate": cand})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{w}");
    let wid = w["id"].as_i64().unwrap();
    let name = cand["name"].as_str().unwrap().to_string();

    // With no filter only the Xbox 360 torrent is read (the Dreamcast one is not).
    let (st, r) = call(
        app,
        "POST",
        &format!("/api/wanted/{wid}/search-files"),
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["torrents"], 1, "{r}");
    let res = r["results"].as_array().unwrap();
    let title_of = |v: &Value| v["title"].as_str().unwrap().to_string();
    let best = &res[0];
    assert!(
        title_of(best).starts_with("Halo 3 (USA)"),
        "{name}: the right game comes first: {r}"
    );
    assert_eq!(
        best["verdict"]["rejected"].as_array().unwrap().len(),
        0,
        "{r}"
    );
    assert!(res.iter().all(|x| !title_of(x).contains("Fable")), "{r}");
    assert!(
        res.iter().all(|x| !title_of(x).contains("Halo Wars")),
        "{r}"
    );
    let odst = res.iter().find(|x| title_of(x).contains("ODST")).unwrap();
    assert!(
        !odst["verdict"]["rejected"].as_array().unwrap().is_empty(),
        "a different game is rejected: {r}"
    );

    // A filter picks the torrent file by its name, even when it isn't an Xbox one.
    let (_, r2) = call(
        app,
        "POST",
        &format!("/api/wanted/{wid}/search-files"),
        Some(json!({"dir": 0, "filter": "dreamcast"})),
    )
    .await;
    assert_eq!(r2["torrents"], 1);
    let (st, _) = call(
        app,
        "POST",
        &format!("/api/wanted/{wid}/search-files"),
        Some(json!({"dir": 5})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // A rejected file is refused unless forced; the right one is sent alone.
    let grab = |index: &Value, force: bool| json!({"dir": 0, "torrent_file": "Redump - Microsoft - Xbox 360.torrent", "index": index, "force": force});
    let (st, v) = call(
        app,
        "POST",
        &format!("/api/wanted/{wid}/grab-file"),
        Some(grab(&odst["index"], false)),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert!(qb.lock().unwrap().torrents.is_empty());
    let (st, v) = call(
        app,
        "POST",
        &format!("/api/wanted/{wid}/grab-file"),
        Some(grab(&best["index"], false)),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let hash = torrent::parse(&t).unwrap().info_hash;
    {
        let g = qb.lock().unwrap();
        let on: Vec<usize> = g.torrents[&hash]
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| f.2 > 0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(on, vec![best["index"].as_u64().unwrap() as usize]);
    }
    // The game is downloading now: a second grab is refused.
    let (st, v) = call(
        app,
        "POST",
        &format!("/api/wanted/{wid}/grab-file"),
        Some(grab(&best["index"], false)),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
}
