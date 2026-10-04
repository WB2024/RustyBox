//! Sending games from libraries to the console: a plan first (what goes where, what is already
//! there, what is too big for the drive, what a replace would remove), then the transfer.

use serde::Serialize;

use super::{
    Console, ConsoleGame, Sent,
    ftp::{Ftp, ftp_path},
    max_file_for, scan_games, send_tree,
};
use crate::{
    convert::{Ctl, plan::safe_name},
    error::Error,
    library::{Item, LibPath, tidy::expected},
    transfer::{Loc, run::GAME_CONTENT},
};

#[derive(Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub title_id: Option<String>,
    pub kind: String,
    pub bytes: u64,
    pub steps: Vec<String>,
    /// Where it ends up on the console.
    pub output: String,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub work: Option<Work>,
}

#[derive(Clone)]
pub struct Work {
    pub src: Loc,
    pub src_rel: String,
    pub dest: String,
    /// Old copies on the console to remove: (FTP path of the title folder, remove before sending).
    pub removals: Vec<(String, bool)>,
}

#[derive(Serialize)]
pub struct Plan {
    pub console: String,
    pub dest: String,
    pub entries: Vec<Entry>,
    pub problems: Vec<String>,
    pub ok: bool,
}

pub struct Options<'a> {
    /// The games folder to send into, as the user wrote it (default: the console's first).
    pub dest: Option<&'a str>,
    pub layout: &'a str,
    pub replace: bool,
}

fn fmt_bytes(n: u64) -> String {
    let u = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < u.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", u[i])
    }
}

/// Something to send: a game from a library, or one found in a folder (a USB stick, say).
#[derive(Clone)]
pub struct SendItem {
    pub name: String,
    pub title_id: Option<String>,
    /// `iso` or `god`.
    pub kind: String,
    pub relpath: String,
    pub size: i64,
    pub available: bool,
    pub content_kind: String,
    pub health: Option<String>,
    pub discs: Option<i64>,
    pub loc: Loc,
}

impl SendItem {
    pub fn from_library(item: &Item, lp: &LibPath) -> SendItem {
        SendItem {
            name: item.game_name.clone().unwrap_or_else(|| item.name.clone()),
            title_id: item.title_id.clone(),
            kind: item.kind.clone(),
            relpath: item.relpath.clone(),
            size: item.size,
            available: item.available,
            content_kind: item.content_kind.clone(),
            health: item.health.clone(),
            discs: item.discs,
            loc: Loc::of(lp),
        }
    }
}

pub fn build(
    console: &Console,
    ftp: &mut Ftp,
    items: &[SendItem],
    opt: &Options,
) -> Result<Plan, Error> {
    let mut problems = Vec::new();
    let dest_root = match opt.dest {
        Some(d) => {
            let d = ftp_path(d);
            if !console
                .game_paths
                .iter()
                .any(|g| ftp_path(g).eq_ignore_ascii_case(&d))
            {
                return Err(Error::validation(format!(
                    "{d} isn't one of this console's games folders. Add it to the console first."
                )));
            }
            d
        }
        None => ftp_path(&console.game_paths[0]),
    };
    // Only look at the console's titles when a replace needs them.
    let on_console: Vec<ConsoleGame> = if opt.replace {
        let never = || false;
        let quiet = |_: f32, _: &str| {};
        scan_games(
            ftp,
            console,
            &Ctl {
                cancelled: &never,
                progress: &quiet,
            },
        )?
    } else {
        vec![]
    };
    let max = max_file_for(&dest_root);
    let mut entries = Vec::new();
    let mut seen_dest: Vec<String> = Vec::new();
    for item in items {
        let name = item.name.clone();
        let mut e = Entry {
            name: name.clone(),
            title_id: item.title_id.clone(),
            kind: item.kind.clone(),
            bytes: item.size.max(0) as u64,
            steps: vec![],
            output: String::new(),
            problems: vec![],
            warnings: vec![],
            work: None,
        };
        if !item.available {
            e.problems
                .push("This game's folder is offline right now".into());
        }
        if item.content_kind != "game" {
            e.problems.push(format!(
                "This is {} content. Use Install content for add-ons and updates.",
                if item.content_kind == "dlc" {
                    "add-on (DLC)"
                } else {
                    "other"
                }
            ));
        }
        if let Some(h) = &item.health {
            e.warnings.push(format!("The source looks damaged: {h}"));
        }
        // Where it goes: `Name/TitleID` for a GOD folder, the file itself for an ISO.
        let rel = match (item.kind.as_str(), &item.title_id) {
            ("god", Some(t)) => expected(opt.layout, t, Some(&name)),
            ("god", None) => item.relpath.clone(),
            ("iso", _) => item
                .relpath
                .rsplit('/')
                .next()
                .map(safe_name_keep_ext)
                .unwrap_or_default(),
            (k, _) => {
                e.problems
                    .push(format!("{k} items can't be sent to the games folder"));
                String::new()
            }
        };
        let dest = format!("{dest_root}/{rel}");
        e.output = dest.clone();
        if seen_dest.contains(&dest) {
            e.problems
                .push("Two of the chosen games would go to the same place".into());
        }
        seen_dest.push(dest.clone());
        let tree = item.loc.tree(&item.relpath);
        let tree = match tree {
            Ok(t) => t,
            Err(err) => {
                e.problems.push(format!("Can't read the source: {err}"));
                entries.push(e);
                continue;
            }
        };
        e.bytes = tree.iter().filter(|t| !t.is_dir).map(|t| t.size).sum();
        if let Some(max) = max
            && let Some(big) = tree.iter().filter(|t| !t.is_dir).max_by_key(|t| t.size)
            && big.size > max
        {
            e.problems.push(format!(
                "A file of {} is too big for a USB stick (FAT32 holds files under 4 GB). {}",
                fmt_bytes(big.size),
                if item.kind == "iso" {
                    "Convert the ISO to GOD first: its files are small."
                } else {
                    ""
                }
            ));
        }
        // What is already on the console.
        let mut removals = Vec::new();
        if opt.replace {
            if item.kind != "god" {
                e.warnings.push(
                    "Replace only swaps Games on Demand folders; an ISO is simply overwritten if it has the same name."
                        .into(),
                );
            } else if let Some(t) = &item.title_id {
                for g in on_console
                    .iter()
                    .filter(|g| g.title_id.eq_ignore_ascii_case(t))
                {
                    if g.content
                        .iter()
                        .filter(|c| GAME_CONTENT.iter().any(|x| x.eq_ignore_ascii_case(c)))
                        .count()
                        == 0
                    {
                        continue;
                    }
                    if item.discs.unwrap_or(1) > 1 {
                        e.problems.push(format!("\"{}\" is a multi-disc game on the console; remove it from there first, then send it again.", g.path));
                        continue;
                    }
                    removals.push((g.path.clone(), g.path.eq_ignore_ascii_case(&dest)));
                }
            }
        } else if let Ok(Some(st)) = ftp.stat(&dest)
            && (st.is_dir || st.size > 0)
        {
            e.warnings.push(
                "It's already on the console at that place: identical files are skipped and a clashing one is a problem. Tick Replace to swap it."
                    .into(),
            );
        }
        if !opt.replace
            && let Some(t) = &item.title_id
        {
            // Found elsewhere on the console under another name?
            let found: Vec<ConsoleGame> = scan_games_quick(ftp, console, t);
            for g in found
                .into_iter()
                .filter(|g| !g.path.eq_ignore_ascii_case(&dest))
            {
                e.warnings.push(format!(
                    "This game is already on the console as \"{}\". Tick Replace to swap that copy for this one.",
                    g.path
                ));
            }
        }
        for (p, before) in &removals {
            let line = format!(
                "Remove the old copy \"{p}\" (only the game's own files; saved games, add-ons and updates are kept)"
            );
            if *before {
                e.steps.push(line);
            }
        }
        e.steps
            .push(format!("Copy {} to the console", fmt_bytes(e.bytes)));
        for (p, before) in &removals {
            if !*before {
                e.steps.push(format!(
                    "Remove the old copy \"{p}\" once the new one is in place"
                ));
            }
        }
        e.work = Some(Work {
            src: item.loc.clone(),
            src_rel: item.relpath.clone(),
            dest,
            removals,
        });
        entries.push(e);
    }
    if entries.is_empty() {
        problems.push("Choose at least one game".into());
    }
    let ok = problems.is_empty() && entries.iter().all(|e| e.problems.is_empty());
    Ok(Plan {
        console: console.name.clone(),
        dest: dest_root,
        entries,
        problems,
        ok,
    })
}

fn safe_name_keep_ext(file: &str) -> String {
    match file.rsplit_once('.') {
        Some((stem, ext)) if !ext.is_empty() && ext.len() <= 5 => {
            format!("{}.{ext}", safe_name(stem))
        }
        _ => safe_name(file),
    }
}

/// Title folders for one game, found by listing only the games folders' direct children where
/// the usual layouts put them (`Name/TitleID` or `TitleID`).
fn scan_games_quick(ftp: &mut Ftp, c: &Console, title_id: &str) -> Vec<ConsoleGame> {
    let mut out = Vec::new();
    for root in &c.game_paths {
        let root = ftp_path(root);
        let Ok(top) = ftp.list(&root) else { continue };
        for e in top.into_iter().filter(|e| e.is_dir) {
            if e.name.eq_ignore_ascii_case(title_id) {
                out.push(game_at(&format!("{root}/{}", e.name), title_id));
            } else if let Ok(inner) = ftp.list(&format!("{root}/{}", e.name)) {
                for x in inner
                    .into_iter()
                    .filter(|x| x.is_dir && x.name.eq_ignore_ascii_case(title_id))
                {
                    out.push(game_at(&format!("{root}/{}/{}", e.name, x.name), title_id));
                }
            }
        }
    }
    out
}

fn game_at(path: &str, title_id: &str) -> ConsoleGame {
    ConsoleGame {
        title_id: title_id.to_uppercase(),
        name: String::new(),
        path: path.to_string(),
        content: vec![],
        size: 0,
    }
}

/// Remove a game's own files from a title folder on the console, leaving saved games, add-ons,
/// updates and anything else. The folder (and a now-empty name folder above it) goes only if
/// nothing else is in it.
pub fn remove_old_copy(ftp: &mut Ftp, path: &str) -> Result<String, Error> {
    let mut kept = Vec::new();
    for e in ftp.list(path)? {
        if e.is_dir && GAME_CONTENT.iter().any(|g| g.eq_ignore_ascii_case(&e.name)) {
            ftp.delete_tree(&format!("{path}/{}", e.name))?;
        } else {
            kept.push(e.name);
        }
    }
    if kept.is_empty() {
        ftp.remove_dir(path)?;
        let parent = super::ftp::parent(path);
        if parent.matches('/').count() >= 2 && ftp.list(&parent).is_ok_and(|l| l.is_empty()) {
            let _ = ftp.remove_dir(&parent);
        }
        Ok(format!("removed {path}"))
    } else {
        Ok(format!(
            "removed the game from {path} and kept {}",
            kept.join(", ")
        ))
    }
}

/// Carry out one entry. Returns a note for the log.
pub fn run_entry(ftp: &mut Ftp, w: &Work, overwrite: bool, ctl: &Ctl) -> Result<String, Error> {
    let mut notes = Vec::new();
    for (p, _) in w.removals.iter().filter(|(_, before)| *before) {
        notes.push(remove_old_copy(ftp, p)?);
    }
    let s: Sent = send_tree(ftp, &w.src, &w.src_rel, &w.dest, overwrite, ctl)?;
    notes.push(format!(
        "sent {} file(s){}",
        s.files,
        if s.skipped > 0 {
            format!(", {} already there", s.skipped)
        } else {
            String::new()
        }
    ));
    for (p, _) in w.removals.iter().filter(|(_, before)| !*before) {
        notes.push(remove_old_copy(ftp, p)?);
    }
    Ok(notes.join("; "))
}
