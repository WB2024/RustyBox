//! A drive shared from a web browser. The browser is played by a loop that answers the server's
//! jobs with the real drive agent, so everything RustyBox does to a drive agent is checked to work
//! the same through the browser relay, including scanning (which the relay does piece by piece).

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    http::{Method, Request},
};
use http_body_util::BodyExt;
use rustybox::{
    agent::{self, AgentState},
    library::scan::{self, MetaState, Scanner, Walk},
    remote::Remote,
    web::{self, Config},
    xbox::{stfs, xex, xiso},
};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

struct Setup {
    root: PathBuf,
    port: u16,
}

fn make_god(dir: &std::path::Path, title: u32, name: &str, parts: u32) {
    let ct = dir.join(format!("{title:08X}")).join("00007000");
    fs::create_dir_all(ct.join("ABCDEF01.data")).unwrap();
    let mut c = stfs::build(b"LIVE", 0x7000, title, 1, name, name);
    c.resize(0x3A8, 0);
    c[0x3A0..0x3A4].copy_from_slice(&parts.to_le_bytes());
    fs::write(ct.join("ABCDEF01"), c).unwrap();
    for i in 0..parts.min(2) {
        fs::write(
            ct.join(format!("ABCDEF01.data/Data{i:04}")),
            vec![7u8; 5000],
        )
        .unwrap();
    }
}

async fn start(name: &str) -> Setup {
    let base = std::env::temp_dir().join(format!("rustybox_browser_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let root = base.join("drive");
    fs::create_dir_all(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let cfg = Config {
        config_dir: base.join("config"),
        roots: vec![],
        abgx360: None,
        dotenv: Default::default(),
        igdb_urls: None,
        mock: false,
        auth: None,
    };
    let state = web::build_state(cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    state
        .listen_port
        .store(port, std::sync::atomic::Ordering::Relaxed);
    let app = web::router(state);
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    Setup { root, port }
}

/// The "browser": poll for jobs, answer each with the real agent over this folder.
fn browser(port: u16, id: String, token: String, root: PathBuf, workers: usize) {
    let agent = agent::router(Arc::new(
        AgentState::new(&root, TOKEN.into(), "drive".into(), false).unwrap(),
    ));
    for _ in 0..workers {
        let (id, token, agent) = (id.clone(), token.clone(), agent.clone());
        tokio::spawn(async move {
            let base = format!("http://127.0.0.1:{port}/api/browser-agent/{id}");
            loop {
                let (b, t) = (base.clone(), token.clone());
                let job = tokio::task::spawn_blocking(move || {
                    ureq::get(&format!("{b}/poll"))
                        .set("Authorization", &format!("Bearer {t}"))
                        .call()
                        .map_err(|e| e.to_string())
                })
                .await
                .unwrap();
                let Ok(res) = job else { continue };
                if res.status() == 204 {
                    continue;
                }
                let j: Value = res.into_json().unwrap();
                let jid = j["job"].as_u64().unwrap();
                let body = if j["body_len"].as_u64().unwrap() > 0 {
                    let (b, t) = (base.clone(), token.clone());
                    tokio::task::spawn_blocking(move || {
                        let mut v = Vec::new();
                        std::io::Read::read_to_end(
                            &mut ureq::get(&format!("{b}/job/{jid}/body"))
                                .set("Authorization", &format!("Bearer {t}"))
                                .call()
                                .unwrap()
                                .into_reader(),
                            &mut v,
                        )
                        .unwrap();
                        v
                    })
                    .await
                    .unwrap()
                } else {
                    vec![]
                };
                let uri = format!(
                    "/agent/v1/{}?{}",
                    j["path"].as_str().unwrap(),
                    j["query"].as_str().unwrap()
                );
                let mut rq = Request::builder()
                    .method(Method::from_bytes(j["method"].as_str().unwrap().as_bytes()).unwrap())
                    .uri(uri)
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .header("content-type", "application/json");
                if let Some(r) = j["range"].as_str() {
                    rq = rq.header("range", r);
                }
                let res = agent
                    .clone()
                    .oneshot(rq.body(Body::from(body)).unwrap())
                    .await
                    .unwrap();
                let (status, ctype, crange) = (
                    res.status().as_u16(),
                    res.headers()
                        .get("content-type")
                        .map(|v| v.to_str().unwrap().to_string())
                        .unwrap_or_default(),
                    res.headers()
                        .get("content-range")
                        .map(|v| v.to_str().unwrap().to_string()),
                );
                let data = res.into_body().collect().await.unwrap().to_bytes();
                let (b, t) = (base.clone(), token.clone());
                tokio::task::spawn_blocking(move || {
                    let mut r = ureq::post(&format!("{b}/job/{jid}/answer"))
                        .set("Authorization", &format!("Bearer {t}"))
                        .set("x-agent-status", &status.to_string())
                        .set("x-agent-content-type", &ctype);
                    if let Some(c) = crange {
                        r = r.set("x-agent-content-range", &c);
                    }
                    let _ = r.send_bytes(&data);
                })
                .await
                .unwrap();
            }
        });
    }
}

async fn register(port: u16, name: &str) -> Value {
    let n = name.to_string();
    tokio::task::spawn_blocking(move || {
        ureq::post(&format!(
            "http://127.0.0.1:{port}/api/browser-agent/register"
        ))
        .send_json(json!({"name": n}))
        .unwrap()
        .into_json::<Value>()
        .unwrap()
    })
    .await
    .unwrap()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

#[tokio::test]
async fn a_drive_in_a_browser_is_listed_read_scanned_and_written_like_an_agent() {
    let s = start("main").await;
    make_god(&s.root.join("Games/Alan Wake"), 0x4D53_0805, "Alan Wake", 2);
    make_god(&s.root.join("Games/Broken"), 0x4D53_0806, "Broken", 5);
    let mut iso = xiso::build_simple_disc(&xex::build(0x08FE_F3C1, 0x4653_07D3, 1, 1));
    iso.extend((0..600_000u32).map(|i| (i % 251) as u8));
    fs::create_dir_all(s.root.join("ISOs")).unwrap();
    fs::write(s.root.join("ISOs/Game.iso"), &iso).unwrap();
    fs::write(s.root.join("ISOs/readme.txt"), b"hello").unwrap();

    let reg = register(s.port, "Will's laptop").await;
    let (id, token, url) = (
        reg["id"].as_str().unwrap().to_string(),
        reg["token"].as_str().unwrap().to_string(),
        reg["url"].as_str().unwrap().to_string(),
    );
    assert!(url.contains("/browser-agent/"), "{reg}");
    assert_eq!(
        rustybox::remote::clean_url(&url).unwrap(),
        url,
        "the library form accepts a browser drive's address"
    );
    let r = Remote::new(&url, &token);

    // Nobody is asking for work yet, so it is "not connected" at once, not after a long wait.
    let r2 = r.clone();
    let e = blocking(move || r2.info()).await.unwrap_err();
    assert!(
        matches!(&e, rustybox::error::Error::Coded { code, .. } if code == "BROWSER_OFFLINE"),
        "{e:?}"
    );
    // A wrong token is refused.
    let bad = Remote::new(&url, "wrong-wrong-wrong-wrong");
    assert!(blocking(move || bad.info()).await.is_err());

    browser(s.port, id.clone(), token.clone(), s.root.clone(), 3);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let r2 = r.clone();
    let info = blocking(move || r2.info()).await.unwrap();
    assert_eq!(info.name, "drive");
    let r2 = r.clone();
    let list = blocking(move || r2.list("ISOs")).await.unwrap();
    assert_eq!(list.len(), 2, "{list:?}");

    // Ranged reads come back exactly.
    let r2 = r.clone();
    let part = blocking(move || {
        let mut v = Vec::new();
        std::io::Read::read_to_end(
            &mut r2.read("ISOs/Game.iso", 1000, Some(5000)).unwrap(),
            &mut v,
        )
        .unwrap();
        v
    })
    .await;
    assert_eq!(part, iso[1000..6000]);

    // Scanning gives the same answer as scanning the folder directly, for both kinds.
    let never = || false;
    let quiet = |_: usize| {};
    for (name, sc) in [("god", Scanner::God), ("iso", Scanner::Iso)] {
        let w = Walk {
            scanner: sc,
            cancelled: &never,
            progress: &quiet,
        };
        let (mut want, _) = scan::walk(&s.root, &w).unwrap();
        scan::enrich(&s.root, sc, &mut want, &HashMap::new(), &w).unwrap();
        let r2 = r.clone();
        let mut got = blocking(move || r2.scan(name, &HashMap::new()))
            .await
            .unwrap();
        want.sort_by(|a, b| a.relpath.cmp(&b.relpath));
        got.sort_by(|a, b| a.relpath.cmp(&b.relpath));
        assert_eq!(got, want, "{name} scan differs");
        assert!(!got.is_empty());
    }
    // The broken GOD package is reported as such (two of five data files).
    let r2 = r.clone();
    let god = blocking(move || r2.scan("god", &HashMap::new()))
        .await
        .unwrap();
    let broken = god.iter().find(|f| f.relpath.contains("Broken")).unwrap();
    assert!(
        matches!(&broken.meta, MetaState::Failed(e) if e.contains("missing")),
        "{:?}",
        broken.meta
    );

    // Writes go through in pieces and finish under the real name.
    let r2 = r.clone();
    blocking(move || {
        assert!(
            !r2.write_chunk("Out/a.bin", 0, 10, false, b"01234")
                .unwrap()
                .0
        );
        let (done, off) = r2.write_chunk("Out/a.bin", 5, 10, false, b"56789").unwrap();
        assert!(done && off == 10);
    })
    .await;
    assert_eq!(fs::read(s.root.join("Out/a.bin")).unwrap(), b"0123456789");
    let r2 = r.clone();
    blocking(move || {
        r2.rename("Out/a.bin", "Out/b.bin").unwrap();
        r2.mkdir("Out/sub").unwrap();
        r2.delete("Out/b.bin").unwrap();
    })
    .await;
    assert!(!s.root.join("Out/b.bin").exists() && s.root.join("Out/sub").is_dir());

    // A library made for the drive can be pointed at a new connection and keeps its folders.
    let (port, u, t) = (s.port, url.clone(), token.clone());
    let updated = blocking(move || {
        let made: Value = ureq::post(&format!("http://127.0.0.1:{port}/api/libraries"))
            .send_json(json!({"name": "Xbox drive", "kind": "god", "paths": [{"remote": {"url": u, "token": t, "subdir": "Games"}, "writable": true}]}))
            .unwrap()
            .into_json()
            .unwrap();
        let id = made["id"].as_i64().unwrap();
        let r: Value = ureq::post(&format!("http://127.0.0.1:{port}/api/libraries/{id}/reconnect"))
            .send_json(json!({"url": u, "token": t}))
            .unwrap()
            .into_json()
            .unwrap();
        r["updated"].as_u64().unwrap()
    })
    .await;
    assert_eq!(updated, 1);
}

#[tokio::test]
async fn a_browser_that_goes_away_is_reported_and_its_drive_is_remembered() {
    let s = start("gone").await;
    let reg = register(s.port, "Laptop").await;
    // Registering again with what the browser kept gives the same drive back.
    let (id, token) = (reg["id"].clone(), reg["token"].clone());
    let port = s.port;
    let again = blocking(move || {
        ureq::post(&format!(
            "http://127.0.0.1:{port}/api/browser-agent/register"
        ))
        .send_json(json!({"name": "Laptop 2", "id": id, "token": token}))
        .unwrap()
        .into_json::<Value>()
        .unwrap()
    })
    .await;
    assert_eq!((&again["id"], &again["token"]), (&reg["id"], &reg["token"]));
    // The list of drives survives a restart (kept in the config folder).
    assert!(
        fs::read_to_string(s.root.parent().unwrap().join("config/browser-agents.json"))
            .unwrap()
            .contains(reg["id"].as_str().unwrap())
    );
}
