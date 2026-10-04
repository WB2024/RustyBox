//! Planning an import: what is in the source, where each game would go, and everything that could
//! stand in the way, worked out before anything is touched.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::{
    engine::tree_bytes,
    loc::{Loc, TreeEntry},
};
use crate::{
    convert::plan::{ItemRef, safe_name},
    error::Error,
    fsops::FsInfo,
    library::{
        self, LibPath,
        scan::{Found, MetaState, Scanner, Walk},
    },
    xbox::meta::content_kind,
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Copy,
    Move,
    Hardlink,
    Symlink,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Copy => "Copy",
            Mode::Move => "Move",
            Mode::Hardlink => "Hard link",
            Mode::Symlink => "Symlink",
        }
    }
}

/// Where the things to import come from.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// A folder on this server, anywhere inside the allowed roots.
    Folder { path: String },
    /// A folder inside a library folder, which can be on a drive attached to another computer.
    Drive {
        library_id: i64,
        path_id: i64,
        #[serde(default)]
        rel: String,
    },
    /// Games already in libraries (for sending them somewhere else).
    Items { items: Vec<ItemRef> },
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct DestRef {
    pub library_id: i64,
    pub path_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    pub source: Source,
    /// Which of the source's games to import (the ids from the scan). Ignored for `Items`.
    #[serde(default)]
    pub select: Vec<String>,
    pub mode: Mode,
    pub dest: DestRef,
    /// `name_titleid` or `titleid`; defaults to the setting.
    pub layout: Option<String>,
    /// Convert ISOs to Games on Demand on the way in (the destination must be a GOD library).
    #[serde(default)]
    pub convert_iso: bool,
    #[serde(default)]
    pub overwrite: bool,
    /// Replace a game that is already in the destination: its old copy (found by title ID and
    /// disc, whatever its folder is called) is removed and the new one put in. Saved games, add-ons
    /// and title updates are never touched. Implies `overwrite` for files that clash.
    #[serde(default)]
    pub replace: bool,
    /// Also copy each game on to another place, such as the Xbox's drive.
    pub also_to: Option<DestRef>,
}

/// Something in the source that can be imported.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    /// Its path inside the source; also its id.
    pub id: String,
    /// `iso`, `god` or `file`.
    pub kind: String,
    pub name: String,
    pub size: u64,
    pub title_id: Option<String>,
    pub media_id: Option<String>,
    pub disc: Option<i64>,
    pub discs: Option<i64>,
    /// `game`, `dlc`, `update` or `other`.
    pub content_kind: String,
    /// What is wrong with it (an incomplete copy, an unreadable header).
    pub health: Option<String>,
}

/// The source, resolved to a place and a folder in it.
#[derive(Clone)]
pub struct Resolved {
    pub loc: Loc,
    /// The folder inside `loc` that candidate paths are relative to.
    pub base: String,
    pub label: String,
    /// The library the source belongs to, if any (to refresh after a move).
    pub library_id: Option<i64>,
    /// May the source be changed (needed to move from it)?
    pub writable: bool,
}

fn join(base: &str, rel: &str) -> String {
    match (
        base.trim_matches('/').is_empty(),
        rel.trim_matches('/').is_empty(),
    ) {
        (true, _) => rel.trim_matches('/').to_string(),
        (false, true) => base.trim_matches('/').to_string(),
        (false, false) => format!("{}/{}", base.trim_matches('/'), rel.trim_matches('/')),
    }
}

fn can_write(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(dir.as_os_str().as_bytes())
        .is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
}

/// Find the folder a `Source::Folder` or `Source::Drive` names.
pub fn resolve(conn: &Connection, source: &Source, roots: &[PathBuf]) -> Result<Resolved, Error> {
    match source {
        Source::Folder { path } => {
            let canon = library::validate_path(roots, path)?;
            Ok(Resolved {
                writable: can_write(&canon),
                label: canon.to_string_lossy().to_string(),
                loc: Loc::Local(canon),
                base: String::new(),
                library_id: None,
            })
        }
        Source::Drive {
            library_id,
            path_id,
            rel,
        } => {
            let lp = library::get_path(conn, *library_id, *path_id)?;
            crate::fsops::join_rel(Path::new("/"), rel)?; // no `..`
            let label = format!(
                "{}{}",
                if lp.label.is_empty() {
                    &lp.path
                } else {
                    &lp.label
                },
                if rel.trim_matches('/').is_empty() {
                    String::new()
                } else {
                    format!(" / {}", rel.trim_matches('/'))
                }
            );
            Ok(Resolved {
                loc: Loc::of(&lp),
                base: rel.trim_matches('/').to_string(),
                label,
                library_id: Some(*library_id),
                writable: lp.writable,
            })
        }
        Source::Items { .. } => Err(Error::validation(
            "Games chosen from libraries have no folder to scan",
        )),
    }
}

fn from_found(f: &Found) -> Candidate {
    let (title_id, media_id, disc, discs, name, ck, health) = match &f.meta {
        MetaState::Set(m) => (
            Some(m.title_id.clone()),
            m.media_id.clone(),
            m.disc.map(|d| d as i64),
            m.discs.map(|d| d as i64),
            m.game_name.clone(),
            content_kind(m.content_type),
            m.health.clone(),
        ),
        MetaState::Failed(e) => (
            f.title_id.clone(),
            None,
            None,
            None,
            f.title_id
                .as_deref()
                .and_then(|t| crate::xbox::catalog::get().name(t, None))
                .map(str::to_string),
            "game",
            Some(e.clone()),
        ),
        MetaState::Keep => (f.title_id.clone(), None, None, None, None, "game", None),
    };
    Candidate {
        id: f.relpath.clone(),
        kind: f.kind.clone(),
        name: name.unwrap_or_else(|| f.name.clone()),
        size: f.size,
        title_id,
        media_id,
        disc,
        discs,
        content_kind: ck.to_string(),
        health,
    }
}

/// Look in the source for ISOs and Games on Demand folders. Works on a folder here or on a drive.
pub fn discover(src: &Resolved) -> Result<Vec<Candidate>, Error> {
    let mut found: Vec<Found> = Vec::new();
    match &src.loc {
        Loc::Local(root) => {
            let base = crate::fsops::contained(root, &src.base)?;
            for scanner in [Scanner::Iso, Scanner::God] {
                let (never, quiet) = (|| false, |_: usize| {});
                let w = Walk {
                    scanner,
                    cancelled: &never,
                    progress: &quiet,
                };
                let (mut f, _) = library::scan::walk(&base, &w)?;
                library::scan::enrich(&base, scanner, &mut f, &HashMap::new(), &w)?;
                found.extend(f);
            }
        }
        Loc::Remote(r) => {
            let r = r.joined(&src.base);
            for kind in ["iso", "god"] {
                found.extend(r.scan(kind, &HashMap::new())?);
            }
        }
    }
    let mut out: Vec<Candidate> = found.iter().map(from_found).collect();
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    Ok(out)
}

// ── The plan ─────────────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub title_id: Option<String>,
    pub disc: Option<i64>,
    pub discs: Option<i64>,
    /// What will happen, in order, in words.
    pub steps: Vec<String>,
    /// Where it ends up (the main place, then the extra one if there is one).
    pub outputs: Vec<String>,
    pub bytes_in: u64,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub work: Option<Work>,
}

/// What the runner needs to carry an entry out.
#[derive(Clone)]
pub struct Work {
    pub src: Loc,
    pub src_rel: String,
    pub src_library: Option<i64>,
    pub kind: String,
    pub mode: Mode,
    pub convert: bool,
    pub dst: Loc,
    pub dst_rel: String,
    pub dst_library: i64,
    pub also: Option<(Loc, String, i64)>,
    /// For a conversion: the game's title ID, which names the folder the conversion builds.
    pub title_id: Option<String>,
    /// Old copies to remove from the destinations.
    pub removals: Vec<Removal>,
}

/// An old copy of a game that a replace removes.
#[derive(Clone)]
pub struct Removal {
    pub loc: Loc,
    pub rel: String,
    /// `iso` (a file) or `god` (a title folder: only its game content is removed).
    pub kind: String,
    /// In the extra place rather than the main one.
    pub also: bool,
    /// The old copy is where the new one goes, so it has to go first.
    pub before: bool,
    pub label: String,
}

/// A game already in a destination library.
#[derive(Debug, Clone)]
pub struct Present {
    pub library: i64,
    pub title_id: String,
    pub media_id: Option<String>,
    pub disc: Option<i64>,
    pub discs: Option<i64>,
    pub kind: String,
    pub relpath: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DestSummary {
    pub label: String,
    pub free: u64,
    pub needed: u64,
    pub fs_type: String,
}

#[derive(Clone, Serialize)]
pub struct Plan {
    pub mode: Mode,
    pub entries: Vec<Entry>,
    pub destinations: Vec<DestSummary>,
    pub problems: Vec<String>,
    pub ok: bool,
}

/// Everything about a destination the plan needs.
struct Dest {
    lp: LibPath,
    kind: String,
    loc: Loc,
    info: Option<FsInfo>,
    name: String,
    problem: Option<String>,
}

fn dest_of(conn: &Connection, r: DestRef) -> Result<Dest, Error> {
    let lib = library::get_library(conn, r.library_id)?;
    let lp = library::get_path(conn, r.library_id, r.path_id)?;
    let loc = Loc::of(&lp);
    let name = format!(
        "{} / {}",
        lib.name,
        if lp.label.is_empty() {
            &lp.path
        } else {
            &lp.label
        }
    );
    let mut problem = None;
    if !lp.writable {
        problem = Some(format!(
            "{name} is read-only. Turn on Writable for it first."
        ));
    }
    let info = match loc.info() {
        Ok(i) => Some(i),
        Err(e) => {
            problem = Some(format!("{name} isn't reachable: {e}"));
            None
        }
    };
    Ok(Dest {
        lp,
        kind: lib.kind,
        loc,
        info,
        name,
        problem,
    })
}

/// Where a game goes inside a destination.
fn place(dest_kind: &str, c: &Candidate, layout: &str, converting: bool) -> String {
    let file_name = c.id.rsplit('/').next().unwrap_or(&c.id).to_string();
    match (dest_kind, c.kind.as_str()) {
        ("iso", "iso") => file_name,
        ("god", "god") | ("god", "iso") if converting || c.kind == "god" => {
            match (&c.title_id, layout) {
                (Some(t), "titleid") => t.to_uppercase(),
                (Some(t), _) => {
                    let n = safe_name(&c.name);
                    if n.is_empty() {
                        t.to_uppercase()
                    } else {
                        format!("{n}/{}", t.to_uppercase())
                    }
                }
                // Without a title ID there is no tidy name to give it: keep where it was.
                (None, _) => c.id.clone(),
            }
        }
        _ => c.id.clone(),
    }
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

fn no_links_here(fs_type: &str) -> bool {
    matches!(
        fs_type,
        "vfat" | "msdos" | "exfat" | "ntfs" | "ntfs3" | "fuseblk" | "fuse.exfat"
    )
}

/// The pieces of the plan that need the database, gathered first.
pub struct Gathered {
    pub source: SourceSet,
    pub dest: DestPick,
    pub also: Option<DestPick>,
    /// Games already in the destination libraries: (library, title ID, media ID, path in the library).
    pub present: Vec<Present>,
}

pub enum SourceSet {
    Scanned(Resolved),
    Items(Vec<(library::Item, LibPath)>),
}

pub struct DestPick(Dest);

pub fn gather(conn: &Connection, req: &Request, roots: &[PathBuf]) -> Result<Gathered, Error> {
    let source = match &req.source {
        Source::Items { items } => {
            let mut out = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for r in items {
                if seen.insert((r.library_id, r.item_id)) {
                    out.push(library::get_item(conn, r.library_id, r.item_id)?);
                }
            }
            SourceSet::Items(out)
        }
        other => SourceSet::Scanned(resolve(conn, other, roots)?),
    };
    let mut present = Vec::new();
    let mut libs = vec![req.dest.library_id];
    libs.extend(req.also_to.map(|a| a.library_id));
    for lib in libs {
        let mut stmt = conn.prepare(
            "SELECT title_id, media_id, relpath, disc, discs, kind FROM items WHERE library_id = ?1 AND title_id IS NOT NULL AND kind IN ('iso', 'god')
             AND (content_type IS NULL OR content_type IN ('00007000', '00005000', '000D0000', '00004000'))",
        )?;
        let rows = stmt.query_map([lib], |r| {
            Ok(Present {
                library: lib,
                title_id: r.get(0)?,
                media_id: r.get(1)?,
                relpath: r.get(2)?,
                disc: r.get(3)?,
                discs: r.get(4)?,
                kind: r.get(5)?,
            })
        })?;
        present.extend(rows.collect::<Result<Vec<_>, _>>()?);
    }
    Ok(Gathered {
        source,
        dest: DestPick(dest_of(conn, req.dest)?),
        also: req
            .also_to
            .map(|r| dest_of(conn, r))
            .transpose()?
            .map(DestPick),
        present,
    })
}

/// A candidate with where it is read from: its place and path, its library, and whether that can be changed.
type Cand = (Candidate, Loc, String, Option<i64>, bool);

/// Turn what the source holds into candidates, with the place each one is read from.
fn candidates(g: &Gathered, req: &Request) -> Result<Vec<Cand>, Error> {
    let mut out = Vec::new();
    match &g.source {
        SourceSet::Scanned(r) => {
            let all = discover(r)?;
            let want: std::collections::HashSet<&String> = req.select.iter().collect();
            for c in all.into_iter().filter(|c| want.contains(&c.id)) {
                let rel = join(&r.base, &c.id);
                out.push((c, r.loc.clone(), rel, r.library_id, r.writable));
            }
            for id in &req.select {
                if !out.iter().any(|o| &o.0.id == id) {
                    return Err(Error::validation(format!(
                        "{id} wasn't found in the source any more. Scan it again."
                    )));
                }
            }
        }
        SourceSet::Items(items) => {
            for (item, lp) in items {
                let c = Candidate {
                    id: item.relpath.clone(),
                    kind: item.kind.clone(),
                    name: item.game_name.clone().unwrap_or_else(|| item.name.clone()),
                    size: item.size.max(0) as u64,
                    title_id: item.title_id.clone(),
                    media_id: item.media_id.clone(),
                    disc: item.disc,
                    discs: item.discs,
                    content_kind: item.content_kind.clone(),
                    health: item.health.clone(),
                };
                out.push((
                    c,
                    Loc::of(lp),
                    item.relpath.clone(),
                    Some(lp.library_id),
                    lp.writable,
                ));
            }
        }
    }
    Ok(out)
}

/// A place for people to read: the full path here, or the drive's name and the path on it.
fn shown(loc: &Loc, name: &str, rel: &str) -> String {
    match loc {
        Loc::Local(_) => loc.describe(rel),
        Loc::Remote(_) => format!("{name}: {}", rel.trim_start_matches('/')),
    }
}

/// What a replace removes from one destination for one game: its old copies, found by title ID and
/// disc rather than by folder name.
struct Wanted<'a> {
    c: &'a Candidate,
    lib_id: i64,
    loc: &'a Loc,
    new_rel: &'a str,
    dest_kind: &'a str,
    label: &'a str,
    also: bool,
}

fn removals_for(g: &Gathered, w: &Wanted) -> (Vec<Removal>, Vec<String>, Vec<String>) {
    let (mut out, mut problems, mut warnings) = (Vec::new(), Vec::new(), Vec::new());
    let Some(t) = &w.c.title_id else {
        warnings.push(format!(
            "Can't tell which copy in {} to replace: this game has no title ID.",
            w.label
        ));
        return (out, problems, warnings);
    };
    for p in g.present.iter().filter(|p| {
        p.library == w.lib_id
            && p.title_id.eq_ignore_ascii_case(t)
            && p.kind == w.dest_kind
            && (w.c.disc.is_none() || p.disc.is_none() || w.c.disc == p.disc)
    }) {
        if w.dest_kind == "iso" && p.relpath == w.new_rel {
            continue; // the new file simply overwrites it
        }
        if p.discs.unwrap_or(1) > 1 || w.c.discs.unwrap_or(1) > 1 {
            problems.push(format!(
                "\"{}\" in {} is a multi-disc game, and replacing one disc isn't supported yet. Remove the game from {} first, then send it again.",
                p.relpath, w.label, w.label
            ));
            continue;
        }
        let nested = w.new_rel != p.relpath
            && (w.new_rel.starts_with(&format!("{}/", p.relpath))
                || p.relpath.starts_with(&format!("{}/", w.new_rel)));
        if nested {
            problems.push(format!(
                "The old copy (\"{}\") and the new place (\"{}\") are inside each other.",
                p.relpath, w.new_rel
            ));
            continue;
        }
        if let (Some(a), Some(b)) = (&w.c.media_id, &p.media_id)
            && !a.eq_ignore_ascii_case(b)
        {
            warnings.push(format!(
                "The old copy in {} has media ID {b} and this one {a}: it may be a different release of the game.",
                w.label
            ));
        }
        out.push(Removal {
            loc: w.loc.clone(),
            rel: p.relpath.clone(),
            kind: p.kind.clone(),
            also: w.also,
            before: p.kind == "god" && p.relpath == w.new_rel,
            label: w.label.to_string(),
        });
    }
    (out, problems, warnings)
}

fn removal_text(r: &Removal) -> String {
    format!(
        "Remove the old copy \"{}\" from {}{}",
        r.rel,
        r.label,
        if r.kind == "god" {
            " (only the game's own files: saved games, add-ons and title updates are kept)"
        } else {
            ""
        }
    )
}

pub fn build(
    g: &Gathered,
    req: &Request,
    default_layout: &str,
    staging_free: Option<u64>,
) -> Result<Plan, Error> {
    let layout = req
        .layout
        .clone()
        .unwrap_or_else(|| default_layout.to_string());
    let mut problems = Vec::new();
    if !crate::settings::LAYOUTS.contains(&layout.as_str()) {
        problems.push(format!("Unknown GOD layout '{layout}'"));
    }
    let (dest, also) = (&g.dest.0, g.also.as_ref().map(|d| &d.0));
    if let Some(p) = &dest.problem {
        problems.push(p.clone());
    }
    if let Some(a) = also
        && let Some(p) = &a.problem
    {
        problems.push(p.clone());
    }
    let cands = candidates(g, req)?;
    if cands.is_empty() {
        return Err(Error::validation("Choose at least one game to import"));
    }

    let mut needed: BTreeMap<i64, u64> = BTreeMap::new(); // by destination path id
    let mut need_local_stage = 0u64;
    let mut entries = Vec::new();
    let mut seen_outputs: HashMap<String, usize> = HashMap::new();

    for (idx, (c, src, src_rel, src_lib, src_writable)) in cands.into_iter().enumerate() {
        let mut e = Entry {
            id: c.id.clone(),
            name: c.name.clone(),
            kind: c.kind.clone(),
            title_id: c.title_id.clone(),
            disc: c.disc,
            discs: c.discs,
            steps: vec![],
            outputs: vec![],
            bytes_in: c.size,
            problems: vec![],
            warnings: vec![],
            work: None,
        };
        if let Some(h) = &c.health {
            e.warnings.push(format!("The source looks damaged: {h}"));
        }
        if c.content_kind != "game" && c.kind != "file" {
            e.warnings.push(format!(
                "This is {} content, not a game.",
                match c.content_kind.as_str() {
                    "dlc" => "add-on (DLC)",
                    "update" => "title update",
                    _ => "other",
                }
            ));
        }
        let converting = req.convert_iso && c.kind == "iso" && dest.kind == "god";
        // Is this game already in a destination, perhaps under a different name?
        if let Some(t) = &c.title_id {
            for (label, lib_id) in std::iter::once((&dest.name, dest.lp.library_id))
                .chain(also.map(|a| (&a.name, a.lp.library_id)))
            {
                let same = |media: &Option<String>| {
                    c.media_id.is_none() || media.is_none() || *media == c.media_id
                };
                if !req.replace
                    && let Some(p) = g
                        .present
                        .iter()
                        .find(|p| p.library == lib_id && &p.title_id == t && same(&p.media_id))
                {
                    e.warnings.push(format!(
                        "This game is already in {label} (as \"{}\"). Tick \"Replace the game\" to swap the old copy for this one.",
                        p.relpath
                    ));
                }
            }
        }

        // What kind of thing can go where.
        match (c.kind.as_str(), dest.kind.as_str(), converting) {
            ("iso", "iso", _) | ("god", "god", _) | ("iso", "god", true) => {}
            ("iso", "god", false) => e.problems.push("This is an ISO and the destination is a GOD library. Tick \"convert ISOs to GOD\", or choose an ISO library.".into()),
            ("god", "iso", _) => e.problems.push("This is a GOD folder and the destination is an ISO library. Use Convert to ISO from the library page.".into()),
            (_, other, _) if matches!(other, "iso" | "god") => e.problems.push(format!("{} items can't go into a {} library.", c.kind.to_uppercase(), other.to_uppercase())),
            _ => {}
        }
        if converting && matches!(req.mode, Mode::Hardlink | Mode::Symlink) {
            e.problems.push(
                "Links can't be used when converting: the converted game is new data.".into(),
            );
        }

        let dst_rel = place(&dest.kind, &c, &layout, converting);
        let mut removals = Vec::new();
        if req.replace {
            let (r, p, w) = removals_for(
                g,
                &Wanted {
                    c: &c,
                    lib_id: dest.lp.library_id,
                    loc: &dest.loc,
                    new_rel: &dst_rel,
                    dest_kind: &dest.kind,
                    label: &dest.name,
                    also: false,
                },
            );
            e.problems.extend(p);
            e.warnings.extend(w);
            removals.extend(r);
        }
        // The tree being imported (sizes, the biggest file).
        let tree: Vec<TreeEntry> = match src.tree(&src_rel) {
            Ok(t) => t,
            Err(err) => {
                e.problems.push(format!("Can't read the source: {err}"));
                entries.push(e);
                continue;
            }
        };
        let bytes = tree_bytes(&tree);
        e.bytes_in = bytes;
        let is_file = tree.len() == 1 && tree[0].rel.is_empty();

        // Mode feasibility.
        let same_device =
            src.device(&src_rel).is_some() && src.device(&src_rel) == dest.loc.device(&dst_rel);
        match req.mode {
            Mode::Copy => {}
            Mode::Move if !src_writable => {
                e.problems.push(
                    "The source can't be changed, so it can't be moved from. Use Copy.".into(),
                );
            }
            Mode::Hardlink | Mode::Symlink if !converting => {
                if src.is_remote() || dest.loc.is_remote() {
                    e.problems.push(format!("{} only works between folders on this server, not with a drive on another computer. Use Copy.", req.mode.label()));
                } else {
                    if req.mode == Mode::Hardlink && !same_device {
                        e.problems.push("A hard link needs both folders on the same filesystem, and these are on different ones. Use Copy or Move.".into());
                    }
                    if let Some(i) = &dest.info
                        && no_links_here(&i.fs_type)
                    {
                        e.problems.push(format!(
                            "The destination is {}, which can't hold links. Use Copy or Move.",
                            i.fs_type.to_uppercase()
                        ));
                    }
                    if req.mode == Mode::Symlink {
                        e.warnings.push("A symlink points at the original's place inside RustyBox, so it only works here (not for Samba, Aurora or the host), and breaks if the original is moved.".into());
                    }
                }
            }
            _ => {}
        }

        // Destination checks: conflicts, file size limits.
        let dest_tree = dest.loc.tree(&dst_rel).ok();
        let renames = req.mode == Mode::Move && same_device && dest_tree.is_none() && !converting;
        if !converting {
            if let Some(existing) = &dest_tree {
                let mut clash = Vec::new();
                let mut identical = 0;
                for t in tree.iter().filter(|t| !t.is_dir) {
                    let rel = if is_file {
                        String::new()
                    } else {
                        t.rel.clone()
                    };
                    if let Some(x) = existing.iter().find(|x| x.rel == rel && !x.is_dir) {
                        if x.size == t.size {
                            identical += 1;
                        } else {
                            clash.push(t.rel.clone());
                        }
                    }
                }
                if !(clash.is_empty() || req.overwrite || req.replace) {
                    e.problems.push(format!("Already there with a different size: {}{}. Tick \"Replace the game\" to swap it.", clash[0], if clash.len() > 1 { format!(" (and {} more)", clash.len() - 1) } else { String::new() }));
                } else if identical > 0 {
                    e.warnings.push(format!(
                        "{identical} file(s) are already there and won't be copied again.{}",
                        if req.mode == Mode::Move {
                            " The source copy is removed."
                        } else {
                            ""
                        }
                    ));
                } else if existing.iter().any(|x| !x.is_dir) || is_file {
                    e.warnings.push("The folder already exists: new files are added to it (multi-disc games share a folder).".into());
                }
            }
            if let Some(i) = &dest.info
                && let Some(max) = i.max_file
                && let Some(big) = tree
                    .iter()
                    .filter(|t| !t.is_dir && t.size > max)
                    .max_by_key(|t| t.size)
            {
                e.problems.push(format!(
                    "{} is {}, over the {} limit of 4 GB for a single file. {}",
                    if big.rel.is_empty() {
                        "The file".to_string()
                    } else {
                        big.rel.clone()
                    },
                    fmt_bytes(big.size),
                    i.fs_type.to_uppercase(),
                    if c.kind == "iso" {
                        "Convert it to GOD instead (its files are small)."
                    } else {
                        ""
                    }
                ));
            }
        } else if let Some(existing) = &dest_tree {
            // A converted game goes to Name/TitleID; if that exists, the new disc's container is added to it.
            if existing.iter().any(|x| !x.is_dir) {
                e.warnings.push("The game's folder already exists: the converted disc is added to it. An identical container is a conflict.".into());
            }
        }

        // Space.
        let out_bytes = if converting {
            bytes + bytes / 100 + 1024 * 1024
        } else if matches!(req.mode, Mode::Hardlink | Mode::Symlink) || renames {
            0
        } else {
            bytes
        };
        *needed.entry(dest.lp.id).or_default() += out_bytes;
        if converting && (dest.loc.is_remote() || src.is_remote()) {
            need_local_stage += bytes + bytes / 100;
        }

        // The steps in words.
        if converting {
            if src.is_remote() {
                e.steps.push("Copy the ISO here from the drive".into());
            }
            e.steps.push("Convert to Games on Demand".into());
        }
        e.steps.extend(
            removals
                .iter()
                .filter(|r| !r.also && r.before)
                .map(removal_text),
        );
        e.steps.push(match (converting, req.mode) {
            (true, Mode::Move) => format!("Place it in {} (and remove the ISO)", dest.name),
            (true, _) => format!("Place it in {} (the ISO stays where it is)", dest.name),
            (false, m) => format!(
                "{} to {}{}",
                m.label(),
                dest.name,
                if renames {
                    " (instant: same filesystem)"
                } else {
                    ""
                }
            ),
        });
        e.steps.extend(
            removals
                .iter()
                .filter(|r| !r.also && !r.before)
                .map(|r| format!("{} once the new copy is in place", removal_text(r))),
        );
        e.outputs.push(shown(&dest.loc, &dest.name, &dst_rel));
        if seen_outputs
            .insert(dest.loc.describe(&dst_rel), idx)
            .is_some()
            && !converting
            && c.kind != "god"
        {
            e.problems
                .push("Two of the chosen games would go to the same place.".into());
        }

        // The extra destination.
        let mut also_work = None;
        if let Some(a) = also {
            let a_rel = place(
                &a.kind,
                &Candidate {
                    kind: if converting {
                        "god".into()
                    } else {
                        c.kind.clone()
                    },
                    ..c.clone()
                },
                &layout,
                false,
            );
            let a_rel = if a.kind == "god" || a.kind == "iso" {
                a_rel
            } else {
                dst_rel.clone()
            };
            if a.kind != dest.kind && a.kind != "custom" {
                e.warnings.push(format!(
                    "The extra place is a {} library; the game is copied there as it is.",
                    a.kind.to_uppercase()
                ));
            }
            // What would be copied there: the same files as arrive in the main destination.
            let a_bytes = if converting {
                bytes + bytes / 100
            } else {
                bytes
            };
            *needed.entry(a.lp.id).or_default() += a_bytes;
            if let Some(i) = &a.info
                && let Some(max) = i.max_file
                && !converting
                && let Some(big) = tree
                    .iter()
                    .filter(|t| !t.is_dir && t.size > max)
                    .max_by_key(|t| t.size)
            {
                e.problems.push(format!(
                    "{} is {}: too big for {} ({}).",
                    if big.rel.is_empty() {
                        "The file".to_string()
                    } else {
                        big.rel.clone()
                    },
                    fmt_bytes(big.size),
                    a.name,
                    i.fs_type.to_uppercase()
                ));
            }
            if let Ok(existing) = a.loc.tree(&a_rel)
                && existing.iter().any(|x| !x.is_dir)
            {
                e.warnings.push(format!("Something is already at {}: identical files are skipped, different ones are a problem.", a.name));
            }
            if req.replace {
                let (r, p, w) = removals_for(
                    g,
                    &Wanted {
                        c: &c,
                        lib_id: a.lp.library_id,
                        loc: &a.loc,
                        new_rel: &a_rel,
                        dest_kind: &a.kind,
                        label: &a.name,
                        also: true,
                    },
                );
                e.problems.extend(p);
                e.warnings.extend(w);
                e.steps
                    .extend(r.iter().filter(|x| x.before).map(removal_text));
                e.steps.push(format!("Copy to {}", a.name));
                e.steps.extend(
                    r.iter()
                        .filter(|x| !x.before)
                        .map(|x| format!("{} once the new copy is in place", removal_text(x))),
                );
                removals.extend(r);
            } else {
                e.steps.push(format!("Copy to {}", a.name));
            }
            e.outputs.push(shown(&a.loc, &a.name, &a_rel));
            also_work = Some((a.loc.clone(), a_rel, a.lp.library_id));
        }

        e.work = Some(Work {
            src,
            src_rel,
            src_library: src_lib,
            kind: c.kind.clone(),
            mode: req.mode,
            convert: converting,
            dst: dest.loc.clone(),
            dst_rel,
            dst_library: dest.lp.library_id,
            also: also_work,
            title_id: c.title_id.clone(),
            removals,
        });
        entries.push(e);
    }

    // Free space on each destination (a place used twice is counted once).
    let mut summaries = Vec::new();
    for d in std::iter::once(dest).chain(also) {
        let need = needed.get(&d.lp.id).copied().unwrap_or(0);
        if summaries.iter().any(|s: &DestSummary| s.label == d.name) {
            continue;
        }
        let free = d.info.as_ref().map(|i| i.free).unwrap_or(0);
        if d.info.is_some() && need + 256 * 1024 * 1024 > free && need > 0 {
            problems.push(format!(
                "Not enough free space on {}: about {} are needed (plus a margin) and {} are free.",
                d.name,
                fmt_bytes(need),
                fmt_bytes(free)
            ));
        }
        summaries.push(DestSummary {
            label: d.name.clone(),
            free,
            needed: need,
            fs_type: d
                .info
                .as_ref()
                .map(|i| i.fs_type.clone())
                .unwrap_or_default(),
        });
    }
    if let Some(free) = staging_free
        && need_local_stage > 0
        && need_local_stage + 256 * 1024 * 1024 > free
    {
        problems.push(format!("Not enough free space here to prepare the conversion: about {} are needed temporarily and {} are free.", fmt_bytes(need_local_stage), fmt_bytes(free)));
    }
    let ok = problems.is_empty() && entries.iter().all(|e| e.problems.is_empty());
    Ok(Plan {
        mode: req.mode,
        entries,
        destinations: summaries,
        problems,
        ok,
    })
}
