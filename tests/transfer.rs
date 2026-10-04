use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use rustybox::{
    agent::{self, AgentState},
    convert::Ctl,
    remote::Remote,
    transfer::{
        Loc,
        engine::{self, LinkKind, Moved, Progress},
    },
};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const NEVER: &(dyn Fn() -> bool + Sync) = &|| false;
const QUIET: &(dyn Fn(f32, &str) + Sync) = &|_, _| {};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rustybox_transfer_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    fs::canonicalize(d).unwrap()
}

/// A game-shaped folder: nested files, an empty one, and one bigger than a copy piece.
fn make_game(root: &Path) {
    fs::create_dir_all(root.join("Alan Wake/4D530805/00007000/ABC.data")).unwrap();
    fs::write(root.join("Alan Wake/4D530805/00007000/ABC"), b"header").unwrap();
    fs::write(
        root.join("Alan Wake/4D530805/00007000/ABC.data/Data0000"),
        vec![7u8; 9 * 1024 * 1024],
    )
    .unwrap();
    fs::write(
        root.join("Alan Wake/4D530805/00007000/ABC.data/Data0001"),
        b"",
    )
    .unwrap();
    fs::create_dir_all(root.join("Alan Wake/empty-dir")).unwrap();
}

fn same_tree(a: &Path, b: &Path) {
    let list = |r: &Path| -> Vec<(String, u64, bool)> {
        let mut v = Vec::new();
        let mut stack = vec![r.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let m = fs::metadata(e.path()).unwrap();
                v.push((
                    e.path()
                        .strip_prefix(r)
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                    if m.is_dir() { 0 } else { m.len() },
                    m.is_dir(),
                ));
                if m.is_dir() {
                    stack.push(e.path());
                }
            }
        }
        v.sort();
        v
    };
    assert_eq!(list(a), list(b));
}

struct Prog;
impl Prog {
    fn run<T>(f: impl FnOnce(&Progress) -> T) -> T {
        let ctl = Ctl {
            cancelled: NEVER,
            progress: QUIET,
        };
        f(&Progress {
            ctl: &ctl,
            total: 100,
            base: 0,
            label: "test",
        })
    }
}

async fn start_agent(name: &str) -> (PathBuf, String) {
    let root = tmp(name);
    let state = AgentState::new(&root, TOKEN.into(), "PC".into(), false).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = agent::router(Arc::new(state));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (root, url)
}

#[test]
fn a_tree_is_copied_exactly_and_a_second_run_copies_nothing() {
    let (a, b) = (tmp("copy_a"), tmp("copy_b"));
    make_game(&a);
    let (src, dst) = (Loc::Local(a.clone()), Loc::Local(b.clone()));
    let s = Prog::run(|p| engine::copy_tree(&src, "Alan Wake", &dst, "Games/Alan Wake", false, p))
        .unwrap();
    assert_eq!((s.files, s.skipped, s.bytes), (3, 0, 6 + 9 * 1024 * 1024));
    same_tree(&a.join("Alan Wake"), &b.join("Games/Alan Wake"));
    assert!(b.join("Games/Alan Wake/empty-dir").is_dir());
    // Run again: everything is already there with the right size.
    let s = Prog::run(|p| engine::copy_tree(&src, "Alan Wake", &dst, "Games/Alan Wake", false, p))
        .unwrap();
    assert_eq!((s.files, s.skipped), (0, 3));
    // A file that differs is never silently replaced, unless asked.
    fs::write(
        b.join("Games/Alan Wake/4D530805/00007000/ABC"),
        b"different length",
    )
    .unwrap();
    let e = Prog::run(|p| engine::copy_tree(&src, "Alan Wake", &dst, "Games/Alan Wake", false, p))
        .unwrap_err();
    assert!(e.to_string().contains("different size"), "{e}");
    let s = Prog::run(|p| engine::copy_tree(&src, "Alan Wake", &dst, "Games/Alan Wake", true, p))
        .unwrap();
    assert_eq!(s.files, 3, "overwriting copies them all again");
    assert_eq!(
        fs::read(b.join("Games/Alan Wake/4D530805/00007000/ABC")).unwrap(),
        b"header"
    );
}

#[test]
fn an_interrupted_copy_carries_on_from_the_partial_file() {
    let (a, b) = (tmp("resume_a"), tmp("resume_b"));
    make_game(&a);
    let big = "Alan Wake/4D530805/00007000/ABC.data/Data0000";
    // A half-written copy from an earlier attempt: the first 8 MiB, as a .part file.
    fs::create_dir_all(b.join(Path::new(big).parent().unwrap())).unwrap();
    fs::write(b.join(format!("{big}.part")), vec![7u8; 8 * 1024 * 1024]).unwrap();
    let (src, dst) = (Loc::Local(a.clone()), Loc::Local(b.clone()));
    let s =
        Prog::run(|p| engine::copy_tree(&src, "Alan Wake", &dst, "Alan Wake", false, p)).unwrap();
    assert_eq!(s.files, 3);
    assert_eq!(
        fs::read(b.join(big)).unwrap(),
        fs::read(a.join(big)).unwrap()
    );
    assert!(!b.join(format!("{big}.part")).exists());
}

#[test]
fn cancelling_stops_a_copy_and_leaves_no_finished_file() {
    let (a, b) = (tmp("cancel_a"), tmp("cancel_b"));
    make_game(&a);
    let ctl = Ctl {
        cancelled: &|| true,
        progress: QUIET,
    };
    let r = engine::copy_tree(
        &Loc::Local(a),
        "Alan Wake",
        &Loc::Local(b.clone()),
        "Alan Wake",
        false,
        &Progress {
            ctl: &ctl,
            total: 1,
            base: 0,
            label: "",
        },
    );
    assert!(r.unwrap_err().to_string().contains("Cancelled"));
    assert!(
        !b.join("Alan Wake/4D530805/00007000/ABC.data/Data0000")
            .exists()
    );
}

#[test]
fn hard_links_share_the_data_and_symlinks_point_back_at_the_original() {
    let (a, b) = (tmp("link_a"), tmp("link_b"));
    make_game(&a);
    let ctl = Ctl {
        cancelled: NEVER,
        progress: QUIET,
    };
    let s = engine::link_tree(
        &a,
        "Alan Wake",
        &b,
        "Games/Alan Wake",
        LinkKind::Hard,
        false,
        &ctl,
    )
    .unwrap();
    assert_eq!(s.files, 3);
    let (x, y) = (
        a.join("Alan Wake/4D530805/00007000/ABC"),
        b.join("Games/Alan Wake/4D530805/00007000/ABC"),
    );
    assert_eq!(
        fs::metadata(&x).unwrap().ino(),
        fs::metadata(&y).unwrap().ino()
    );
    assert!(fs::metadata(&x).unwrap().nlink() >= 2);
    // Again: nothing to do. Different content in the way: refused.
    let s = engine::link_tree(
        &a,
        "Alan Wake",
        &b,
        "Games/Alan Wake",
        LinkKind::Hard,
        false,
        &ctl,
    )
    .unwrap();
    assert_eq!((s.files, s.skipped), (0, 3));

    let c = tmp("link_c");
    engine::link_tree(
        &a,
        "Alan Wake",
        &c,
        "Alan Wake",
        LinkKind::Soft,
        false,
        &ctl,
    )
    .unwrap();
    let link = c.join("Alan Wake/4D530805/00007000/ABC");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&link).unwrap(), x);
    // The folders are real, so a scan still walks into them.
    assert!(
        !fs::symlink_metadata(c.join("Alan Wake/4D530805"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::write(c.join("Alan Wake/other"), b"x").unwrap();
    let e = engine::link_tree(
        &a,
        "Alan Wake/4D530805/00007000/ABC",
        &c,
        "Alan Wake/other",
        LinkKind::Soft,
        false,
        &ctl,
    )
    .unwrap_err();
    assert!(e.to_string().contains("already there"), "{e}");
}

#[test]
fn hard_links_across_filesystems_are_refused_with_the_reason() {
    let shm = Path::new("/dev/shm");
    if !shm.is_dir()
        || fs::metadata(shm).map(|m| m.dev()).ok()
            == fs::metadata(std::env::temp_dir()).map(|m| m.dev()).ok()
    {
        return; // no second filesystem to try with
    }
    let a = tmp("xdev_a");
    make_game(&a);
    let b = shm.join(format!("rustybox_xdev_{}", std::process::id()));
    fs::create_dir_all(&b).unwrap();
    let ctl = Ctl {
        cancelled: NEVER,
        progress: QUIET,
    };
    let e = engine::link_tree(&a, "Alan Wake", &b, "x", LinkKind::Hard, false, &ctl).unwrap_err();
    assert!(e.to_string().contains("different ones"), "{e}");

    // A move across filesystems copies, checks, then removes the original.
    let m = Prog::run(|p| {
        engine::move_tree(
            &Loc::Local(a.clone()),
            "Alan Wake",
            &Loc::Local(b.clone()),
            "Moved/Alan Wake",
            false,
            p,
        )
    })
    .unwrap();
    assert_eq!(m, Moved::Copied);
    assert!(
        !a.join("Alan Wake").exists() && b.join("Moved/Alan Wake/4D530805/00007000/ABC").exists()
    );
    let _ = fs::remove_dir_all(&b);
}

#[test]
fn a_move_on_one_filesystem_is_an_instant_rename() {
    let a = tmp("move_a");
    make_game(&a);
    let ino = fs::metadata(a.join("Alan Wake/4D530805")).unwrap().ino();
    let b = tmp("move_b");
    let m = Prog::run(|p| {
        engine::move_tree(
            &Loc::Local(a.clone()),
            "Alan Wake",
            &Loc::Local(b.clone()),
            "Games/Alan Wake",
            false,
            p,
        )
    })
    .unwrap();
    assert_eq!(m, Moved::Renamed);
    assert_eq!(
        fs::metadata(b.join("Games/Alan Wake/4D530805"))
            .unwrap()
            .ino(),
        ino,
        "the same folder, not a copy"
    );
    assert!(!a.join("Alan Wake").exists());
    // The top folder can't be moved.
    assert!(
        Prog::run(|p| engine::move_tree(
            &Loc::Local(b.clone()),
            "",
            &Loc::Local(a.clone()),
            "x",
            false,
            p
        ))
        .is_err()
    );
    // Moving into a folder that already exists (another disc of the same game) merges, and files
    // already there are left alone. A file that would clash is refused, with the original untouched.
    make_game(&a);
    fs::write(a.join("Alan Wake/4D530805/00007000/DISC2"), b"disc two").unwrap();
    let m = Prog::run(|p| {
        engine::move_tree(
            &Loc::Local(a.clone()),
            "Alan Wake",
            &Loc::Local(b.clone()),
            "Games/Alan Wake",
            false,
            p,
        )
    })
    .unwrap();
    assert_eq!(m, Moved::Copied, "merged by copying");
    assert!(b.join("Games/Alan Wake/4D530805/00007000/DISC2").exists());
    assert!(b.join("Games/Alan Wake/4D530805/00007000/ABC").exists());
    assert!(!a.join("Alan Wake").exists());
    make_game(&a);
    fs::write(
        a.join("Alan Wake/4D530805/00007000/ABC"),
        b"a clashing file",
    )
    .unwrap();
    let e = Prog::run(|p| {
        engine::move_tree(
            &Loc::Local(a.clone()),
            "Alan Wake",
            &Loc::Local(b.clone()),
            "Games/Alan Wake",
            false,
            p,
        )
    })
    .unwrap_err();
    assert!(e.to_string().contains("different size"), "{e}");
    assert!(
        a.join("Alan Wake").exists(),
        "the original is untouched when the move is refused"
    );
}

#[tokio::test]
async fn trees_go_to_and_come_from_a_drive_agent_and_moving_to_one_removes_the_original() {
    let (agent_root, url) = start_agent("agent").await;
    let a = tmp("agent_local");
    make_game(&a);
    let (a2, agent_root2) = (a.clone(), agent_root.clone());
    tokio::task::spawn_blocking(move || {
        let local = Loc::Local(a2.clone());
        let drive = Loc::Remote(Remote::new(&url, TOKEN).with_prefix("Games"));
        // Local to drive: the big file goes in two pieces.
        let s =
            Prog::run(|p| engine::copy_tree(&local, "Alan Wake", &drive, "Alan Wake", false, p))
                .unwrap();
        assert_eq!((s.files, s.skipped), (3, 0));
        same_tree(&a2.join("Alan Wake"), &agent_root2.join("Games/Alan Wake"));
        // Again: skipped.
        let s =
            Prog::run(|p| engine::copy_tree(&local, "Alan Wake", &drive, "Alan Wake", false, p))
                .unwrap();
        assert_eq!((s.files, s.skipped), (0, 3));

        // Drive to local.
        let back = tmp("agent_back");
        let s = Prog::run(|p| {
            engine::copy_tree(
                &drive,
                "Alan Wake",
                &Loc::Local(back.clone()),
                "Copy",
                false,
                p,
            )
        })
        .unwrap();
        assert_eq!(s.files, 3);
        same_tree(&a2.join("Alan Wake"), &back.join("Copy"));

        // A half-sent file on the drive resumes.
        fs::write(
            agent_root2.join("Games/Alan Wake/4D530805/00007000/ABC.data/Data0000"),
            b"",
        )
        .unwrap();
        fs::remove_file(agent_root2.join("Games/Alan Wake/4D530805/00007000/ABC.data/Data0000"))
            .unwrap();
        fs::write(
            agent_root2.join("Games/Alan Wake/4D530805/00007000/ABC.data/Data0000.part"),
            vec![7u8; 8 * 1024 * 1024],
        )
        .unwrap();
        let s =
            Prog::run(|p| engine::copy_tree(&local, "Alan Wake", &drive, "Alan Wake", false, p))
                .unwrap();
        assert_eq!(s.files, 1);
        assert_eq!(
            fs::metadata(agent_root2.join("Games/Alan Wake/4D530805/00007000/ABC.data/Data0000"))
                .unwrap()
                .len(),
            9 * 1024 * 1024
        );

        // Moving onto the drive: copied, checked, then the original is gone.
        let m = Prog::run(|p| engine::move_tree(&local, "Alan Wake", &drive, "Moved", false, p))
            .unwrap();
        assert_eq!(m, Moved::Copied);
        assert!(!a2.join("Alan Wake").exists());
        assert!(
            agent_root2
                .join("Games/Moved/4D530805/00007000/ABC")
                .exists()
        );
    })
    .await
    .unwrap();
}
