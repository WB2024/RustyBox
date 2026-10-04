use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use rustybox::{
    agent::{self, AgentState},
    error::Error,
    library::scan::MetaState,
    remote::Remote,
    xbox::stfs,
};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

struct Drive {
    root: PathBuf,
    url: String,
}

async fn start(name: &str, read_only: bool) -> Drive {
    let root = std::env::temp_dir().join(format!("rustybox_agent_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let state = AgentState::new(&root, TOKEN.into(), "Test drive".into(), read_only).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = agent::router(Arc::new(state));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Drive { root, url }
}

/// Run a blocking client call off the async threads.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

fn code(e: &Error) -> String {
    match e {
        Error::Coded { code, .. } => code.clone(),
        other => format!("{other:?}"),
    }
}

#[tokio::test]
async fn the_token_is_required_and_the_drive_describes_itself() {
    let d = start("auth", false).await;
    let (url, bad) = (
        d.url.clone(),
        Remote::new(&d.url, "wrong-token-wrong-token"),
    );
    let e = blocking(move || bad.info()).await.unwrap_err();
    assert_eq!(code(&e), "AGENT_AUTH");

    let ok = Remote::new(&url, TOKEN);
    let info = blocking(move || ok.info()).await.unwrap();
    assert_eq!((info.name.as_str(), info.read_only), ("Test drive", false));
    assert!(info.fs.total > 0 && info.fs.free > 0, "{info:?}");

    // Nothing is listening: a clear, recoverable error.
    let gone = Remote::new("http://127.0.0.1:9", TOKEN);
    let e = blocking(move || gone.info()).await.unwrap_err();
    assert_eq!(code(&e), "AGENT_UNREACHABLE");
}

#[tokio::test]
async fn files_can_be_written_in_pieces_listed_read_back_renamed_and_removed() {
    let d = start("files", false).await;
    let r = Remote::new(&d.url, TOKEN);
    let root = d.root.clone();
    blocking(move || {
        // Resumable write.
        let (done, off) = r
            .write_chunk("Games/Halo 3/data.bin", 0, 10, false, b"01234")
            .unwrap();
        assert_eq!((done, off), (false, 5));
        assert_eq!(r.write_status("Games/Halo 3/data.bin").unwrap().offset, 5);
        assert_eq!(
            code(
                &r.write_chunk("Games/Halo 3/data.bin", 2, 10, false, b"x")
                    .unwrap_err()
            ),
            "OFFSET_MISMATCH"
        );
        let (done, _) = r
            .write_chunk("Games/Halo 3/data.bin", 5, 10, false, b"56789")
            .unwrap();
        assert!(done);
        assert_eq!(
            std::fs::read(root.join("Games/Halo 3/data.bin")).unwrap(),
            b"0123456789"
        );
        assert_eq!(
            code(
                &r.write_chunk("Games/Halo 3/data.bin", 0, 3, false, b"abc")
                    .unwrap_err()
            ),
            "EXISTS"
        );

        // Listing and stat.
        let list = r.list("Games").unwrap();
        assert_eq!(
            (list.len(), list[0].name.as_str(), list[0].is_dir),
            (1, "Halo 3", true)
        );
        let s = r.stat("Games/Halo 3/data.bin").unwrap();
        assert_eq!((s.exists, s.is_dir, s.size), (true, false, 10));
        assert!(!r.stat("nope").unwrap().exists);

        // Reading, in whole and in ranges.
        let mut all = Vec::new();
        r.read("Games/Halo 3/data.bin", 0, None)
            .unwrap()
            .read_to_end(&mut all)
            .unwrap();
        assert_eq!(all, b"0123456789");
        let mut part = Vec::new();
        r.read("Games/Halo 3/data.bin", 3, Some(4))
            .unwrap()
            .read_to_end(&mut part)
            .unwrap();
        assert_eq!(part, b"3456");

        // Folder changes.
        r.mkdir("Games/New").unwrap();
        r.rename("Games/Halo 3", "Games/New/Halo 3").unwrap();
        assert!(root.join("Games/New/Halo 3/data.bin").exists());
        assert_eq!(
            code(&r.rename("Games/New", "Games/New").unwrap_err()),
            "EXISTS"
        );
        r.delete("Games/New").unwrap();
        assert!(!root.join("Games/New").exists());
        assert!(r.delete("").is_err(), "the root can never be removed");
    })
    .await;
}

#[tokio::test]
async fn nothing_can_reach_outside_the_shared_folder() {
    let d = start("escape", false).await;
    let outside = d
        .root
        .parent()
        .unwrap()
        .join(format!("rustybox_agent_secret_{}", std::process::id()));
    std::fs::write(&outside, b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, d.root.join("link.bin")).unwrap();
    let r = Remote::new(&d.url, TOKEN);
    let root = d.root.clone();
    blocking(move || {
        for bad in ["../x", "a/../../x"] {
            assert!(r.list(bad).is_err(), "{bad}");
            assert!(r.write_chunk(bad, 0, 1, false, b"x").is_err(), "{bad}");
        }
        // A leading slash is not an absolute path: it is read as relative to the shared folder.
        assert!(!r.stat("/etc/passwd").unwrap().exists);
        r.write_chunk("/etc/note.bin", 0, 1, false, b"x").unwrap();
        assert!(root.join("etc/note.bin").exists() && !Path::new("/etc/note.bin").exists());
        assert!(
            r.read("link.bin", 0, None).is_err(),
            "a symlink out of the folder isn't followed"
        );
        assert!(r.delete("link.bin").is_err());
        assert!(r.write_chunk(".hidden", 0, 1, false, b"x").is_err());
        assert!(r.write_chunk("x.part", 0, 1, false, b"x").is_err());
    })
    .await;
    assert_eq!(std::fs::read(&outside).unwrap(), b"secret");
    let _ = std::fs::remove_file(&outside);
}

#[tokio::test]
async fn a_read_only_drive_refuses_every_change() {
    let d = start("ro", true).await;
    std::fs::write(d.root.join("keep.txt"), b"keep").unwrap();
    let r = Remote::new(&d.url, TOKEN);
    let root = d.root.clone();
    blocking(move || {
        assert!(r.info().unwrap().read_only);
        assert_eq!(r.list("").unwrap().len(), 1, "reading is fine");
        for e in [
            r.write_chunk("a.bin", 0, 1, false, b"x").unwrap_err(),
            r.mkdir("dir").unwrap_err(),
            r.rename("keep.txt", "moved.txt").unwrap_err(),
            r.delete("keep.txt").unwrap_err(),
        ] {
            assert_eq!(code(&e), "READ_ONLY");
        }
        assert!(root.join("keep.txt").exists() && !root.join("a.bin").exists());
    })
    .await;
}

fn make_god(dir: &Path, title: u32, name: &str) {
    let ct = dir.join(format!("{title:08X}")).join("00007000");
    std::fs::create_dir_all(&ct).unwrap();
    std::fs::write(
        ct.join("ABCDEF01"),
        stfs::build(b"LIVE", 0x7000, title, 1, name, name),
    )
    .unwrap();
}

#[tokio::test]
async fn the_drive_is_scanned_where_it_is_and_unchanged_games_are_not_read_again() {
    let d = start("scan", false).await;
    make_god(&d.root.join("Games/Alan Wake"), 0x4D53_0805, "Alan Wake");
    make_god(&d.root.join("Games/Mystery"), 0xABCD_EF12, "");
    std::fs::write(
        d.root.join("Games/readme.txt"),
        b"ignored by the GOD scanner",
    )
    .unwrap();
    let r = Remote::new(&d.url, TOKEN);
    let first = {
        let r = r.clone();
        blocking(move || r.scan("god", &HashMap::new()))
            .await
            .unwrap()
    };
    assert_eq!(first.len(), 2);
    let wake = first
        .iter()
        .find(|f| f.title_id.as_deref() == Some("4D530805"))
        .unwrap();
    assert_eq!(
        (wake.relpath.as_str(), wake.kind.as_str()),
        ("Games/Alan Wake/4D530805", "god")
    );
    let MetaState::Set(m) = &wake.meta else {
        panic!("header should have been read: {:?}", wake.meta)
    };
    assert_eq!(
        (m.title_id.as_str(), m.game_name.as_deref()),
        ("4D530805", Some("Alan Wake"))
    );

    // Told that everything is known, the agent doesn't read headers again.
    let known: HashMap<_, _> = first
        .iter()
        .map(|f| {
            (
                f.relpath.clone(),
                rustybox::library::scan::Existing {
                    size: f.size as i64,
                    mtime: f.mtime,
                    meta_ok: true,
                },
            )
        })
        .collect();
    let again = blocking(move || r.scan("god", &known)).await.unwrap();
    assert!(again.iter().all(|f| f.meta == MetaState::Keep), "{again:?}");

    let bad = Remote::new(&d.url, TOKEN);
    assert!(
        blocking(move || bad.scan("nonsense", &HashMap::new()))
            .await
            .is_err()
    );
}
