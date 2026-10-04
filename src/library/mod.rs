//! Libraries: named collections of paths (on any disk or share mounted into the container)
//! that RustyBox presents as one place.

pub mod dedupe;
pub mod health;
pub mod misplaced;
pub mod scan;
pub mod tidy;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// What a library holds. The kind picks the scanner and, later, the install rules.
pub struct Kind {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub scanner: scan::Scanner,
    pub blurb: &'static str,
}

pub const KINDS: &[Kind] = &[
    Kind {
        id: "iso",
        label: "ISO games",
        icon: "💿",
        scanner: scan::Scanner::Iso,
        blurb: "Xbox 360 disc images (.iso)",
    },
    Kind {
        id: "god",
        label: "GOD games",
        icon: "🎮",
        scanner: scan::Scanner::God,
        blurb: "Games on Demand folders (title ID / content type / container)",
    },
    Kind {
        id: "mods",
        label: "Mods",
        icon: "🧩",
        scanner: scan::Scanner::Files,
        blurb: "Game mods",
    },
    Kind {
        id: "dlc",
        label: "Add-ons (DLC)",
        icon: "📦",
        scanner: scan::Scanner::Files,
        blurb: "Downloadable content (add-ons) for games",
    },
    Kind {
        id: "cheats",
        label: "Cheats",
        icon: "🕹️",
        scanner: scan::Scanner::Files,
        blurb: "Cheat files and patches",
    },
    Kind {
        id: "trainers",
        label: "Trainers",
        icon: "🏋️",
        scanner: scan::Scanner::Files,
        blurb: "Game trainers",
    },
    Kind {
        id: "homebrew",
        label: "Homebrew",
        icon: "🛠️",
        scanner: scan::Scanner::Files,
        blurb: "Homebrew apps and dashboards",
    },
    Kind {
        id: "saves",
        label: "Game saves",
        icon: "💾",
        scanner: scan::Scanner::Files,
        blurb: "Save files",
    },
    Kind {
        id: "patches",
        label: "Title updates",
        icon: "🩹",
        scanner: scan::Scanner::Files,
        blurb: "Title updates and patches",
    },
    Kind {
        id: "emulators",
        label: "Emulators",
        icon: "👾",
        scanner: scan::Scanner::Files,
        blurb: "Emulators and their ROMs",
    },
    Kind {
        id: "custom",
        label: "Custom",
        icon: "📁",
        scanner: scan::Scanner::Files,
        blurb: "Any files you want to keep together",
    },
];

pub fn kind(id: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.id == id)
}

#[derive(Debug, Clone, Serialize)]
pub struct LibPath {
    pub id: i64,
    pub library_id: i64,
    /// A folder inside the container, or for a remote folder the agent's address.
    pub path: String,
    pub label: String,
    pub writable: bool,
    pub priority: i64,
    pub last_scan: Option<i64>,
    /// Set when this folder is a drive on another machine, served by an agent.
    pub remote_url: Option<String>,
    /// The agent's secret. Never sent to the browser.
    #[serde(skip)]
    pub remote_token: Option<String>,
    /// The folder on the drive this library folder starts at (empty: the whole drive).
    pub remote_subdir: Option<String>,
    /// What this folder is for: `` (games, or anything), `dlc` or `updates` (a tidy moves each
    /// add-on and title update into the folder with its role), `content` (the console's own
    /// `Content` folder), or `trainers`, `mods`, `homebrew`, `emulators`.
    pub role: String,
}

/// The roles a library folder can have.
pub const ROLES: &[&str] = &[
    "",
    "dlc",
    "updates",
    "content",
    "trainers",
    "mods",
    "homebrew",
    "emulators",
];

impl LibPath {
    pub fn is_remote(&self) -> bool {
        self.remote_url.is_some()
    }

    /// A client for this folder's agent, if it has one.
    pub fn remote(&self) -> Option<crate::remote::Remote> {
        Some(
            crate::remote::Remote::new(
                self.remote_url.as_deref()?,
                self.remote_token.as_deref().unwrap_or(""),
            )
            .with_prefix(self.remote_subdir.as_deref().unwrap_or("")),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Library {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub paths: Vec<LibPath>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewPath {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub writable: bool,
    /// Instead of a folder here, a drive on another machine.
    #[serde(default)]
    pub remote: Option<NewRemote>,
    /// What the folder holds (see `ROLES`).
    #[serde(default)]
    pub role: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewRemote {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub token: String,
    /// Use the address and token of this folder (of any library) instead of typing them again.
    /// The token never leaves the server.
    #[serde(default)]
    pub same_as: Option<i64>,
    /// Make the folder on the drive if it isn't there yet.
    #[serde(default)]
    pub create: bool,
    /// A folder on the drive to start from, such as `Games`.
    #[serde(default)]
    pub subdir: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: i64,
    pub path_id: i64,
    pub path_label: String,
    pub relpath: String,
    pub name: String,
    pub kind: String,
    pub size: i64,
    pub mtime: i64,
    pub title_id: Option<String>,
    pub available: bool,
    pub media_id: Option<String>,
    pub game_name: Option<String>,
    pub disc: Option<i64>,
    pub discs: Option<i64>,
    pub platform: Option<String>,
    /// `ok` once the file's header has been read, `error` if it couldn't be, empty if not tried.
    pub meta: Option<String>,
    pub meta_error: Option<String>,
    /// `game`, `dlc` (add-on content), `update` or `other`. ISO and unreadable items count as games.
    pub content_kind: String,
    /// What is wrong with the files (for example an incomplete copy), if anything.
    pub health: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Stats {
    pub items: i64,
    pub bytes: i64,
}

/// A path the user may add: absolute, an existing folder, and inside one of the allowed roots.
/// Returns the canonical path, which is what gets stored.
pub fn validate_path(roots: &[PathBuf], raw: &str) -> Result<PathBuf, Error> {
    let raw = raw.trim();
    if !raw.starts_with('/') {
        return Err(Error::validation("Use a full path that starts with /"));
    }
    let canon = std::fs::canonicalize(raw).map_err(|_| {
        Error::validation(format!(
            "{raw} doesn't exist (or isn't mounted in the container)"
        ))
    })?;
    if !canon.is_dir() {
        return Err(Error::validation(format!("{raw} isn't a folder")));
    }
    if !is_within_roots(roots, &canon) {
        return Err(Error::validation(format!(
            "{raw} is outside the folders RustyBox may use. Mount it under one of: {}",
            roots_text(roots)
        )));
    }
    Ok(canon)
}

pub fn is_within_roots(roots: &[PathBuf], canon: &Path) -> bool {
    roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .any(|r| canon.starts_with(r))
}

pub fn roots_text(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|r| r.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn clean_name(name: &str) -> Result<String, Error> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 64 || n.chars().any(char::is_control) {
        return Err(Error::validation(
            "Give the library a name of up to 64 characters",
        ));
    }
    Ok(n.to_string())
}

fn map_unique(e: rusqlite::Error, what: &str) -> Error {
    match &e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Error::conflict(what)
        }
        _ => e.into(),
    }
}

fn now() -> i64 {
    crate::jobs::now() as i64
}

// ── Libraries ────────────────────────────────────────────────────────────────

pub fn create_library(
    conn: &mut Connection,
    name: &str,
    kind_id: &str,
    paths: &[(PathBuf, NewPath)],
) -> Result<i64, Error> {
    let name = clean_name(name)?;
    if kind(kind_id).is_none() {
        return Err(Error::validation(format!(
            "Unknown library kind '{kind_id}'"
        )));
    }
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO libraries(name, kind, created) VALUES (?1, ?2, ?3)",
        params![name, kind_id, now()],
    )
    .map_err(|e| map_unique(e, &format!("A library called '{name}' already exists")))?;
    let id = tx.last_insert_rowid();
    for (i, (canon, p)) in paths.iter().enumerate() {
        tx.execute(
            "INSERT INTO library_paths(library_id, path, label, writable, priority, remote_url, remote_token, remote_subdir, role) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                canon.to_string_lossy(),
                p.label.trim(),
                p.writable,
                i as i64,
                p.remote.as_ref().map(|r| r.url.clone()),
                p.remote.as_ref().map(|r| r.token.clone()),
                p.remote.as_ref().and_then(|r| r.subdir.clone()),
                check_role(&p.role)?
            ],
        )
        .map_err(|e| map_unique(e, "That folder is listed twice"))?;
    }
    tx.commit()?;
    Ok(id)
}

fn read_paths(conn: &Connection, library_id: Option<i64>) -> Result<Vec<LibPath>, Error> {
    let mut stmt = conn.prepare(
        "SELECT id, library_id, path, label, writable, priority, last_scan, remote_url, remote_token, remote_subdir, role FROM library_paths
         WHERE (?1 IS NULL OR library_id = ?1) ORDER BY priority, id",
    )?;
    let rows = stmt.query_map(params![library_id], |r| {
        Ok(LibPath {
            id: r.get(0)?,
            library_id: r.get(1)?,
            path: r.get(2)?,
            label: r.get(3)?,
            writable: r.get(4)?,
            priority: r.get(5)?,
            last_scan: r.get(6)?,
            remote_url: r.get(7)?,
            remote_token: r.get(8)?,
            remote_subdir: r.get(9)?,
            role: r.get(10)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn list_libraries(conn: &Connection) -> Result<Vec<Library>, Error> {
    let paths = read_paths(conn, None)?;
    let mut stmt =
        conn.prepare("SELECT id, name, kind FROM libraries ORDER BY name COLLATE NOCASE")?;
    let libs = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for l in libs {
        let (id, name, kind) = l?;
        out.push(Library {
            id,
            name,
            kind,
            paths: paths
                .iter()
                .filter(|p| p.library_id == id)
                .cloned()
                .collect(),
        });
    }
    Ok(out)
}

pub fn get_library(conn: &Connection, id: i64) -> Result<Library, Error> {
    let row = conn
        .query_row(
            "SELECT name, kind FROM libraries WHERE id = ?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    let (name, kind) = row.ok_or_else(|| Error::not_found(format!("No library #{id}")))?;
    Ok(Library {
        id,
        name,
        kind,
        paths: read_paths(conn, Some(id))?,
    })
}

pub fn rename_library(conn: &Connection, id: i64, name: &str) -> Result<(), Error> {
    let name = clean_name(name)?;
    let n = conn
        .execute(
            "UPDATE libraries SET name = ?1 WHERE id = ?2",
            params![name, id],
        )
        .map_err(|e| map_unique(e, &format!("A library called '{name}' already exists")))?;
    if n == 0 {
        Err(Error::not_found(format!("No library #{id}")))
    } else {
        Ok(())
    }
}

/// Removes the library and its index. Files on disk are never touched.
pub fn delete_library(conn: &Connection, id: i64) -> Result<(), Error> {
    let n = conn.execute("DELETE FROM libraries WHERE id = ?1", [id])?;
    if n == 0 {
        Err(Error::not_found(format!("No library #{id}")))
    } else {
        Ok(())
    }
}

pub fn add_path(
    conn: &Connection,
    library_id: i64,
    canon: &Path,
    p: &NewPath,
) -> Result<i64, Error> {
    get_library(conn, library_id)?;
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(priority) + 1, 0) FROM library_paths WHERE library_id = ?1",
        [library_id],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO library_paths(library_id, path, label, writable, priority, remote_url, remote_token, remote_subdir, role) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            library_id,
            canon.to_string_lossy(),
            p.label.trim(),
            p.writable,
            next,
            p.remote.as_ref().map(|r| r.url.clone()),
            p.remote.as_ref().map(|r| r.token.clone()),
            p.remote.as_ref().and_then(|r| r.subdir.clone()),
            check_role(&p.role)?
        ],
    )
    .map_err(|e| map_unique(e, "That folder is already in this library"))?;
    Ok(conn.last_insert_rowid())
}

fn check_role(role: &str) -> Result<&str, Error> {
    if ROLES.contains(&role) {
        Ok(role)
    } else {
        Err(Error::validation(format!(
            "Unknown folder role '{role}' (use games, dlc or updates)"
        )))
    }
}

pub fn update_path(
    conn: &Connection,
    library_id: i64,
    path_id: i64,
    label: Option<&str>,
    writable: Option<bool>,
    role: Option<&str>,
) -> Result<(), Error> {
    get_path(conn, library_id, path_id)?;
    if let Some(r) = role {
        conn.execute(
            "UPDATE library_paths SET role = ?1 WHERE id = ?2",
            params![check_role(r)?, path_id],
        )?;
    }
    if let Some(l) = label {
        conn.execute(
            "UPDATE library_paths SET label = ?1 WHERE id = ?2",
            params![l.trim(), path_id],
        )?;
    }
    if let Some(w) = writable {
        conn.execute(
            "UPDATE library_paths SET writable = ?1 WHERE id = ?2",
            params![w, path_id],
        )?;
    }
    Ok(())
}

/// Make this folder the library's primary one: where things that belong to the library are put
/// (for example by Fix misplaced items). It is the first folder in the library's order.
pub fn make_primary(conn: &Connection, library_id: i64, path_id: i64) -> Result<(), Error> {
    get_path(conn, library_id, path_id)?;
    conn.execute(
        "UPDATE library_paths SET priority = (SELECT MIN(priority) - 1 FROM library_paths WHERE library_id = ?1) WHERE id = ?2",
        params![library_id, path_id],
    )?;
    Ok(())
}

/// Removes the path and its index entries. Files on disk are never touched.
pub fn delete_path(conn: &Connection, library_id: i64, path_id: i64) -> Result<(), Error> {
    get_path(conn, library_id, path_id)?;
    conn.execute("DELETE FROM library_paths WHERE id = ?1", [path_id])?;
    Ok(())
}

/// A folder by its id alone, whichever library it is in.
pub fn path_by_id(conn: &Connection, path_id: i64) -> Result<LibPath, Error> {
    read_paths(conn, None)?
        .into_iter()
        .find(|p| p.id == path_id)
        .ok_or_else(|| Error::not_found(format!("No folder #{path_id}")))
}

pub fn get_path(conn: &Connection, library_id: i64, path_id: i64) -> Result<LibPath, Error> {
    read_paths(conn, Some(library_id))?
        .into_iter()
        .find(|p| p.id == path_id)
        .ok_or_else(|| Error::not_found(format!("No folder #{path_id} in library #{library_id}")))
}

pub fn stats(conn: &Connection) -> Result<std::collections::HashMap<i64, Stats>, Error> {
    let mut stmt = conn.prepare(
        "SELECT library_id, count(*), COALESCE(sum(size), 0) FROM items GROUP BY library_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            Stats {
                items: r.get(1)?,
                bytes: r.get(2)?,
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

// ── Items ────────────────────────────────────────────────────────────────────

pub struct ItemQuery {
    pub q: String,
    pub path_id: Option<i64>,
    pub limit: i64,
    pub offset: i64,
    /// `name` (the default), `size`, `mtime` or `title`.
    pub sort: String,
    pub descending: bool,
}

/// The ORDER BY for a sort name. Only these exact strings ever reach the SQL.
fn order_by(sort: &str, descending: bool) -> String {
    let dir = if descending { "DESC" } else { "ASC" };
    let main = match sort {
        "size" => format!("i.size {dir}"),
        "mtime" => format!("i.mtime {dir}"),
        "title" => format!("i.title_id {dir}"),
        _ => format!("COALESCE(i.game_name, i.name) COLLATE NOCASE {dir}"),
    };
    format!("{main}, i.relpath")
}

pub fn list_items(
    conn: &Connection,
    library_id: i64,
    query: &ItemQuery,
) -> Result<(Vec<Item>, i64), Error> {
    let like = format!(
        "%{}%",
        query
            .q
            .trim()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let filter = "i.library_id = ?1 AND (?2 IS NULL OR i.path_id = ?2) AND (i.name LIKE ?3 ESCAPE '\\' OR i.relpath LIKE ?3 ESCAPE '\\' OR i.title_id LIKE ?3 ESCAPE '\\' OR i.game_name LIKE ?3 ESCAPE '\\')";
    let total: i64 = conn.query_row(
        &format!("SELECT count(*) FROM items i WHERE {filter}"),
        params![library_id, query.path_id, like],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {ITEM_COLS} FROM items i JOIN library_paths p ON p.id = i.path_id
         WHERE {filter} ORDER BY {} LIMIT ?4 OFFSET ?5",
        order_by(&query.sort, query.descending)
    ))?;
    let rows = stmt.query_map(
        params![library_id, query.path_id, like, query.limit, query.offset],
        map_item,
    )?;
    Ok((rows.collect::<Result<_, _>>()?, total))
}

const ITEM_COLS: &str = "i.id, i.path_id, p.label, p.path, i.relpath, i.name, i.kind, i.size, i.mtime, i.title_id, i.available, \
     i.media_id, i.game_name, i.disc, i.discs, i.platform, i.meta, i.meta_error, i.content_type, i.health";

fn map_item(r: &rusqlite::Row) -> rusqlite::Result<Item> {
    let label: String = r.get(2)?;
    let path: String = r.get(3)?;
    Ok(Item {
        id: r.get(0)?,
        path_id: r.get(1)?,
        path_label: if label.is_empty() { path } else { label },
        relpath: r.get(4)?,
        name: r.get(5)?,
        kind: r.get(6)?,
        size: r.get(7)?,
        mtime: r.get(8)?,
        title_id: r.get(9)?,
        available: r.get(10)?,
        media_id: r.get(11)?,
        game_name: r.get(12)?,
        disc: r.get(13)?,
        discs: r.get(14)?,
        platform: r.get(15)?,
        meta: r.get(16)?,
        meta_error: r.get(17)?,
        content_kind: crate::xbox::meta::content_kind(
            r.get::<_, Option<String>>(18)?
                .and_then(|c| u32::from_str_radix(&c, 16).ok()),
        )
        .to_string(),
        health: r.get(19)?,
    })
}

/// An item with the folder it lives in, for download.
pub fn get_item(
    conn: &Connection,
    library_id: i64,
    item_id: i64,
) -> Result<(Item, LibPath), Error> {
    let item = conn
        .query_row(
            &format!("SELECT {ITEM_COLS} FROM items i JOIN library_paths p ON p.id = i.path_id WHERE i.id = ?1 AND i.library_id = ?2"),
            params![item_id, library_id],
            map_item,
        )
        .optional()?
        .ok_or_else(|| Error::not_found(format!("No item #{item_id}")))?;
    let path = get_path(conn, library_id, item.path_id)?;
    Ok((item, path))
}

// ── Games: every copy of a title, across all libraries ──────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Copy {
    pub item_id: i64,
    pub library_id: i64,
    pub library_name: String,
    pub path_label: String,
    pub relpath: String,
    pub kind: String,
    pub size: i64,
    pub available: bool,
    pub disc: Option<i64>,
    pub discs: Option<i64>,
    pub media_id: Option<String>,
    /// What is wrong with this copy's files, if anything (e.g. an incomplete copy).
    pub health: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Game {
    pub title_id: String,
    pub name: Option<String>,
    pub copies: Vec<Copy>,
    pub bytes: i64,
    pub has_iso: bool,
    pub has_god: bool,
    /// The same disc of the same release more than once in one library (e.g. the same ISO on two
    /// disks of it).
    pub duplicate: bool,
    /// What IGDB says about it, if it has been looked up and matched.
    pub info: Option<crate::igdb::store::GameMeta>,
}

/// All games found in ISO and GOD libraries, one row per title ID.
pub fn games(conn: &Connection, q: &str) -> Result<Vec<Game>, Error> {
    let mut stmt = conn.prepare(
        "SELECT i.title_id, i.game_name, i.name, i.id, i.library_id, l.name, p.label, p.path, i.relpath, i.kind, i.size, i.available, i.disc, i.discs, i.media_id, i.health
         FROM items i JOIN libraries l ON l.id = i.library_id JOIN library_paths p ON p.id = i.path_id
         WHERE i.title_id IS NOT NULL AND i.kind IN ('iso', 'god')
           AND (i.content_type IS NULL OR i.content_type IN ('00007000', '00005000', '000D0000', '00004000'))
         ORDER BY i.title_id, i.disc, i.relpath",
    )?;
    let rows = stmt.query_map([], |r| {
        let label: String = r.get(6)?;
        let path: String = r.get(7)?;
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
            Copy {
                item_id: r.get(3)?,
                library_id: r.get(4)?,
                library_name: r.get(5)?,
                path_label: if label.is_empty() { path } else { label },
                relpath: r.get(8)?,
                kind: r.get(9)?,
                size: r.get(10)?,
                available: r.get(11)?,
                disc: r.get(12)?,
                discs: r.get(13)?,
                media_id: r.get(14)?,
                health: r.get(15)?,
            },
        ))
    })?;
    let mut by_title: std::collections::BTreeMap<String, Game> = std::collections::BTreeMap::new();
    for row in rows {
        let (tid, game_name, _item_name, copy) = row?;
        let g = by_title.entry(tid.clone()).or_insert_with(|| Game {
            title_id: tid,
            name: None,
            copies: vec![],
            bytes: 0,
            has_iso: false,
            has_god: false,
            duplicate: false,
            info: None,
        });
        if g.name.is_none() {
            g.name = game_name;
        }
        g.bytes += copy.size;
        g.has_iso |= copy.kind == "iso";
        g.has_god |= copy.kind == "god";
        g.copies.push(copy);
    }
    let needle = q.trim().to_lowercase();
    let mut info = crate::igdb::store::all_ok(conn)?;
    let mut out = Vec::new();
    for (tid, mut g) in by_title {
        g.info = info.remove(&tid);
        let mut seen = std::collections::HashSet::new();
        // The same disc of the same release twice in one library (or one drive) is a duplicate.
        // A copy in a library and another on the Xbox's drive is the point, not a duplicate, and
        // different discs or different titles that share a title ID (indie games) are not either.
        g.duplicate = !g.copies.iter().all(|c| {
            seen.insert((
                c.library_id,
                c.kind.clone(),
                c.disc.unwrap_or(0),
                // With no media ID to tell releases apart (indie games share one title ID),
                // the folder name has to.
                c.media_id.clone().unwrap_or_else(|| {
                    if c.kind == "god" {
                        c.relpath
                            .split('/')
                            .next()
                            .unwrap_or_default()
                            .to_lowercase()
                    } else {
                        String::new()
                    }
                }),
            ))
        });
        if !needle.is_empty() {
            let files: Vec<&str> = g.copies.iter().map(|c| c.relpath.as_str()).collect();
            let hay = format!(
                "{} {} {}",
                tid,
                g.name.clone().unwrap_or_default(),
                files.join(" ")
            )
            .to_lowercase();
            if !hay.contains(&needle) {
                continue;
            }
        }
        out.push(g);
    }
    out.sort_by_key(|g| {
        g.name
            .clone()
            .unwrap_or_else(|| g.title_id.clone())
            .to_lowercase()
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[allow(clippy::too_many_arguments)]
    fn item(
        c: &Connection,
        lib: i64,
        path: i64,
        rel: &str,
        kind: &str,
        title: Option<&str>,
        name: Option<&str>,
        disc: Option<i64>,
    ) {
        c.execute(
            "INSERT INTO items(library_id, path_id, relpath, name, kind, size, mtime, title_id, game_name, disc, available) VALUES (?1, ?2, ?3, ?3, ?4, 100, 0, ?5, ?6, ?7, 1)",
            params![lib, path, rel, kind, title, name, disc],
        )
        .unwrap();
    }

    #[test]
    fn games_group_copies_across_libraries_and_flag_duplicates() {
        let db = Db::open_memory().unwrap();
        db.with(|c| {
            c.execute("INSERT INTO libraries(id,name,kind,created) VALUES (1,'ISOs','iso',0),(2,'GOD','god',0)", [])?;
            c.execute("INSERT INTO library_paths(id,library_id,path,label) VALUES (1,1,'/a','Disk A'),(2,1,'/b','Disk B'),(3,2,'/c','GOD disk')", [])?;
            // Halo: the same ISO on two disks (a duplicate), plus a GOD copy.
            item(c, 1, 1, "Halo.iso", "iso", Some("4D5307E6"), Some("Halo 3"), Some(1));
            item(c, 1, 2, "Halo copy.iso", "iso", Some("4D5307E6"), Some("Halo 3"), Some(1));
            item(c, 2, 3, "Halo 3/4D5307E6", "god", Some("4D5307E6"), Some("Halo 3"), Some(1));
            // Two discs of one game are not duplicates.
            item(c, 1, 1, "Lost 1.iso", "iso", Some("41560001"), Some("Lost Odyssey"), Some(1));
            item(c, 1, 1, "Lost 2.iso", "iso", Some("41560001"), Some("Lost Odyssey"), Some(2));
            // The same GOD in a library and on the Xbox's drive (another library) is not a duplicate.
            c.execute("INSERT INTO libraries(id,name,kind,created) VALUES (3,'Xbox drive','god',0)", [])?;
            c.execute("INSERT INTO library_paths(id,library_id,path,label) VALUES (4,3,'/d','my-pc')", [])?;
            item(c, 2, 3, "Fable/4D5307F1", "god", Some("4D5307F1"), Some("Fable II"), Some(1));
            item(c, 3, 4, "Fable II/4D5307F1", "god", Some("4D5307F1"), Some("Fable II"), Some(1));
            // Several indie games share one title ID but have their own media IDs.
            for (rel, media) in [("A/584E07D2", "00000001"), ("B/584E07D2", "00000002")] {
                item(c, 2, 3, rel, "god", Some("584E07D2"), Some("Indie Games"), Some(1));
                c.execute("UPDATE items SET media_id = ?1 WHERE relpath = ?2", params![media, rel])?;
            }
            // ...or none at all, told apart by folder name.
            item(c, 2, 3, "C/584E07D2", "god", Some("584E07D2"), Some("Indie Games"), Some(1));
            item(c, 2, 3, "D/584E07D2", "god", Some("584E07D2"), Some("Indie Games"), Some(1));
            // Unknown name, and an item with no title ID at all (must not appear).
            item(c, 2, 3, "Mystery/AAAAAAAA", "god", Some("AAAAAAAA"), None, None);
            item(c, 1, 1, "Broken.iso", "iso", None, None, None);
            // Add-on content (DLC) isn't a game and stays out of the list.
            item(c, 2, 3, "Halo DLC/4D5307E6", "god", Some("4D5307E6"), Some("Halo 3"), Some(1));
            c.execute("UPDATE items SET content_type = '00000002' WHERE relpath = 'Halo DLC/4D5307E6'", [])?;
            let g = games(c, "")?;
            let by = |t: &str| g.iter().find(|g| g.title_id == t).unwrap();
            assert_eq!(g.len(), 5);
            assert!(!by("4D5307F1").duplicate, "library plus drive");
            assert!(!by("584E07D2").duplicate, "different media IDs");
            let halo = by("4D5307E6");
            assert!(halo.duplicate && halo.has_iso && halo.has_god);
            assert_eq!(halo.copies.len(), 3);
            assert_eq!(halo.bytes, 300);
            assert!(!by("41560001").duplicate);
            assert!(by("AAAAAAAA").name.is_none());
            // Search by name, title ID or file name.
            assert_eq!(games(c, "odyssey")?.len(), 1);
            assert_eq!(games(c, "aaaaaaaa")?.len(), 1);
            assert_eq!(games(c, "Halo copy")?.len(), 1);
            Ok(())
        })
        .unwrap();
    }
}

// ── Comparing two libraries ──────────────────────────────────────────────────

/// One game's copies on each side of a comparison.
#[derive(Debug, Clone, Serialize)]
pub struct Match {
    pub title_id: String,
    pub name: Option<String>,
    pub media_id: Option<String>,
    pub disc: Option<i64>,
    pub discs: Option<i64>,
    pub a: Vec<CompareCopy>,
    pub b: Vec<CompareCopy>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompareCopy {
    pub item_id: i64,
    pub library_id: i64,
    pub relpath: String,
    pub kind: String,
    pub size: i64,
    pub available: bool,
    pub health: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    pub both: Vec<Match>,
    pub only_a: Vec<Match>,
    pub only_b: Vec<Match>,
}

/// A game in one library as the comparison reads it: title, name, media, disc, discs, and the copy.
type CompareRow = (
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    CompareCopy,
);

/// Compare the games in two libraries. A game is the same if it has the same title ID and the same
/// media ID (which tells discs and regions apart); when a media ID isn't known on one side, the
/// title ID and disc number are used instead. DLC, updates and other non-game content are left out.
pub fn compare(conn: &Connection, a: i64, b: i64) -> Result<Comparison, Error> {
    get_library(conn, a)?;
    get_library(conn, b)?;
    let load = |lib: i64| -> Result<Vec<CompareRow>, Error> {
        let mut stmt = conn.prepare(
            "SELECT title_id, game_name, media_id, disc, discs, id, relpath, kind, size, available, health FROM items
             WHERE library_id = ?1 AND title_id IS NOT NULL AND kind IN ('iso', 'god')
               AND (content_type IS NULL OR content_type IN ('00007000', '00005000', '000D0000', '00004000')) ORDER BY title_id, disc, relpath",
        )?;
        let rows = stmt.query_map([lib], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                CompareCopy {
                    item_id: r.get(5)?,
                    library_id: lib,
                    relpath: r.get(6)?,
                    kind: r.get(7)?,
                    size: r.get(8)?,
                    available: r.get(9)?,
                    health: r.get(10)?,
                },
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    };
    let (la, lb) = (load(a)?, load(b)?);
    let mut matches: Vec<Match> = Vec::new();
    let same = |m: &Match, t: &str, media: &Option<String>, disc: Option<i64>| {
        m.title_id == t
            && match (&m.media_id, media) {
                (Some(x), Some(y)) => x == y,
                _ => m.disc.unwrap_or(1) == disc.unwrap_or(1),
            }
    };
    for (side, rows) in [(0, la), (1, lb)] {
        for (t, name, media, disc, discs, copy) in rows {
            let idx = matches.iter().position(|m| same(m, &t, &media, disc));
            let m = match idx {
                Some(i) => &mut matches[i],
                None => {
                    matches.push(Match {
                        title_id: t.clone(),
                        name: name.clone(),
                        media_id: media.clone(),
                        disc,
                        discs,
                        a: vec![],
                        b: vec![],
                    });
                    matches.last_mut().unwrap()
                }
            };
            if m.name.is_none() {
                m.name = name;
            }
            if m.media_id.is_none() {
                m.media_id = media;
            }
            if side == 0 {
                m.a.push(copy)
            } else {
                m.b.push(copy)
            }
        }
    }
    let by_name = |a: &Match, b: &Match| {
        a.name
            .clone()
            .unwrap_or_else(|| a.title_id.clone())
            .to_lowercase()
            .cmp(
                &b.name
                    .clone()
                    .unwrap_or_else(|| b.title_id.clone())
                    .to_lowercase(),
            )
            .then(a.disc.cmp(&b.disc))
    };
    matches.sort_by(by_name);
    let mut out = Comparison {
        both: vec![],
        only_a: vec![],
        only_b: vec![],
    };
    for m in matches {
        match (m.a.is_empty(), m.b.is_empty()) {
            (false, false) => out.both.push(m),
            (false, true) => out.only_a.push(m),
            (true, false) => out.only_b.push(m),
            (true, true) => {}
        }
    }
    Ok(out)
}
