use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use rustybox::{
    web::{self, Config},
    xbox::stfs,
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Env {
    app: Router,
    root: PathBuf,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("rustybox_import_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    for d in ["inbox", "isos", "god", "extra"] {
        fs::create_dir_all(base.join("data").join(d)).unwrap();
    }
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

fn p(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn write_god(dir: &Path, title: u32, name: &str, parts: u32, data: &[usize]) {
    let ct = dir.join(format!("{title:08X}")).join("00007000");
    fs::create_dir_all(ct.join("ABCDEF01.data")).unwrap();
    let mut c = stfs::build(b"LIVE", 0x7000, title, 1, name, name);
    c[0x3A0..0x3A4].copy_from_slice(&parts.to_le_bytes());
    fs::write(ct.join("ABCDEF01"), c).unwrap();
    for (i, n) in data.iter().enumerate() {
        fs::write(ct.join(format!("ABCDEF01.data/Data{i:04}")), vec![9u8; *n]).unwrap();
    }
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

async fn settle(app: &Router) {
    for _ in 0..600 {
        let (_, jobs) = call(app, "GET", "/api/jobs", None).await;
        if jobs.as_array().unwrap().iter().all(|j| {
            matches!(
                j["status"].as_str().unwrap(),
                "done" | "failed" | "cancelled"
            )
        }) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    panic!("jobs did not settle");
}

/// (library id, path id)
async fn lib(e: &Env, name: &str, kind: &str, dir: &str, writable: bool) -> (i64, i64) {
    let (s, v) = call(&e.app, "POST", "/api/libraries", Some(json!({"name": name, "kind": kind, "paths": [{"path": p(&e.root.join(dir)), "label": dir, "writable": writable}]}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["id"].as_i64().unwrap();
    let (_, l) = call(&e.app, "GET", &format!("/api/libraries/{id}"), None).await;
    (id, l["paths"][0]["id"].as_i64().unwrap())
}

fn cons_url(id: u32, tail: &str) -> String {
    format!("/api/consoles/{id}{tail}")
}

async fn make_console(e: &Env, port: u16) -> u32 {
    let (s, v) = call(&e.app, "POST", "/api/consoles", Some(json!({"name": "Living room", "host": "127.0.0.1", "port": port, "user": "xbox", "password": "xbox"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v.get("password").is_none(),
        "the password never goes to the browser: {v}"
    );
    assert_eq!(v["password_set"], true);
    v["id"].as_u64().unwrap() as u32
}

#[tokio::test]
async fn a_console_can_be_browsed_scanned_compared_and_sent_games() {
    let e = env("console");
    let srv_root = e.root.parent().unwrap().join("fakeconsole");
    let _ = fs::remove_dir_all(&srv_root);
    rustybox::console::seed_mock(&srv_root).unwrap();
    let srv = rustybox::console::fake::start(&srv_root, "xbox", "xbox").unwrap();
    let id = make_console(&e, srv.addr.port()).await;

    // Test the connection: the drives are listed. A wrong password is explained.
    let (s, v) = call(
        &e.app,
        "POST",
        "/api/consoles/test",
        Some(json!({"id": id, "name": "x", "host": "127.0.0.1", "port": srv.addr.port()})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v["drives"].as_array().unwrap().iter().any(|d| d == "Hdd1"),
        "{v}"
    );
    let (s, v) = call(&e.app, "POST", "/api/consoles/test", Some(json!({"name": "x", "host": "127.0.0.1", "port": srv.addr.port(), "user": "xbox", "password": "wrong"}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(v["error"], "CONSOLE_LOGIN");
    // Nothing listening: a clear message rather than a hang.
    let (s, v) = call(&e.app, "POST", "/api/consoles/test", Some(json!({"name": "x", "host": "127.0.0.1", "port": 1, "user": "xbox", "password": "xbox"}))).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(v["error"], "CONSOLE_UNREACHABLE");

    // Browse, make a folder, rename it, and delete only after a confirmation preview.
    let (_, v) = call(&e.app, "GET", &cons_url(id, "/ls?path=Hdd1:%5CGames"), None).await;
    assert_eq!(v["path"], "/Hdd1/Games");
    assert_eq!(v["entries"].as_array().unwrap().len(), 2, "{v}");
    let (s, _) = call(
        &e.app,
        "POST",
        &cons_url(id, "/mkdir"),
        Some(json!({"path": "Hdd1/Games/Temp/Deep"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &e.app,
        "POST",
        &cons_url(id, "/rename"),
        Some(json!({"from": "Hdd1/Games/Temp", "to": "Hdd1/Games/Temp2"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(srv_root.join("Hdd1/Games/Temp2/Deep").is_dir());
    let (s, _) = call(
        &e.app,
        "POST",
        &cons_url(id, "/mkdir"),
        Some(json!({"path": "Hdd1"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "never the top of a drive");
    let (_, pre) = call(
        &e.app,
        "POST",
        &cons_url(id, "/delete"),
        Some(json!({"path": "Hdd1/Games/Temp2"})),
    )
    .await;
    assert_eq!(pre["is_dir"], true);
    assert!(
        srv_root.join("Hdd1/Games/Temp2").exists(),
        "a preview deletes nothing"
    );
    let (_, v) = call(
        &e.app,
        "POST",
        &cons_url(id, "/delete"),
        Some(json!({"path": "Hdd1/Games/Temp2", "confirm": true})),
    )
    .await;
    wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert!(!srv_root.join("Hdd1/Games/Temp2").exists());

    // Scan, then compare with a library that has Alan Wake, Fable II and an ISO game.
    let (_, v) = call(&e.app, "POST", &cons_url(id, "/scan"), None).await;
    wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    let (_, g) = call(&e.app, "GET", &cons_url(id, "/games"), None).await;
    assert_eq!(g["games"].as_array().unwrap().len(), 2, "{g}");
    write_god(
        &e.root.join("god/Alan Wake"),
        0x4D53_0805,
        "Alan Wake",
        2,
        &[100, 100],
    );
    write_god(
        &e.root.join("god/Fable II"),
        0x4D53_07F1,
        "Fable II",
        2,
        &[4000, 4000],
    );
    let god = lib(&e, "GODs", "god", "god", true).await;
    call(
        &e.app,
        "POST",
        &format!("/api/libraries/{}/scan", god.0),
        None,
    )
    .await;
    settle(&e.app).await;
    let (_, c) = call(
        &e.app,
        "GET",
        &cons_url(id, &format!("/compare?library={}", god.0)),
        None,
    )
    .await;
    assert_eq!(c["both"].as_array().unwrap().len(), 1, "{c}");
    assert_eq!(c["only_library"].as_array().unwrap().len(), 1, "{c}");
    assert_eq!(
        c["only_console"].as_array().unwrap().len(),
        1,
        "Gears is only on the console: {c}"
    );
    let fable = c["only_library"][0].clone();

    // Plan, then send Fable II.
    let body = json!({"items": [{"library_id": god.0, "item_id": fable["item_id"]}], "layout": "name_titleid"});
    let (s, plan) = call(
        &e.app,
        "POST",
        &cons_url(id, "/send/plan"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(plan["ok"], true, "{plan}");
    assert_eq!(
        plan["entries"][0]["output"], "/Hdd1/Games/Fable II/4D5307F1",
        "{plan}"
    );
    assert!(
        !srv_root.join("Hdd1/Games/Fable II").exists(),
        "planning changes nothing"
    );
    let (s, v) = call(&e.app, "POST", &cons_url(id, "/send/start"), Some(body)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let on = srv_root.join("Hdd1/Games/Fable II/4D5307F1/00007000/ABCDEF01.data/Data0000");
    assert_eq!(fs::metadata(on).unwrap().len(), 4000);

    // The old Alan Wake on the console is under another name and has a save beside it; replace it.
    let old = srv_root.join("Hdd1/Games/Alan Wake/4D530805");
    fs::create_dir_all(old.join("00000001")).unwrap();
    fs::write(old.join("00000001/save.bin"), b"progress").unwrap();
    let (_, items) = call(
        &e.app,
        "GET",
        &format!("/api/libraries/{}/items?q=Alan", god.0),
        None,
    )
    .await;
    let aw = items["items"][0]["id"].clone();
    let rbody = json!({"items": [{"library_id": god.0, "item_id": aw}], "replace": true, "layout": "titleid"});
    let (_, plan) = call(
        &e.app,
        "POST",
        &cons_url(id, "/send/plan"),
        Some(rbody.clone()),
    )
    .await;
    let steps = plan["entries"][0]["steps"].to_string();
    assert!(
        steps.contains("Remove the old copy") && steps.contains("/Hdd1/Games/Alan Wake/4D530805"),
        "{steps}"
    );
    let (_, v) = call(&e.app, "POST", &cons_url(id, "/send/start"), Some(rbody)).await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    assert_eq!(
        fs::read(old.join("00000001/save.bin")).unwrap(),
        b"progress",
        "saves are never touched"
    );
    assert!(
        srv_root.join("Hdd1/Games/4D530805/00007000").is_dir(),
        "the new copy is in the layout asked for"
    );
    assert!(
        !old.join("00007000").exists(),
        "the old game files are gone"
    );

    // A USB stick can't take a file over 4 GB: refused in the plan, not halfway through.
    let (_, v) = call(&e.app, "PUT", &cons_url(id, ""), Some(json!({"name": "Living room", "host": "127.0.0.1", "port": srv.addr.port(), "game_paths": ["Hdd1/Games", "Usb0/Games"], "user": "xbox", "password": ""}))).await;
    assert_eq!(v["game_paths"].as_array().unwrap().len(), 2, "{v}");
    let (s, _) = call(
        &e.app,
        "POST",
        &cons_url(id, "/send/plan"),
        Some(json!({"items": [{"library_id": god.0, "item_id": aw}], "dest": "Usb0/Games"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = call(
        &e.app,
        "POST",
        &cons_url(id, "/send/plan"),
        Some(json!({"items": [{"library_id": god.0, "item_id": aw}], "dest": "Hdd1/Elsewhere"})),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "only the console's own games folders: {v}"
    );

    // Fetch a console folder into a library folder here.
    let (_, v) = call(&e.app, "POST", &cons_url(id, "/fetch"), Some(json!({"path": "Hdd1/Games/Gears of War", "library_id": god.0, "path_id": god.1, "rel": "From console/Gears of War"}))).await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    assert!(
        e.root
            .join("god/From console/Gears of War/4D5307D5/00007000/A1B2C3D4E5.data/Data0000")
            .is_file()
    );

    // Removing the console forgets it, and its password file entry with it.
    let (s, _) = call(&e.app, "DELETE", &cons_url(id, ""), None).await;
    assert_eq!(s, StatusCode::OK);
    let (_, l) = call(&e.app, "GET", "/api/consoles", None).await;
    assert_eq!(l.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn games_in_a_folder_such_as_a_usb_stick_can_be_sent_straight_to_the_console() {
    let e = env("consolefolder");
    let srv_root = e.root.parent().unwrap().join("fakeconsole2");
    let _ = fs::remove_dir_all(&srv_root);
    rustybox::console::seed_mock(&srv_root).unwrap();
    let srv = rustybox::console::fake::start(&srv_root, "xbox", "xbox").unwrap();
    let id = make_console(&e, srv.addr.port()).await;
    // A "stick" with a GOD game on it, mounted inside an allowed folder.
    write_god(
        &e.root.join("inbox/Stick/Alan Wake"),
        0x4D53_0805,
        "Alan Wake",
        2,
        &[2000, 2000],
    );
    fs::write(e.root.join("inbox/Stick/readme.txt"), b"hi").unwrap();
    let folder = p(&e.root.join("inbox/Stick"));
    let (st, scan) = call(
        &e.app,
        "POST",
        "/api/import/scan",
        Some(json!({"source": {"kind": "folder", "path": folder}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{scan}");
    let cand = scan["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["title_id"] == "4D530805")
        .unwrap();
    let body =
        json!({"folder": {"path": folder, "select": [cand["id"]]}, "layout": "name_titleid"});
    let (st, plan) = call(
        &e.app,
        "POST",
        &cons_url(id, "/send/plan"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{plan}");
    assert_eq!(plan["ok"], true, "{plan}");
    assert_eq!(
        plan["entries"][0]["output"], "/Hdd1/Games/Alan Wake/4D530805",
        "{plan}"
    );
    // Something that isn't in the folder is refused rather than guessed at.
    let (st, _) = call(
        &e.app,
        "POST",
        &cons_url(id, "/send/plan"),
        Some(json!({"folder": {"path": folder, "select": ["Nope"]}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (_, v) = call(&e.app, "POST", &cons_url(id, "/send/start"), Some(body)).await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let data = srv_root.join("Hdd1/Games/Alan Wake/4D530805/00007000/ABCDEF01.data/Data0000");
    assert_eq!(fs::metadata(data).unwrap().len(), 2000);
}
