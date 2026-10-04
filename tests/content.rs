use std::{fs, io::Write, net::SocketAddr, path::PathBuf};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
    routing::get,
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

/// A stand-in for db.arisen.studio. Its JSON has a byte-order mark, like the real one.
async fn fake_arisen() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let b = base.clone();
    let json_route = |body: String| {
        get(move || {
            let mut v = vec![0xEF, 0xBB, 0xBF];
            v.extend(body.clone().into_bytes());
            async move { v }
        })
    };
    let trainer_zip = zip_of(&[
        ("Trainer(RETROBYTE).xex", b"trainer-bytes"),
        ("readme.txt", b"hi"),
    ]);
    let patches = zip_of(&[(
        "4D530805 - Alan Wake.patch.toml",
        b"title_id = \"4D530805\"",
    )]);
    let app = Router::new()
        .route("/data/categories.json", json_route(r#"{"Categories":[{"Title":"Alan Wake","Id":"aw","Type":"game","Regions":["4D530805"]}]}"#.into()))
        .route("/data/xbox360/game-mods.json", json_route(format!(r#"{{"Library":[{{"Platform":"XBOX","CategoryId":"aw","Id":7,"Name":"Flashlight Mod","CreatedBy":"x","Version":"1","DownloadFiles":[{{"Name":"Flashlight","Url":"{b}/files/mod.zip","InstallPaths":["Hdd:\\Arisen Studio\\{{CATEGORYID}}\\{{NAME}}\\"]}}]}}]}}"#)))
        .route("/data/xbox360/homebrew.json", json_route(r#"{"Library":[]}"#.into()))
        .route("/data/xbox360/trainers.json", json_route(format!(r#"{{"Library":[{{"TitleId":"4D530805","Trainers":[{{"Name":"Trainer(RETROBYTE)","Url":"{b}/files/trainer.zip","InstallPaths":["{{AURORAPATH}}\\User\\Trainers\\4D530805\\Trainer(RETROBYTE)\\Trainer(RETROBYTE).xex"]}}]}},{{"TitleId":"41560817","Trainers":[{{"Name":"Evil","Url":"https://evil.example/x.zip","InstallPaths":["Hdd:\\x\\"]}}]}}]}}"#)))
        .route("/data/xbox360/game-cheats.json", json_route(r#"{"GameCheats":[{"Region":"4D530805","Game":"Alan Wake","Version":"1","Cheats":[{"Name":"God mode","Offsets":[]}]}]}"#.into()))
        .route("/data/game-saves.json", json_route(r#"{"GameSaves":[{"Platform":"PS3","Id":1,"CategoryId":"x","Name":"ps3 save","DownloadFiles":[]},{"Platform":"XBOX","Id":2,"CategoryId":"aw","Name":"Xbox save","Region":"4D530805","DownloadFiles":[]}]}"#.into()))
        .route("/data/xbox360/game-patches.zip", get(move || { let p = patches.clone(); async move { p } }))
        .route("/files/mod.zip", get(move || { let z = zip_of(&[("Flash.xex", b"flash"), ("../escape.txt", b"no")]); async move { z } }))
        .route("/files/trainer.zip", get(move || { let z = trainer_zip.clone(); async move { z } }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    base
}

#[tokio::test]
async fn the_database_is_fetched_searched_and_installed_on_the_console() {
    let base = fake_arisen().await;
    // SAFETY: this is the only test in this binary, so nothing else reads the environment meanwhile.
    unsafe { std::env::set_var("RUSTYBOX_ARISEN_BASE", &base) };
    let e = env("db");
    let srv_root = e.root.parent().unwrap().join("fakeconsole");
    let _ = fs::remove_dir_all(&srv_root);
    rustybox::console::seed_mock(&srv_root).unwrap();
    let srv = rustybox::console::fake::start(&srv_root, "xbox", "xbox").unwrap();
    let (_, c) = call(
        &e.app,
        "POST",
        "/api/consoles",
        Some(json!({"name": "Xbox", "host": "127.0.0.1", "port": srv.addr.port()})),
    )
    .await;
    let cid = c["id"].as_u64().unwrap();

    // Nothing yet, then fetched.
    let (_, s) = call(&e.app, "GET", "/api/content/status", None).await;
    assert_eq!(s["fetched"], Value::Null);
    let (st, v) = call(&e.app, "POST", "/api/content/refresh", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let (_, s) = call(&e.app, "GET", "/api/content/status", None).await;
    assert_eq!(s["counts"]["mods"], 1);
    assert_eq!(s["counts"]["trainers"], 2);
    assert_eq!(s["counts"]["saves"], 1, "PS3 saves are left out: {s}");
    assert_eq!(s["counts"]["patches"], 1);
    assert_eq!(s["counts"]["cheats"], 1);

    // Search by name, game and title ID; the mod got its game from the category.
    let (_, l) = call(&e.app, "GET", "/api/content?kind=mods&q=alan", None).await;
    assert_eq!(l["total"], 1, "{l}");
    assert_eq!(l["items"][0]["title_id"], "4D530805");
    assert_eq!(l["items"][0]["game"], "Alan Wake");
    let (_, l) = call(&e.app, "GET", "/api/content?kind=trainers&q=41560817", None).await;
    assert_eq!(l["total"], 1);
    // "Only games I own": no library has any games yet.
    let (_, l) = call(&e.app, "GET", "/api/content?kind=trainers&owned=true", None).await;
    assert_eq!(l["total"], 0);
    let (st, _) = call(&e.app, "GET", "/api/content?kind=bogus", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Install a trainer: the plan says where it goes; nothing is sent until Start.
    let (_, l) = call(
        &e.app,
        "GET",
        "/api/content?kind=trainers&q=retrobyte",
        None,
    )
    .await;
    let id = l["items"][0]["id"].as_str().unwrap().to_string();
    let body = json!({"console_id": cid, "id": id});
    let (st, p) = call(
        &e.app,
        "POST",
        "/api/content/install/plan",
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["plan"]["ok"], true);
    assert_eq!(
        p["plan"]["files"][0]["targets"][0],
        "/Hdd1/Aurora/User/Trainers/4D530805/Trainer(RETROBYTE)/Trainer(RETROBYTE).xex"
    );
    assert!(!srv_root.join("Hdd1/Aurora/User/Trainers/4D530805").exists());
    let (_, v) = call(&e.app, "POST", "/api/content/install/start", Some(body)).await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let installed = srv_root
        .join("Hdd1/Aurora/User/Trainers/4D530805/Trainer(RETROBYTE)/Trainer(RETROBYTE).xex");
    assert_eq!(fs::read(installed).unwrap(), b"trainer-bytes");

    // A mod with placeholders in its path; a zip entry trying to escape is dropped.
    let (_, l) = call(&e.app, "GET", "/api/content?kind=mods", None).await;
    let body = json!({"console_id": cid, "id": l["items"][0]["id"]});
    let (_, v) = call(&e.app, "POST", "/api/content/install/start", Some(body)).await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    assert_eq!(
        fs::read(srv_root.join("Hdd1/Arisen Studio/aw/Flashlight Mod/Flash.xex")).unwrap(),
        b"flash"
    );
    assert!(
        !srv_root.join("Hdd1/Arisen Studio/aw/escape.txt").exists()
            && !srv_root.join("Hdd1/Arisen Studio/escape.txt").exists()
    );

    // The database can't send RustyBox to download from anywhere else.
    let (_, l) = call(&e.app, "GET", "/api/content?kind=trainers&q=evil", None).await;
    let (_, v) = call(
        &e.app,
        "POST",
        "/api/content/install/start",
        Some(json!({"console_id": cid, "id": l["items"][0]["id"]})),
    )
    .await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "failed", "{j}");
    assert!(
        j["job"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Refusing to download"),
        "{j}"
    );
    // Cheats are data: nothing to install.
    let (_, l) = call(&e.app, "GET", "/api/content?kind=cheats", None).await;
    assert_eq!(l["items"][0]["cheats"][0], "God mode");
    let (st, _) = call(
        &e.app,
        "POST",
        "/api/content/install/plan",
        Some(json!({"console_id": cid, "id": l["items"][0]["id"]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Keep a download in a library, then install from the library.
    let (_, lb) = call(&e.app, "POST", "/api/libraries", Some(json!({"name": "Trainers", "kind": "trainers", "paths": [{"path": e.root.join("trainers").to_string_lossy(), "label": "t", "writable": true}]}))).await;
    let lid = lb["id"].as_i64().unwrap();
    let (_, lib) = call(&e.app, "GET", &format!("/api/libraries/{lid}"), None).await;
    let pid = lib["paths"][0]["id"].as_i64().unwrap();
    let (_, l) = call(
        &e.app,
        "GET",
        "/api/content?kind=trainers&q=retrobyte",
        None,
    )
    .await;
    let (_, v) = call(
        &e.app,
        "POST",
        "/api/content/download",
        Some(json!({"id": l["items"][0]["id"], "library_id": lid, "path_id": pid})),
    )
    .await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    assert!(
        e.root
            .join("trainers/4D530805/Trainer(RETROBYTE)/trainer.zip")
            .is_file()
    );
    let (_, items) = call(&e.app, "GET", &format!("/api/libraries/{lid}/items"), None).await;
    let it = items["items"][0].clone();
    // Local file: the plan suggests the Aurora trainers folder from its place in the library.
    let local = json!({"console_id": cid, "local": {"library_id": lid, "item_id": it["id"]}});
    let (st, p) = call(&e.app, "POST", "/api/content/install/plan", Some(local)).await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert!(
        p["dest"]
            .as_str()
            .unwrap()
            .contains("User/Trainers/4D530805"),
        "{p}"
    );
    // And actually install from the library: the zip is unpacked into the suggested folder.
    let (_, v) = call(
        &e.app,
        "POST",
        "/api/content/install/start",
        Some(json!({"console_id": cid, "local": {"library_id": lid, "item_id": it["id"]}})),
    )
    .await;
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let dir = srv_root
        .join("Hdd1/Aurora/User/Trainers/4D530805")
        .join(p["dest"].as_str().unwrap().rsplit('/').next().unwrap());
    assert_eq!(
        fs::read(dir.join("Trainer(RETROBYTE).xex")).unwrap(),
        b"trainer-bytes"
    );
    assert!(dir.join("readme.txt").is_file());
}
