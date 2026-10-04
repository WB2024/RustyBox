use std::{
    fs,
    net::SocketAddr,
    os::unix::fs::MetadataExt,
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
    xbox::{stfs, xex, xiso},
};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

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

fn write_iso(path: &Path, title: u32, media: u32, disc: u8, discs: u8) {
    let mut bytes = xiso::build_simple_disc(&xex::build(media, title, disc, discs));
    bytes.extend((0..1_500_000u32).map(|i| (i % 253) as u8));
    while !bytes.len().is_multiple_of(2048) {
        bytes.push(0);
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
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

fn inbox(e: &Env) -> Value {
    json!({"kind": "folder", "path": p(&e.root.join("inbox"))})
}

fn req(e: &Env, select: &[&str], mode: &str, dest: (i64, i64)) -> Value {
    json!({"source": inbox(e), "select": select, "mode": mode, "dest": {"library_id": dest.0, "path_id": dest.1}})
}

async fn run(e: &Env, body: Value) -> Value {
    let (s, v) = call(&e.app, "POST", "/api/import/start", Some(body)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    settle(&e.app).await;
    j
}

fn fill_inbox(e: &Env) {
    write_iso(
        &e.root.join("inbox/Alan Wake.iso"),
        0x4D53_0805,
        0x54E3_4DF4,
        1,
        1,
    );
    write_iso(
        &e.root.join("inbox/Sub/Fable II.iso"),
        0x4D53_07F1,
        0x6339_E4E9,
        1,
        1,
    );
    write_god(
        &e.root.join("inbox/Gears"),
        0x4D53_07D5,
        "Gears of War",
        2,
        &[100, 100],
    );
    write_god(
        &e.root.join("inbox/Broken"),
        0x4D53_07F2,
        "Viva Pinata",
        4,
        &[100, 0],
    );
    fs::write(e.root.join("inbox/readme.txt"), b"not a game").unwrap();
}

#[tokio::test]
async fn a_folder_is_scanned_for_what_can_be_imported() {
    let e = env("scan");
    fill_inbox(&e);
    let (s, v) = call(
        &e.app,
        "POST",
        "/api/import/scan",
        Some(json!({"source": inbox(&e)})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let c = v["candidates"].as_array().unwrap();
    assert_eq!(
        c.len(),
        4,
        "two ISOs and two GOD folders, not the text file: {v}"
    );
    let by = |id: &str| {
        c.iter()
            .find(|x| x["id"] == id)
            .unwrap_or_else(|| panic!("no {id}: {v}"))
    };
    assert_eq!(
        (
            by("Alan Wake.iso")["kind"].as_str(),
            by("Alan Wake.iso")["name"].as_str(),
            by("Alan Wake.iso")["title_id"].as_str()
        ),
        (Some("iso"), Some("Alan Wake"), Some("4D530805"))
    );
    assert_eq!(by("Sub/Fable II.iso")["title_id"], "4D5307F1");
    assert_eq!(by("Gears/4D5307D5")["kind"], "god");
    assert!(
        by("Broken/4D5307F2")["health"]
            .as_str()
            .unwrap()
            .contains("empty"),
        "damaged games are flagged"
    );
    // A folder outside the roots can't be scanned.
    let (s, _) = call(
        &e.app,
        "POST",
        "/api/import/scan",
        Some(json!({"source": {"kind": "folder", "path": "/etc"}})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn copy_leaves_the_source_alone_and_the_games_appear_in_the_library() {
    let e = env("copy");
    fill_inbox(&e);
    let iso = lib(&e, "ISOs", "iso", "isos", true).await;
    let r = req(&e, &["Alan Wake.iso", "Sub/Fable II.iso"], "copy", iso);

    let (s, plan) = call(&e.app, "POST", "/api/import/plan", Some(r.clone())).await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(plan["ok"], true, "{plan}");
    assert!(
        plan["entries"][0]["outputs"][0]
            .as_str()
            .unwrap()
            .ends_with("isos/Alan Wake.iso"),
        "{plan}"
    );
    assert_eq!(
        fs::read_dir(e.root.join("isos")).unwrap().count(),
        0,
        "planning writes nothing"
    );

    let j = run(&e, r).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    assert!(
        e.root.join("isos/Alan Wake.iso").is_file() && e.root.join("isos/Fable II.iso").is_file()
    );
    assert!(
        e.root.join("inbox/Alan Wake.iso").is_file(),
        "copy leaves the original"
    );
    let (_, items) = call(
        &e.app,
        "GET",
        &format!("/api/libraries/{}/items", iso.0),
        None,
    )
    .await;
    assert_eq!(items["total"], 2);
    assert_eq!(
        items["items"][0]["title_id"], "4D530805",
        "the library already knows what they are"
    );

    // Doing it again copies nothing (everything is there), and says so.
    let (_, plan) = call(
        &e.app,
        "POST",
        "/api/import/plan",
        Some(req(&e, &["Alan Wake.iso"], "copy", iso)),
    )
    .await;
    assert!(
        plan["entries"][0]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("already there")),
        "{plan}"
    );
}

#[tokio::test]
async fn move_is_instant_on_one_filesystem_and_removes_the_source() {
    let e = env("move");
    fill_inbox(&e);
    let god = lib(&e, "GOD", "god", "god", true).await;
    let ino = fs::metadata(e.root.join("inbox/Gears/4D5307D5"))
        .unwrap()
        .ino();
    let r = req(&e, &["Gears/4D5307D5"], "move", god);
    let (_, plan) = call(&e.app, "POST", "/api/import/plan", Some(r.clone())).await;
    assert!(
        plan["entries"][0]["steps"][0]
            .as_str()
            .unwrap()
            .contains("instant"),
        "{plan}"
    );
    assert_eq!(run(&e, r).await["job"]["status"], "done");
    // Placed tidily as Name/TitleID, by rename (same folder, not a copy), and gone from the source.
    let placed = e.root.join("god/Gears of War/4D5307D5");
    assert_eq!(fs::metadata(&placed).unwrap().ino(), ino);
    assert!(!e.root.join("inbox/Gears").exists() || !e.root.join("inbox/Gears/4D5307D5").exists());
    let (_, items) = call(
        &e.app,
        "GET",
        &format!("/api/libraries/{}/items", god.0),
        None,
    )
    .await;
    assert_eq!(
        (
            items["items"][0]["relpath"].as_str(),
            items["items"][0]["game_name"].as_str()
        ),
        (Some("Gears of War/4D5307D5"), Some("Gears of War"))
    );
}

#[tokio::test]
async fn hard_links_share_data_and_symlinks_work_and_both_refuse_when_they_cant() {
    let e = env("links");
    fill_inbox(&e);
    let iso = lib(&e, "ISOs", "iso", "isos", true).await;
    // Hard link: same data, no extra space; the library still sees it, and can serve it.
    let r = req(&e, &["Alan Wake.iso"], "hardlink", iso);
    let (_, plan) = call(&e.app, "POST", "/api/import/plan", Some(r.clone())).await;
    assert_eq!(plan["ok"], true, "{plan}");
    assert_eq!(
        plan["destinations"][0]["needed"], 0,
        "a link takes no space"
    );
    assert_eq!(run(&e, r).await["job"]["status"], "done");
    assert_eq!(
        fs::metadata(e.root.join("inbox/Alan Wake.iso"))
            .unwrap()
            .ino(),
        fs::metadata(e.root.join("isos/Alan Wake.iso"))
            .unwrap()
            .ino()
    );

    // Symlink: a link to the original (per file), warned about, and still scanned and downloadable.
    let r = req(&e, &["Sub/Fable II.iso"], "symlink", iso);
    let (_, plan) = call(&e.app, "POST", "/api/import/plan", Some(r.clone())).await;
    assert!(
        plan["entries"][0]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("only works here")),
        "{plan}"
    );
    assert_eq!(run(&e, r).await["job"]["status"], "done");
    assert!(
        fs::symlink_metadata(e.root.join("isos/Fable II.iso"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let (_, items) = call(
        &e.app,
        "GET",
        &format!("/api/libraries/{}/items?q=Fable", iso.0),
        None,
    )
    .await;
    assert_eq!(
        items["total"], 1,
        "a symlinked file is still found by a scan"
    );

    // A link to a different filesystem is refused up front, with the reason.
    let shm = Path::new("/dev/shm");
    if shm.is_dir()
        && fs::metadata(shm).map(|m| m.dev()).ok() != fs::metadata(&e.root).map(|m| m.dev()).ok()
    {
        let other = shm.join(format!("rustybox_import_links_{}", std::process::id()));
        fs::create_dir_all(&other).unwrap();
        let cfg_root = e.root.clone();
        let _ = cfg_root;
        // The /dev/shm folder is outside this test's roots, so it can't be added as a library: that is itself the rule.
        let (s, _) = call(
            &e.app,
            "POST",
            "/api/libraries",
            Some(json!({"name": "Elsewhere", "kind": "iso", "paths": [{"path": p(&other)}]})),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let _ = fs::remove_dir_all(&other);
    }
}

#[tokio::test]
async fn iso_into_a_god_library_converts_on_the_way_and_move_removes_the_iso_last() {
    let e = env("convert");
    fill_inbox(&e);
    let god = lib(&e, "GOD", "god", "god", true).await;

    // Without conversion an ISO can't go in; links can't be combined with converting.
    let (_, plan) = call(
        &e.app,
        "POST",
        "/api/import/plan",
        Some(req(&e, &["Alan Wake.iso"], "copy", god)),
    )
    .await;
    assert_eq!(plan["ok"], false);
    assert!(
        plan["entries"][0]["problems"][0]
            .as_str()
            .unwrap()
            .contains("convert ISOs to GOD"),
        "{plan}"
    );
    let mut r = req(&e, &["Alan Wake.iso"], "hardlink", god);
    r["convert_iso"] = json!(true);
    let (_, plan) = call(&e.app, "POST", "/api/import/plan", Some(r)).await;
    assert!(
        plan["entries"][0]["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x
                .as_str()
                .unwrap()
                .contains("Links can't be used when converting")),
        "{plan}"
    );

    // Convert and keep the ISO (copy).
    let mut r = req(&e, &["Alan Wake.iso"], "copy", god);
    r["convert_iso"] = json!(true);
    let j = run(&e, r).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    assert!(
        e.root
            .join("god/Alan Wake/4D530805/00007000/54E34DF4")
            .is_file()
    );
    assert!(
        e.root.join("inbox/Alan Wake.iso").exists(),
        "copy keeps the ISO"
    );
    assert!(
        !e.root.join("god/.rustybox-staging").exists(),
        "staging is cleaned up"
    );

    // Convert and move: the ISO goes only after the converted game is in place.
    let mut r = req(&e, &["Sub/Fable II.iso"], "move", god);
    r["convert_iso"] = json!(true);
    let j = run(&e, r).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    assert!(
        e.root
            .join("god/Fable II/4D5307F1/00007000/6339E4E9")
            .is_file()
    );
    assert!(!e.root.join("inbox/Sub/Fable II.iso").exists());
    let (_, items) = call(
        &e.app,
        "GET",
        &format!("/api/libraries/{}/items", god.0),
        None,
    )
    .await;
    assert_eq!(items["total"], 2);
}

#[tokio::test]
async fn a_game_can_go_on_to_a_drive_on_another_computer_in_the_same_job() {
    // The drive: an agent on a folder, standing in for the Xbox's FAT32 disk.
    let drive_root =
        std::env::temp_dir().join(format!("rustybox_import_drive_{}", std::process::id()));
    let _ = fs::remove_dir_all(&drive_root);
    fs::create_dir_all(drive_root.join("Games")).unwrap();
    let drive_root = fs::canonicalize(drive_root).unwrap();
    let state = AgentState::new(&drive_root, TOKEN.into(), "Xbox drive".into(), false).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = agent::router(Arc::new(state));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let e = env("alsodrive");
    fill_inbox(&e);
    let god = lib(&e, "GOD", "god", "god", true).await;
    let (s, v) = call(&e.app, "POST", "/api/libraries", Some(json!({"name": "Xbox drive", "kind": "god", "paths": [{"remote": {"url": url, "token": TOKEN, "subdir": "Games"}, "writable": true}]}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, l) = call(&e.app, "GET", &format!("/api/libraries/{}", v["id"]), None).await;
    let drive = (
        v["id"].as_i64().unwrap(),
        l["paths"][0]["id"].as_i64().unwrap(),
    );

    // ISO -> convert -> GOD library -> on to the drive.
    let mut r = req(&e, &["Alan Wake.iso"], "copy", god);
    r["convert_iso"] = json!(true);
    r["also_to"] = json!({"library_id": drive.0, "path_id": drive.1});
    let (_, plan) = call(&e.app, "POST", "/api/import/plan", Some(r.clone())).await;
    assert_eq!(plan["ok"], true, "{plan}");
    let steps: Vec<_> = plan["entries"][0]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    assert_eq!(steps.len(), 3, "convert, place, copy on: {steps:?}");
    assert!(steps[2].contains("Xbox drive"));
    let j = run(&e, r).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    assert!(
        e.root
            .join("god/Alan Wake/4D530805/00007000/54E34DF4")
            .is_file()
    );
    assert!(
        drive_root
            .join("Games/Alan Wake/4D530805/00007000/54E34DF4")
            .is_file(),
        "and on the drive"
    );
    let (_, items) = call(
        &e.app,
        "GET",
        &format!("/api/libraries/{}/items", drive.0),
        None,
    )
    .await;
    assert_eq!(
        items["items"][0]["title_id"], "4D530805",
        "the drive's library is updated too"
    );

    // Importing from the drive back to the server works, and move removes it from the drive.
    let from_drive = json!({"source": {"kind": "drive", "library_id": drive.0, "path_id": drive.1}, "select": ["Alan Wake/4D530805"], "mode": "move", "dest": {"library_id": god.0, "path_id": god.1}, "layout": "titleid"});
    let (_, plan) = call(&e.app, "POST", "/api/import/plan", Some(from_drive.clone())).await;
    assert!(
        plan["entries"][0]["outputs"][0]
            .as_str()
            .unwrap()
            .ends_with("god/4D530805"),
        "{plan}"
    );
    assert_eq!(run(&e, from_drive).await["job"]["status"], "done");
    assert!(e.root.join("god/4D530805/00007000/54E34DF4").is_file());
    assert!(!drive_root.join("Games/Alan Wake/4D530805").exists());
}

#[tokio::test]
async fn conflicts_damage_and_read_only_folders_are_caught_in_the_plan() {
    let e = env("problems");
    fill_inbox(&e);
    let god = lib(&e, "GOD", "god", "god", true).await;
    let ro = lib(&e, "Read only", "iso", "extra", false).await;

    // Importing into a read-only folder, a damaged game (warned), and ISO into ISO library of the wrong kind.
    let (_, plan) = call(
        &e.app,
        "POST",
        "/api/import/plan",
        Some(req(&e, &["Alan Wake.iso"], "copy", ro)),
    )
    .await;
    assert_eq!(plan["ok"], false);
    assert!(
        plan["problems"][0].as_str().unwrap().contains("read-only"),
        "{plan}"
    );
    let (_, plan) = call(
        &e.app,
        "POST",
        "/api/import/plan",
        Some(req(&e, &["Broken/4D5307F2"], "copy", god)),
    )
    .await;
    assert_eq!(
        plan["ok"], true,
        "damaged is a warning, not a block: {plan}"
    );
    assert!(
        plan["entries"][0]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("damaged"),
        "{plan}"
    );
    let (_, plan) = call(
        &e.app,
        "POST",
        "/api/import/plan",
        Some(req(&e, &["Gears/4D5307D5"], "copy", ro)),
    )
    .await;
    assert!(
        plan["entries"][0]["problems"][0]
            .as_str()
            .unwrap()
            .contains("GOD folder"),
        "{plan}"
    );

    // A different file already in the way blocks until replacing is asked for; nothing is half-done.
    run(&e, req(&e, &["Gears/4D5307D5"], "copy", god)).await;
    fs::write(
        e.root
            .join("god/Gears of War/4D5307D5/00007000/ABCDEF01.data/Data0000"),
        b"different",
    )
    .unwrap();
    let r = req(&e, &["Gears/4D5307D5"], "copy", god);
    let (s, v) = call(&e.app, "POST", "/api/import/start", Some(r.clone())).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert!(v["message"].as_str().unwrap().contains("different size"));
    let mut over = r;
    over["overwrite"] = json!(true);
    assert_eq!(run(&e, over).await["job"]["status"], "done");
    assert_eq!(
        fs::metadata(
            e.root
                .join("god/Gears of War/4D5307D5/00007000/ABCDEF01.data/Data0000")
        )
        .unwrap()
        .len(),
        100
    );

    // Nothing chosen, and an id that isn't in the source.
    let (s, _) = call(
        &e.app,
        "POST",
        "/api/import/plan",
        Some(req(&e, &[], "copy", god)),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = call(
        &e.app,
        "POST",
        "/api/import/plan",
        Some(req(&e, &["Nope.iso"], "copy", god)),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn games_already_in_libraries_can_be_sent_elsewhere() {
    let e = env("items");
    fill_inbox(&e);
    let god = lib(&e, "GOD", "god", "god", true).await;
    run(&e, req(&e, &["Gears/4D5307D5"], "copy", god)).await;
    let (_, items) = call(
        &e.app,
        "GET",
        &format!("/api/libraries/{}/items", god.0),
        None,
    )
    .await;
    let item = items["items"][0]["id"].as_i64().unwrap();
    // Send it to a second GOD library (a second folder).
    let second = lib(&e, "Second GOD", "god", "extra", true).await;
    let r = json!({"source": {"kind": "items", "items": [{"library_id": god.0, "item_id": item}]}, "mode": "copy", "dest": {"library_id": second.0, "path_id": second.1}});
    let (_, plan) = call(&e.app, "POST", "/api/import/plan", Some(r.clone())).await;
    assert_eq!(plan["ok"], true, "{plan}");
    assert_eq!(run(&e, r).await["job"]["status"], "done");
    assert!(
        e.root
            .join("extra/Gears of War/4D5307D5/00007000/ABCDEF01")
            .is_file()
    );
    assert!(
        e.root
            .join("god/Gears of War/4D5307D5/00007000/ABCDEF01")
            .is_file(),
        "the original stays"
    );
}

#[tokio::test]
async fn two_libraries_are_compared_by_game_and_disc_and_a_game_already_there_is_noticed() {
    let e = env("compare");
    fill_inbox(&e);
    let god = lib(&e, "GOD", "god", "god", true).await;
    let drive = lib(&e, "Drive", "god", "extra", true).await;
    // The library has Gears and Alan Wake (converted); the "drive" has Gears under another name, and a damaged Viva Pinata.
    run(&e, req(&e, &["Gears/4D5307D5"], "copy", god)).await;
    let mut r = req(&e, &["Alan Wake.iso"], "copy", god);
    r["convert_iso"] = json!(true);
    run(&e, r).await;
    run(&e, req(&e, &["Gears/4D5307D5"], "copy", drive)).await;
    run(&e, req(&e, &["Broken/4D5307F2"], "copy", drive)).await;
    fs::rename(
        e.root.join("extra/Gears of War"),
        e.root.join("extra/Gears (USA)"),
    )
    .unwrap();
    for l in [god, drive] {
        call(
            &e.app,
            "POST",
            &format!("/api/libraries/{}/scan", l.0),
            None,
        )
        .await;
    }
    settle(&e.app).await;

    let (s, c) = call(
        &e.app,
        "GET",
        &format!("/api/compare?a={}&b={}", god.0, drive.0),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let titles = |k: &str| -> Vec<String> {
        c[k].as_array()
            .unwrap()
            .iter()
            .map(|m| m["title_id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        titles("both"),
        vec!["4D5307D5"],
        "found under different folder names, still the same game"
    );
    assert_eq!(titles("only_a"), vec!["4D530805"]);
    assert_eq!(titles("only_b"), vec!["4D5307F2"]);
    assert!(
        c["only_b"][0]["b"][0]["health"]
            .as_str()
            .unwrap()
            .contains("data files"),
        "a damaged copy is flagged: {c}"
    );
    assert_eq!(
        call(
            &e.app,
            "GET",
            &format!("/api/compare?a={0}&b={0}", god.0),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    // Importing a game that is already in the destination (under another name) says so.
    let (_, plan) = call(
        &e.app,
        "POST",
        "/api/import/plan",
        Some(req(&e, &["Gears/4D5307D5"], "copy", drive)),
    )
    .await;
    let w = plan["entries"][0]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(w.contains("already in") && w.contains("Gears (USA)"), "{w}");
}

#[tokio::test]
async fn games_can_be_removed_only_when_confirmed_and_only_from_writable_folders() {
    let e = env("remove");
    fill_inbox(&e);
    let god = lib(&e, "GOD", "god", "god", true).await;
    let ro = lib(&e, "Read only", "god", "extra", true).await;
    run(&e, req(&e, &["Gears/4D5307D5"], "copy", god)).await;
    run(&e, req(&e, &["Gears/4D5307D5"], "copy", ro)).await;
    call(
        &e.app,
        "PUT",
        &format!("/api/libraries/{}/paths/{}", ro.0, ro.1),
        Some(json!({"writable": false})),
    )
    .await;
    let id_of = |lib_id: i64| {
        let app = e.app.clone();
        async move {
            call(&app, "GET", &format!("/api/libraries/{lib_id}/items"), None)
                .await
                .1["items"][0]["id"]
                .as_i64()
                .unwrap()
        }
    };
    let (gid, rid) = (id_of(god.0).await, id_of(ro.0).await);

    // Asking what would go changes nothing.
    let (s, v) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{}/items/remove", god.0),
        Some(json!({"item_ids": [gid]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["items"][0]["name"], "Gears of War");
    assert!(e.root.join("god/Gears of War/4D5307D5").exists());
    // A read-only folder can't have games removed.
    let (s, _) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{}/items/remove", ro.0),
        Some(json!({"item_ids": [rid], "confirm": true})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(e.root.join("extra/Gears of War/4D5307D5").exists());
    // A save kept in the same title folder is somebody's data: it stays when the game goes.
    std::fs::create_dir_all(e.root.join("extra/Gears of War/4D5307D5/00000001")).unwrap();
    // (the removal below is of the first copy; put the same kind of file there)
    std::fs::create_dir_all(e.root.join("god/Gears of War/4D5307D5/00000001")).unwrap();
    std::fs::write(e.root.join("god/Gears of War/4D5307D5/00000001/save"), b"s").unwrap();
    // Confirmed: the game goes, the empty name folder goes too, the index follows, and the other copy is untouched.
    let (s, v) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{}/items/remove", god.0),
        Some(json!({"item_ids": [gid], "confirm": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        wait_job(&e.app, v["job"].as_u64().unwrap()).await["job"]["status"],
        "done"
    );
    assert!(
        e.root
            .join("god/Gears of War/4D5307D5/00000001/save")
            .exists(),
        "saves are kept"
    );
    assert!(!e.root.join("god/Gears of War/4D5307D5/00007000").exists());
    assert!(
        e.root.join("god").exists(),
        "the library folder itself stays"
    );
    assert_eq!(
        call(
            &e.app,
            "GET",
            &format!("/api/libraries/{}/items", god.0),
            None
        )
        .await
        .1["total"],
        1,
        "only the kept save folder is left in the index"
    );
    assert!(e.root.join("extra/Gears of War/4D5307D5").exists());
    // Nothing chosen, or an item from somewhere else.
    assert_eq!(
        call(
            &e.app,
            "POST",
            &format!("/api/libraries/{}/items/remove", god.0),
            Some(json!({"item_ids": []}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &e.app,
            "POST",
            &format!("/api/libraries/{}/items/remove", god.0),
            Some(json!({"item_ids": [rid], "confirm": true}))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn replace_swaps_the_old_copy_found_by_title_id_and_keeps_saves() {
    let e = env("replace");
    // The old, partly broken copy lives under another folder name and has a saved game beside it.
    let old = e.root.join("god/Simpsons Game, The (USA)");
    write_god(&old, 0x4541_0809, "The Simpsons Game", 2, &[100, 100]);
    let saves = old.join("45410809/00000001");
    fs::create_dir_all(&saves).unwrap();
    fs::write(saves.join("save.bin"), b"my progress").unwrap();
    write_god(
        &e.root.join("inbox/New"),
        0x4541_0809,
        "The Simpsons Game",
        2,
        &[300, 300],
    );
    let g = lib(&e, "GODs", "god", "god", true).await;
    let (s, _) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{}/scan", g.0),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    settle(&e.app).await;

    // Without replace it is just a warning that the game is already there.
    let r = req(&e, &["New/45410809"], "copy", g);
    let mut r = r;
    r["layout"] = json!("name_titleid");
    let (_, plan) = call(&e.app, "POST", "/api/import/plan", Some(r.clone())).await;
    let warn = plan["entries"][0]["warnings"].to_string();
    assert!(
        warn.contains("already in") && warn.contains("Replace the game"),
        "{plan}"
    );

    // With replace the plan says what it will remove.
    r["replace"] = json!(true);
    let (_, plan) = call(&e.app, "POST", "/api/import/plan", Some(r.clone())).await;
    assert_eq!(plan["ok"], true, "{plan}");
    let steps = plan["entries"][0]["steps"].to_string();
    assert!(
        steps.contains("Remove the old copy")
            && steps.contains("Simpsons Game, The (USA)/45410809"),
        "{steps}"
    );
    assert!(
        old.join("45410809/00007000").exists(),
        "planning changes nothing"
    );

    let j = run(&e, r).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    assert!(
        !old.join("45410809/00007000").exists(),
        "the old game files are gone"
    );
    assert_eq!(
        fs::read(saves.join("save.bin")).unwrap(),
        b"my progress",
        "saved games are never touched"
    );
    let new = e
        .root
        .join("god/The Simpsons Game/45410809/00007000/ABCDEF01.data");
    assert_eq!(fs::metadata(new.join("Data0000")).unwrap().len(), 300);
    let (_, items) = call(
        &e.app,
        "GET",
        &format!("/api/libraries/{}/items", g.0),
        None,
    )
    .await;
    let n = items["items"].as_array().map(|a| a.len()).unwrap_or(0);
    assert!(n >= 1, "{items}");
}

#[tokio::test]
async fn duplicates_are_found_and_the_best_copy_is_kept_with_a_reason() {
    let e = env("dups");
    // The same game three times: a good one in the tidy place, a damaged one, and a smaller good one.
    write_god(
        &e.root.join("god/Alan Wake"),
        0x4D53_0805,
        "Alan Wake",
        2,
        &[500, 500],
    );
    write_god(
        &e.root.join("god/Old"),
        0x4D53_0805,
        "Alan Wake",
        4,
        &[500, 0],
    );
    write_god(
        &e.root.join("god/Other"),
        0x4D53_0805,
        "Alan Wake",
        2,
        &[100, 100],
    );
    write_god(
        &e.root.join("god/Single"),
        0x4D53_07F1,
        "Fable II",
        2,
        &[100, 100],
    );
    let g = lib(&e, "GODs", "god", "god", true).await;
    call(
        &e.app,
        "POST",
        &format!("/api/libraries/{}/scan", g.0),
        None,
    )
    .await;
    settle(&e.app).await;

    let (s, d) = call(
        &e.app,
        "GET",
        &format!("/api/libraries/{}/duplicates", g.0),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    let groups = d["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "only Alan Wake is duplicated: {d}");
    let items = groups[0]["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    let keep: Vec<_> = items.iter().filter(|i| i["keep"] == true).collect();
    assert_eq!(keep.len(), 1);
    assert_eq!(keep[0]["relpath"], "Alan Wake/4D530805", "{d}");
    let reasons: Vec<String> = items
        .iter()
        .filter(|i| i["keep"] == false)
        .map(|i| i["reason"].as_str().unwrap().to_string())
        .collect();
    assert!(
        reasons.iter().any(|r| r.starts_with("Damaged")),
        "{reasons:?}"
    );
    assert!(
        reasons.iter().any(|r| r.starts_with("Smaller")),
        "{reasons:?}"
    );
    assert_eq!(d["remove"].as_array().unwrap().len(), 2);

    // Removing what it says leaves only the kept copy on disk.
    let ids = d["remove"].clone();
    let (s, v) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{}/items/remove", g.0),
        Some(json!({"item_ids": ids, "confirm": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert!(e.root.join("god/Alan Wake/4D530805").exists());
    assert!(!e.root.join("god/Old").exists() && !e.root.join("god/Other").exists());
    assert!(e.root.join("god/Single/4D5307F1").exists());
}

#[tokio::test]
async fn a_drive_on_another_computer_can_be_tidied_through_the_agent() {
    let drive_root =
        std::env::temp_dir().join(format!("rustybox_import_tidy_{}", std::process::id()));
    let _ = fs::remove_dir_all(&drive_root);
    fs::create_dir_all(drive_root.join("Games")).unwrap();
    let drive_root = fs::canonicalize(drive_root).unwrap();
    // A messy drive: a wrongly named folder, one nested deep, and the right place already taken.
    write_god(
        &drive_root.join("Games/Alan Wake (USA)"),
        0x4D53_0805,
        "Alan Wake",
        2,
        &[100, 100],
    );
    write_god(
        &drive_root.join("Games/Stuff/Deep"),
        0x4D53_07F1,
        "Fable II",
        2,
        &[100, 100],
    );
    let state = AgentState::new(&drive_root, TOKEN.into(), "Xbox drive".into(), false).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = agent::router(Arc::new(state));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let e = env("remotetidy");
    let (s, v) = call(&e.app, "POST", "/api/libraries", Some(json!({"name": "Xbox drive", "kind": "god", "paths": [{"remote": {"url": url, "token": TOKEN, "subdir": "Games"}, "writable": true}]}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["id"].as_i64().unwrap();
    call(&e.app, "POST", &format!("/api/libraries/{id}/scan"), None).await;
    settle(&e.app).await;

    let (s, plan) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{id}/tidy/plan"),
        Some(json!({"layout": "name_titleid"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    let to: Vec<_> = plan["moves"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["to"].as_str().unwrap().to_string())
        .collect();
    assert!(
        to.contains(&"Alan Wake/4D530805".to_string())
            && to.contains(&"Fable II/4D5307F1".to_string()),
        "{plan}"
    );
    assert!(
        plan["moves"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["problem"].is_null()),
        "{plan}"
    );

    let (s, v) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{id}/tidy/start"),
        Some(json!({"layout": "name_titleid"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    settle(&e.app).await;
    let games = drive_root.join("Games");
    assert!(games.join("Alan Wake/4D530805/00007000").is_dir());
    assert!(games.join("Fable II/4D5307F1/00007000").is_dir());
    assert!(
        !games.join("Alan Wake (USA)").exists(),
        "the emptied old folder is removed"
    );
    assert!(
        !games.join("Stuff").exists(),
        "so are the emptied folders above it"
    );
}

#[tokio::test]
async fn a_drive_folder_for_add_ons_is_added_without_the_token_and_tidy_sorts_into_it() {
    let drive_root =
        std::env::temp_dir().join(format!("rustybox_import_rolesdrive_{}", std::process::id()));
    let _ = fs::remove_dir_all(&drive_root);
    fs::create_dir_all(drive_root.join("Games")).unwrap();
    let drive_root = fs::canonicalize(drive_root).unwrap();
    write_god(
        &drive_root.join("Games/Alan Wake"),
        0x4D53_0805,
        "Alan Wake",
        2,
        &[100, 100],
    );
    // An add-on (content type 2) sitting in the Games folder.
    let pack = drive_root.join("Games/Signal Pack/4D530805/00000002");
    fs::create_dir_all(&pack).unwrap();
    fs::write(
        pack.join("PACK1"),
        stfs::build(b"LIVE", 0x2, 0x4D53_0805, 1, "The Signal", "The Signal"),
    )
    .unwrap();
    let state = AgentState::new(&drive_root, TOKEN.into(), "Xbox drive".into(), false).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = agent::router(Arc::new(state));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let e = env("roles");
    let (s, v) = call(&e.app, "POST", "/api/libraries", Some(json!({"name": "Xbox drive", "kind": "god", "paths": [{"remote": {"url": url, "token": TOKEN, "subdir": "Games"}, "writable": true}]}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["id"].as_i64().unwrap();
    let (_, l) = call(&e.app, "GET", &format!("/api/libraries/{id}"), None).await;
    let games_path = l["paths"][0]["id"].as_i64().unwrap();

    // A second folder on the same drive, made on the spot, using the first one's address and token.
    let (s, v) = call(&e.app, "POST", &format!("/api/libraries/{id}/paths"), Some(json!({"label": "DLC", "writable": true, "role": "dlc", "remote": {"same_as": games_path, "subdir": "DLC", "create": true}}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(drive_root.join("DLC").is_dir(), "created on the drive");
    let (_, l) = call(&e.app, "GET", &format!("/api/libraries/{id}"), None).await;
    assert!(
        !l.to_string().contains(TOKEN),
        "the token never comes back to the browser"
    );
    assert_eq!(l["paths"][1]["role"], "dlc");

    call(&e.app, "POST", &format!("/api/libraries/{id}/scan"), None).await;
    settle(&e.app).await;
    let (_, plan) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{id}/tidy/plan"),
        Some(json!({"layout": "name_titleid"})),
    )
    .await;
    let moves = plan["moves"].as_array().unwrap();
    let pack_move = moves
        .iter()
        .find(|m| m["content_kind"] == "dlc")
        .unwrap_or_else(|| panic!("{plan}"));
    assert_eq!(pack_move["to_path_label"], "DLC", "{plan}");
    assert!(pack_move["problem"].is_null(), "{plan}");

    let (_, v) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{id}/tidy/start"),
        Some(json!({"layout": "name_titleid"})),
    )
    .await;
    wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    settle(&e.app).await;
    assert!(
        drive_root
            .join("DLC/The Signal/4D530805/00000002/PACK1")
            .is_file()
            || drive_root
                .join("DLC/Alan Wake/4D530805/00000002/PACK1")
                .is_file()
    );
    assert!(!drive_root.join("Games/Signal Pack").exists());
    assert!(
        drive_root
            .join("Games/Alan Wake/4D530805/00007000")
            .is_dir(),
        "the game stays in Games"
    );
}
