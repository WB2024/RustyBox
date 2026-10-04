//! XboxUnity: search for games and list their title updates, and download them.

use std::{io::Read, path::Path, time::Duration};

use serde::Serialize;
use serde_json::Value;

use crate::{convert::Ctl, error::Error};

pub const DEFAULT_BASE: &str = "https://xboxunity.net";

pub struct Client {
    base: String,
    mock: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Title {
    pub title_id: String,
    pub name: String,
    pub updates: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Update {
    pub tuid: String,
    pub version: String,
    pub media_id: String,
    pub name: String,
    pub size_kb: u64,
    pub date: String,
    pub base_version: String,
}

fn text(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

impl Client {
    pub fn new(mock: bool) -> Client {
        let base = std::env::var("RUSTYBOX_XBOXUNITY_BASE").unwrap_or_else(|_| DEFAULT_BASE.into());
        Client {
            base: base.trim_end_matches('/').to_string(),
            mock,
        }
    }

    fn agent(secs: u64) -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(secs))
            .user_agent("RustyBox")
            .build()
    }

    fn json(&self, path_and_query: &str) -> Result<Value, Error> {
        let url = format!("{}/Resources/Lib/{path_and_query}", self.base);
        Self::agent(30)
            .get(&url)
            .call()
            .map_err(|e| Error::backend(format!("XboxUnity didn't answer: {e}")))?
            .into_json()
            .map_err(|e| Error::backend(format!("XboxUnity sent something unexpected: {e}")))
    }

    /// Games matching a name or title ID that have at least one update.
    pub fn search(&self, query: &str) -> Result<Vec<Title>, Error> {
        if self.mock {
            return Ok(vec![Title {
                title_id: "4D530805".into(),
                name: "Alan Wake".into(),
                updates: 2,
            }]);
        }
        let q: String = query
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
            .collect();
        let v = self.json(&format!(
            "TitleList.php?page=0&count=30&search={}&sort=3&direction=1&category=0&filter=0",
            q.replace(' ', "%20")
        ))?;
        Ok(v.get("Items")
            .and_then(|i| i.as_array())
            .map(|a| {
                a.iter()
                    .filter(|t| text(t, "TitleType") != "Xbox1")
                    .map(|t| Title {
                        title_id: text(t, "TitleID").to_uppercase(),
                        name: text(t, "Name"),
                        updates: text(t, "Updates").parse().unwrap_or(0),
                    })
                    .filter(|t| t.updates > 0)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Every update for a title, across all its releases.
    pub fn updates(&self, title_id: &str) -> Result<Vec<Update>, Error> {
        if !title_id.len().eq(&8) || !title_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::validation(
                "A title ID is eight letters and digits (0-9, A-F)",
            ));
        }
        if self.mock {
            return Ok(vec![
                Update {
                    tuid: "1001".into(),
                    version: "1".into(),
                    media_id: "54E34DF4".into(),
                    name: "Alan Wake".into(),
                    size_kb: 2,
                    date: "2012-01-01".into(),
                    base_version: "00000000".into(),
                },
                Update {
                    tuid: "1002".into(),
                    version: "2".into(),
                    media_id: "54E34DF4".into(),
                    name: "Alan Wake".into(),
                    size_kb: 2,
                    date: "2012-06-01".into(),
                    base_version: "00000000".into(),
                },
            ]);
        }
        let v = self.json(&format!(
            "TitleUpdateInfo.php?titleid={}",
            title_id.to_uppercase()
        ))?;
        let mut out = Vec::new();
        for block in v
            .get("MediaIDS")
            .and_then(|m| m.as_array())
            .into_iter()
            .flatten()
        {
            let media = text(block, "MediaID");
            for u in block
                .get("Updates")
                .and_then(|u| u.as_array())
                .into_iter()
                .flatten()
            {
                out.push(Update {
                    tuid: text(u, "TitleUpdateID"),
                    version: text(u, "Version"),
                    media_id: media.to_uppercase(),
                    name: text(u, "Name"),
                    size_kb: text(u, "Size").parse().unwrap_or(0),
                    date: text(u, "UploadDate").chars().take(10).collect(),
                    base_version: text(u, "BaseVersion"),
                });
            }
        }
        out.sort_by(|a, b| {
            (&a.media_id, a.version.parse::<u32>().unwrap_or(0))
                .cmp(&(&b.media_id, b.version.parse::<u32>().unwrap_or(0)))
        });
        Ok(out)
    }

    /// Download an update to `dest` (as `.part`, renamed when whole).
    pub fn download(
        &self,
        title_id: &str,
        up: &Update,
        dest: &Path,
        ctl: &Ctl,
    ) -> Result<u64, Error> {
        if !up.tuid.bytes().all(|b| b.is_ascii_digit()) || up.tuid.is_empty() {
            return Err(Error::validation("That isn't a valid update ID"));
        }
        let part = format!("{}.part", dest.display());
        let result = (|| -> Result<u64, Error> {
            if self.mock {
                let bytes = super::build_tu(
                    u32::from_str_radix(title_id, 16).unwrap_or(0),
                    u32::from_str_radix(&up.media_id, 16).unwrap_or(0),
                    up.version.parse().unwrap_or(1),
                    &up.name,
                );
                std::fs::write(&part, &bytes)?;
                return Ok(bytes.len() as u64);
            }
            let url = format!(
                "{}/Resources/Lib/TitleUpdate.php?tuid={}",
                self.base, up.tuid
            );
            let r = Self::agent(3600)
                .get(&url)
                .call()
                .map_err(|e| Error::backend(format!("XboxUnity wouldn't send that update: {e}")))?;
            let total: u64 = r
                .header("Content-Length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let mut rd = r.into_reader().take(512 * 1024 * 1024);
            let mut out = std::fs::File::create(&part)?;
            let mut buf = vec![0u8; 64 * 1024];
            let mut done = 0u64;
            loop {
                ctl.check()?;
                let n = rd.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut out, &buf[..n])?;
                done += n as u64;
                if total > 0 {
                    (ctl.progress)(done as f32 / total as f32, "Downloading");
                }
            }
            // It has to be a real update package, not an error page.
            let head = std::fs::read(&part)
                .map(|b| b.into_iter().take(0x3A4 + 0x1400).collect::<Vec<u8>>())?;
            match crate::xbox::stfs::parse(&head) {
                Ok(h) if h.content_type == super::TU_CONTENT_TYPE => Ok(done),
                _ => Err(Error::backend(
                    "XboxUnity sent something that isn't a title update",
                )),
            }
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
