use std::{
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
    xbox::{xex, xiso},
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Env {
    app: Router,
    root: PathBuf,
}

fn env(name: &str) -> Env {
    env_with(name, None)
}

fn env_with(name: &str, abgx360: Option<PathBuf>) -> Env {
    let base = std::env::temp_dir().join(format!("rustybox_conv_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    for d in ["isos", "isos_out", "god", "god_ro", "folders"] {
        std::fs::create_dir_all(base.join("data").join(d)).unwrap();
    }
    let root = std::fs::canonicalize(base.join("data")).unwrap();
    let cfg = Config {
        config_dir: base.join("config"),
        roots: vec![root.clone()],
        abgx360,
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

/// A valid disc image for a game, a few MB so it spans several GOD blocks.
fn write_iso(path: &Path, title: u32, media: u32, disc: u8, discs: u8) {
    let mut bytes = xiso::build_simple_disc(&xex::build(media, title, disc, discs));
    bytes.extend((0..2_500_000u32).map(|i| (i % 253) as u8));
    while !bytes.len().is_multiple_of(2048) {
        bytes.push(0);
    }
    std::fs::write(path, bytes).unwrap();
}

async fn make_lib(e: &Env, name: &str, kind: &str, dir: &str, writable: bool) -> (i64, i64) {
    let (s, v) = call(&e.app, "POST", "/api/libraries", Some(json!({"name": name, "kind": kind, "paths": [{"path": p(&e.root.join(dir)), "label": dir, "writable": writable}]}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["id"].as_i64().unwrap();
    let (_, lib) = call(&e.app, "GET", &format!("/api/libraries/{id}"), None).await;
    (id, lib["paths"][0]["id"].as_i64().unwrap())
}

async fn wait_job(e: &Env, id: u64) -> Value {
    for _ in 0..600 {
        let (_, j) = call(&e.app, "GET", &format!("/api/jobs/{id}"), None).await;
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

async fn scan(e: &Env, lib: i64) {
    let (_, v) = call(&e.app, "POST", &format!("/api/libraries/{lib}/scan"), None).await;
    let j = wait_job(e, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
}

async fn items(e: &Env, lib: i64) -> Vec<Value> {
    let (_, v) = call(&e.app, "GET", &format!("/api/libraries/{lib}/items"), None).await;
    v["items"].as_array().unwrap().clone()
}

#[tokio::test]
async fn iso_to_god_and_back_through_the_api() {
    let e = env("full");
    // 4D530805 = Alan Wake in the title list.
    write_iso(
        &e.root.join("isos/Alan Wake.iso"),
        0x4D53_0805,
        0x54E3_4DF4,
        1,
        1,
    );
    let (iso_lib, _) = make_lib(&e, "ISOs", "iso", "isos", false).await;
    let (god_lib, god_path) = make_lib(&e, "GOD", "god", "god", true).await;
    let (out_lib, out_path) = make_lib(&e, "ISO out", "iso", "isos_out", true).await;
    scan(&e, iso_lib).await;
    let iso_item = items(&e, iso_lib).await[0]["id"].as_i64().unwrap();

    // Plan: nothing is written, and the plan says where things would go.
    let req = json!({"op": "iso_to_god", "items": [{"library_id": iso_lib, "item_id": iso_item}], "dest_library_id": god_lib, "dest_path_id": god_path});
    let (s, plan) = call(&e.app, "POST", "/api/convert/plan", Some(req.clone())).await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(plan["ok"], true, "{plan}");
    let entry = &plan["entries"][0];
    assert_eq!(entry["name"], "Alan Wake");
    assert!(
        entry["output"]
            .as_str()
            .unwrap()
            .ends_with("god/Alan Wake/4D530805/00007000/54E34DF4"),
        "{entry}"
    );
    assert_eq!(
        std::fs::read_dir(e.root.join("god")).unwrap().count(),
        0,
        "planning writes nothing"
    );

    // Run it.
    let (s, v) = call(&e.app, "POST", "/api/convert/start", Some(req.clone())).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let j = wait_job(&e, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let container = e.root.join("god/Alan Wake/4D530805/00007000/54E34DF4");
    assert!(
        container.is_file()
            && e.root
                .join("god/Alan Wake/4D530805/00007000/54E34DF4.data/Data0000")
                .is_file()
    );
    assert!(
        !e.root.join("god/.rustybox-staging").exists(),
        "staging is cleaned up"
    );

    // The GOD library already shows it (the job rescans), named and identified.
    let god_items = items(&e, god_lib).await;
    assert_eq!(god_items.len(), 1);
    assert_eq!(
        (
            god_items[0]["title_id"].as_str(),
            god_items[0]["game_name"].as_str()
        ),
        (Some("4D530805"), Some("Alan Wake"))
    );

    // Converting again is refused until replacing is asked for, and the old copy is untouched.
    let before = std::fs::metadata(&container).unwrap().modified().unwrap();
    let (s, v) = call(&e.app, "POST", "/api/convert/start", Some(req.clone())).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert!(v["message"].as_str().unwrap().contains("Already exists"));
    assert_eq!(
        std::fs::metadata(&container).unwrap().modified().unwrap(),
        before
    );
    let mut again = req.clone();
    again["overwrite"] = json!(true);
    let (_, plan) = call(&e.app, "POST", "/api/convert/plan", Some(again.clone())).await;
    assert_eq!(plan["ok"], true);
    assert!(
        plan["entries"][0]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("will be replaced"))
    );
    let (_, v) = call(&e.app, "POST", "/api/convert/start", Some(again)).await;
    assert_eq!(
        wait_job(&e, v["job"].as_u64().unwrap()).await["job"]["status"],
        "done"
    );

    // And back to an ISO in another library.
    let god_item = items(&e, god_lib).await[0]["id"].as_i64().unwrap();
    let back = json!({"op": "god_to_iso", "items": [{"library_id": god_lib, "item_id": god_item}], "dest_library_id": out_lib, "dest_path_id": out_path});
    let (_, plan) = call(&e.app, "POST", "/api/convert/plan", Some(back.clone())).await;
    assert_eq!(plan["ok"], true, "{plan}");
    assert!(
        plan["entries"][0]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w
                .as_str()
                .unwrap()
                .contains("ISO of this game already exists"))
    );
    let (_, v) = call(&e.app, "POST", "/api/convert/start", Some(back)).await;
    assert_eq!(
        wait_job(&e, v["job"].as_u64().unwrap()).await["job"]["status"],
        "done"
    );
    let out = items(&e, out_lib).await;
    assert_eq!(out.len(), 1);
    assert_eq!(
        (
            out[0]["title_id"].as_str(),
            out[0]["media_id"].as_str(),
            out[0]["name"].as_str()
        ),
        (Some("4D530805"), Some("54E34DF4"), Some("Alan Wake"))
    );
}

#[tokio::test]
async fn unpack_and_pack_a_game_folder() {
    let e = env("folders");
    write_iso(
        &e.root.join("isos/Fable II.iso"),
        0x4D53_07F1,
        0x6339_E4E9,
        2,
        2,
    );
    let (iso_lib, _) = make_lib(&e, "ISOs", "iso", "isos", false).await;
    let (out_lib, out_path) = make_lib(&e, "Folders", "custom", "folders", true).await;
    let (iso_out, iso_out_path) = make_lib(&e, "ISO out", "iso", "isos_out", true).await;
    scan(&e, iso_lib).await;
    let item = items(&e, iso_lib).await[0]["id"].as_i64().unwrap();

    let req = json!({"op": "extract", "items": [{"library_id": iso_lib, "item_id": item}], "dest_library_id": out_lib, "dest_path_id": out_path});
    let (_, plan) = call(&e.app, "POST", "/api/convert/plan", Some(req.clone())).await;
    assert!(
        plan["entries"][0]["output"]
            .as_str()
            .unwrap()
            .ends_with("folders/Fable II (Disc 2)"),
        "disc number in the name: {plan}"
    );
    let (_, v) = call(&e.app, "POST", "/api/convert/start", Some(req.clone())).await;
    assert_eq!(
        wait_job(&e, v["job"].as_u64().unwrap()).await["job"]["status"],
        "done"
    );
    let game = e.root.join("folders/Fable II (Disc 2)");
    assert!(game.join("default.xex").is_file());

    // A folder that exists is never replaced.
    let (s, _) = call(&e.app, "POST", "/api/convert/start", Some(req)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // Pack it back up.
    let pack = json!({"op": "create", "source_folder": p(&game), "dest_library_id": iso_out, "dest_path_id": iso_out_path});
    let (_, v) = call(&e.app, "POST", "/api/convert/start", Some(pack)).await;
    assert_eq!(
        wait_job(&e, v["job"].as_u64().unwrap()).await["job"]["status"],
        "done"
    );
    let made = items(&e, iso_out).await;
    assert_eq!(made.len(), 1);
    assert_eq!(
        made[0]["title_id"], "4D5307F1",
        "the new image is a readable game"
    );
}

#[tokio::test]
async fn bad_requests_are_refused_before_anything_is_written() {
    let e = env("refuse");
    write_iso(&e.root.join("isos/g.iso"), 0x4D53_0805, 1, 1, 1);
    std::fs::write(e.root.join("isos/broken.iso"), vec![5u8; 50_000]).unwrap();
    let (iso_lib, _) = make_lib(&e, "ISOs", "iso", "isos", false).await;
    let (god_lib, god_path) = make_lib(&e, "GOD", "god", "god", true).await;
    let (ro_lib, ro_path) = make_lib(&e, "RO", "god", "god_ro", false).await;
    scan(&e, iso_lib).await;
    let all = items(&e, iso_lib).await;
    let good = all.iter().find(|i| i["name"] == "g").unwrap()["id"]
        .as_i64()
        .unwrap();
    let broken = all.iter().find(|i| i["name"] == "broken").unwrap()["id"]
        .as_i64()
        .unwrap();
    let r = |items: Vec<i64>, lib: i64, path: i64, op: &str| json!({"op": op, "items": items.iter().map(|i| json!({"library_id": iso_lib, "item_id": i})).collect::<Vec<_>>(), "dest_library_id": lib, "dest_path_id": path});

    // Read-only destination.
    let (_, plan) = call(
        &e.app,
        "POST",
        "/api/convert/plan",
        Some(r(vec![good], ro_lib, ro_path, "iso_to_god")),
    )
    .await;
    assert_eq!(plan["ok"], false);
    assert!(plan["problems"][0].as_str().unwrap().contains("read-only"));
    // An unreadable image says why.
    let (_, plan) = call(
        &e.app,
        "POST",
        "/api/convert/plan",
        Some(r(vec![broken], god_lib, god_path, "iso_to_god")),
    )
    .await;
    assert_eq!(plan["ok"], false);
    assert!(
        plan["entries"][0]["problems"][0]
            .as_str()
            .unwrap()
            .contains("Can't read this image"),
        "{plan}"
    );
    // The wrong kind of item for the operation.
    let (_, plan) = call(
        &e.app,
        "POST",
        "/api/convert/plan",
        Some(r(vec![good], god_lib, god_path, "god_to_iso")),
    )
    .await;
    assert!(
        plan["entries"][0]["problems"][0]
            .as_str()
            .unwrap()
            .contains("needs GOD"),
        "{plan}"
    );
    // Starting a plan with problems is refused outright, and nothing is created.
    let (s, v) = call(
        &e.app,
        "POST",
        "/api/convert/start",
        Some(r(vec![broken], god_lib, god_path, "iso_to_god")),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(std::fs::read_dir(e.root.join("god")).unwrap().count(), 0);
    // Nothing chosen.
    let (s, _) = call(
        &e.app,
        "POST",
        "/api/convert/plan",
        Some(r(vec![], god_lib, god_path, "iso_to_god")),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // A source folder outside the allowed roots.
    let (s, _) = call(&e.app, "POST", "/api/convert/plan", Some(json!({"op": "create", "source_folder": "/etc", "dest_library_id": god_lib, "dest_path_id": god_path}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn settings_can_be_changed_and_are_validated() {
    let e = env("settings");
    let (_, v) = call(&e.app, "GET", "/api/settings", None).await;
    assert_eq!(
        (v["convert_threads"].as_i64(), v["god_layout"].as_str()),
        (Some(2), Some("name_titleid"))
    );
    let (s, v) = call(
        &e.app,
        "PUT",
        "/api/settings",
        Some(json!({"convert_threads": 4, "god_layout": "titleid"})),
    )
    .await;
    assert_eq!(
        (s, v["convert_threads"].as_i64()),
        (StatusCode::OK, Some(4))
    );
    let (s, _) = call(
        &e.app,
        "PUT",
        "/api/settings",
        Some(json!({"convert_threads": 0})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, v) = call(&e.app, "GET", "/api/settings", None).await;
    assert_eq!(
        v["convert_threads"], 4,
        "a refused change leaves the settings alone"
    );
    assert!(v.get("auth_password_hash").is_none());
}

/// A stand-in that, like the real program, exits 0 while reporting an error.
fn fake_abgx_error(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("abgx360-fake-error");
    std::fs::write(&script, "#!/bin/sh\necho \"ERROR: $2 isn't recognized as an XBOX 360 ISO or Stealth file!\"\nexit 0\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// A stand-in for abgx360 that records how it was called.
fn fake_abgx(dir: &Path, exit_code: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join(format!("abgx360-fake-{exit_code}"));
    std::fs::write(&script, format!("#!/bin/sh\necho \"called with: $@\"\necho \"a warning\" >&2\nprintf 'progress 50%%\\r'\nexit {exit_code}\n")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn log_of(job: &Value) -> String {
    job["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "log")
        .map(|e| e["msg"].as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn checking_an_iso_cannot_write_and_fixing_needs_a_writable_folder_and_a_backup() {
    let tools = std::env::temp_dir().join(format!("rustybox_abgx_{}", std::process::id()));
    std::fs::create_dir_all(&tools).unwrap();
    let e = env_with("abgx", Some(fake_abgx(&tools, 0)));
    write_iso(&e.root.join("isos/Game.iso"), 0x4D53_0805, 1, 1, 1);
    write_iso(&e.root.join("isos_out/Other.iso"), 0x4D53_07F1, 2, 1, 1);
    let (ro_lib, _) = make_lib(&e, "Read only", "iso", "isos", false).await;
    let (rw_lib, _) = make_lib(&e, "Writable", "iso", "isos_out", true).await;
    scan(&e, ro_lib).await;
    scan(&e, rw_lib).await;
    let ro_item = items(&e, ro_lib).await[0]["id"].as_i64().unwrap();
    let rw_item = items(&e, rw_lib).await[0]["id"].as_i64().unwrap();
    let original = std::fs::read(e.root.join("isos/Game.iso")).unwrap();

    // A plain check on a read-only folder works, and its command line cannot write.
    let (s, v) = call(
        &e.app,
        "POST",
        "/api/iso/check",
        Some(json!({"library_id": ro_lib, "item_id": ro_item})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let j = wait_job(&e, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let log = log_of(&j);
    assert!(
        log.contains("--nowrite")
            && log.contains("--af0")
            && log.contains("-b ")
            && log.contains("Game.iso"),
        "{log}"
    );
    assert!(log.contains("a warning"), "stderr is shown too: {log}");
    assert_eq!(
        std::fs::read(e.root.join("isos/Game.iso")).unwrap(),
        original
    );

    // Fixing in a read-only folder is refused.
    let (s, _) = call(
        &e.app,
        "POST",
        "/api/iso/check",
        Some(json!({"library_id": ro_lib, "item_id": ro_item, "fix": true})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Fixing in a writable folder backs up first.
    let (s, v) = call(
        &e.app,
        "POST",
        "/api/iso/check",
        Some(json!({"library_id": rw_lib, "item_id": rw_item, "fix": true})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let j = wait_job(&e, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done", "{j}");
    let log = log_of(&j);
    assert!(log.contains("--af3") && !log.contains("--nowrite"), "{log}");
    let backup = e.root.join("isos_out/Other.iso.rustybox-backup");
    assert_eq!(
        std::fs::read(&backup).unwrap(),
        std::fs::read(e.root.join("isos_out/Other.iso")).unwrap()
    );
    // The backup isn't mistaken for a game.
    assert_eq!(items(&e, rw_lib).await.len(), 1);

    // An existing backup is never overwritten; asking for no backup is allowed.
    let (s, _) = call(
        &e.app,
        "POST",
        "/api/iso/check",
        Some(json!({"library_id": rw_lib, "item_id": rw_item, "fix": true})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, v) = call(
        &e.app,
        "POST",
        "/api/iso/check",
        Some(json!({"library_id": rw_lib, "item_id": rw_item, "fix": true, "backup": false})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        wait_job(&e, v["job"].as_u64().unwrap()).await["job"]["status"],
        "done"
    );
    let _ = std::fs::remove_dir_all(&tools);
}

#[tokio::test]
async fn abgx_failures_and_absence_are_reported() {
    let tools = std::env::temp_dir().join(format!("rustybox_abgx_fail_{}", std::process::id()));
    std::fs::create_dir_all(&tools).unwrap();
    let e = env_with("abgx_fail", Some(fake_abgx(&tools, 3)));
    write_iso(&e.root.join("isos/Game.iso"), 0x4D53_0805, 1, 1, 1);
    let (lib, _) = make_lib(&e, "ISOs", "iso", "isos", false).await;
    scan(&e, lib).await;
    let item = items(&e, lib).await[0]["id"].as_i64().unwrap();
    let (_, v) = call(
        &e.app,
        "POST",
        "/api/iso/check",
        Some(json!({"library_id": lib, "item_id": item})),
    )
    .await;
    let j = wait_job(&e, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "failed");
    assert!(
        j["job"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("exit code 3"),
        "{j}"
    );

    // abgx360 can exit 0 and still report an error; that is a failure too.
    let e2 = env_with("abgx_err", Some(fake_abgx_error(&tools)));
    write_iso(&e2.root.join("isos/Game.iso"), 0x4D53_0805, 1, 1, 1);
    let (lib2, _) = make_lib(&e2, "ISOs", "iso", "isos", false).await;
    scan(&e2, lib2).await;
    let item2 = items(&e2, lib2).await[0]["id"].as_i64().unwrap();
    let (_, v) = call(
        &e2.app,
        "POST",
        "/api/iso/check",
        Some(json!({"library_id": lib2, "item_id": item2})),
    )
    .await;
    let j = wait_job(&e2, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "failed", "{j}");
    assert!(
        j["job"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("isn't recognized"),
        "{j}"
    );

    // Not installed: a clear refusal, not a crash.
    let none = env_with("abgx_none", Some(PathBuf::from("/does/not/exist")));
    let (s, v) = call(
        &none.app,
        "POST",
        "/api/iso/check",
        Some(json!({"library_id": 1, "item_id": 1})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["message"].as_str().unwrap().contains("isn't installed"));
    let _ = std::fs::remove_dir_all(&tools);
}

#[tokio::test]
async fn tidy_renames_god_folders_from_what_the_scan_found() {
    let e = env("tidy");
    // A GOD title in a badly named folder; its header says it is Alan Wake.
    let dir = e.root.join("god/whatever it was called/4D530805/00007000");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ABCDEF01"),
        rustybox::xbox::stfs::build(b"LIVE", 0x7000, 0x4D53_0805, 1, "Alan Wake", "Alan Wake"),
    )
    .unwrap();
    let (lib, _) = make_lib(&e, "GOD", "god", "god", true).await;
    scan(&e, lib).await;

    let (s, plan) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{lib}/tidy/plan"),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(plan["moves"].as_array().unwrap().len(), 1);
    assert_eq!(plan["moves"][0]["to"], "Alan Wake/4D530805");
    assert!(dir.exists(), "planning moves nothing");

    let (s, v) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{lib}/tidy/start"),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        wait_job(&e, v["job"].as_u64().unwrap()).await["job"]["status"],
        "done"
    );
    assert!(
        e.root
            .join("god/Alan Wake/4D530805/00007000/ABCDEF01")
            .is_file()
    );
    assert!(!e.root.join("god/whatever it was called").exists());
    // The index followed the rename.
    let now = items(&e, lib).await;
    assert_eq!(
        (now.len(), now[0]["relpath"].as_str()),
        (1, Some("Alan Wake/4D530805"))
    );

    // Nothing left to do: refused rather than running an empty job.
    let (s, _) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{lib}/tidy/start"),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Only GOD libraries can be tidied.
    let (iso_lib, _) = make_lib(&e, "ISOs", "iso", "isos", false).await;
    let (s, _) = call(
        &e.app,
        "POST",
        &format!("/api/libraries/{iso_lib}/tidy/plan"),
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_discs_of_a_multi_disc_game_convert_into_one_game_folder() {
    let e = env("multidisc");
    for (n, media) in [(1u8, 0x1111_0001u32), (2, 0x1111_0002), (3, 0x1111_0003)] {
        write_iso(
            &e.root.join(format!("isos/Lost Odyssey (Disc {n}).iso")),
            0x4D53_07F1,
            media,
            n,
            3,
        );
    }
    let (iso_lib, _) = make_lib(&e, "ISOs", "iso", "isos", false).await;
    let (god_lib, god_path) = make_lib(&e, "GOD", "god", "god", true).await;
    scan(&e, iso_lib).await;
    let all = items(&e, iso_lib).await;
    let ids: Vec<Value> = all
        .iter()
        .map(|i| json!({"library_id": iso_lib, "item_id": i["id"]}))
        .collect();
    let req = json!({"op": "iso_to_god", "items": ids, "dest_library_id": god_lib, "dest_path_id": god_path});
    let (_, plan) = call(&e.app, "POST", "/api/convert/plan", Some(req.clone())).await;
    assert_eq!(plan["ok"], true, "{plan}");
    let outs: Vec<String> = plan["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["output"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(outs.len(), 3);
    assert_eq!(
        outs.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "each disc has its own container"
    );
    // Every disc goes under the same game folder and title ID.
    let parents: std::collections::HashSet<_> = outs
        .iter()
        .map(|o| Path::new(o).parent().unwrap().to_path_buf())
        .collect();
    assert_eq!(parents.len(), 1, "{outs:?}");
    let (_, v) = call(&e.app, "POST", "/api/convert/start", Some(req)).await;
    assert_eq!(
        wait_job(&e, v["job"].as_u64().unwrap()).await["job"]["status"],
        "done"
    );
    for o in &outs {
        assert!(Path::new(o).exists(), "{o}");
    }
}
