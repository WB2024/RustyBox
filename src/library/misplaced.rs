//! Finding things on an Xbox drive that are in the wrong place, and moving them (only when told to).
//!
//! The console reads add-ons (DLC) and title updates only from
//! `Content/0000000000000000/<TitleID>/<type>/<file>`, and Aurora lists games only from its games
//! folder. So an add-on kept in `Games`, or a package in a folder that isn't a profile inside
//! `Content`, is never used. The kind of file comes from the folder it sits in (`00000002` add-on,
//! `000B0000` title update), so nothing has to be read from the files.

use std::collections::HashMap;

use serde::Serialize;

use super::{LibPath, Library, tidy};
use crate::{error::Error, perms, transfer::loc::Loc, xbox::catalog};

/// The shared (all profiles) folder inside `Content`.
const SHARED: &str = "0000000000000000";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Move {
    pub from_path_id: i64,
    pub from_label: String,
    pub from: String,
    pub to_path_id: Option<i64>,
    pub to_label: String,
    pub to: String,
    /// `dlc`, `update` or `trainer`.
    pub kind: String,
    pub name: String,
    pub title_id: String,
    pub size: u64,
    /// The same file is already at the destination, so this one is a spare copy.
    pub duplicate: bool,
    /// Why this one can't be moved (it is left as it is).
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MisplacedPlan {
    pub moves: Vec<Move>,
}

fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn kind_of_type(t: &str) -> Option<&'static str> {
    match t.to_ascii_uppercase().as_str() {
        "00000002" => Some("dlc"),
        "000B0000" => Some("update"),
        _ => None,
    }
}

/// `…/<TitleID>/<type>/<file>` somewhere in `rel`: (title id, type, file name).
fn package_in(rel: &str) -> Option<(String, String, String)> {
    let parts: Vec<&str> = rel.split('/').collect();
    (0..parts.len().saturating_sub(2)).find_map(|i| {
        let (tid, ty) = (parts[i], parts[i + 1]);
        (is_hex(tid, 8) && is_hex(ty, 8) && kind_of_type(ty).is_some() && parts.len() == i + 3)
            .then(|| {
                (
                    tid.to_uppercase(),
                    ty.to_uppercase(),
                    parts[i + 2].to_string(),
                )
            })
    })
}

fn first_with_role<'a>(paths: &'a [LibPath], role: &str) -> Option<&'a LibPath> {
    paths.iter().find(|p| p.role == role)
}

/// Look through the drive's folders and list what should move.
pub fn plan(paths: &[LibPath]) -> Result<MisplacedPlan, Error> {
    let content = first_with_role(paths, "content");
    let trainers = first_with_role(paths, "trainers");
    let mut moves = Vec::new();

    let mut package = |lp: &LibPath, rel: &str, size: u64| {
        let Some((tid, ty, file)) = package_in(rel) else {
            return;
        };
        let kind = kind_of_type(&ty).unwrap_or("dlc");
        let to = format!("{SHARED}/{tid}/{ty}/{file}");
        // Already where it belongs.
        if content.is_some_and(|c| c.id == lp.id) && rel.eq_ignore_ascii_case(&to) {
            return;
        }
        let name = catalog::get()
            .name(&tid, None)
            .map(str::to_string)
            .unwrap_or_else(|| tid.clone());
        let mut m = Move {
            from_path_id: lp.id,
            from_label: lp.label.clone(),
            from: rel.to_string(),
            to_path_id: content.map(|c| c.id),
            to_label: content.map(|c| c.label.clone()).unwrap_or_default(),
            to: to.clone(),
            kind: kind.into(),
            name,
            title_id: tid,
            size,
            duplicate: false,
            problem: None,
        };
        match content {
            None => {
                m.problem = Some(
                    "Add your Content folder on the Xbox drive page first (what it holds: Console content)"
                        .into(),
                )
            }
            Some(c) => match Loc::of(c).stat(&to) {
                Ok(None) => {}
                Ok(Some(st)) if st.size == size => {
                    m.duplicate = true;
                    m.problem = Some(
                        "The same file is already in Content, so this is a spare copy. It is left for you to delete".into(),
                    );
                }
                Ok(Some(_)) => {
                    m.problem =
                        Some("A different file with that name is already in Content".into())
                }
                Err(e) => m.problem = Some(e.to_string()),
            },
        }
        moves.push(m);
    };

    for lp in paths
        .iter()
        .filter(|p| p.role.is_empty() || p.role == "content")
    {
        let is_content = lp.role == "content";
        for e in Loc::of(lp).tree("")? {
            if e.is_dir {
                continue;
            }
            // In Content, everything under a profile folder is where the console expects it.
            let top = e.rel.split('/').next().unwrap_or("");
            if is_content && is_hex(top, 16) {
                continue;
            }
            package(lp, &e.rel, e.size);
        }
    }

    // Loose files in a title's folder inside a profile (for example a trainer): not content the
    // console reads, so they go to the Trainers folder when there is one.
    if let Some(c) = content {
        let mut seen = std::collections::HashSet::new();
        for e in Loc::of(c).tree("")? {
            let parts: Vec<&str> = e.rel.split('/').collect();
            if parts.len() < 3
                || !is_hex(parts[0], 16)
                || !is_hex(parts[1], 8)
                || is_hex(parts[2], 8)
            {
                continue;
            }
            // A folder, or a file, straight inside the title's folder.
            let item = parts[..3].join("/");
            if !seen.insert(item.clone()) {
                continue;
            }
            let tid = parts[1].to_uppercase();
            let game = catalog::get().name(&tid, None).unwrap_or(&tid).to_string();
            let size: u64 = Loc::of(c)
                .tree(&item)
                .map(|t| t.iter().filter(|x| !x.is_dir).map(|x| x.size).sum())
                .unwrap_or_else(|_| e.size);
            let to = format!(
                "{} ({tid})/{}",
                crate::convert::plan::safe_name(&game),
                parts[2]
            );
            let mut m = Move {
                from_path_id: c.id,
                from_label: c.label.clone(),
                from: item,
                to_path_id: trainers.map(|t| t.id),
                to_label: trainers.map(|t| t.label.clone()).unwrap_or_default(),
                to: to.clone(),
                kind: "trainer".into(),
                name: parts[2].to_string(),
                title_id: tid,
                size,
                duplicate: false,
                problem: None,
            };
            match trainers {
                None => m.problem = Some(
                    "Add a Trainers folder on the Xbox drive page first (what it holds: Trainers)"
                        .into(),
                ),
                Some(t) => match Loc::of(t).stat(&to) {
                    Ok(None) => {}
                    Ok(Some(_)) => {
                        m.problem = Some("Something with that name is already in Trainers".into())
                    }
                    Err(e) => m.problem = Some(e.to_string()),
                },
            }
            moves.push(m);
        }
    }
    moves.sort_by(|a, b| (&a.kind, &a.from).cmp(&(&b.kind, &b.from)));
    Ok(MisplacedPlan { moves })
}

// ── Any library: things that belong in another library ───────────────────────

const GAME_TYPES: &[&str] = &["00007000", "00005000", "00004000", "000D0000"];

/// Package types we know how to place. Anything else in a type's position is just a folder.
const KNOWN_TYPES: &[&str] = &[
    "00000001", "00000002", "00004000", "00005000", "00007000", "000B0000", "000D0000",
];

/// Which library kind a set of package types belongs in. A game folder that also holds its
/// add-ons or updates is still a game folder.
fn kind_for_types(types: &std::collections::BTreeSet<String>) -> Option<&'static str> {
    if types.is_empty() {
        None
    } else if types.iter().any(|t| GAME_TYPES.contains(&t.as_str())) {
        Some("god")
    } else if types.iter().all(|t| t == "00000002") {
        Some("dlc")
    } else if types.iter().all(|t| t == "000B0000") {
        Some("patches")
    } else if types.iter().all(|t| t == "00000001") {
        Some("saves")
    } else {
        Some("mixed")
    }
}

fn label_of_kind(kind: &str) -> &'static str {
    match kind {
        "god" => "game (GOD)",
        "iso" => "game (ISO)",
        "dlc" => "add-on (DLC)",
        "patches" => "title update",
        "saves" => "saves",
        _ => "item",
    }
}

fn ext_is(name: &str, exts: &[&str]) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, e)| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The folders on this server, one level at a time, that Fix misplaced looks at for a library:
/// its own folders, and the folders of other libraries that sit inside them.
fn areas<'a>(libs: &'a [Library], lib: &'a Library) -> Vec<&'a LibPath> {
    let mine: Vec<&LibPath> = lib.paths.iter().filter(|p| !p.is_remote()).collect();
    let mut out: Vec<&LibPath> = mine.clone();
    for l in libs.iter().filter(|l| l.id != lib.id) {
        for p in l.paths.iter().filter(|p| !p.is_remote()) {
            let inside = mine.iter().any(|m| {
                p.path
                    .trim_end_matches('/')
                    .starts_with(&format!("{}/", m.path.trim_end_matches('/')))
            });
            if inside {
                out.push(p);
            }
        }
    }
    out
}

/// Look through a library (and the other libraries' folders inside it) for games, add-ons, title
/// updates and saves that are not in the library they belong to, and plan moving each into the
/// primary folder of that library. What isn't recognised is left alone.
pub fn plan_library(libs: &[Library], library_id: i64) -> Result<MisplacedPlan, Error> {
    let lib = libs
        .iter()
        .find(|l| l.id == library_id)
        .ok_or_else(|| Error::not_found(format!("No library #{library_id}")))?;
    let all_paths: Vec<&LibPath> = libs.iter().flat_map(|l| l.paths.iter()).collect();
    let target_of = |kind: &str| libs.iter().find(|l| l.kind == kind);
    let mut moves = Vec::new();

    for area in areas(libs, lib) {
        let root = std::path::Path::new(&area.path);
        let loc = Loc::of(area);
        // Other libraries' folders inside this one are looked at as areas of their own.
        let nested: Vec<String> = all_paths
            .iter()
            .filter(|p| p.id != area.id && !p.is_remote())
            .filter_map(|p| {
                std::path::Path::new(&p.path)
                    .strip_prefix(root)
                    .ok()
                    .and_then(|r| r.components().next())
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
            })
            .collect();
        let mut units: std::collections::BTreeMap<String, Vec<crate::transfer::loc::TreeEntry>> =
            std::collections::BTreeMap::new();
        for e in loc.tree("")? {
            let (top, rest) = match e.rel.split_once('/') {
                Some((t, r)) => (t.to_string(), r.to_string()),
                None => (e.rel.clone(), String::new()),
            };
            if nested.contains(&top) {
                continue;
            }
            let mut e = e;
            e.rel = rest;
            units.entry(top).or_default().push(e);
        }
        for (name, entries) in units {
            let is_file = entries.len() == 1 && entries[0].rel.is_empty() && !entries[0].is_dir;
            let mut types = std::collections::BTreeSet::new();
            let mut tid = String::new();
            for e in &entries {
                // The unit's own name counts: a folder called `41560855` is a title ID.
                let full = format!("{name}/{}", e.rel);
                let parts: Vec<&str> = full.split('/').collect();
                for i in 0..parts.len().saturating_sub(1) {
                    if is_hex(parts[i], 8)
                        && KNOWN_TYPES.contains(&parts[i + 1].to_uppercase().as_str())
                    {
                        types.insert(parts[i + 1].to_uppercase());
                        if tid.is_empty() {
                            tid = parts[i].to_uppercase();
                        }
                    }
                }
            }
            let has_iso = entries
                .iter()
                .any(|e| !e.is_dir && ext_is(&e.rel, &["iso"]))
                || (is_file && ext_is(&name, &["iso"]));
            let archive = is_file && ext_is(&name, &["zip", "7z", "rar"]);
            let has_exe = entries
                .iter()
                .any(|e| !e.is_dir && ext_is(&e.rel, &["xex"]));
            let kind = match kind_for_types(&types) {
                _ if has_iso => "iso",
                // Saves next to an executable are a trainer or an app keeping its own files.
                Some("saves") if has_exe => continue,
                Some(k) => k,
                None if archive => "archive",
                None => continue,
            };
            let size: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            let mut m = Move {
                from_path_id: area.id,
                from_label: area.label.clone(),
                from: name.clone(),
                to_path_id: None,
                to_label: String::new(),
                to: name.clone(),
                kind: kind.into(),
                name: name.clone(),
                title_id: tid,
                size,
                duplicate: false,
                problem: None,
            };
            match kind {
                "archive" => {
                    m.problem = Some(
                        "An archive: extract it first, RustyBox won't guess what is in it".into(),
                    )
                }
                "mixed" => m.problem = Some(
                    "Holds more than one kind of content (games, add-ons, updates); split it first"
                        .into(),
                ),
                k => {
                    match target_of(k) {
                        None => {
                            m.problem = Some(format!(
                                "You have no library for {}; add one first",
                                label_of_kind(k)
                            ))
                        }
                        Some(t) if t.id == area.library_id => continue, // already in the right library
                        Some(t) => {
                            match t.paths.iter().find(|p| !p.is_remote() && p.writable) {
                                None => {
                                    m.problem = Some(format!(
                                        "The {} library has no writable folder on this server",
                                        t.name
                                    ))
                                }
                                Some(prim) => {
                                    m.to_path_id = Some(prim.id);
                                    m.to_label = format!("{} › {}", t.name, prim.label);
                                    let to_loc = Loc::of(prim);
                                    match to_loc.stat(&name) {
                                Ok(Some(_)) => m.problem = Some("Something with that name is already in the primary folder".into()),
                                Ok(None) => {
                                    if let (Some(a), Some(b)) = (loc.device(&name), to_loc.device("")) && a != b {
                                        m.problem = Some("It is on a different disk from the primary folder; use Transfer or Import to copy it".into());
                                    }
                                }
                                Err(e) => m.problem = Some(e.to_string()),
                            }
                                }
                            }
                        }
                    }
                }
            }
            moves.push(m);
        }
    }
    moves.sort_by(|a, b| (&a.kind, &a.from).cmp(&(&b.kind, &b.from)));
    Ok(MisplacedPlan { moves })
}

/// Do the moves that have no problem: renames on the same disk, never overwriting. Returns how
/// many were moved and what failed.
pub fn apply(p: &MisplacedPlan, paths: &HashMap<i64, LibPath>) -> (usize, Vec<String>) {
    let (mut moved, mut failed) = (0, Vec::new());
    for m in p.moves.iter().filter(|m| m.problem.is_none()) {
        let (Some(from_lp), Some(to_lp)) = (
            paths.get(&m.from_path_id),
            m.to_path_id.and_then(|i| paths.get(&i)),
        ) else {
            continue;
        };
        let r = (|| -> Result<(), Error> {
            let (a, b) = (tidy::Whole::of(from_lp), tidy::Whole::of(to_lp));
            let to_loc = Loc::of(to_lp);
            if to_loc.stat(&m.to)?.is_some() {
                return Err(Error::coded(
                    409,
                    "EXISTS",
                    "it appeared there in the meantime",
                ));
            }
            if let Some((parent, _)) = m.to.rsplit_once('/') {
                to_loc.mkdir(parent)?;
            }
            a.loc.rename(&a.full(&m.from), &b.full(&m.to))
        })();
        match r {
            Ok(()) => {
                moved += 1;
                if let Some(to) = Loc::of(to_lp).local_path(&m.to) {
                    perms::own_parents(&to);
                }
                tidy::remove_empty_parents(&Loc::of(from_lp), &m.from);
            }
            Err(e) => failed.push(format!("{}: {e}", m.from)),
        }
    }
    (moved, failed)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;

    fn lp(id: i64, path: &Path, role: &str, label: &str) -> LibPath {
        LibPath {
            id,
            library_id: 1,
            path: path.to_string_lossy().to_string(),
            label: label.into(),
            writable: true,
            priority: id,
            remote_url: None,
            remote_token: None,
            remote_subdir: None,
            role: role.into(),
            last_scan: None,
        }
    }

    #[test]
    fn finds_addons_in_games_and_strays_in_content_and_moves_them() {
        let d = std::env::temp_dir().join(format!("rustybox_misplaced_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let (games, content, trainers) = (d.join("Games"), d.join("Content"), d.join("Trainers"));
        for p in [
            games.join("Pack/584109DB/00000002"),
            games.join("Game/4D530805/00007000/AAAA.data"),
            content.join("Stray/584E07D2/00000002"),
            content.join("0000000000000000/584E07D2/00000002"),
            content.join("E000000000000001/58411202/trainer"),
            content.join("E000000000000001/58411202/00000001"),
            trainers.clone(),
        ] {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(games.join("Pack/584109DB/00000002/PKG1"), vec![1u8; 10]).unwrap();
        fs::write(
            games.join("Game/4D530805/00007000/AAAA.data/Data0000"),
            b"x",
        )
        .unwrap();
        fs::write(content.join("Stray/584E07D2/00000002/PKG2"), vec![2u8; 20]).unwrap();
        // A spare copy of one already in Content.
        fs::write(
            content.join("0000000000000000/584E07D2/00000002/PKG3"),
            vec![3u8; 5],
        )
        .unwrap();
        fs::create_dir_all(games.join("Copy/584E07D2/00000002")).unwrap();
        fs::write(games.join("Copy/584E07D2/00000002/PKG3"), vec![3u8; 5]).unwrap();
        fs::write(
            content.join("E000000000000001/58411202/trainer/a.xex"),
            b"t",
        )
        .unwrap();
        fs::write(
            content.join("E000000000000001/58411202/00000001/save"),
            b"s",
        )
        .unwrap();

        let paths = vec![
            lp(1, &games, "", "Games"),
            lp(2, &content, "content", "Content"),
            lp(3, &trainers, "trainers", "Trainers"),
        ];
        let plan = plan(&paths).unwrap();
        let by = |f: &str| plan.moves.iter().find(|m| m.from.ends_with(f)).unwrap();
        assert_eq!(plan.moves.len(), 4, "{:?}", plan.moves);
        assert_eq!(by("PKG1").to, "0000000000000000/584109DB/00000002/PKG1");
        assert_eq!(by("PKG2").kind, "dlc");
        assert!(by("PKG3").duplicate && by("PKG3").problem.is_some());
        assert_eq!(by("trainer").kind, "trainer");
        assert!(by("trainer").to.ends_with("(58411202)/trainer"));

        let map = paths.iter().map(|p| (p.id, p.clone())).collect();
        let (moved, failed) = apply(&plan, &map);
        assert_eq!((moved, failed.len()), (3, 0), "{failed:?}");
        assert!(
            content
                .join("0000000000000000/584109DB/00000002/PKG1")
                .exists()
        );
        assert!(
            content
                .join("0000000000000000/584E07D2/00000002/PKG2")
                .exists()
        );
        assert!(
            games.join("Copy/584E07D2/00000002/PKG3").exists(),
            "spare copy is never deleted"
        );
        assert!(!games.join("Pack").exists(), "emptied folders go");
        assert!(
            games
                .join("Game/4D530805/00007000/AAAA.data/Data0000")
                .exists()
        );
        assert!(
            content
                .join("E000000000000001/58411202/00000001/save")
                .exists()
        );
        assert_eq!(
            fs::read_dir(
                trainers.join(
                    fs::read_dir(&trainers)
                        .unwrap()
                        .next()
                        .unwrap()
                        .unwrap()
                        .file_name()
                )
            )
            .unwrap()
            .count(),
            1
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn root_library_sorts_into_primary_folders_of_the_matching_libraries() {
        let d =
            std::env::temp_dir().join(format!("rustybox_misplaced_root_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let w = |p: &Path, f: &str| {
            fs::create_dir_all(p).unwrap();
            fs::write(p.join(f), b"x").unwrap();
        };
        let (gods, isos, tus, dlc, dlc_new) = (
            d.join("GODs"),
            d.join("ISOs"),
            d.join("TitleUpdates"),
            d.join("DLC"),
            d.join("DLC2"),
        );
        w(&gods.join("Alan Wake/4D530805/00007000"), "a");
        w(&gods.join("Pack (Addon)/584109DB/00000002"), "d");
        w(&d.join("41560855/00000002"), "p");
        w(&d.join("Tropico"), "t.iso");
        w(&d.join("Black Ops"), "readme.txt");
        w(&d.join("Torrents"), "a.torrent");
        w(&tus.join("Game (Title Update)/5351081B/000B0000"), "u");
        w(&d.join("Mixed/AAAAAAAA/000B0000"), "g");
        w(&gods.join("Alan Wake/4D530805/00000002"), "extra");
        w(&d.join("Mixed/AAAAAAAA/00000002"), "d");
        fs::create_dir_all(&isos).unwrap();
        fs::create_dir_all(&dlc).unwrap();
        fs::write(d.join("Black Ops.iso"), b"i").unwrap();
        fs::write(d.join("Black Ops.7z"), b"z").unwrap();
        let mk = |id: i64, lib: i64, p: &Path, prio: i64| {
            let mut x = lp(id, p, "", "p");
            x.library_id = lib;
            x.priority = prio;
            x
        };
        let l = |id: i64, name: &str, kind: &str, paths: Vec<LibPath>| Library {
            id,
            name: name.into(),
            kind: kind.into(),
            paths,
        };
        // DLC has a spare folder first in the list; the primary (lowest priority number) is DLC.
        let libs = vec![
            l(1, "Root", "custom", vec![mk(1, 1, &d, 0)]),
            l(2, "GODs", "god", vec![mk(2, 2, &gods, 0)]),
            l(3, "ISOs", "iso", vec![mk(3, 3, &isos, 0)]),
            l(4, "TUs", "patches", vec![mk(4, 4, &tus, 0)]),
            l(
                5,
                "DLC",
                "dlc",
                vec![mk(5, 5, &dlc_new, -1), mk(6, 5, &dlc, 0)],
            ),
        ];
        fs::create_dir_all(&dlc_new).unwrap();
        let mut libs = libs;
        libs[4].paths.sort_by_key(|p| p.priority);
        let plan = plan_library(&libs, 1).unwrap();
        let by = |n: &str| {
            plan.moves
                .iter()
                .find(|m| m.name == n)
                .unwrap_or_else(|| panic!("{n} in {:?}", plan.moves))
        };
        assert_eq!(by("41560855").kind, "dlc");
        assert_eq!(by("41560855").to_path_id, Some(5));
        assert_eq!(
            by("Pack (Addon)").to_path_id,
            Some(5),
            "found inside GODs and sent to the primary DLC folder"
        );
        assert_eq!(by("Tropico").kind, "iso");
        assert_eq!(by("Black Ops.iso").to_path_id, Some(3));
        assert!(
            by("Black Ops.7z")
                .problem
                .as_deref()
                .unwrap()
                .contains("archive")
        );
        assert!(
            by("Mixed")
                .problem
                .as_deref()
                .unwrap()
                .contains("more than one")
        );
        assert!(
            plan.moves
                .iter()
                .all(|m| m.name != "Alan Wake" && m.name != "Torrents" && m.name != "Black Ops")
        );
        let map = libs
            .iter()
            .flat_map(|l| l.paths.iter().cloned())
            .map(|p| (p.id, p))
            .collect();
        let (moved, failed) = apply(&plan, &map);
        assert_eq!(failed.len(), 0, "{failed:?}");
        assert_eq!(moved, 4);
        assert!(dlc_new.join("41560855/00000002/p").exists());
        assert!(dlc_new.join("Pack (Addon)/584109DB/00000002/d").exists());
        assert!(isos.join("Tropico/t.iso").exists() && isos.join("Black Ops.iso").exists());
        assert!(gods.join("Alan Wake/4D530805/00007000/a").exists());
        // Running it again finds nothing more to move.
        let again = plan_library(&libs, 1).unwrap();
        assert!(
            again.moves.iter().all(|m| m.problem.is_some()),
            "{:?}",
            again.moves
        );
        let _ = fs::remove_dir_all(&d);
    }
}
