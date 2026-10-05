use std::{
    collections::HashMap,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Query, State},
    http::{Request, StatusCode, header},
    routing::get,
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
    state: Arc<web::AppState>,
    root: PathBuf,
}

fn env(name: &str) -> Env {
    let base = std::env::temp_dir().join(format!("rustybox_grab_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    for d in ["downloads", "isos", "god"] {
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

fn write_iso(path: &Path, title: u32, media: u32, disc: u8, discs: u8) {
    let mut bytes = xiso::build_simple_disc(&xex::build(media, title, disc, discs));
    bytes.extend((0..1_500_000u32).map(|i| (i % 253) as u8));
    while !bytes.len().is_multiple_of(2048) {
        bytes.push(0);
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn rss(items: &[(&str, &str, u64)]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0"?><rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel><title>x</title>"#,
    );
    for (title, guid, mb) in items {
        s.push_str(&format!(r#"<item><title>{title}</title><guid>{guid}</guid><link>http://nzb.invalid/{guid}.nzb?apikey=SECRETKEY99</link><pubDate>Sat, 19 Sep 2026 12:00:00 +0000</pubDate><enclosure url="http://nzb.invalid/{guid}.nzb" length="{}" type="application/x-nzb"/><newznab:attr name="category" value="1000"/><newznab:attr name="category" value="1050"/><newznab:attr name="size" value="{}"/><newznab:attr name="grabs" value="40"/></item>"#, mb * 1_048_576, mb * 1_048_576));
    }
    s + "</channel></rss>"
}

/// SABnzbd as a script: what it was asked to add, and what its queue and history say.
#[derive(Default)]
struct Sab {
    added: Vec<(String, String, String)>,
    queue: Vec<Value>,
    history: Vec<Value>,
    cats: Vec<String>,
}

async fn fake_services() -> (String, Arc<Mutex<Sab>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let sab: Arc<Mutex<Sab>> = Arc::new(Mutex::new(Sab {
        cats: vec!["*".into(), "movies".into()],
        ..Default::default()
    }));
    let s2 = sab.clone();
    let app = Router::new()
        // An indexer that carries Xbox 360 games.
        .route("/good/api", get(|Query(q): Query<HashMap<String, String>>| async move {
            if q.get("apikey").map(String::as_str) != Some("SECRETKEY99") {
                return r#"<?xml version="1.0"?><error code="100" description="Incorrect user credentials"/>"#.to_string();
            }
            match q.get("t").map(String::as_str) {
                Some("caps") => r#"<caps><categories><category id="1000" name="Console"><subcat id="1050" name="Xbox 360"/></category></categories></caps>"#.into(),
                _ if q.get("q").is_some_and(|s| s.to_lowercase().contains("halo 3")) => rss(&[
                    ("Halo.3.PAL.XBOX360-GAC", "g1", 7139),
                    ("Halo 3 XBOX360-CCCLX", "g2", 7193),
                    ("Halo.3.ODST.X360-Allstars", "g3", 7485),
                    ("Halo.Reach.PAL.SPANiSH.XBOX360-TibuRon", "g4", 7071),
                    ("Halo.3.USA.XBOX360-ZRY", "g5", 7300),
                    ("Halo.3.PS3-GRP", "g6", 9000),
                    ("Halo 3 [MULTI][XBOX360].part01", "g7", 751),
                ]),
                _ if q.get("q").is_some_and(|s| s.to_lowercase().contains("fable")) => rss(&[("Fable.II.USA.XBOX360-GRP", "f1", 7000)]),
                _ => rss(&[]),
            }
        }))
        // One that has no Xbox 360 category at all.
        .route("/other/api", get(|Query(q): Query<HashMap<String, String>>| async move {
            match q.get("t").map(String::as_str) {
                Some("caps") => r#"<caps><categories><category id="2000" name="Movies"/></categories></caps>"#.to_string(),
                _ => rss(&[]),
            }
        }))
        .route("/prowlarr/api/v1/indexer", get(|headers: axum::http::HeaderMap| async move {
            if headers.get("x-api-key").and_then(|k| k.to_str().ok()) != Some("PROWLARRKEY") {
                return (StatusCode::UNAUTHORIZED, "no".to_string());
            }
            (StatusCode::OK, json!([{"id": 7, "name": "Geek", "protocol": "usenet", "enable": true}, {"id": 8, "name": "Torrenty", "protocol": "torrent", "enable": true}, {"id": 9, "name": "Off", "protocol": "usenet", "enable": false}]).to_string())
        }))
        .route("/prowlarr/7/api", get(|Query(q): Query<HashMap<String, String>>| async move {
            let _ = q;
            r#"<caps><categories><category id="1000" name="Console"><subcat id="1050" name="Xbox 360"/></category></categories></caps>"#.to_string()
        }))
        .route("/sab/api", get(move |State(s): State<Arc<Mutex<Sab>>>, Query(q): Query<HashMap<String, String>>| async move {
            if q.get("apikey").map(String::as_str) != Some("SABKEY1234") {
                return json!({"status": false, "error": "API Key Incorrect"}).to_string();
            }
            let mut g = s.lock().unwrap();
            match q.get("mode").map(String::as_str).unwrap_or("") {
                "version" => json!({"version": "4.5.0"}).to_string(),
                "get_cats" => json!({"categories": g.cats}).to_string(),
                "set_config" => {
                    g.cats.push(q["name"].clone());
                    json!({"status": true}).to_string()
                }
                "addurl" => {
                    let n = g.added.len() + 1;
                    g.added.push((q["name"].clone(), q["nzbname"].clone(), q.get("cat").cloned().unwrap_or_default()));
                    json!({"status": true, "nzo_ids": [format!("SABnzbd_nzo_{n}")]}).to_string()
                }
                "queue" => json!({"queue": {"slots": g.queue}}).to_string(),
                "history" => json!({"history": {"slots": g.history}}).to_string(),
                _ => json!({"status": true}).to_string(),
            }
        }))
        .with_state(s2);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, sab)
}

#[tokio::test]
async fn wanted_games_are_searched_ranked_grabbed_followed_imported_and_failures_are_blocked() {
    let (base, sab) = fake_services().await;
    let e = env("flow");
    let app = &e.app;

    // Indexers: a good one, a bad key (refused), one with no Xbox 360 category.
    let (st, v) = call(
        app,
        "POST",
        "/api/grab/indexers",
        Some(json!({"name": "Good", "url": format!("{base}/good"), "api_key": "SECRETKEY99"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        v.get("api_key").is_none() && v["api_key_set"] == true,
        "keys never come back: {v}"
    );
    let good = v["id"].as_u64().unwrap();
    let (_, v) = call(
        app,
        "POST",
        "/api/grab/indexers",
        Some(json!({"name": "Other", "url": format!("{base}/other"), "api_key": "KEYKEYKEY1"})),
    )
    .await;
    let other = v["id"].as_u64().unwrap();
    let (st, v) = call(
        app,
        "POST",
        "/api/grab/indexers/test",
        Some(json!({"id": good})),
    )
    .await;
    assert_eq!(
        (st, v["has_xbox360"].as_bool()),
        (StatusCode::OK, Some(true)),
        "{v}"
    );
    let (_, v) = call(
        app,
        "POST",
        "/api/grab/indexers/test",
        Some(json!({"id": other})),
    )
    .await;
    assert_eq!(v["has_xbox360"], false, "{v}");
    let (st, v) = call(
        app,
        "POST",
        "/api/grab/indexers/test",
        Some(json!({"name": "Bad", "url": format!("{base}/good"), "api_key": "WRONGKEY00"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{v}");
    assert_eq!(v["error"], "INDEXER_LOGIN");
    // Prowlarr: its usenet, enabled indexers are added through its own Newznab addresses.
    let (st, v) = call(
        app,
        "POST",
        "/api/grab/prowlarr",
        Some(json!({"url": format!("{base}/prowlarr"), "api_key": "WRONG"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{v}");
    let (st, v) = call(
        app,
        "POST",
        "/api/grab/prowlarr",
        Some(json!({"url": format!("{base}/prowlarr"), "api_key": "PROWLARRKEY"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        v["added"],
        json!(["Geek (via Prowlarr)", "Torrenty (via Prowlarr)"]),
        "{v}"
    );
    assert_eq!(v["indexers"][3]["protocol"], "torrent", "{v}");

    // SABnzbd: wrong key refused; right key shows its categories; the xbox360 category is made.
    let (st, _) = call(
        app,
        "POST",
        "/api/grab/sab/test",
        Some(json!({"url": format!("{base}/sab"), "api_key": "WRONGWRONG"})),
    )
    .await;
    assert_ne!(st, StatusCode::OK);
    let (st, v) = call(
        app,
        "POST",
        "/api/grab/sab/test",
        Some(json!({"url": format!("{base}/sab"), "api_key": "SABKEY1234"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        (v["version"].as_str(), v["category_exists"].as_bool()),
        (Some("4.5.0"), Some(false)),
        "{v}"
    );
    // Library to import into, and the path SABnzbd reports mapped to ours.
    let (st0, lb) = call(app, "POST", "/api/libraries", Some(json!({"name": "Usenet ISOs", "kind": "iso", "paths": [{"path": e.root.join("isos").to_string_lossy(), "label": "isos", "writable": true}]}))).await;
    assert_eq!(st0, StatusCode::OK, "{lb}");
    let lid = lb["id"].as_i64().unwrap();
    let (_, l) = call(app, "GET", &format!("/api/libraries/{lid}"), None).await;
    let pid = l["paths"][0]["id"].as_i64().unwrap();
    // A second folder standing in for the Xbox's drive, and the (mock) console to send to afterwards.
    fs::create_dir_all(e.root.join("drivecopy")).unwrap();
    let (_, db) = call(app, "POST", "/api/libraries", Some(json!({"name": "Drive copy", "kind": "iso", "paths": [{"path": e.root.join("drivecopy").to_string_lossy(), "label": "drive", "writable": true}]}))).await;
    let dlid = db["id"].as_i64().unwrap();
    let (_, dl) = call(app, "GET", &format!("/api/libraries/{dlid}"), None).await;
    let dpid = dl["paths"][0]["id"].as_i64().unwrap();
    let (_, consoles) = call(app, "GET", "/api/consoles", None).await;
    let cid = consoles[0]["id"].as_u64().unwrap();
    let (st, cfg) = call(app, "PUT", "/api/grab/config", Some(json!({"sab": {"url": format!("{base}/sab"), "api_key": "SABKEY1234", "category": "xbox360"}, "path_maps": [{"from": "/sab-data", "to": e.root.to_string_lossy()}], "import_library": [lid, pid], "also_drive": [dlid, dpid], "also_console": {"console_id": cid}, "auto_grab": false, "auto_import": true}))).await;
    assert_eq!(st, StatusCode::OK, "{cfg}");
    assert!(
        !cfg.to_string().contains("SABKEY1234") && !cfg.to_string().contains("SECRETKEY99"),
        "no secret in the config the browser sees"
    );
    let (st, _) = call(
        app,
        "POST",
        "/api/grab/sab/category",
        Some(json!({"name": "xbox360"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(sab.lock().unwrap().cats.contains(&"xbox360".to_string()));

    // Add a game from IGDB (mock) and search.
    let (_, found) = call(app, "GET", "/api/wanted/lookup?q=Halo%203", None).await;
    assert!(
        found["games"].as_array().is_some_and(|g| !g.is_empty()),
        "{found}"
    );
    let igdb_id = found["games"][0]["candidate"]["id"].as_i64().unwrap();
    let (st, v) = call(
        app,
        "POST",
        "/api/wanted",
        Some(json!({"igdb_id": igdb_id, "candidate": found["games"][0]["candidate"]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let wid = v["id"].as_i64().unwrap();
    let (st, _) = call(
        app,
        "POST",
        "/api/wanted",
        Some(json!({"igdb_id": igdb_id, "candidate": found["games"][0]["candidate"]})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "a game is wanted once");
    let (st, s) = call(app, "POST", &format!("/api/wanted/{wid}/search"), None).await;
    assert_eq!(st, StatusCode::OK, "{s}");
    assert_eq!(
        s["searched"],
        2 - 1 + 1,
        "the indexer with no Xbox 360 category is skipped, Prowlarr's is searched: {s}"
    );
    let results = s["results"].as_array().unwrap();
    let ok: Vec<&str> = results
        .iter()
        .filter(|r| r["verdict"]["rejected"].as_array().unwrap().is_empty())
        .map(|r| r["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        ok[0], "Halo.3.USA.XBOX360-ZRY",
        "USA ranks first under the default profile: {ok:?}"
    );
    assert!(
        ok.contains(&"Halo.3.PAL.XBOX360-GAC") && ok.contains(&"Halo 3 XBOX360-CCCLX"),
        "{ok:?}"
    );
    assert_eq!(
        ok.len(),
        3,
        "ODST, Reach, the PS3 one and the fragment are all refused: {ok:?}"
    );
    let rejected: Vec<String> = results
        .iter()
        .filter(|r| !r["verdict"]["rejected"].as_array().unwrap().is_empty())
        .map(|r| r["verdict"]["rejected"].to_string())
        .collect();
    assert!(
        rejected.iter().any(|r| r.contains("PS3"))
            && rejected.iter().any(|r| r.contains("fragment"))
            && rejected
                .iter()
                .filter(|r| r.contains("Not this game"))
                .count()
                >= 2,
        "{rejected:?}"
    );
    assert!(
        !s.to_string().contains("SECRETKEY99"),
        "the NZB link (with the indexer's key) never reaches the browser"
    );

    // A rejected release can't be grabbed unless forced; an accepted one goes to SABnzbd.
    let bad = results
        .iter()
        .find(|r| r["title"] == "Halo.3.PS3-GRP")
        .unwrap();
    let (st, _) = call(
        app,
        "POST",
        &format!("/api/wanted/{wid}/grab"),
        Some(json!({"guid": bad["guid"], "indexer_id": bad["indexer_id"]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, v) = call(app, "POST", &format!("/api/wanted/{wid}/auto"), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["grabbed"], "Halo.3.USA.XBOX360-ZRY");
    let added = sab.lock().unwrap().added.clone();
    assert_eq!(added.len(), 1);
    assert!(
        added[0].0.contains("g5.nzb")
            && added[0].1 == "Halo.3.USA.XBOX360-ZRY"
            && added[0].2 == "xbox360",
        "{added:?}"
    );
    let (st, _) = call(app, "POST", &format!("/api/wanted/{wid}/auto"), None).await;
    assert_eq!(st, StatusCode::CONFLICT, "not downloaded twice");

    // Following it: queued, downloading at 40%, then complete with a folder SABnzbd reports.
    sab.lock().unwrap().queue = vec![
        json!({"nzo_id": "SABnzbd_nzo_1", "filename": "Halo.3.USA.XBOX360-ZRY", "status": "Downloading", "percentage": "40", "mb": "7300", "mbleft": "4380", "timeleft": "0:20:00"}),
    ];
    web::poll_downloads(&e.state).await;
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(
        (
            a["grabs"][0]["grab"]["status"].as_str(),
            a["grabs"][0]["grab"]["progress"].as_f64()
        ),
        (Some("downloading"), Some(40.0)),
        "{a}"
    );
    let dl = e.root.join("downloads/Halo.3.USA.XBOX360-ZRY");
    fs::create_dir_all(&dl).unwrap();
    write_iso(&dl.join("halo3.iso"), 0x4D53_07E6, 0x1CDE_207A, 1, 1);
    fs::write(dl.join("halo3.nfo"), b"info").unwrap();
    {
        let mut g = sab.lock().unwrap();
        g.queue.clear();
        g.history = vec![
            json!({"nzo_id": "SABnzbd_nzo_1", "name": "Halo.3.USA.XBOX360-ZRY", "status": "Completed", "storage": "/sab-data/downloads/Halo.3.USA.XBOX360-ZRY", "fail_message": ""}),
        ];
    }
    web::poll_downloads(&e.state).await;
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(
        a["grabs"][0]["grab"]["status"], "importing",
        "SABnzbd's path was mapped to ours and the import started: {a}"
    );
    let job = a["grabs"][0]["grab"]["import_job"].as_u64().unwrap();
    assert_eq!(wait_job(app, job).await["job"]["status"], "done");
    web::poll_downloads(&e.state).await;
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(a["grabs"][0]["grab"]["status"], "imported", "{a}");
    assert!(
        e.root.join("isos/halo3.iso").is_file() && !dl.join("halo3.iso").exists(),
        "moved into the library"
    );
    // ...and copied on to the "drive" in the same job, then sent to the console in a follow-up job.
    assert_eq!(
        fs::metadata(e.root.join("drivecopy/halo3.iso"))
            .unwrap()
            .len(),
        fs::metadata(e.root.join("isos/halo3.iso")).unwrap().len()
    );
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    let extra = a["grabs"][0]["grab"]["extra_job"]
        .as_u64()
        .unwrap_or_else(|| panic!("a job sent it to the console: {a}"));
    assert_eq!(wait_job(app, extra).await["job"]["status"], "done");
    let on_console = e
        .root
        .parent()
        .unwrap()
        .join("config/mock-console/Hdd1/Games/halo3.iso");
    assert_eq!(
        fs::metadata(&on_console).unwrap().len(),
        fs::metadata(e.root.join("isos/halo3.iso")).unwrap().len(),
        "on the console: {}",
        on_console.display()
    );
    let (_, w) = call(app, "GET", "/api/wanted", None).await;
    assert_eq!(w["items"][0]["wanted"]["status"], "downloaded", "{w}");

    // A second game whose download fails is blocklisted and the search skips that release next time.
    let (_, found) = call(app, "GET", "/api/wanted/lookup?q=Fable%20II", None).await;
    let (_, v) = call(app, "POST", "/api/wanted", Some(json!({"igdb_id": found["games"][0]["candidate"]["id"], "candidate": found["games"][0]["candidate"]}))).await;
    let fid = v["id"].as_i64().unwrap();
    let (_, v) = call(app, "POST", &format!("/api/wanted/{fid}/auto"), None).await;
    assert_eq!(v["grabbed"], "Fable.II.USA.XBOX360-GRP", "{v}");
    sab.lock().unwrap().history = vec![
        json!({"nzo_id": "SABnzbd_nzo_2", "name": "Fable.II.USA.XBOX360-GRP", "status": "Failed", "storage": "", "fail_message": "Aborted, cannot be completed: missing articles"}),
    ];
    web::poll_downloads(&e.state).await;
    let (_, a) = call(app, "GET", "/api/grab/activity", None).await;
    assert_eq!(
        (
            a["grabs"][0]["grab"]["status"].as_str(),
            a["grabs"][0]["grab"]["error"]
                .as_str()
                .map(|e| e.contains("missing articles"))
        ),
        (Some("failed"), Some(true)),
        "{a}"
    );
    let (_, b) = call(app, "GET", "/api/grab/blocklist", None).await;
    assert_eq!(b["blocked"][0]["title"], "Fable.II.USA.XBOX360-GRP");
    let (_, s) = call(app, "POST", &format!("/api/wanted/{fid}/search"), None).await;
    assert!(
        s["results"][0]["verdict"]["rejected"]
            .to_string()
            .contains("blocklist"),
        "{s}"
    );
    let (_, v) = call(app, "POST", &format!("/api/wanted/{fid}/auto"), None).await;
    assert_eq!(v["grabbed"], Value::Null, "nothing acceptable is left");
    let (_, w) = call(app, "GET", "/api/wanted", None).await;
    let fable = w["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["wanted"]["id"] == fid)
        .unwrap();
    assert_eq!(
        fable["wanted"]["status"], "wanted",
        "back on the list: {fable}"
    );
}

#[tokio::test]
async fn discover_browses_filters_shows_similar_games_and_grabs_to_usenet() {
    let (base, sab) = fake_services().await;
    let e = env("discover");
    let app = &e.app;
    // Browse: popular first, filters, search, paging.
    let (st, g) = call(app, "GET", "/api/discover", None).await;
    assert_eq!(st, StatusCode::OK, "{g}");
    let games = g["games"].as_array().unwrap();
    assert_eq!(
        (games.len(), g["has_more"].as_bool(), g["page"].as_u64()),
        (24, Some(true), Some(1)),
        "{g}"
    );
    assert!(games[0]["rating"].as_f64() >= games[10]["rating"].as_f64());
    assert!(
        games[0]["cover"]
            .as_str()
            .unwrap()
            .starts_with("data:image/svg"),
        "mock covers are inline"
    );
    let (_, p2) = call(app, "GET", "/api/discover?page=2", None).await;
    assert_eq!(p2["has_more"], false);
    assert_eq!(p2["games"].as_array().unwrap().len(), 6);
    let (_, genres) = call(app, "GET", "/api/discover/genres", None).await;
    let racing = genres["genres"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "Racing")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let (_, r) = call(
        app,
        "GET",
        &format!("/api/discover?genre={racing}&sort=newest"),
        None,
    )
    .await;
    let names: Vec<&str> = r["games"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 4, "{names:?}");
    assert_eq!(names[0], "Forza Motorsport 4", "newest first");
    let (_, y) = call(app, "GET", "/api/discover?years=2010-2011", None).await;
    assert!(
        y["games"].as_array().unwrap().iter().all(|x| {
            let t = x["year"].as_i64().unwrap();
            (1_262_304_000..1_325_376_000).contains(&t)
        }),
        "{y}"
    );
    let (_, s) = call(app, "GET", "/api/discover?q=gears", None).await;
    assert_eq!(s["games"].as_array().unwrap().len(), 3);
    assert_eq!(s["relevance"], true);
    // The richer filters: lists with counts, themes, series, co-op, hiding, suggestions, random.
    let (_, fx) = call(app, "GET", "/api/discover/facets", None).await;
    for k in ["genre", "theme", "mode", "persp", "age"] {
        assert!(!fx[k].as_array().unwrap().is_empty(), "{k}: {fx}");
    }
    let horror = fx["theme"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "Horror")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let (_, h) = call(
        app,
        "GET",
        &format!("/api/discover?theme={horror}&sort=rating"),
        None,
    )
    .await;
    assert_eq!(h["total"], h["games"].as_array().unwrap().len(), "{h}");
    assert!(h["total"].as_i64().unwrap() > 0);
    let (_, cnt) = call(app, "GET", "/api/discover/facets/theme/counts", None).await;
    assert_eq!(cnt["counts"][horror.to_string()], h["total"], "{cnt}");
    let (st, _) = call(app, "GET", "/api/discover/facets/bogus/counts", None).await;
    assert_ne!(st, StatusCode::OK);
    let (_, sg) = call(app, "GET", "/api/discover/suggest?kind=series&q=gea", None).await;
    let gears = sg["results"][0]["id"].as_i64().unwrap();
    let (_, gw) = call(app, "GET", &format!("/api/discover?series={gears}"), None).await;
    assert_eq!(gw["games"].as_array().unwrap().len(), 3, "{gw}");
    let (_, lc) = call(
        app,
        "GET",
        "/api/discover?local_coop=1&hide=owned,wanted",
        None,
    )
    .await;
    assert!(lc["total"].as_i64().unwrap() > 0, "{lc}");
    let (st, rnd) = call(
        app,
        "GET",
        &format!("/api/discover/random?series={gears}"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(rnd["id"].is_i64(), "{rnd}");
    let first = gw["games"][0]["id"].as_i64().unwrap();
    let (_, mr) = call(
        app,
        "GET",
        &format!("/api/discover/{first}/more?series={gears}&series_name=Gears"),
        None,
    )
    .await;
    assert_eq!(
        mr["shelves"][0]["games"].as_array().unwrap().len(),
        2,
        "the other two, not itself: {mr}"
    );
    let (st, _) = call(app, "GET", "/api/discover?sort=bogus", None).await;
    assert_ne!(st, StatusCode::OK, "an unknown sort is refused");

    // A game's page: details, screenshots, similar games on the Xbox 360.
    let halo = games.iter().find(|x| x["name"] == "Halo 3").unwrap()["id"]
        .as_i64()
        .unwrap();
    let (st, d) = call(app, "GET", &format!("/api/discover/{halo}"), None).await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["game"]["name"], "Halo 3");
    assert!(
        d["storyline"].is_string()
            && d["screenshots"].as_array().unwrap().len() == 3
            && d["modes"].as_array().unwrap().len() >= 2
    );
    let similar = d["similar"].as_array().unwrap();
    assert!(
        !similar.is_empty()
            && similar
                .iter()
                .all(|x| x["genres"] == d["game"]["genres"] && x["id"] != d["game"]["id"]),
        "{d}"
    );
    assert_eq!(d["game"]["status"]["state"], "none");

    // "Want it": on the list, and the status follows it everywhere.
    let (st, v) = call(
        app,
        "POST",
        &format!("/api/discover/{halo}/grab"),
        Some(json!({"mode": "add"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        (v["added"].as_bool(), v["grabbed"].is_null()),
        (Some(true), true)
    );
    let (_, d) = call(app, "GET", &format!("/api/discover/{halo}"), None).await;
    assert_eq!(d["game"]["status"]["state"], "wanted");
    let (_, v) = call(
        app,
        "POST",
        &format!("/api/discover/{halo}/grab"),
        Some(json!({"mode": "add"})),
    )
    .await;
    assert_eq!(v["added"], false, "adding again is harmless");
    let (st, _) = call(
        app,
        "POST",
        &format!("/api/discover/{halo}/grab"),
        Some(json!({"mode": "teleport"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Set up Usenet (fake indexer + SABnzbd), then "grab best" from the same page.
    let (_, v) = call(
        app,
        "POST",
        "/api/grab/indexers",
        Some(json!({"name": "Good", "url": format!("{base}/good"), "api_key": "SECRETKEY99"})),
    )
    .await;
    call(
        app,
        "POST",
        "/api/grab/indexers/test",
        Some(json!({"id": v["id"]})),
    )
    .await;
    call(app, "PUT", "/api/grab/config", Some(json!({"sab": {"url": format!("{base}/sab"), "api_key": "SABKEY1234", "category": "xbox360"}}))).await;
    let (st, v) = call(
        app,
        "POST",
        &format!("/api/discover/{halo}/grab"),
        Some(json!({"mode": "best"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        (v["added"].as_bool(), v["grabbed"].as_str()),
        (Some(false), Some("Halo.3.USA.XBOX360-ZRY")),
        "{v}"
    );
    assert_eq!(sab.lock().unwrap().added.len(), 1);
    let (_, g) = call(app, "GET", "/api/discover?q=halo", None).await;
    let h = g["games"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "Halo 3")
        .unwrap();
    assert_eq!(h["status"]["state"], "downloading", "{h}");
    let (st, _) = call(
        app,
        "POST",
        &format!("/api/discover/{halo}/grab"),
        Some(json!({"mode": "best"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "already downloading");
    // A game with nothing on Usenet: it is added, and nothing is grabbed.
    let (_, aw) = call(app, "GET", "/api/discover?q=alan", None).await;
    let other = aw["games"][0]["id"].as_i64().unwrap();
    let (_, v) = call(
        app,
        "POST",
        &format!("/api/discover/{other}/grab"),
        Some(json!({"mode": "best"})),
    )
    .await;
    assert_eq!(
        (v["added"].as_bool(), v["grabbed"].is_null()),
        (Some(true), true),
        "{v}"
    );
}
