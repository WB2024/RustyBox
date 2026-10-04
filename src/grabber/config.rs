//! The grabber's settings: indexers, SABnzbd, the quality profile, path mapping and automation.
//! Kept in `grabber.json` (owner-only: it holds API keys).

use std::{
    path::{Path, PathBuf},
    sync::RwLock,
};

use serde::{Deserialize, Serialize};

use super::{newznab::Indexer, qbit::Qbit, release::Profile, sab::Sab};
use crate::error::Error;

/// Translate a path SABnzbd reports (inside its container) into the one RustyBox sees.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathMap {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub indexers: Vec<Indexer>,
    pub sab: Sab,
    /// qBittorrent, for torrents from the torrent indexers and from folders of `.torrent` files.
    pub qbit: Qbit,
    /// Folders of `.torrent` files to pick from (as RustyBox sees them).
    pub torrent_dirs: Vec<String>,
    /// How a finished torrent reaches the library: `hardlink` (instant, and the torrent keeps
    /// seeding; copies when the disks differ), `copy`, or `move` (the torrent stops seeding).
    pub torrent_import_mode: String,
    /// After a torrent is imported: `keep` it in qBittorrent, or `remove` it with its files.
    pub torrent_after_import: String,
    pub profile: Profile,
    pub path_maps: Vec<PathMap>,
    /// Look for wanted games every this many hours (0 = only when asked).
    pub search_every_hours: u32,
    /// Grab the best release automatically (otherwise searches only list them).
    pub auto_grab: bool,
    /// Import a finished download automatically.
    pub auto_import: bool,
    /// Where finished games go: a library folder (library id, folder id).
    pub import_library: Option<(i64, i64)>,
    /// `move` (the download is removed) or `copy`.
    pub import_mode: String,
    /// Convert a downloaded ISO to Games on Demand when the library is a GOD library.
    pub convert_iso: bool,
    /// Also copy each imported game to this library folder (the Xbox's drive, say), in the same job.
    pub also_drive: Option<(i64, i64)>,
    /// Then send each imported game to this console over FTP.
    pub also_console: Option<AlsoConsole>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlsoConsole {
    pub console_id: u32,
    /// One of the console's games folders (default: its first).
    pub dest: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            indexers: vec![],
            sab: Sab {
                url: String::new(),
                api_key: String::new(),
                category: "xbox360".into(),
            },
            qbit: Qbit {
                category: "xbox360".into(),
                ..Default::default()
            },
            torrent_dirs: vec![],
            torrent_import_mode: "hardlink".into(),
            torrent_after_import: "keep".into(),
            profile: Profile::default(),
            path_maps: vec![],
            search_every_hours: 0,
            auto_grab: false,
            auto_import: true,
            import_library: None,
            import_mode: "move".into(),
            convert_iso: true,
            also_drive: None,
            also_console: None,
        }
    }
}

impl Config {
    /// The path as RustyBox sees it.
    pub fn map_path(&self, p: &str) -> String {
        for m in &self.path_maps {
            let from = m.from.trim_end_matches('/');
            if !from.is_empty() && (p == from || p.starts_with(&format!("{from}/"))) {
                return format!("{}{}", m.to.trim_end_matches('/'), &p[from.len()..]);
            }
        }
        p.to_string()
    }

    /// What the browser may see: no keys.
    pub fn public(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.insert(
                "indexers".into(),
                self.indexers
                    .iter()
                    .map(|i| i.public())
                    .collect::<Vec<_>>()
                    .into(),
            );
            if let Some(s) = o.get_mut("sab").and_then(|s| s.as_object_mut()) {
                s.remove("api_key");
                s.insert("api_key_set".into(), (!self.sab.api_key.is_empty()).into());
            }
            if let Some(q) = o.get_mut("qbit").and_then(|s| s.as_object_mut()) {
                q.remove("password");
                q.insert(
                    "password_set".into(),
                    (!self.qbit.password.is_empty()).into(),
                );
            }
        }
        v
    }

    pub fn validate(mut self) -> Result<Config, Error> {
        self.sab = self.sab.validate()?;
        self.qbit = self.qbit.validate()?;
        if !["hardlink", "copy", "move"].contains(&self.torrent_import_mode.as_str()) {
            return Err(Error::validation(
                "A torrent is imported by hardlink, copy or move",
            ));
        }
        if !["keep", "remove"].contains(&self.torrent_after_import.as_str()) {
            return Err(Error::validation(
                "After importing a torrent: keep it or remove it",
            ));
        }
        self.torrent_dirs = {
            let mut seen = Vec::new();
            for d in &self.torrent_dirs {
                let d = d.trim().trim_end_matches('/').to_string();
                if !d.is_empty() && !seen.contains(&d) {
                    seen.push(d);
                }
            }
            seen
        };
        if !["move", "copy"].contains(&self.import_mode.as_str()) {
            return Err(Error::validation("Import mode is move or copy"));
        }
        if self.search_every_hours > 24 * 30 {
            return Err(Error::validation(
                "Search at most once a month, or 0 for only when asked",
            ));
        }
        for m in &mut self.path_maps {
            m.from = m.from.trim().to_string();
            m.to = m.to.trim().to_string();
            if m.from.is_empty()
                || m.to.is_empty()
                || !m.from.starts_with('/')
                || !m.to.starts_with('/')
            {
                return Err(Error::validation(
                    "A path mapping needs two absolute paths, like /downloads to /data/downloads",
                ));
            }
        }
        if self.profile.formats.is_empty() {
            return Err(Error::validation("Choose at least one format you want"));
        }
        if self.profile.min_iso_mb > self.profile.max_iso_mb {
            return Err(Error::validation(
                "The smallest disc size is bigger than the largest",
            ));
        }
        Ok(self)
    }
}

pub struct Store {
    path: PathBuf,
    cur: RwLock<Config>,
}

impl Store {
    pub fn load(dir: &Path) -> Store {
        let path = dir.join("grabber.json");
        let cur = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Store {
            path,
            cur: RwLock::new(cur),
        }
    }

    pub fn get(&self) -> Config {
        self.cur.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Change the config and save it (validated first).
    pub fn update<T>(&self, f: impl FnOnce(&mut Config) -> Result<T, Error>) -> Result<T, Error> {
        let mut guard = self.cur.write().unwrap_or_else(|e| e.into_inner());
        let mut next = guard.clone();
        let out = f(&mut next)?;
        let next = next.validate()?;
        crate::fsops::write_private(&self.path, &serde_json::to_vec_pretty(&next)?)?;
        *guard = next;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sab_paths_are_mapped_on_folder_boundaries() {
        let c = Config {
            path_maps: vec![PathMap {
                from: "/data".into(),
                to: "/data/main20tb".into(),
            }],
            ..Default::default()
        };
        assert_eq!(
            c.map_path("/data/Download/USENET/Complete/xbox360/Halo"),
            "/data/main20tb/Download/USENET/Complete/xbox360/Halo"
        );
        assert_eq!(c.map_path("/data"), "/data/main20tb");
        assert_eq!(
            c.map_path("/database/x"),
            "/database/x",
            "a folder that merely starts with the same letters isn't mapped"
        );
        assert_eq!(c.map_path("/elsewhere/x"), "/elsewhere/x");
    }

    #[test]
    fn keys_stay_server_side_and_the_config_is_validated_and_saved_privately() {
        let d = std::env::temp_dir().join(format!("rustybox_grabcfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let st = Store::load(&d);
        st.update(|c| {
            c.indexers.push(Indexer {
                id: 1,
                name: "Geek".into(),
                url: "https://api.nzbgeek.info".into(),
                api_key: "SECRETKEY123".into(),
                ..Default::default()
            });
            c.sab = Sab {
                url: "http://sab:8085".into(),
                api_key: "SABSECRET123".into(),
                category: "xbox360".into(),
            };
            Ok(())
        })
        .unwrap();
        let text = st.get().public().to_string();
        assert!(
            !text.contains("SECRETKEY123") && !text.contains("SABSECRET123"),
            "{text}"
        );
        assert!(text.contains("\"api_key_set\":true"));
        assert_eq!(Store::load(&d).get().indexers[0].api_key, "SECRETKEY123");
        assert!(
            st.update(|c| {
                c.import_mode = "teleport".into();
                Ok(())
            })
            .is_err()
        );
        assert_eq!(st.get().import_mode, "move", "a refused change is not kept");
        assert!(
            st.update(|c| {
                c.path_maps.push(PathMap {
                    from: "data".into(),
                    to: "/x".into(),
                });
                Ok(())
            })
            .is_err()
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(d.join("grabber.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
