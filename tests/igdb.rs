use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use rustybox::{
    igdb::Urls,
    web::{self, Config},
    xbox::{xex, xiso},
};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A stand-in for Twitch and IGDB on localhost.
struct Fake {
    port: u16,
    token_calls: Arc<AtomicUsize>,
    api_calls: Arc<AtomicUsize>,
}

fn game(id: i64, name: &str, cover: &str) -> Value {
    json!({"id": id, "name": name, "summary": format!("About {name}"), "first_release_date": 1273795200, "cover": {"image_id": cover},
           "genres": [{"name": "Shooter"}], "total_rating": 81.5, "url": format!("https://www.igdb.com/games/{id}"),
           "involved_companies": [{"company": {"name": "Remedy"}, "developer": true, "publisher": false}, {"company": {"name": "Microsoft"}, "developer": false, "publisher": true}]})
}

fn fake_igdb() -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (token_calls, api_calls) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (t, a) = (token_calls.clone(), api_calls.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (t, a) = (t.clone(), a.clone());
            std::thread::spawn(move || handle(stream, t, a));
        }
    });
    Fake {
        port,
        token_calls,
        api_calls,
    }
}

fn handle(mut s: std::net::TcpStream, tokens: Arc<AtomicUsize>, api: Arc<AtomicUsize>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_end, len) = loop {
        let n = s.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (p + 4, len);
        }
    };
    while buf.len() < head_end + len {
        let n = s.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
    let request_line = head.lines().next().unwrap_or("").to_string();
    let header = |name: &str| {
        head.lines().find_map(|l| {
            l.to_lowercase()
                .strip_prefix(&format!("{name}:"))
                .map(|v| v.trim().to_string())
        })
    };

    let (status, ctype, payload): (u16, &str, Vec<u8>) = if request_line
        .starts_with("POST /oauth2/token")
    {
        tokens.fetch_add(1, Ordering::SeqCst);
        if body.contains("client_secret=bad") {
            (
                400,
                "application/json",
                br#"{"status":400,"message":"invalid client secret"}"#.to_vec(),
            )
        } else {
            (
                200,
                "application/json",
                br#"{"access_token":"tok123","expires_in":5000000,"token_type":"bearer"}"#.to_vec(),
            )
        }
    } else if request_line.starts_with("POST /v4/games") {
        api.fetch_add(1, Ordering::SeqCst);
        if header("authorization").as_deref() != Some("bearer tok123")
            || header("client-id").is_none()
        {
            (401, "application/json", b"{}".to_vec())
        } else if let Some(q) = body
            .split("search \"")
            .nth(1)
            .and_then(|r| r.split('"').next())
        {
            let v = match q.to_lowercase().as_str() {
                // The DLC comes first, as IGDB often returns it.
                "alan wake" => json!([
                    game(2, "Alan Wake: The Signal", "co2sig"),
                    game(1, "Alan Wake", "co1aw")
                ]),
                "forza motorsport 4" => json!([game(3, "Totally Unrelated Game", "co3x")]),
                _ => json!([]),
            };
            (200, "application/json", v.to_string().into_bytes())
        } else if let Some(id) = body
            .split("where id = ")
            .nth(1)
            .and_then(|r| r.trim_end_matches(';').trim().parse::<i64>().ok())
        {
            (
                200,
                "application/json",
                json!([game(id, &format!("Chosen {id}"), "co9chosen")])
                    .to_string()
                    .into_bytes(),
            )
        } else {
            (
                200,
                "application/json",
                br#"[{"id":1,"name":"x"}]"#.to_vec(),
            )
        }
    } else if request_line.starts_with("GET /images/t_cover_big/") {
        (200, "image/jpeg", b"FAKEJPEGBYTES".to_vec())
    } else {
        (404, "text/plain", b"nope".to_vec())
    };
    let _ = write!(
        s,
        "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = s.write_all(&payload);
}

struct Env {
    app: Router,
    root: PathBuf,
}

fn env(name: &str, fake: Option<&Fake>, dotenv: &[(&str, &str)]) -> Env {
    let base = std::env::temp_dir().join(format!("rustybox_igdb_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("data/isos")).unwrap();
    let root = std::fs::canonicalize(base.join("data")).unwrap();
    let cfg = Config {
        config_dir: base.join("config"),
        roots: vec![root.clone()],
        abgx360: None,
        dotenv: dotenv
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>(),
        igdb_urls: fake.map(|f| Urls {
            token: format!("http://127.0.0.1:{}/oauth2/token", f.port),
            api: format!("http://127.0.0.1:{}/v4", f.port),
            images: format!("http://127.0.0.1:{}/images", f.port),
        }),
        mock: false,
        auth: None,
    };
    Env {
        app: web::router(web::build_state(cfg).unwrap()),
        root,
    }
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (s, _, b) = call_raw(app, method, uri, body).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

async fn call_raw(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
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
    let (s, h) = (res.status(), res.headers().clone());
    (
        s,
        h,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

fn write_iso(path: &Path, title: u32, media: u32) {
    let mut bytes = xiso::build_simple_disc(&xex::build(media, title, 1, 1));
    bytes.extend(vec![1u8; 10_000]);
    while !bytes.len().is_multiple_of(2048) {
        bytes.push(0);
    }
    std::fs::write(path, bytes).unwrap();
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

/// Wait for every job (a scan can start a lookup job after it) to finish.
async fn settle(app: &Router) {
    for _ in 0..400 {
        let (_, jobs) = call(app, "GET", "/api/jobs", None).await;
        if jobs.as_array().unwrap().iter().all(|j| {
            matches!(
                j["status"].as_str().unwrap(),
                "done" | "failed" | "cancelled"
            )
        }) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("jobs did not settle");
}

async fn library_with_games(e: &Env) -> i64 {
    // 4D530805 Alan Wake, 4D530910 Forza Motorsport 4 (the fake IGDB only has an unrelated result for it).
    write_iso(&e.root.join("isos/Alan Wake.iso"), 0x4D53_0805, 0x54E3_4DF4);
    write_iso(&e.root.join("isos/Forza.iso"), 0x4D53_0910, 0x61D6_2D9F);
    let (s, v) = call(&e.app, "POST", "/api/libraries", Some(json!({"name": "ISOs", "kind": "iso", "paths": [{"path": e.root.join("isos").to_string_lossy(), "writable": false}]}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["id"].as_i64().unwrap()
}

#[tokio::test]
async fn credentials_are_checked_before_and_after_saving_and_the_secret_never_comes_back() {
    let fake = fake_igdb();
    let e = env("creds", Some(&fake), &[]);

    let (_, st) = call(&e.app, "GET", "/api/igdb/status", None).await;
    assert_eq!(st["configured"], false);
    // Nothing configured: a clear refusal.
    let (s, v) = call(&e.app, "POST", "/api/igdb/test", Some(json!({}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["message"].as_str().unwrap().contains("Settings"));

    // Credentials typed into the form can be tried without saving them.
    let (s, v) = call(
        &e.app,
        "POST",
        "/api/igdb/test",
        Some(json!({"client_id": "myid", "client_secret": "bad"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(
        v["message"].as_str().unwrap().contains("didn't accept"),
        "{v}"
    );
    let (s, v) = call(
        &e.app,
        "POST",
        "/api/igdb/test",
        Some(json!({"client_id": "myid", "client_secret": "goodsecret1234"})),
    )
    .await;
    assert_eq!(
        (s, v["ok"].clone(), v["token_days"].as_u64()),
        (StatusCode::OK, json!(true), Some(57))
    );
    assert_eq!(
        call(&e.app, "GET", "/api/igdb/status", None).await.1["configured"],
        false,
        "testing doesn't save"
    );

    // Save them: the secret is never returned, only that it is set and its last four characters.
    let (s, v) = call(
        &e.app,
        "PUT",
        "/api/settings",
        Some(json!({"igdb_client_id": "myid", "igdb_client_secret": "goodsecret1234"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(v.get("igdb_client_secret").is_none());
    assert_eq!(
        (
            v["igdb_client_secret_set"].clone(),
            v["igdb_client_secret_hint"].as_str(),
            v["igdb_client_id"].as_str()
        ),
        (json!(true), Some("••••1234"), Some("myid"))
    );
    assert!(
        !v.to_string().contains("goodsecret"),
        "the secret must not appear anywhere in the response"
    );
    assert!(
        !call(&e.app, "GET", "/api/settings", None)
            .await
            .1
            .to_string()
            .contains("goodsecret")
    );
    let st = call(&e.app, "GET", "/api/igdb/status", None).await.1;
    assert_eq!(
        (st["configured"].clone(), st["source"].as_str()),
        (json!(true), Some("settings"))
    );
    // Saved credentials are used when none are typed, and saving without a secret keeps the old one.
    assert_eq!(
        call(&e.app, "POST", "/api/igdb/test", Some(json!({})))
            .await
            .0,
        StatusCode::OK
    );
    let (_, v) = call(
        &e.app,
        "PUT",
        "/api/settings",
        Some(json!({"igdb_client_id": "newid"})),
    )
    .await;
    assert_eq!(v["igdb_client_secret_set"], true);
    // Removing them.
    let (_, v) = call(
        &e.app,
        "PUT",
        "/api/settings",
        Some(json!({"igdb_remove": true})),
    )
    .await;
    assert_eq!(
        (
            v["igdb_client_secret_set"].clone(),
            v["igdb"]["configured"].clone()
        ),
        (json!(false), json!(false))
    );
}

#[tokio::test]
async fn the_environment_or_a_dotenv_file_wins_over_saved_settings() {
    let fake = fake_igdb();
    // The development file uses these names.
    let e = env(
        "dotenv",
        Some(&fake),
        &[
            ("clientid", "fromfileid1234"),
            ("clientsecret", "fromfilesecret5678"),
        ],
    );
    call(
        &e.app,
        "PUT",
        "/api/settings",
        Some(json!({"igdb_client_id": "savedid", "igdb_client_secret": "savedsecret"})),
    )
    .await;
    let st = call(&e.app, "GET", "/api/igdb/status", None).await.1;
    assert_eq!(
        (st["source"].as_str(), st["client_id_hint"].as_str()),
        (Some("environment"), Some("••••1234"))
    );
    assert_eq!(
        call(&e.app, "POST", "/api/igdb/test", Some(json!({})))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn games_are_matched_with_covers_and_dlc_is_not_mistaken_for_the_game() {
    let fake = fake_igdb();
    let e = env("match", Some(&fake), &[]);
    call(
        &e.app,
        "PUT",
        "/api/settings",
        Some(json!({"igdb_client_id": "id", "igdb_client_secret": "secret"})),
    )
    .await;
    let lib = library_with_games(&e).await;

    // A scan finds the games, and (automatic lookups are on) a lookup follows by itself.
    let (_, v) = call(&e.app, "POST", &format!("/api/libraries/{lib}/scan"), None).await;
    wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    settle(&e.app).await;

    let (_, g) = call(&e.app, "GET", "/api/games", None).await;
    let games = g["games"].as_array().unwrap();
    let wake = games.iter().find(|x| x["title_id"] == "4D530805").unwrap();
    let info = &wake["info"];
    assert_eq!(
        (
            info["igdb_id"].as_i64(),
            info["name"].as_str(),
            info["status"].as_str()
        ),
        (Some(1), Some("Alan Wake"), Some("ok")),
        "the right game, not 'The Signal': {wake}"
    );
    assert_eq!(
        (
            info["developers"][0].as_str(),
            info["publishers"][0].as_str(),
            info["genres"][0].as_str()
        ),
        (Some("Remedy"), Some("Microsoft"), Some("Shooter"))
    );
    assert_eq!(info["summary"], "About Alan Wake");

    // The cover was downloaded once and is served from RustyBox.
    let cover = info["cover"].as_str().unwrap();
    let (s, h, bytes) = call_raw(&e.app, "GET", &format!("/api/covers/{cover}"), None).await;
    assert_eq!(
        (
            s,
            bytes.as_slice(),
            h[header::CONTENT_TYPE].to_str().unwrap()
        ),
        (StatusCode::OK, b"FAKEJPEGBYTES".as_slice(), "image/jpeg")
    );

    // An unrelated result is refused, and the miss is remembered.
    let forza = games.iter().find(|x| x["title_id"] == "4D530910").unwrap();
    assert!(forza["info"].is_null());
    let (_, m) = call(&e.app, "GET", "/api/games/4D530910/info", None).await;
    assert_eq!(m["status"], "none");

    // One token served all the lookups, and nothing is looked up twice.
    assert_eq!(fake.token_calls.load(Ordering::SeqCst), 1);
    let before = fake.api_calls.load(Ordering::SeqCst);
    let (s, v) = call(&e.app, "POST", "/api/igdb/fetch", Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK);
    let j = wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    assert_eq!(j["job"]["status"], "done");
    assert_eq!(
        fake.api_calls.load(Ordering::SeqCst),
        before,
        "everything already had its answer"
    );
}

#[tokio::test]
async fn a_match_chosen_by_hand_is_kept_and_covers_cannot_escape() {
    let fake = fake_igdb();
    let e = env("manual", Some(&fake), &[]);
    call(
        &e.app,
        "PUT",
        "/api/settings",
        Some(json!({"igdb_client_id": "id", "igdb_client_secret": "secret", "igdb_auto": false})),
    )
    .await;
    let lib = library_with_games(&e).await;
    let (_, v) = call(&e.app, "POST", &format!("/api/libraries/{lib}/scan"), None).await;
    wait_job(&e.app, v["job"].as_u64().unwrap()).await;
    settle(&e.app).await;
    assert!(
        call(&e.app, "GET", "/api/games/4D530910/info", None)
            .await
            .1
            .is_null(),
        "automatic lookups were off"
    );

    // Search, then choose.
    let (s, found) = call(
        &e.app,
        "POST",
        "/api/igdb/search",
        Some(json!({"q": "Alan Wake"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(found[0]["name"], "Alan Wake: The Signal");
    assert!(
        found[0]["cover_url"]
            .as_str()
            .unwrap()
            .contains("/t_cover_small/co2sig.jpg")
    );
    let (s, m) = call(
        &e.app,
        "PUT",
        "/api/games/4D530910/info",
        Some(json!({"igdb_id": 4242})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{m}");
    assert_eq!(
        (
            m["status"].as_str(),
            m["name"].as_str(),
            m["igdb_id"].as_i64()
        ),
        (Some("manual"), Some("Chosen 4242"), Some(4242))
    );

    // Looking the game up again doesn't undo a human's choice.
    let (_, r) = call(&e.app, "POST", "/api/games/4D530910/info/refresh", None).await;
    assert_eq!(
        (r["status"].as_str(), r["name"].as_str()),
        (Some("manual"), Some("Chosen 4242"))
    );
    // Clearing it lets automatic matching have another go.
    assert_eq!(
        call(&e.app, "DELETE", "/api/games/4D530910/info", None)
            .await
            .0,
        StatusCode::OK
    );
    assert!(
        call(&e.app, "GET", "/api/games/4D530910/info", None)
            .await
            .1
            .is_null()
    );

    // Bad input.
    assert_eq!(
        call(&e.app, "GET", "/api/games/nothex/info", None).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &e.app,
            "PUT",
            "/api/games/4D530910/info",
            Some(json!({"igdb_id": 0}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for bad in [
        "..%2F..%2Fetc%2Fpasswd",
        "%2E%2E%2Fsecret.jpg",
        "x.png",
        "noext",
        ".jpg",
        "a.b.jpg",
    ] {
        assert_eq!(
            call_raw(&e.app, "GET", &format!("/api/covers/{bad}"), None)
                .await
                .0,
            StatusCode::NOT_FOUND,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn mock_mode_works_without_credentials_or_network() {
    let base = std::env::temp_dir().join(format!("rustybox_igdb_mock_{}", std::process::id()));
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
    let app = web::router(web::build_state(cfg).unwrap());
    let (_, libs) = call(&app, "GET", "/api/libraries", None).await;
    for l in libs.as_array().unwrap() {
        let (_, v) = call(
            &app,
            "POST",
            &format!("/api/libraries/{}/scan", l["id"]),
            None,
        )
        .await;
        wait_job(&app, v["job"].as_u64().unwrap()).await;
    }
    settle(&app).await;
    let (_, g) = call(&app, "GET", "/api/games", None).await;
    let with_info = g["games"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["info"]["cover"].is_string())
        .count();
    assert!(with_info >= 4, "mock games get sample details: {g}");
    let cover = g["games"][1]["info"]["cover"].as_str().unwrap();
    let (s, h, _) = call_raw(&app, "GET", &format!("/api/covers/{cover}"), None).await;
    assert_eq!(
        (s, h[header::CONTENT_TYPE].to_str().unwrap()),
        (StatusCode::OK, "image/svg+xml")
    );
}
