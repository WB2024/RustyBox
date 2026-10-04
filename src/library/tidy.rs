//! Tidying a GOD library: renaming title folders to a consistent layout (`Game Name/TitleID`),
//! and sorting add-ons and title updates into the folders set aside for them.
//!
//! A plan is made first and shown; applying it renames folders in place (never copies, never
//! overwrites a file) and removes only the old folders it leaves empty. A game never goes into a
//! folder that already exists; an add-on or update may join one (a game has many of them), but
//! only if none of its files would clash.

use std::collections::HashMap;

use rusqlite::Connection;
use serde::Serialize;

use super::{LibPath, get_library, get_path};
use crate::{
    convert::plan::safe_name, error::Error, perms, remote::Remote, transfer::loc::Loc,
    xbox::meta::content_kind,
};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Move {
    pub item_id: i64,
    pub path_id: i64,
    pub path_label: String,
    /// The folder it goes to, when that isn't the one it is in.
    pub to_path_id: i64,
    pub to_path_label: String,
    pub name: String,
    pub title_id: String,
    /// `game`, `dlc` or `update`.
    pub content_kind: String,
    pub from: String,
    pub to: String,
    /// It joins a folder that already exists, file by file.
    pub merge: bool,
    /// Why this one can't be done (it is left as it is).
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TidyPlan {
    pub moves: Vec<Move>,
    pub unchanged: usize,
    /// God items skipped because their title ID couldn't be read.
    pub skipped: usize,
    pub layout: String,
}

/// What the plan needs from the database.
pub struct Row {
    pub item_id: i64,
    pub path_id: i64,
    pub relpath: String,
    pub title_id: Option<String>,
    pub game_name: Option<String>,
    pub content_kind: String,
}

pub fn gather(
    conn: &Connection,
    library_id: i64,
) -> Result<(Vec<Row>, HashMap<i64, LibPath>), Error> {
    let lib = get_library(conn, library_id)?;
    if lib.kind != "god" {
        return Err(Error::validation("Only GOD libraries can be tidied"));
    }
    let mut stmt = conn.prepare("SELECT id, path_id, relpath, title_id, game_name, content_type FROM items WHERE library_id = ?1 AND kind = 'god' ORDER BY relpath")?;
    let rows = stmt
        .query_map([library_id], |r| {
            let ct: Option<String> = r.get(5)?;
            let code = ct.and_then(|c| u32::from_str_radix(&c, 16).ok());
            Ok(Row {
                item_id: r.get(0)?,
                path_id: r.get(1)?,
                relpath: r.get(2)?,
                title_id: r.get(3)?,
                game_name: r.get(4)?,
                content_kind: content_kind(code).to_string(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // The console's own `Content` folder (and trainers, mods, homebrew, emulators) are laid out
    // the way the console and other tools expect: tidy never renames or moves anything in them.
    let mut paths: HashMap<i64, LibPath> = lib.paths.into_iter().map(|p| (p.id, p)).collect();
    // A folder called Content is the console's, whatever role it was given in the past (an old
    // "Add-ons" role on it once had tidy filing saves and add-ons away as `Name/TitleID`).
    for p in paths.values_mut() {
        let last = p
            .remote_subdir
            .as_deref()
            .unwrap_or(&p.path)
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("");
        if last.eq_ignore_ascii_case("content") {
            p.role = "content".into();
        }
    }
    let rows = rows
        .into_iter()
        .filter(|r| {
            paths
                .get(&r.path_id)
                .is_none_or(|p| matches!(p.role.as_str(), "" | "dlc" | "updates"))
        })
        .collect();
    Ok((rows, paths))
}

/// Where a title folder should live: `Name/TITLEID` or just `TITLEID`.
pub fn expected(layout: &str, title_id: &str, name: Option<&str>) -> String {
    let tid = title_id.to_uppercase();
    match name.map(safe_name).filter(|n| !n.is_empty()) {
        Some(n) if layout == "name_titleid" => format!("{n}/{tid}"),
        _ => tid,
    }
}

fn label(lp: &LibPath) -> String {
    if lp.label.is_empty() {
        lp.path.clone()
    } else {
        lp.label.clone()
    }
}

/// The folder an item belongs in: the one with the role for its kind of content, if the library
/// has one; otherwise it stays where it is.
fn home<'a>(kind: &str, current: &'a LibPath, ordered: &[&'a LibPath]) -> &'a LibPath {
    let role = match kind {
        "dlc" => "dlc",
        "update" => "updates",
        "game" => "",
        _ => return current,
    };
    if current.role == role {
        return current;
    }
    // Games go to a folder with no special role; add-ons and updates to the one set aside for them.
    ordered
        .iter()
        .copied()
        .find(|p| p.role == role)
        .unwrap_or(current)
}

/// A place reached from the top of its disk or drive, so a folder can be moved between two
/// folders on the same disk.
pub(super) struct Whole {
    pub(super) loc: Loc,
    base: String,
}

impl Whole {
    pub(super) fn of(lp: &LibPath) -> Whole {
        match (&lp.remote_url, lp.remote_subdir.as_deref()) {
            (Some(url), sub) => Whole {
                loc: Loc::Remote(Remote::new(url, lp.remote_token.as_deref().unwrap_or(""))),
                base: sub.unwrap_or("").trim_matches('/').to_string(),
            },
            (None, _) => Whole {
                loc: Loc::Local(std::path::PathBuf::from("/")),
                base: lp.path.trim_matches('/').to_string(),
            },
        }
    }

    pub(super) fn full(&self, rel: &str) -> String {
        match (self.base.is_empty(), rel.is_empty()) {
            (true, _) => rel.to_string(),
            (false, true) => self.base.clone(),
            (false, false) => format!("{}/{rel}", self.base),
        }
    }
}

/// Can things be moved from one folder to the other without copying?
fn same_disk(a: &LibPath, b: &LibPath) -> bool {
    if a.id == b.id {
        return true;
    }
    match (&a.remote_url, &b.remote_url) {
        (Some(x), Some(y)) => x == y && a.remote_token == b.remote_token,
        (None, None) => {
            Loc::of(a).device("").is_some() && Loc::of(a).device("") == Loc::of(b).device("")
        }
        _ => false,
    }
}

pub fn plan(rows: &[Row], paths: &HashMap<i64, LibPath>, layout: &str) -> TidyPlan {
    let mut moves = Vec::new();
    let (mut unchanged, mut skipped) = (0, 0);
    let mut ordered: Vec<&LibPath> = paths.values().collect();
    ordered.sort_by_key(|p| (p.priority, p.id));
    let mut claimed: HashMap<(i64, String), i64> = HashMap::new();
    for r in rows {
        let Some(tid) = r
            .title_id
            .as_deref()
            .filter(|t| t.len() == 8 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        else {
            skipped += 1;
            continue;
        };
        let Some(lp) = paths.get(&r.path_id) else {
            continue;
        };
        // Saves, profiles and other things that aren't games or their add-ons are left exactly as
        // they are: a console looks for them by their folder names.
        if r.content_kind == "other" {
            unchanged += 1;
            continue;
        }
        // A title the list doesn't know keeps the name folder it already has.
        let current_name = r
            .relpath
            .rsplit_once('/')
            .map(|(parent, _)| parent.rsplit('/').next().unwrap_or(parent).to_string());
        let name = r.game_name.clone().or(current_name);
        let to = expected(layout, tid, name.as_deref());
        let target = home(&r.content_kind, lp, &ordered);
        if to == r.relpath && target.id == lp.id {
            unchanged += 1;
            continue;
        }
        let merging = r.content_kind != "game";
        let loc = Loc::of(target);
        let mut merge = false;
        let mut problem = None;
        if !lp.writable || !target.writable {
            problem = Some("This folder is read-only".to_string());
        } else if !same_disk(lp, target) {
            problem = Some(format!(
                "{} is on a different disk from {}, so it can't be moved there without copying. Use Move from the Compare page.",
                label(lp),
                label(target)
            ));
        } else if loc.stat(&to).ok().flatten().is_some() {
            if merging {
                merge = true;
                let clash = clashes(lp, &r.relpath, target, &to);
                if let Some(c) = clash {
                    problem = Some(format!("{c} is already in the folder it would join"));
                }
            } else {
                problem =
                    Some("A folder with the new name already exists (not merged)".to_string());
            }
        } else if let Some(other) = claimed.insert((target.id, to.clone()), r.item_id) {
            if merging {
                merge = true;
            } else {
                problem = Some(format!("Same destination as item #{other}"));
            }
        } else if target.id == lp.id && to.starts_with(&format!("{}/", r.relpath)) {
            problem = Some("The new place is inside the old one".to_string());
        }
        moves.push(Move {
            item_id: r.item_id,
            path_id: r.path_id,
            path_label: label(lp),
            to_path_id: target.id,
            to_path_label: label(target),
            name: name.unwrap_or_else(|| tid.to_string()),
            title_id: tid.to_uppercase(),
            content_kind: r.content_kind.clone(),
            from: r.relpath.clone(),
            to,
            merge,
            problem,
        });
    }
    TidyPlan {
        moves,
        unchanged,
        skipped,
        layout: layout.to_string(),
    }
}

/// The first file that is in both folders (so joining them would overwrite something).
fn clashes(from_lp: &LibPath, from: &str, to_lp: &LibPath, to: &str) -> Option<String> {
    let src = Loc::of(from_lp).tree(from).ok()?;
    let dst = Loc::of(to_lp).tree(to).ok()?;
    src.iter()
        .filter(|e| !e.is_dir)
        .find(|e| dst.iter().any(|d| !d.is_dir && d.rel == e.rel))
        .map(|e| e.rel.clone())
}

/// Carry out the moves that have no problem. Returns (moved, failed messages).
pub fn apply(p: &TidyPlan, paths: &HashMap<i64, LibPath>) -> (usize, Vec<String>) {
    let (mut moved, mut failed) = (0, Vec::new());
    for m in p.moves.iter().filter(|m| m.problem.is_none()) {
        let (Some(from_lp), Some(to_lp)) = (paths.get(&m.path_id), paths.get(&m.to_path_id)) else {
            continue;
        };
        let result = if from_lp.id == to_lp.id && !m.merge {
            rename_within(from_lp, &m.from, &m.to)
        } else {
            move_across(from_lp, &m.from, to_lp, &m.to, m.merge)
        };
        match result {
            Ok(()) => {
                moved += 1;
                if let Some(to) = Loc::of(to_lp).local_path(&m.to) {
                    perms::own_parents(&to);
                }
                remove_empty_parents(&Loc::of(from_lp), &m.from);
            }
            Err(e) => failed.push(format!("{}: {e}", m.from)),
        }
    }
    (moved, failed)
}

fn rename_within(lp: &LibPath, from: &str, to: &str) -> Result<(), Error> {
    let loc = Loc::of(lp);
    if loc.stat(to)?.is_some() {
        return Err(Error::coded(
            409,
            "EXISTS",
            "the new folder appeared in the meantime",
        ));
    }
    if let Some((parent, _)) = to.rsplit_once('/') {
        loc.mkdir(parent)?;
    }
    loc.rename(from, to)
}

/// Move a folder between two folders on the same disk, or join it to one that exists: file by
/// file, never overwriting.
fn move_across(
    from_lp: &LibPath,
    from: &str,
    to_lp: &LibPath,
    to: &str,
    merge: bool,
) -> Result<(), Error> {
    let (a, b) = (Whole::of(from_lp), Whole::of(to_lp));
    let to_loc = Loc::of(to_lp);
    let exists = to_loc.stat(to)?.is_some();
    if exists && !merge {
        return Err(Error::coded(
            409,
            "EXISTS",
            "the new folder appeared in the meantime",
        ));
    }
    if !exists {
        if let Some((parent, _)) = to.rsplit_once('/') {
            to_loc.mkdir(parent)?;
        }
        return a.loc.rename(&a.full(from), &b.full(to));
    }
    let src = Loc::of(from_lp);
    let tree = src.tree(from)?;
    for e in tree.iter().filter(|e| e.is_dir) {
        to_loc.mkdir(&format!("{to}/{}", e.rel))?;
    }
    for e in tree.iter().filter(|e| !e.is_dir) {
        let dest = format!("{to}/{}", e.rel);
        if to_loc.stat(&dest)?.is_some() {
            return Err(Error::coded(
                409,
                "EXISTS",
                format!("{} is already there", e.rel),
            ));
        }
        a.loc
            .rename(&a.full(&format!("{from}/{}", e.rel)), &b.full(&dest))?;
    }
    // Only now is the old folder empty of files; remove it (and any empty folders in it).
    if src.tree(from)?.iter().all(|e| e.is_dir) {
        src.delete(from)?;
    }
    Ok(())
}

/// Remove folders the move left empty, never going above the library folder.
pub(super) fn remove_empty_parents(loc: &Loc, from: &str) {
    let mut dir = from.rsplit_once('/').map(|(p, _)| p.to_string());
    while let Some(d) = dir {
        if d.is_empty() || !loc.tree(&d).is_ok_and(|t| t.is_empty()) || loc.delete(&d).is_err() {
            break;
        }
        dir = d.rsplit_once('/').map(|(p, _)| p.to_string());
    }
}

pub fn path_of(conn: &Connection, library_id: i64, path_id: i64) -> Result<LibPath, Error> {
    get_path(conn, library_id, path_id)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;

    fn lp(path: &Path, writable: bool) -> HashMap<i64, LibPath> {
        HashMap::from([(
            1,
            LibPath {
                id: 1,
                library_id: 1,
                path: path.to_string_lossy().to_string(),
                label: "x".into(),
                writable,
                priority: 0,
                last_scan: None,
                remote_url: None,
                remote_token: None,
                remote_subdir: None,
                role: String::new(),
            },
        )])
    }

    fn row(id: i64, rel: &str, tid: Option<&str>, name: Option<&str>) -> Row {
        Row {
            item_id: id,
            path_id: 1,
            relpath: rel.into(),
            title_id: tid.map(String::from),
            game_name: name.map(String::from),
            content_kind: "game".into(),
        }
    }

    #[test]
    fn expected_layouts() {
        assert_eq!(
            expected("name_titleid", "4d530805", Some("Alan Wake")),
            "Alan Wake/4D530805"
        );
        assert_eq!(
            expected("titleid", "4d530805", Some("Alan Wake")),
            "4D530805"
        );
        assert_eq!(expected("name_titleid", "4D530805", None), "4D530805");
        assert_eq!(
            expected("name_titleid", "4D530805", Some("Halo 3: ODST")),
            "Halo 3 ODST/4D530805"
        );
    }

    #[test]
    fn plans_only_what_needs_to_change_and_applies_it_safely() {
        let d = std::env::temp_dir().join(format!("rustybox_tidy_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        for rel in [
            "4D530805/00007000",
            "Messy Name/Deeper/4D5307F1/00007000",
            "Fine Game/4D530910/00007000",
            "Taken/4D5307D5/00007000",
            "Taken Too/4D5307D5/00007000",
        ] {
            fs::create_dir_all(d.join(rel)).unwrap();
        }
        fs::write(d.join("4D530805/00007000/C"), b"x").unwrap();
        // The right place for Gears of War is already occupied by something else.
        fs::create_dir_all(d.join("Gears of War/4D5307D5")).unwrap();
        let rows = vec![
            row(1, "4D530805", Some("4D530805"), Some("Alan Wake")),
            row(
                2,
                "Messy Name/Deeper/4D5307F1",
                Some("4D5307F1"),
                Some("Fable II"),
            ),
            row(3, "Fine Game/4D530910", Some("4D530910"), Some("Fine Game")),
            row(4, "Taken/4D5307D5", Some("4D5307D5"), Some("Gears of War")),
            row(
                5,
                "Taken Too/4D5307D5",
                Some("4D5307D5"),
                Some("Gears of War"),
            ),
            row(6, "Unreadable", None, None),
        ];
        let paths = lp(&d, true);
        let p = plan(&rows, &paths, "name_titleid");
        assert_eq!((p.unchanged, p.skipped, p.moves.len()), (1, 1, 4));
        let by = |id: i64| p.moves.iter().find(|m| m.item_id == id).unwrap();
        assert_eq!(by(1).to, "Alan Wake/4D530805");
        assert_eq!(by(2).to, "Fable II/4D5307F1");
        assert!(by(4).problem.as_ref().unwrap().contains("already exists"));
        assert!(by(5).problem.is_some());

        let (moved, failed) = apply(&p, &paths);
        assert_eq!((moved, failed.len()), (2, 0));
        assert!(d.join("Alan Wake/4D530805/00007000/C").is_file());
        assert!(!d.join("4D530805").exists());
        assert!(d.join("Fable II/4D5307F1/00007000").is_dir());
        // The emptied "Messy Name/Deeper" folders are gone; the occupied and untouched ones remain.
        assert!(!d.join("Messy Name").exists());
        assert!(
            d.join("Taken/4D5307D5").is_dir()
                && d.join("Gears of War/4D5307D5").is_dir()
                && d.join("Fine Game/4D530910").is_dir()
        );

        // Read-only: nothing is attempted.
        let ro = plan(&rows, &lp(&d, false), "titleid");
        assert!(
            ro.moves
                .iter()
                .all(|m| m.problem.as_deref() == Some("This folder is read-only"))
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn add_ons_and_updates_are_sorted_into_their_folders_and_join_existing_ones() {
        let d = std::env::temp_dir().join(format!("rustybox_tidy_roles_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        for rel in [
            "games/Messy/4D530805/00007000",
            "games/Pack One/4D530805/00000002",
            "games/Pack Two/4D530805/00000002",
            "games/Clash/4D5307F1/00000002",
            "dlc/Fable II/4D5307F1/00000002",
        ] {
            fs::create_dir_all(d.join(rel)).unwrap();
        }
        fs::write(d.join("games/Pack One/4D530805/00000002/one.pkg"), b"1").unwrap();
        fs::write(d.join("games/Pack Two/4D530805/00000002/two.pkg"), b"2").unwrap();
        fs::write(d.join("games/Clash/4D5307F1/00000002/same.pkg"), b"new").unwrap();
        fs::write(d.join("dlc/Fable II/4D5307F1/00000002/same.pkg"), b"old").unwrap();
        let mut paths = lp(&d.join("games"), true);
        let mut dlc = paths[&1].clone();
        (dlc.id, dlc.path, dlc.role, dlc.priority) = (
            2,
            d.join("dlc").to_string_lossy().to_string(),
            "dlc".into(),
            1,
        );
        paths.insert(2, dlc);
        let mk = |id: i64, rel: &str, tid: &str, name: &str, kind: &str| Row {
            item_id: id,
            path_id: 1,
            relpath: rel.into(),
            title_id: Some(tid.into()),
            game_name: Some(name.into()),
            content_kind: kind.into(),
        };
        let rows = vec![
            mk(1, "Messy/4D530805", "4D530805", "Alan Wake", "game"),
            mk(2, "Pack One/4D530805", "4D530805", "Alan Wake", "dlc"),
            mk(3, "Pack Two/4D530805", "4D530805", "Alan Wake", "dlc"),
            mk(4, "Clash/4D5307F1", "4D5307F1", "Fable II", "dlc"),
        ];
        let p = plan(&rows, &paths, "name_titleid");
        let by = |id: i64| p.moves.iter().find(|m| m.item_id == id).unwrap();
        assert_eq!(
            (by(1).to_path_id, by(1).to.as_str()),
            (1, "Alan Wake/4D530805")
        );
        assert_eq!(
            (by(2).to_path_id, by(2).to.as_str()),
            (2, "Alan Wake/4D530805")
        );
        assert!(
            by(3).merge && by(3).problem.is_none(),
            "a second add-on for the same game joins the first"
        );
        assert!(
            by(4).problem.as_ref().unwrap().contains("same.pkg"),
            "{:?}",
            by(4)
        );

        let (moved, failed) = apply(&p, &paths);
        assert_eq!((moved, failed.len()), (3, 0), "{failed:?}");
        assert!(d.join("games/Alan Wake/4D530805/00007000").is_dir());
        let joined = d.join("dlc/Alan Wake/4D530805/00000002");
        assert_eq!(fs::read(joined.join("one.pkg")).unwrap(), b"1");
        assert_eq!(fs::read(joined.join("two.pkg")).unwrap(), b"2");
        assert!(!d.join("games/Pack One").exists() && !d.join("games/Pack Two").exists());
        // The clashing one was left alone and nothing was overwritten.
        assert_eq!(
            fs::read(d.join("dlc/Fable II/4D5307F1/00000002/same.pkg")).unwrap(),
            b"old"
        );
        assert!(d.join("games/Clash/4D5307F1/00000002/same.pkg").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_consoles_content_folder_is_never_tidied() {
        let db = crate::db::Db::open_memory().unwrap();
        db.with(|c| {
            c.execute("INSERT INTO libraries(id,name,kind,created) VALUES (1,'Xbox drive','god',0)", [])?;
            c.execute("INSERT INTO library_paths(id,library_id,path,label,role) VALUES (1,1,'/g','Games',''),(2,1,'/c','Content','content'),(3,1,'/t','Trainers','trainers')", [])?;
            // A folder called Content stays untouched even with an old add-ons role.
            c.execute("INSERT INTO library_paths(id,library_id,path,label,role) VALUES (4,1,'/x/Content','Old','dlc')", [])?;
            for (path, rel) in [(1, "Alan Wake/4D530805"), (2, "E000000000000001/4D530805"), (2, "0000000000000000/584111F7"), (3, "5841096B"), (4, "0000000000000000/4D530805")] {
                c.execute(
                    "INSERT INTO items(library_id,path_id,relpath,name,kind,size,mtime,title_id) VALUES (1,?1,?2,?2,'god',0,0,'4D530805')",
                    rusqlite::params![path, rel],
                )?;
            }
            let (rows, _) = gather(c, 1)?;
            assert_eq!(rows.len(), 1, "only the Games folder is tidied");
            assert_eq!(rows[0].relpath, "Alan Wake/4D530805");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn saves_and_other_content_are_never_renamed() {
        let d = std::env::temp_dir().join(format!("rustybox_tidy_other_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("Terraria Saves/5841128F")).unwrap();
        let paths = lp(&d, true);
        let rows = vec![Row {
            item_id: 1,
            path_id: 1,
            relpath: "Terraria Saves/5841128F".into(),
            title_id: Some("5841128F".into()),
            game_name: Some("Terraria".into()),
            content_kind: "other".into(),
        }];
        let p = plan(&rows, &paths, "name_titleid");
        assert!(p.moves.is_empty(), "{:?}", p.moves);
        assert_eq!(p.unchanged, 1);
        let _ = fs::remove_dir_all(&d);
    }
}
