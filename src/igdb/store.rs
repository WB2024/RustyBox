//! What IGDB told us, kept so each game is only looked up once: the details in the database,
//! the cover images in a folder next to it.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::Candidate;
use crate::error::Error;

/// A game with no match is looked up again after this long (IGDB gains entries over time).
pub const RETRY_UNMATCHED_AFTER: i64 = 30 * 24 * 3600;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GameMeta {
    pub title_id: String,
    /// `ok`: matched automatically. `manual`: chosen by hand (never replaced). `none`: no match found.
    pub status: String,
    pub igdb_id: Option<i64>,
    pub name: Option<String>,
    pub summary: Option<String>,
    pub release: Option<i64>,
    pub genres: Vec<String>,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub rating: Option<f64>,
    pub url: Option<String>,
    /// The cover's file name in the covers folder, if one has been saved.
    pub cover: Option<String>,
    pub fetched_at: i64,
}

fn join(v: &[String]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

fn split(s: Option<String>) -> Vec<String> {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

const COLS: &str = "title_id, status, igdb_id, name, summary, release, genres, developers, publishers, rating, url, cover, fetched_at";

fn map(r: &rusqlite::Row) -> rusqlite::Result<GameMeta> {
    Ok(GameMeta {
        title_id: r.get(0)?,
        status: r.get(1)?,
        igdb_id: r.get(2)?,
        name: r.get(3)?,
        summary: r.get(4)?,
        release: r.get(5)?,
        genres: split(r.get(6)?),
        developers: split(r.get(7)?),
        publishers: split(r.get(8)?),
        rating: r.get(9)?,
        url: r.get(10)?,
        cover: r.get(11)?,
        fetched_at: r.get(12)?,
    })
}

pub fn get(conn: &Connection, title_id: &str) -> Result<Option<GameMeta>, Error> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM game_meta WHERE title_id = ?1"),
            [title_id],
            map,
        )
        .optional()?)
}

pub fn put_match(
    conn: &Connection,
    title_id: &str,
    c: &Candidate,
    cover_file: Option<&str>,
    manual: bool,
) -> Result<(), Error> {
    conn.execute(
        &format!(
            "INSERT INTO game_meta({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(title_id) DO UPDATE SET status = excluded.status, igdb_id = excluded.igdb_id, name = excluded.name, summary = excluded.summary,
                 release = excluded.release, genres = excluded.genres, developers = excluded.developers, publishers = excluded.publishers,
                 rating = excluded.rating, url = excluded.url, cover = excluded.cover, fetched_at = excluded.fetched_at"
        ),
        params![
            title_id,
            if manual { "manual" } else { "ok" },
            c.id,
            c.name,
            c.summary,
            c.release,
            join(&c.genres),
            join(&c.developers),
            join(&c.publishers),
            c.rating,
            c.url,
            cover_file,
            crate::jobs::now() as i64
        ],
    )?;
    Ok(())
}

/// Remember that nothing matched (unless a human already chose one), clearing anything older.
pub fn put_none(conn: &Connection, title_id: &str) -> Result<(), Error> {
    conn.execute(
        &format!(
            "INSERT INTO game_meta({COLS}) VALUES (?1, 'none', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, ?2)
             ON CONFLICT(title_id) DO UPDATE SET status = 'none', igdb_id = NULL, name = NULL, summary = NULL, release = NULL, genres = NULL,
                 developers = NULL, publishers = NULL, rating = NULL, url = NULL, cover = NULL, fetched_at = excluded.fetched_at
             WHERE game_meta.status <> 'manual'"
        ),
        params![title_id, crate::jobs::now() as i64],
    )?;
    Ok(())
}

pub fn remove(conn: &Connection, title_id: &str) -> Result<(), Error> {
    conn.execute("DELETE FROM game_meta WHERE title_id = ?1", [title_id])?;
    Ok(())
}

/// Games in ISO and GOD libraries still needing a lookup, with the name to search for.
/// `retry_unmatched` also includes ones that found nothing a while ago.
pub fn pending(conn: &Connection, retry_unmatched: bool) -> Result<Vec<(String, String)>, Error> {
    let cutoff = crate::jobs::now() as i64 - RETRY_UNMATCHED_AFTER;
    let mut stmt = conn.prepare(
        "SELECT i.title_id, COALESCE(MAX(i.game_name), MIN(i.name)) FROM items i
         LEFT JOIN game_meta m ON m.title_id = i.title_id
         WHERE i.title_id IS NOT NULL AND i.kind IN ('iso', 'god')
           AND (m.title_id IS NULL OR (?1 AND m.status = 'none' AND m.fetched_at < ?2))
         GROUP BY i.title_id ORDER BY i.title_id",
    )?;
    let rows = stmt.query_map(params![retry_unmatched, cutoff], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn covers_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("covers")
}

/// A safe file name for a cover (IGDB image IDs are letters and digits).
pub fn cover_file_name(image_id: &str, ext: &str) -> String {
    let id: String = image_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("{id}.{ext}")
}

/// Every game that has details, for attaching to the games list.
pub fn all_ok(conn: &Connection) -> Result<std::collections::HashMap<String, GameMeta>, Error> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM game_meta WHERE status IN ('ok', 'manual')"
    ))?;
    let rows = stmt.query_map([], map)?;
    Ok(rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|m| (m.title_id.clone(), m))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn cand(id: i64, name: &str) -> Candidate {
        Candidate {
            id,
            name: name.into(),
            summary: Some("s".into()),
            release: Some(1),
            genres: vec!["Action".into()],
            developers: vec!["Dev".into()],
            publishers: vec![],
            rating: Some(80.0),
            url: None,
            cover: Some("co1".into()),
        }
    }

    #[test]
    fn stores_and_reads_back_a_match_and_keeps_manual_choices() {
        let db = Db::open_memory().unwrap();
        db.with(|c| {
            assert!(get(c, "4D530805")?.is_none());
            put_match(c, "4D530805", &cand(1, "Alan Wake"), Some("co1.jpg"), false)?;
            let m = get(c, "4D530805")?.unwrap();
            assert_eq!(
                (
                    m.status.as_str(),
                    m.name.as_deref(),
                    m.cover.as_deref(),
                    m.genres.clone()
                ),
                (
                    "ok",
                    Some("Alan Wake"),
                    Some("co1.jpg"),
                    vec!["Action".to_string()]
                )
            );
            // A later "no match" replaces an automatic result, clearing its details.
            put_none(c, "4D530805")?;
            let m = get(c, "4D530805")?.unwrap();
            assert_eq!((m.status.as_str(), m.name, m.cover), ("none", None, None));
            // But a hand-picked match is never replaced by an automatic "none".
            put_match(c, "4D530805", &cand(2, "Chosen"), None, true)?;
            put_none(c, "4D530805")?;
            let m = get(c, "4D530805")?.unwrap();
            assert_eq!(
                (m.status.as_str(), m.name.as_deref()),
                ("manual", Some("Chosen"))
            );
            remove(c, "4D530805")?;
            assert!(get(c, "4D530805")?.is_none());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn pending_lists_games_without_info_and_retries_old_misses() {
        let db = Db::open_memory().unwrap();
        db.with(|c| {
            c.execute("INSERT INTO libraries(id,name,kind,created) VALUES (1,'a','iso',0)", [])?;
            c.execute("INSERT INTO library_paths(id,library_id,path) VALUES (1,1,'/x')", [])?;
            let add = |rel: &str, tid: Option<&str>, name: Option<&str>| {
                c.execute("INSERT INTO items(library_id,path_id,relpath,name,kind,size,mtime,title_id,game_name) VALUES (1,1,?1,?1,'iso',1,0,?2,?3)", params![rel, tid, name]).unwrap();
            };
            add("a.iso", Some("AAAAAAAA"), Some("Game A"));
            add("a2.iso", Some("AAAAAAAA"), Some("Game A"));
            add("b.iso", Some("BBBBBBBB"), None);
            add("c.iso", Some("CCCCCCCC"), Some("Game C"));
            add("none.iso", None, None);
            let p = pending(c, false)?;
            assert_eq!(p, vec![("AAAAAAAA".to_string(), "Game A".to_string()), ("BBBBBBBB".to_string(), "b.iso".to_string()), ("CCCCCCCC".to_string(), "Game C".to_string())]);
            put_match(c, "AAAAAAAA", &cand(1, "Game A"), None, false)?;
            put_none(c, "CCCCCCCC")?;
            assert_eq!(pending(c, false)?.len(), 1, "matched and recently-missed games are done");
            c.execute("UPDATE game_meta SET fetched_at = fetched_at - ?1 WHERE title_id = 'CCCCCCCC'", [RETRY_UNMATCHED_AFTER + 10])?;
            assert_eq!(pending(c, false)?.len(), 1);
            assert_eq!(pending(c, true)?.len(), 2, "an old miss is retried when asked");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn cover_names_cannot_escape_the_folder() {
        assert_eq!(cover_file_name("co2dft", "jpg"), "co2dft.jpg");
        assert_eq!(cover_file_name("../../etc/passwd", "jpg"), "etcpasswd.jpg");
    }
}
