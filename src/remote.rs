//! Talking to a drive agent running on another machine (see `crate::agent`).
//!
//! These calls block, so use them from blocking tasks. Errors from the agent keep their own
//! code and message, so "that file already exists" or "this drive is FAT32 and can't hold a file
//! that big" reach the user as they are.

use std::{collections::HashMap, io::Read, time::Duration};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    error::Error,
    fsops::{Entry, FsInfo, WriteStatus},
    library::scan::{Existing, Found},
};

#[derive(Debug, Clone, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub version: String,
    pub read_only: bool,
    pub root: String,
    pub fs: FsInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Stat {
    pub exists: bool,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Clone)]
pub struct Remote {
    base: String,
    token: String,
    /// A folder on the drive that every path is relative to (empty: the drive's top).
    prefix: String,
    agent: ureq::Agent,
}

/// `http://host:port` with no trailing slash, or an error saying what is wrong.
pub fn clean_url(raw: &str) -> Result<String, Error> {
    let u = raw.trim().trim_end_matches('/');
    let rest = u
        .strip_prefix("http://")
        .or_else(|| u.strip_prefix("https://"))
        .ok_or_else(|| {
            Error::validation(
                "The address must start with http:// (for example http://192.168.1.48:8099)",
            )
        })?;
    if rest.is_empty() || rest.contains(['/', ' ', '?', '#']) {
        return Err(Error::validation(
            "Use just the address and port, like http://192.168.1.48:8099",
        ));
    }
    Ok(u.to_string())
}

static SHARED_AGENT: std::sync::LazyLock<ureq::Agent> = std::sync::LazyLock::new(|| {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(120))
        .user_agent("RustyBox")
        .build()
});

impl Remote {
    pub fn new(url: &str, token: &str) -> Remote {
        Remote {
            base: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            prefix: String::new(),
            // One shared connection pool: a listing or a copy makes many small requests to the
            // same agent, and each would otherwise open a new connection.
            agent: SHARED_AGENT.clone(),
        }
    }

    /// Make every path relative to a folder on the drive, such as `Games`.
    pub fn with_prefix(mut self, prefix: &str) -> Remote {
        self.prefix = prefix.trim_matches('/').to_string();
        self
    }

    /// The same drive, starting one folder further in.
    pub fn joined(&self, sub: &str) -> Remote {
        let mut r = self.clone();
        r.prefix = self.at(sub);
        r
    }

    /// `rel` as the agent sees it, below the prefix.
    fn at(&self, rel: &str) -> String {
        let rel = rel.trim_matches('/');
        match (self.prefix.is_empty(), rel.is_empty()) {
            (true, _) => rel.to_string(),
            (false, true) => self.prefix.clone(),
            (false, false) => format!("{}/{rel}", self.prefix),
        }
    }

    pub fn url(&self) -> &str {
        &self.base
    }

    fn req(&self, method: &str, path: &str) -> ureq::Request {
        self.agent
            .request(method, &format!("{}/agent/v1/{path}", self.base))
            .set("Authorization", &format!("Bearer {}", self.token))
    }

    fn fail(e: ureq::Error) -> Error {
        match e {
            ureq::Error::Status(code, res) => {
                let body: Value = res.into_json().unwrap_or(Value::Null);
                let message = body["message"]
                    .as_str()
                    .unwrap_or("The drive agent reported an error")
                    .to_string();
                let c = body["error"].as_str().unwrap_or("AGENT_ERROR").to_string();
                if code == 401 {
                    Error::coded(401, "AGENT_AUTH", "The drive agent didn't accept the token")
                } else {
                    Error::coded(code, &c, message)
                }
            }
            ureq::Error::Transport(t) => Error::coded(
                502,
                "AGENT_UNREACHABLE",
                format!(
                    "Can't reach the drive agent: {t}. Is it running, and is its port allowed through the firewall?"
                ),
            ),
        }
    }

    fn json<T: for<'de> Deserialize<'de>>(
        r: Result<ureq::Response, ureq::Error>,
    ) -> Result<T, Error> {
        r.map_err(Self::fail)?
            .into_json()
            .map_err(|e| Error::backend(format!("Unexpected answer from the drive agent: {e}")))
    }

    pub fn info(&self) -> Result<AgentInfo, Error> {
        Self::json(
            self.req("GET", "info")
                .timeout(Duration::from_secs(8))
                .call(),
        )
    }

    pub fn list(&self, rel: &str) -> Result<Vec<Entry>, Error> {
        Self::json(self.req("GET", "list").query("path", &self.at(rel)).call())
    }

    pub fn stat(&self, rel: &str) -> Result<Stat, Error> {
        Self::json(self.req("GET", "stat").query("path", &self.at(rel)).call())
    }

    /// Scan the drive on the agent's side. `scanner` is `iso`, `god` or `files`.
    pub fn scan(
        &self,
        scanner: &str,
        existing: &HashMap<String, Existing>,
    ) -> Result<Vec<Found>, Error> {
        #[derive(Deserialize)]
        struct Out {
            items: Vec<Found>,
        }
        let body = json!({"scanner": scanner, "existing": existing, "path": self.prefix});
        let out: Out = Self::json(
            self.req("POST", "scan")
                .timeout(Duration::from_secs(1800))
                .send_json(body),
        )?;
        Ok(out.items)
    }

    /// Read `len` bytes from `offset` (or everything from `offset` if `len` is `None`).
    pub fn read(
        &self,
        rel: &str,
        offset: u64,
        len: Option<u64>,
    ) -> Result<Box<dyn Read + Send + Sync>, Error> {
        let range = match len {
            Some(l) => format!("bytes={offset}-{}", offset + l.saturating_sub(1)),
            None => format!("bytes={offset}-"),
        };
        let res = self
            .req("GET", "file")
            .query("path", &self.at(rel))
            .set("Range", &range)
            .call()
            .map_err(Self::fail)?;
        Ok(Box::new(res.into_reader()))
    }

    pub fn write_status(&self, rel: &str) -> Result<WriteStatus, Error> {
        Self::json(self.req("GET", "write").query("path", &self.at(rel)).call())
    }

    /// Send one piece of a file. Returns (finished, bytes now on the drive).
    pub fn write_chunk(
        &self,
        rel: &str,
        offset: u64,
        total: u64,
        overwrite: bool,
        data: &[u8],
    ) -> Result<(bool, u64), Error> {
        #[derive(Deserialize)]
        struct Out {
            done: bool,
            offset: u64,
        }
        let o: Out = Self::json(
            self.req("PUT", "write")
                .query("path", &self.at(rel))
                .query("offset", &offset.to_string())
                .query("total", &total.to_string())
                .query("overwrite", if overwrite { "true" } else { "false" })
                .send_bytes(data),
        )?;
        Ok((o.done, o.offset))
    }

    fn post(&self, path: &str, body: Value) -> Result<(), Error> {
        self.req("POST", path)
            .send_json(body)
            .map(|_| ())
            .map_err(Self::fail)
    }

    pub fn mkdir(&self, rel: &str) -> Result<(), Error> {
        self.post("mkdir", json!({"path": self.at(rel)}))
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        self.post("rename", json!({"from": self.at(from), "to": self.at(to)}))
    }

    pub fn delete(&self, rel: &str) -> Result<(), Error> {
        self.post("delete", json!({"path": self.at(rel)}))
    }
}
