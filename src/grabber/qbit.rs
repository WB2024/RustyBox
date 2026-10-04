//! A qBittorrent client (its Web API v2): add a torrent (choosing which files of it to fetch),
//! watch its progress, and take it out again when it has been imported.
//!
//! The password and the session cookie are never shown: they are scrubbed from every error.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Qbit {
    /// `http://192.168.1.110:8080`
    pub url: String,
    pub username: String,
    /// Never sent to the browser.
    pub password: String,
    /// The qBittorrent category downloads go in (it can pick the folder).
    pub category: String,
}

impl Qbit {
    pub fn configured(&self) -> bool {
        !self.url.is_empty()
    }

    pub fn validate(mut self) -> Result<Qbit, Error> {
        self.url = self.url.trim().trim_end_matches('/').to_string();
        self.username = self.username.trim().to_string();
        self.category = self.category.trim().to_string();
        let rest = self
            .url
            .strip_prefix("http://")
            .or_else(|| self.url.strip_prefix("https://"));
        if !self.url.is_empty() && rest.is_none_or(|r| r.is_empty() || r.contains([' ', '?', '#']))
        {
            return Err(Error::validation(
                "qBittorrent's address must look like http://192.168.1.110:8080",
            ));
        }
        if self
            .category
            .chars()
            .any(|c| c.is_control() || "/\\".contains(c))
        {
            return Err(Error::validation("A category name can't contain slashes"));
        }
        Ok(self)
    }
}

/// What qBittorrent says about one torrent.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct TorrentInfo {
    pub hash: String,
    pub name: String,
    /// qBittorrent's own state word (`downloading`, `stalledUP`, `pausedDL`, `error`...).
    pub state: String,
    /// 0 to 100, of the files that were chosen.
    pub percent: f32,
    /// Bytes still to fetch, of the chosen files.
    pub left: u64,
    /// Where the torrent's files are (a folder, or the file for a one-file torrent), as qBittorrent sees it.
    pub content_path: String,
    pub save_path: String,
    pub ratio: f32,
}

/// What a torrent's state means for a grab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Waiting, fetching its metadata or checking.
    Queued,
    Downloading,
    /// Everything chosen has arrived (it may still be seeding).
    Done,
    Failed,
}

impl TorrentInfo {
    pub fn phase(&self) -> Phase {
        match self.state.as_str() {
            "error" | "missingFiles" => Phase::Failed,
            "uploading" | "stalledUP" | "queuedUP" | "forcedUP" | "pausedUP" | "stoppedUP"
            | "checkingUP" => Phase::Done,
            "downloading" | "stalledDL" | "forcedDL" => Phase::Downloading,
            // `pausedDL` with nothing left is a finished torrent of chosen files only.
            "pausedDL" | "stoppedDL" if self.left == 0 && self.percent >= 99.99 => Phase::Done,
            _ => Phase::Queued,
        }
    }
}

/// A torrent as a dashboard shows it.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Live {
    pub hash: String,
    pub name: String,
    pub state: String,
    pub percent: f32,
    pub size: u64,
    /// Bytes per second.
    pub down: u64,
    pub up: u64,
    /// Seconds left, if qBittorrent can say.
    pub eta: Option<u64>,
    pub ratio: f32,
    pub category: String,
    pub added: i64,
}

/// What the whole client is doing.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Overview {
    pub version: String,
    pub down: u64,
    pub up: u64,
    pub free: Option<u64>,
    pub torrents: Vec<Live>,
}

/// One file of a torrent as qBittorrent lists it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FileInfo {
    pub index: usize,
    pub name: String,
    pub size: u64,
    pub priority: u32,
}

// Logged-in sessions by server address, so a poll every few seconds doesn't log in every time.
static SESSIONS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(8))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(120))
        .user_agent("RustyBox")
        .build()
}

enum Body<'a> {
    None,
    Form(&'a [(&'a str, &'a str)]),
    Multipart(String, Vec<u8>),
}

impl Qbit {
    fn scrub(&self, mut msg: String) -> String {
        if self.password.len() >= 4 {
            msg = msg.replace(&self.password, "***");
        }
        if let Some(sid) = SESSIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.url)
            .and_then(|c| c.split_once('=').map(|(_, v)| v.to_string()))
            && sid.len() >= 6
        {
            msg = msg.replace(sid.as_str(), "***");
        }
        msg
    }

    fn sid(&self) -> Option<String> {
        SESSIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&self.url)
            .cloned()
    }

    fn login(&self) -> Result<(), Error> {
        let r = agent()
            .post(&format!("{}/api/v2/auth/login", self.url))
            .set("Referer", &self.url)
            .send_form(&[("username", &self.username), ("password", &self.password)]);
        match r {
            Ok(resp) => {
                // Success sets the session cookie: older versions also say "Ok.", newer ones answer
                // 204 with no body. A wrong password answers 200 "Fails." with no cookie.
                // The cookie is `SID=…` in older versions and `QBT_SID_<port>=…` in newer ones, so
                // whatever name it has is kept and sent back as it came.
                let cookie = resp
                    .header("set-cookie")
                    .and_then(|c| c.split(';').next())
                    .filter(|c| {
                        c.split_once('=')
                            .is_some_and(|(n, v)| n.contains("SID") && !v.is_empty())
                    })
                    .map(String::from);
                match cookie {
                    Some(sid) => {
                        SESSIONS
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(self.url.clone(), sid);
                        Ok(())
                    }
                    _ => Err(Error::coded(
                        401,
                        "QBIT_LOGIN",
                        "qBittorrent refused that user name or password",
                    )),
                }
            }
            Err(ureq::Error::Status(403, _)) => Err(Error::coded(
                403,
                "QBIT_BANNED",
                "qBittorrent has blocked this address after too many wrong logins; wait a few minutes or fix the password",
            )),
            Err(e) => Err(self.transport(e)),
        }
    }

    fn transport(&self, e: ureq::Error) -> Error {
        match e {
            ureq::Error::Status(code, _) => {
                Error::backend(format!("qBittorrent answered {code}"))
            }
            ureq::Error::Transport(t) => Error::coded(
                502,
                "QBIT_UNREACHABLE",
                self.scrub(format!(
                    "Can't reach qBittorrent: {t}. Is it running, is the Web UI switched on, and is the address right?"
                )),
            ),
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        body: &Body,
    ) -> Result<String, Error> {
        for attempt in 0..2 {
            let mut req = agent()
                .request(method, &format!("{}/api/v2/{path}", self.url))
                .set("Referer", &self.url);
            for (k, v) in query {
                req = req.query(k, v);
            }
            if let Some(sid) = self.sid() {
                req = req.set("Cookie", &sid);
            }
            let res = match body {
                Body::None => req.call(),
                Body::Form(f) => req.send_form(f),
                Body::Multipart(boundary, bytes) => req
                    .set(
                        "Content-Type",
                        &format!("multipart/form-data; boundary={boundary}"),
                    )
                    .send_bytes(bytes),
            };
            match res {
                Ok(r) => {
                    return r.into_string().map_err(|e| {
                        Error::backend(
                            self.scrub(format!("qBittorrent's answer couldn't be read: {e}")),
                        )
                    });
                }
                // Not logged in (or the session ran out): log in and try once more.
                Err(ureq::Error::Status(401 | 403, _)) if attempt == 0 => self.login()?,
                Err(ureq::Error::Status(403, _)) => {
                    return Err(Error::coded(
                        401,
                        "QBIT_LOGIN",
                        "qBittorrent wouldn't let RustyBox in (check the user name and password)",
                    ));
                }
                Err(e) => return Err(self.transport(e)),
            }
        }
        Err(Error::backend("qBittorrent wouldn't answer"))
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<String, Error> {
        self.request("GET", path, query, &Body::None)
    }

    fn post(&self, path: &str, form: &[(&str, &str)]) -> Result<String, Error> {
        self.request("POST", path, &[], &Body::Form(form))
    }

    fn json(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, Error> {
        serde_json::from_str(&self.get(path, query)?)
            .map_err(|e| Error::backend(format!("qBittorrent sent something unexpected: {e}")))
    }

    // ── What to do ───────────────────────────────────────────────────────────

    /// Check the address and login; returns qBittorrent's version. (Some setups need no login at
    /// all, on localhost or a trusted subnet: the request is tried first, and logs in only if asked.)
    pub fn version(&self) -> Result<String, Error> {
        Ok(self.get("app/version", &[])?.trim().to_string())
    }

    pub fn categories(&self) -> Result<Vec<String>, Error> {
        let v = self.json("torrents/categories", &[])?;
        let mut names: Vec<String> = v
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        names.sort();
        Ok(names)
    }

    pub fn create_category(&self, name: &str) -> Result<(), Error> {
        self.post(
            "torrents/createCategory",
            &[("category", name), ("savePath", "")],
        )?;
        if self.categories()?.iter().any(|c| c == name) {
            Ok(())
        } else {
            Err(Error::backend(
                "qBittorrent accepted the request but the category isn't there",
            ))
        }
    }

    fn multipart(fields: &[(&str, &str)], file: Option<(&str, &str, &[u8])>) -> (String, Vec<u8>) {
        let boundary = format!("----RustyBox{:x}", crate::jobs::now() ^ 0x5bd1e995);
        let mut b = Vec::new();
        for (k, v) in fields {
            b.extend(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"
                )
                .as_bytes(),
            );
        }
        if let Some((field, name, data)) = file {
            let safe: String = name
                .chars()
                .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
                .collect();
            b.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{safe}\"\r\nContent-Type: application/x-bittorrent\r\n\r\n").as_bytes());
            b.extend(data);
            b.extend(b"\r\n");
        }
        b.extend(format!("--{boundary}--\r\n").as_bytes());
        (boundary, b)
    }

    /// Add a torrent by its file, or by a magnet link. With `paused` it starts stopped, so the
    /// choice of files can be made before anything is fetched.
    pub fn add(&self, source: AddSource, paused: bool) -> Result<(), Error> {
        let mut fields: Vec<(&str, &str)> = vec![];
        if !self.category.is_empty() {
            fields.push(("category", self.category.as_str()));
        }
        if paused {
            // qBittorrent 4 calls it `paused`, 5 calls it `stopped`; each ignores the other.
            fields.push(("paused", "true"));
            fields.push(("stopped", "true"));
        }
        let (boundary, body) = match source {
            AddSource::File { name, bytes } => {
                Self::multipart(&fields, Some(("torrents", name, bytes)))
            }
            AddSource::Magnet(link) => {
                fields.push(("urls", link));
                Self::multipart(&fields, None)
            }
        };
        let r = self.request(
            "POST",
            "torrents/add",
            &[],
            &Body::Multipart(boundary, body),
        )?;
        if r.trim().eq_ignore_ascii_case("fails.") {
            return Err(Error::backend(
                "qBittorrent didn't accept that torrent (is it already added, or damaged?)",
            ));
        }
        Ok(())
    }

    pub fn info(&self, hashes: &[String]) -> Result<Vec<TorrentInfo>, Error> {
        if hashes.is_empty() {
            return Ok(vec![]);
        }
        let joined = hashes.join("|");
        let v = self.json("torrents/info", &[("hashes", joined.as_str())])?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .map(|t| TorrentInfo {
                        hash: t["hash"].as_str().unwrap_or("").to_lowercase(),
                        name: t["name"].as_str().unwrap_or("").into(),
                        state: t["state"].as_str().unwrap_or("").into(),
                        percent: (t["progress"].as_f64().unwrap_or(0.0) * 100.0) as f32,
                        left: t["amount_left"].as_u64().unwrap_or(0),
                        content_path: t["content_path"].as_str().unwrap_or("").into(),
                        save_path: t["save_path"].as_str().unwrap_or("").into(),
                        ratio: t["ratio"].as_f64().unwrap_or(0.0) as f32,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn files(&self, hash: &str) -> Result<Vec<FileInfo>, Error> {
        let v = self.json("torrents/files", &[("hash", hash)])?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .enumerate()
                    .map(|(i, f)| FileInfo {
                        index: f["index"].as_u64().map(|n| n as usize).unwrap_or(i),
                        name: f["name"].as_str().unwrap_or("").into(),
                        size: f["size"].as_u64().unwrap_or(0),
                        priority: f["priority"].as_u64().unwrap_or(1) as u32,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Don't fetch these files (priority 0).
    pub fn skip_files(&self, hash: &str, indexes: &[usize]) -> Result<(), Error> {
        self.set_priority(hash, indexes, 0)
    }

    /// Set the priority of some of a torrent's files (0 = skip, 1 = normal). Done in batches so
    /// the request stays a sensible size.
    pub fn set_priority(&self, hash: &str, indexes: &[usize], prio: u8) -> Result<(), Error> {
        let prio = prio.to_string();
        for chunk in indexes.chunks(1500) {
            let ids = chunk
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join("|");
            self.post(
                "torrents/filePrio",
                &[
                    ("hash", hash),
                    ("id", ids.as_str()),
                    ("priority", prio.as_str()),
                ],
            )?;
        }
        Ok(())
    }

    /// Start a stopped torrent (`start` in qBittorrent 5, `resume` in 4).
    pub fn start(&self, hash: &str) -> Result<(), Error> {
        match self.post("torrents/start", &[("hashes", hash)]) {
            Ok(_) => Ok(()),
            Err(_) => self
                .post("torrents/resume", &[("hashes", hash)])
                .map(|_| ()),
        }
    }

    /// Speeds, free space and every torrent, newest first.
    pub fn overview(&self) -> Result<Overview, Error> {
        let version = self.version()?;
        let t = self.json("transfer/info", &[])?;
        // The main data call also carries the free space of the download folder.
        let free = self
            .json("sync/maindata", &[("rid", "0")])
            .ok()
            .and_then(|v| v["server_state"]["free_space_on_disk"].as_u64());
        let list = self.json(
            "torrents/info",
            &[("sort", "added_on"), ("reverse", "true")],
        )?;
        let torrents = list
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|t| Live {
                        hash: t["hash"].as_str().unwrap_or("").to_lowercase(),
                        name: t["name"].as_str().unwrap_or("").into(),
                        state: t["state"].as_str().unwrap_or("").into(),
                        percent: (t["progress"].as_f64().unwrap_or(0.0) * 100.0) as f32,
                        size: t["size"].as_u64().unwrap_or(0),
                        down: t["dlspeed"].as_u64().unwrap_or(0),
                        up: t["upspeed"].as_u64().unwrap_or(0),
                        // qBittorrent says 8640000 for "never".
                        eta: t["eta"].as_u64().filter(|e| *e < 8_640_000),
                        ratio: t["ratio"].as_f64().unwrap_or(0.0) as f32,
                        category: t["category"].as_str().unwrap_or("").into(),
                        added: t["added_on"].as_i64().unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Overview {
            version,
            down: t["dl_info_speed"].as_u64().unwrap_or(0),
            up: t["up_info_speed"].as_u64().unwrap_or(0),
            free,
            torrents,
        })
    }

    /// Remove a torrent, and with `files` the downloaded data too.
    pub fn remove(&self, hash: &str, files: bool) -> Result<(), Error> {
        self.post(
            "torrents/delete",
            &[
                ("hashes", hash),
                ("deleteFiles", if files { "true" } else { "false" }),
            ],
        )?;
        Ok(())
    }
}

pub enum AddSource<'a> {
    File { name: &'a str, bytes: &'a [u8] },
    Magnet(&'a str),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(state: &str, left: u64, pct: f32) -> TorrentInfo {
        TorrentInfo {
            state: state.into(),
            left,
            percent: pct,
            ..Default::default()
        }
    }

    #[test]
    fn states_become_phases() {
        assert_eq!(t("downloading", 5, 10.0).phase(), Phase::Downloading);
        assert_eq!(t("metaDL", 5, 0.0).phase(), Phase::Queued);
        assert_eq!(t("stalledUP", 0, 100.0).phase(), Phase::Done);
        assert_eq!(t("pausedUP", 0, 100.0).phase(), Phase::Done);
        assert_eq!(t("error", 5, 10.0).phase(), Phase::Failed);
        assert_eq!(t("missingFiles", 5, 10.0).phase(), Phase::Failed);
        assert_eq!(
            t("pausedDL", 0, 100.0).phase(),
            Phase::Done,
            "stopped with nothing left"
        );
        assert_eq!(
            t("pausedDL", 100, 50.0).phase(),
            Phase::Queued,
            "stopped part-way is not done"
        );
    }

    #[test]
    fn the_address_is_checked() {
        let ok = |u: &str| {
            Qbit {
                url: u.into(),
                ..Default::default()
            }
            .validate()
        };
        assert_eq!(
            ok(" http://192.168.1.110:8080/ ").unwrap().url,
            "http://192.168.1.110:8080"
        );
        assert!(ok("192.168.1.110:8080").is_err());
        assert!(ok("http://x y").is_err());
        assert!(ok("").is_ok(), "empty means not set up");
        assert!(
            Qbit {
                category: "a/b".into(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn a_multipart_body_holds_the_fields_and_the_file() {
        let (b, body) = Qbit::multipart(
            &[("category", "xbox360")],
            Some(("torrents", "a\"b.torrent", b"DATA")),
        );
        let s = String::from_utf8_lossy(&body);
        assert!(s.contains(&format!("--{b}\r\n")));
        assert!(s.contains("name=\"category\"\r\n\r\nxbox360"));
        assert!(
            s.contains("filename=\"ab.torrent\""),
            "quotes are dropped from the name"
        );
        assert!(s.contains("\r\n\r\nDATA\r\n"));
        assert!(s.ends_with(&format!("--{b}--\r\n")));
    }
}
