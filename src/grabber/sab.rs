//! A SABnzbd client: hand it an NZB address, then watch the queue and history.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Sab {
    /// `http://192.168.1.110:8085`
    pub url: String,
    /// Never sent to the browser.
    pub api_key: String,
    /// The SABnzbd category downloads go in (it picks the folder and post-processing).
    pub category: String,
}

impl Sab {
    pub fn configured(&self) -> bool {
        !self.url.is_empty() && !self.api_key.is_empty()
    }

    pub fn validate(mut self) -> Result<Sab, Error> {
        self.url = self.url.trim().trim_end_matches('/').to_string();
        self.category = self.category.trim().to_string();
        let rest = self
            .url
            .strip_prefix("http://")
            .or_else(|| self.url.strip_prefix("https://"));
        if !self.url.is_empty() && rest.is_none_or(|r| r.is_empty() || r.contains([' ', '?', '#']))
        {
            return Err(Error::validation(
                "SABnzbd's address must look like http://192.168.1.110:8085",
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

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct QueueSlot {
    pub nzo_id: String,
    pub name: String,
    pub status: String,
    pub percent: f32,
    pub mb: f64,
    pub mb_left: f64,
    pub time_left: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HistorySlot {
    pub nzo_id: String,
    pub name: String,
    /// `Completed` or `Failed` (or still post-processing: `Extracting`, `Verifying`...).
    pub status: String,
    /// The finished folder, as SABnzbd sees it.
    pub storage: String,
    pub fail_message: String,
}

fn scrub(msg: String, key: &str) -> String {
    if key.len() >= 6 {
        msg.replace(key, "***")
    } else {
        msg
    }
}

impl Sab {
    fn call(&self, mode: &str, extra: &[(&str, &str)]) -> Result<Value, Error> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(8))
            .timeout_read(Duration::from_secs(30))
            .user_agent("RustyBox")
            .build();
        let mut req = agent
            .get(&format!("{}/api", self.url))
            .query("mode", mode)
            .query("output", "json")
            .query("apikey", &self.api_key);
        for (k, v) in extra {
            req = req.query(k, v);
        }
        match req.call() {
            Ok(r) => {
                let v: Value = r.into_json().map_err(|e| {
                    Error::backend(format!("SABnzbd sent something unexpected: {e}"))
                })?;
                if let Some(e) = v["error"].as_str() {
                    return Err(if e.to_lowercase().contains("api key") {
                        Error::coded(401, "SAB_LOGIN", "SABnzbd refused the API key")
                    } else {
                        Error::backend(format!("SABnzbd said: {e}"))
                    });
                }
                Ok(v)
            }
            Err(ureq::Error::Status(401 | 403, _)) => Err(Error::coded(
                401,
                "SAB_LOGIN",
                "SABnzbd refused the API key",
            )),
            Err(ureq::Error::Status(code, _)) => {
                Err(Error::backend(format!("SABnzbd answered {code}")))
            }
            Err(ureq::Error::Transport(t)) => Err(Error::coded(
                502,
                "SAB_UNREACHABLE",
                scrub(
                    format!("Can't reach SABnzbd: {t}. Is it running, and is the address right?"),
                    &self.api_key,
                ),
            )),
        }
    }

    pub fn version(&self) -> Result<String, Error> {
        // `version` needs no key, so check the key with a call that does.
        self.call("get_cats", &[])?;
        Ok(self.call("version", &[])?["version"]
            .as_str()
            .unwrap_or("?")
            .to_string())
    }

    pub fn categories(&self) -> Result<Vec<String>, Error> {
        Ok(self.call("get_cats", &[])?["categories"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Make a category (its folder is named after it).
    pub fn create_category(&self, name: &str) -> Result<(), Error> {
        self.call(
            "set_config",
            &[("section", "categories"), ("name", name), ("dir", name)],
        )?;
        if self.categories()?.iter().any(|c| c == name) {
            Ok(())
        } else {
            Err(Error::backend(
                "SABnzbd accepted the request but the category isn't there",
            ))
        }
    }

    /// Ask SABnzbd to fetch an NZB. Returns its job ID.
    pub fn add_url(&self, nzb_url: &str, name: &str) -> Result<String, Error> {
        let mut extra = vec![("name", nzb_url), ("nzbname", name)];
        if !self.category.is_empty() {
            extra.push(("cat", self.category.as_str()));
        }
        let v = self.call("addurl", &extra)?;
        if v["status"].as_bool() == Some(false) {
            return Err(Error::backend("SABnzbd wouldn't take that NZB"));
        }
        v["nzo_ids"][0]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Error::backend("SABnzbd didn't say which job it made"))
    }

    pub fn queue(&self) -> Result<Vec<QueueSlot>, Error> {
        let v = self.call("queue", &[])?;
        Ok(v["queue"]["slots"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|s| QueueSlot {
                        nzo_id: s["nzo_id"].as_str().unwrap_or("").into(),
                        name: s["filename"].as_str().unwrap_or("").into(),
                        status: s["status"].as_str().unwrap_or("").into(),
                        percent: s["percentage"]
                            .as_str()
                            .and_then(|p| p.parse().ok())
                            .or_else(|| s["percentage"].as_f64().map(|p| p as f32))
                            .unwrap_or(0.0),
                        mb: s["mb"].as_str().and_then(|p| p.parse().ok()).unwrap_or(0.0),
                        mb_left: s["mbleft"]
                            .as_str()
                            .and_then(|p| p.parse().ok())
                            .unwrap_or(0.0),
                        time_left: s["timeleft"].as_str().unwrap_or("").into(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn history(&self, limit: u32) -> Result<Vec<HistorySlot>, Error> {
        let v = self.call("history", &[("limit", &limit.to_string())])?;
        Ok(v["history"]["slots"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|s| HistorySlot {
                        nzo_id: s["nzo_id"].as_str().unwrap_or("").into(),
                        name: s["name"].as_str().unwrap_or("").into(),
                        status: s["status"].as_str().unwrap_or("").into(),
                        storage: s["storage"].as_str().unwrap_or("").into(),
                        fail_message: s["fail_message"].as_str().unwrap_or("").into(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Remove a job from the queue (and its files) if it is still there.
    pub fn delete(&self, nzo_id: &str) -> Result<(), Error> {
        self.call(
            "queue",
            &[("name", "delete"), ("value", nzo_id), ("del_files", "1")],
        )?;
        Ok(())
    }
}
