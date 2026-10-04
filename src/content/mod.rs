//! Mods, homebrew, trainers, cheats, saves and patches from the Arisen Studio database: fetched
//! and cached on disk, searched, and installed to the console (download, unpack, send by FTP).

pub mod install;

use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use serde::Serialize;
use serde_json::Value;

use crate::{convert::Ctl, error::Error};

pub const DEFAULT_BASE: &str = "https://db.arisen.studio";
pub const KINDS: [&str; 6] = ["mods", "homebrew", "trainers", "cheats", "saves", "patches"];

/// A file to download for an item, with where on the console it goes (templates).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FileSpec {
    pub name: String,
    pub url: String,
    pub install_paths: Vec<String>,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Item {
    /// `kind:key`, stable between refreshes.
    pub id: String,
    pub kind: String,
    pub name: String,
    pub category: String,
    pub category_title: String,
    pub title_id: Option<String>,
    pub game: Option<String>,
    pub version: String,
    pub author: String,
    pub mode: String,
    pub description: String,
    pub files: Vec<FileSpec>,
    /// For cheats: the cheat names (there is nothing to download, they are data).
    pub cheats: Vec<String>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

fn is_title_id(t: &str) -> bool {
    t.len() == 8 && t.bytes().all(|b| b.is_ascii_hexdigit())
}

fn files_of(v: &Value) -> Vec<FileSpec> {
    v.get("DownloadFiles")
        .or_else(|| v.get("Trainers"))
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|f| FileSpec {
                    name: s(f, "Name"),
                    url: s(f, "Url"),
                    version: s(f, "Version"),
                    install_paths: f
                        .get("InstallPaths")
                        .and_then(|x| x.as_array())
                        .map(|p| {
                            p.iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The list inside a database file, which wraps it in an object (`{"Library": [...]}`).
fn list_of<'a>(v: &'a Value, keys: &[&str]) -> &'a [Value] {
    if let Some(a) = v.as_array() {
        return a;
    }
    for k in keys {
        if let Some(a) = v.get(*k).and_then(|x| x.as_array()) {
            return a;
        }
    }
    &[]
}

fn parse_json(bytes: &[u8]) -> Result<Value, Error> {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    serde_json::from_slice(b)
        .map_err(|e| Error::backend(format!("The database file isn't valid JSON: {e}")))
}

/// Everything parsed out of the cached files.
fn parse_all(dir: &Path) -> Vec<Item> {
    let read = |name: &str| {
        std::fs::read(dir.join(name))
            .ok()
            .and_then(|b| parse_json(&b).ok())
    };
    let cat = crate::xbox::catalog::get();
    let mut cat_title: HashMap<String, String> = HashMap::new();
    let mut cat_tid: HashMap<String, String> = HashMap::new();
    if let Some(c) = read("categories.json") {
        for x in list_of(&c, &["Categories"]) {
            let id = s(x, "Id");
            cat_title.insert(id.clone(), s(x, "Title"));
            if let Some(t) = x
                .get("Regions")
                .and_then(|r| r.as_array())
                .and_then(|r| r.iter().filter_map(|t| t.as_str()).find(|t| is_title_id(t)))
            {
                cat_tid.insert(id, t.to_uppercase());
            }
        }
    }
    let mut out = Vec::new();
    for (kind, file, keys) in [
        ("mods", "game-mods.json", ["Library", "Mods"]),
        ("homebrew", "homebrew.json", ["Library", "Homebrew"]),
        ("saves", "game-saves.json", ["GameSaves", "Library"]),
    ] {
        let Some(v) = read(file) else { continue };
        for x in list_of(&v, &keys) {
            if kind == "saves"
                && !matches!(
                    s(x, "Platform").to_uppercase().as_str(),
                    "XBOX" | "XBOX360" | "X360"
                )
            {
                continue;
            }
            let category = s(x, "CategoryId");
            let title_id = {
                let region = s(x, "Region");
                if is_title_id(&region) {
                    Some(region.to_uppercase())
                } else {
                    cat_tid.get(&category).cloned()
                }
            };
            out.push(Item {
                id: format!(
                    "{kind}:{}",
                    x.get("Id").map(|i| i.to_string()).unwrap_or_default()
                ),
                kind: kind.into(),
                name: s(x, "Name"),
                category_title: cat_title
                    .get(&category)
                    .cloned()
                    .unwrap_or_else(|| category.clone()),
                category,
                game: title_id
                    .as_deref()
                    .and_then(|t| cat.name(t, None))
                    .map(String::from),
                title_id,
                version: s(x, "Version"),
                author: s(x, "CreatedBy"),
                mode: s(x, "GameMode"),
                description: s(x, "Description"),
                files: files_of(x),
                cheats: vec![],
            });
        }
    }
    if let Some(v) = read("trainers.json") {
        for x in list_of(&v, &["Library", "Trainers"]) {
            let tid = s(x, "TitleId").to_uppercase();
            let game = cat.name(&tid, None).map(String::from);
            for (i, f) in files_of(x).into_iter().enumerate() {
                out.push(Item {
                    id: format!("trainers:{tid}:{i}"),
                    kind: "trainers".into(),
                    name: f.name.clone(),
                    category: "trainers".into(),
                    category_title: "Trainers".into(),
                    title_id: Some(tid.clone()),
                    game: game.clone(),
                    version: f.version.clone(),
                    author: String::new(),
                    mode: String::new(),
                    description: s(x, "Description"),
                    files: vec![f],
                    cheats: vec![],
                });
            }
        }
    }
    if let Some(v) = read("game-cheats.json") {
        for (i, x) in list_of(&v, &["GameCheats", "Library", "Cheats"])
            .iter()
            .enumerate()
        {
            let tid = s(x, "Region").to_uppercase();
            out.push(Item {
                id: format!("cheats:{i}"),
                kind: "cheats".into(),
                name: s(x, "Game"),
                category: "cheats".into(),
                category_title: "Game cheats".into(),
                title_id: is_title_id(&tid).then_some(tid),
                game: Some(s(x, "Game")),
                version: s(x, "Version"),
                author: String::new(),
                mode: String::new(),
                description: String::new(),
                files: vec![],
                cheats: x
                    .get("Cheats")
                    .and_then(|c| c.as_array())
                    .map(|a| a.iter().map(|c| s(c, "Name")).collect())
                    .unwrap_or_default(),
            });
        }
    }
    // Patches: one .patch.toml per game, named "TITLEID - Name.patch.toml".
    if let Ok(rd) = std::fs::read_dir(dir.join("patches")) {
        let mut names: Vec<_> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        for n in names.into_iter().filter(|n| n.ends_with(".patch.toml")) {
            let stem = n.trim_end_matches(".patch.toml");
            let (tid, name) = stem.split_once(" - ").unwrap_or(("", stem));
            out.push(Item {
                id: format!("patches:{n}"),
                kind: "patches".into(),
                name: name.to_string(),
                category: "patches".into(),
                category_title: "Patches".into(),
                title_id: is_title_id(tid).then(|| tid.to_uppercase()),
                game: Some(name.to_string()),
                version: String::new(),
                author: String::new(),
                mode: String::new(),
                description: "A patch file for the Xenia emulator".into(),
                files: vec![FileSpec {
                    name: n.clone(),
                    url: format!("local-patch:{n}"),
                    install_paths: vec![],
                    version: String::new(),
                }],
                cheats: vec![],
            });
        }
    }
    out
}

/// The cached database.
pub struct Db {
    dir: PathBuf,
    base: String,
    mock: bool,
    items: RwLock<Option<Arc<Vec<Item>>>>,
}

impl Db {
    pub fn new(config_dir: &Path, mock: bool) -> Db {
        let base = std::env::var("RUSTYBOX_ARISEN_BASE").unwrap_or_else(|_| DEFAULT_BASE.into());
        Db {
            dir: config_dir.join("arisen"),
            base: base.trim_end_matches('/').to_string(),
            mock,
            items: RwLock::new(None),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn is_mock(&self) -> bool {
        self.mock
    }

    /// The parsed items (read from the cache on first use, and again after a refresh).
    pub fn items(&self) -> Arc<Vec<Item>> {
        if let Some(i) = self
            .items
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return i.clone();
        }
        let parsed = Arc::new(parse_all(&self.dir));
        *self.items.write().unwrap_or_else(|e| e.into_inner()) = Some(parsed.clone());
        parsed
    }

    pub fn get(&self, id: &str) -> Option<Item> {
        self.items().iter().find(|i| i.id == id).cloned()
    }

    /// When the cache was last filled (unix seconds).
    pub fn fetched(&self) -> Option<i64> {
        std::fs::metadata(self.dir.join("categories.json"))
            .ok()?
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
    }

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(60))
            .user_agent("RustyBox")
            .build()
    }

    fn get_bytes(&self, path: &str) -> Result<Vec<u8>, Error> {
        let url = format!("{}/{path}", self.base);
        let r = Self::agent()
            .get(&url)
            .call()
            .map_err(|e| Error::backend(format!("Couldn't fetch {url}: {e}")))?;
        let mut b = Vec::new();
        r.into_reader()
            .take(64 * 1024 * 1024)
            .read_to_end(&mut b)
            .map_err(|e| Error::backend(format!("Couldn't read {url}: {e}")))?;
        Ok(b)
    }

    /// Fetch every database file and the patches archive into the cache.
    pub fn refresh(&self, ctl: &Ctl) -> Result<usize, Error> {
        std::fs::create_dir_all(&self.dir)?;
        let files = [
            ("categories.json", "data/categories.json"),
            ("game-mods.json", "data/xbox360/game-mods.json"),
            ("homebrew.json", "data/xbox360/homebrew.json"),
            ("trainers.json", "data/xbox360/trainers.json"),
            ("game-cheats.json", "data/xbox360/game-cheats.json"),
            ("game-saves.json", "data/game-saves.json"),
        ];
        let n = files.len() + 1;
        for (i, (local, remote)) in files.iter().enumerate() {
            ctl.check()?;
            (ctl.progress)(i as f32 / n as f32, &format!("Fetching {local}"));
            let bytes = if self.mock {
                mock_file(local).as_bytes().to_vec()
            } else {
                self.get_bytes(remote)?
            };
            parse_json(&bytes)?; // never keep a file that isn't valid
            let tmp = self.dir.join(format!("{local}.part"));
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, self.dir.join(local))?;
        }
        ctl.check()?;
        (ctl.progress)(files.len() as f32 / n as f32, "Fetching patches");
        let pdir = self.dir.join("patches");
        let zip_bytes = if self.mock {
            None
        } else {
            Some(self.get_bytes("data/xbox360/game-patches.zip")?)
        };
        if let Some(zb) = zip_bytes {
            let mut z = zip::ZipArchive::new(std::io::Cursor::new(zb))
                .map_err(|e| Error::backend(format!("The patches archive is damaged: {e}")))?;
            let _ = std::fs::remove_dir_all(&pdir);
            std::fs::create_dir_all(&pdir)?;
            for i in 0..z.len() {
                let mut f = z.by_index(i).map_err(|e| Error::backend(e.to_string()))?;
                let Some(name) = f
                    .enclosed_name()
                    .and_then(|p| p.file_name().map(|n| n.to_owned()))
                else {
                    continue;
                };
                if f.is_file() && name.to_string_lossy().ends_with(".patch.toml") {
                    let mut data = Vec::new();
                    f.read_to_end(&mut data)?;
                    std::fs::write(pdir.join(name), data)?;
                }
            }
        } else {
            std::fs::create_dir_all(&pdir)?;
            std::fs::write(
                pdir.join("4D530805 - Alan Wake.patch.toml"),
                "title_name = \"Alan Wake\"\ntitle_id = \"4D530805\"\n\n[[patch]]\n    name = \"Unlock FPS\"\n    is_enabled = false\n",
            )?;
        }
        *self.items.write().unwrap_or_else(|e| e.into_inner()) = None;
        let count = self.items().len();
        (ctl.progress)(1.0, "");
        Ok(count)
    }

    /// Download a file to `dest` (written as `.part`, renamed when whole). Only files from the
    /// database's own site are fetched, whatever the database says.
    pub fn download(&self, url: &str, dest: &Path, ctl: &Ctl) -> Result<u64, Error> {
        if let Some(name) = url.strip_prefix("local-patch:") {
            let src = self
                .dir
                .join("patches")
                .join(Path::new(name).file_name().unwrap_or_default());
            std::fs::copy(&src, dest)?;
            return Ok(std::fs::metadata(dest)?.len());
        }
        let part = PathBuf::from(format!("{}.part", dest.display()));
        let result = (|| -> Result<u64, Error> {
            let mut out = std::fs::File::create(&part)?;
            if let Some(name) = url.strip_prefix("mock://").filter(|_| self.mock) {
                let bytes = mock_zip(name)?;
                std::io::Write::write_all(&mut out, &bytes)?;
                return Ok(bytes.len() as u64);
            }
            if !url.starts_with(&format!("{}/", self.base)) {
                return Err(Error::validation(format!(
                    "Refusing to download {url}: files only come from {}",
                    self.base
                )));
            }
            let r = Self::agent()
                .get(url)
                .timeout(Duration::from_secs(3600))
                .call()
                .map_err(|e| Error::backend(format!("Couldn't download {url}: {e}")))?;
            let total: u64 = r
                .header("Content-Length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let mut reader = r.into_reader().take(2 * 1024 * 1024 * 1024);
            let mut buf = vec![0u8; 256 * 1024];
            let mut done = 0u64;
            loop {
                ctl.check()?;
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut out, &buf[..n])?;
                done += n as u64;
                if total > 0 {
                    (ctl.progress)(done as f32 / total as f32, "Downloading");
                }
            }
            Ok(done)
        })();
        match result {
            Ok(n) => {
                std::fs::rename(&part, dest)?;
                Ok(n)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                Err(e)
            }
        }
    }
}

// ── Sample data for --mock ───────────────────────────────────────────────────

fn mock_file(name: &str) -> &'static str {
    match name {
        "categories.json" => {
            r#"{"LastUpdated":"2026-01-01","Categories":[{"Title":"Grand Theft Auto V","Id":"gta5","Type":"game","Regions":["545408A7"]},{"Title":"Utilities","Id":"util","Type":"homebrew","Regions":[]}]}"#
        }
        "game-mods.json" => {
            r#"{"Library":[{"Platform":"XBOX","CategoryId":"gta5","Id":1,"Name":"Sample Menu","CreatedBy":"Someone","Version":"1.0","GameMode":"MP","ModType":"XEX","Description":"A pretend mod for trying the UI.","DownloadFiles":[{"Name":"Sample Menu","Version":"1.0","Url":"mock://sample-menu.zip","InstallPaths":["Hdd:\\Arisen Studio\\{CATEGORYID}\\{NAME}\\Menu.xex"]}]}]}"#
        }
        "homebrew.json" => {
            r#"{"Library":[{"Platform":"XBOX","CategoryId":"util","Id":1,"Name":"Sample Tool","CreatedBy":"","Version":"2.0","GameMode":"","ModType":"XEX","Description":"A pretend homebrew app.","DownloadFiles":[{"Name":"Sample Tool","Version":"2.0","Url":"mock://sample-tool.zip","InstallPaths":["Hdd:\\Arisen Studio\\{CATEGORYID}\\SampleTool\\"]}]}]}"#
        }
        "trainers.json" => {
            r#"{"Library":[{"TitleId":"4D530805","Description":"","Trainers":[{"Name":"Trainer(RETROBYTE)","Type":"Aurora","Url":"mock://trainer.zip","InstallPaths":["{AURORAPATH}\\User\\Trainers\\4D530805\\Trainer(RETROBYTE)\\Trainer(RETROBYTE).xex"]}]}]}"#
        }
        "game-cheats.json" => {
            r#"{"GameCheats":[{"Region":"4D5307D5","Game":"Gears of War","Version":"01.00","Cheats":[{"Name":"Infinite Ammo","Offsets":[]},{"Name":"No Reload","Offsets":[]}]}]}"#
        }
        "game-saves.json" => {
            r#"{"GameSaves":[{"Platform":"XBOX","Id":1,"CategoryId":"gta5","Name":"100% Save","Region":"545408A7","Version":"1.0","GameMode":"SP","Description":"A pretend save.","DownloadFiles":[{"Name":"Save","Url":"mock://save.zip","InstallPaths":["Hdd:\\Content\\0000000000000000\\545408A7\\00000001\\"]}]}]}"#
        }
        _ => "{}",
    }
}

/// A tiny zip for a mock download: one file named after the thing.
fn mock_zip(name: &str) -> Result<Vec<u8>, Error> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        let inner = match name {
            "sample-menu.zip" => "Menu.xex",
            "trainer.zip" => "Trainer(RETROBYTE).xex",
            "save.zip" => "SAVE.bin",
            _ => "Tool.xex",
        };
        z.start_file(inner, o)
            .map_err(|e| Error::backend(e.to_string()))?;
        z.write_all(b"pretend file contents")?;
        z.finish().map_err(|e| Error::backend(e.to_string()))?;
    }
    Ok(buf.into_inner())
}

/// Items matching a search and filters.
pub fn search<'a>(
    items: &'a [Item],
    kind: &str,
    q: &str,
    title_ids: Option<&std::collections::HashSet<String>>,
) -> Vec<&'a Item> {
    let q = q.trim().to_lowercase();
    items
        .iter()
        .filter(|i| i.kind == kind)
        .filter(|i| {
            q.is_empty()
                || i.name.to_lowercase().contains(&q)
                || i.game
                    .as_deref()
                    .is_some_and(|g| g.to_lowercase().contains(&q))
                || i.category_title.to_lowercase().contains(&q)
                || i.title_id
                    .as_deref()
                    .is_some_and(|t| t.to_lowercase().contains(&q))
        })
        .filter(|i| match (title_ids, &i.title_id) {
            (None, _) => true,
            (Some(set), Some(t)) => set.contains(t),
            (Some(_), None) => false,
        })
        .collect()
}
