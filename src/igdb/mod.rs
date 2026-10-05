//! IGDB (via Twitch): cover art and information for games.
//!
//! IGDB uses Twitch's *client credentials* flow: the server swaps a Client ID and Secret for an
//! access token, with no user login and no redirect. The token lasts about two months and is
//! cached here. IGDB allows about four requests a second, so requests are spaced out.

pub mod discover;
pub mod rank;
pub mod store;

use std::{
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;

/// The Xbox 360's platform ID in IGDB.
const XBOX_360: u32 = 12;
/// Main game, remake, remaster, expanded game, port: not DLC, bundles or add-ons.
const GAME_TYPES: &str = "0,8,9,10,11";
/// Four requests a second is the limit; stay just under it.
const MIN_GAP: Duration = Duration::from_millis(260);

#[derive(Debug, Clone, PartialEq)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
}

/// Where to talk to. Tests point these at a local stand-in.
#[derive(Debug, Clone)]
pub struct Urls {
    pub token: String,
    pub api: String,
    pub images: String,
}

impl Default for Urls {
    fn default() -> Self {
        Urls {
            token: "https://id.twitch.tv/oauth2/token".into(),
            api: "https://api.igdb.com/v4".into(),
            images: "https://images.igdb.com/igdb/image/upload".into(),
        }
    }
}

/// A game as IGDB describes it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub id: i64,
    pub name: String,
    pub summary: Option<String>,
    /// Unix time of the first release.
    pub release: Option<i64>,
    pub genres: Vec<String>,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub rating: Option<f64>,
    pub url: Option<String>,
    /// IGDB's image ID for the cover (`co1abc`).
    pub cover: Option<String>,
}

pub struct Client {
    urls: Urls,
    mock: bool,
    agent: ureq::Agent,
    token: Mutex<Option<(String, u64)>>, // token, unix expiry
    last_call: Mutex<Instant>,
    /// Filter lists and counts for Discover, kept for a while.
    disc: Mutex<discover::Cache>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Client {
    pub fn new(urls: Urls, mock: bool) -> Client {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(20))
            .user_agent("RustyBox")
            .build();
        Client {
            urls,
            mock,
            agent,
            token: Mutex::new(None),
            last_call: Mutex::new(Instant::now() - MIN_GAP),
            disc: Mutex::new(discover::Cache::default()),
        }
    }

    pub fn is_mock(&self) -> bool {
        self.mock
    }

    /// A small cover image the browser can load straight from IGDB (for choosing between matches).
    pub fn thumb_url(&self, image_id: &str) -> Option<String> {
        (!self.mock && image_id.bytes().all(|b| b.is_ascii_alphanumeric()))
            .then(|| format!("{}/t_cover_small/{image_id}.jpg", self.urls.images))
    }

    /// Wait so requests stay under IGDB's rate limit.
    fn pace(&self) {
        let mut last = self.last_call.lock().unwrap_or_else(|e| e.into_inner());
        let since = last.elapsed();
        if since < MIN_GAP {
            std::thread::sleep(MIN_GAP - since);
        }
        *last = Instant::now();
    }

    /// A valid token, fetching a new one when there is none or it is nearly out of date.
    /// Returns the token and how many days it has left.
    pub fn token(&self, creds: &Credentials, force: bool) -> Result<(String, u64), Error> {
        let mut cached = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((t, exp)) = cached.as_ref()
            && !force
            && *exp > now() + 3600
        {
            return Ok((t.clone(), (exp - now()) / 86400));
        }
        let res = self
            .agent
            .post(&self.urls.token)
            .send_form(&[("client_id", &creds.client_id), ("client_secret", &creds.client_secret), ("grant_type", "client_credentials")])
            .map_err(|e| match e {
                ureq::Error::Status(400, _) | ureq::Error::Status(403, _) => Error::validation("Twitch didn't accept that Client ID and Secret. Check them in your Twitch developer console."),
                ureq::Error::Status(code, _) => Error::backend(format!("Twitch answered with an error ({code})")),
                e => Error::backend(format!("Could not reach Twitch: {e}")),
            })?;
        let v: Value = res
            .into_json()
            .map_err(|e| Error::backend(format!("Unexpected answer from Twitch: {e}")))?;
        let token = v["access_token"]
            .as_str()
            .ok_or_else(|| Error::backend("Twitch's answer had no access token"))?
            .to_string();
        let expires_in = v["expires_in"].as_u64().unwrap_or(3600);
        *cached = Some((token.clone(), now() + expires_in));
        Ok((token, expires_in / 86400))
    }

    /// An IGDB query (Apicalypse syntax). Retries once with a fresh token if the old one was refused.
    pub(crate) fn query(
        &self,
        creds: &Credentials,
        endpoint: &str,
        body: &str,
    ) -> Result<Value, Error> {
        let mut force = false;
        for attempt in 0..3 {
            let (token, _) = self.token(creds, force)?;
            self.pace();
            let res = self
                .agent
                .post(&format!("{}/{}", self.urls.api, endpoint))
                .set("Client-ID", &creds.client_id)
                .set("Authorization", &format!("Bearer {token}"))
                .set("Accept", "application/json")
                .send_string(body);
            match res {
                Ok(r) => {
                    return r
                        .into_json()
                        .map_err(|e| Error::backend(format!("Unexpected answer from IGDB: {e}")));
                }
                Err(ureq::Error::Status(401, _)) if attempt == 0 => force = true,
                Err(ureq::Error::Status(429, _)) if attempt < 2 => {
                    std::thread::sleep(Duration::from_secs(1))
                }
                Err(ureq::Error::Status(code, _)) => {
                    return Err(Error::backend(format!(
                        "IGDB answered with an error ({code})"
                    )));
                }
                Err(e) => return Err(Error::backend(format!("Could not reach IGDB: {e}"))),
            }
        }
        Err(Error::backend("IGDB kept refusing the request"))
    }

    /// Check the credentials work. Returns the days the token lasts.
    pub fn test(&self, creds: &Credentials) -> Result<u64, Error> {
        if self.mock {
            return Ok(60);
        }
        let (_, days) = self.token(creds, true)?;
        self.query(creds, "games", "fields name; limit 1;")?;
        Ok(days)
    }

    pub(crate) const FIELDS: &'static str = "name,summary,first_release_date,cover.image_id,genres.name,involved_companies.company.name,involved_companies.developer,involved_companies.publisher,total_rating,url";

    /// Xbox 360 games matching `name`, best matches first as IGDB ranks them.
    pub fn search(&self, creds: &Credentials, name: &str) -> Result<Vec<Candidate>, Error> {
        if self.mock {
            return Ok(mock_candidates(name));
        }
        let q = name.replace(['\\', '"'], " ");
        let body = format!(
            "search \"{q}\"; fields {}; where platforms = ({XBOX_360}) & game_type = ({GAME_TYPES}); limit 10;",
            Self::FIELDS
        );
        let v = self.query(creds, "games", &body)?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(parse_candidate).collect())
            .unwrap_or_default())
    }

    /// One game by its IGDB ID (for a match chosen by hand).
    pub fn by_id(&self, creds: &Credentials, id: i64) -> Result<Option<Candidate>, Error> {
        if self.mock {
            if let Some((g, _)) = discover::mock_catalog().into_iter().find(|g| g.0.id == id) {
                return Ok(Some(g));
            }
            return Ok(Some(mock_candidates("Chosen game").remove(0)).map(|mut c| {
                c.id = id;
                c
            }));
        }
        let v = self.query(
            creds,
            "games",
            &format!("fields {}; where id = {id};", Self::FIELDS),
        )?;
        Ok(v.as_array()
            .and_then(|a| a.first())
            .and_then(parse_candidate))
    }

    /// The cover image (JPEG) for an IGDB image ID.
    pub fn cover(&self, image_id: &str) -> Result<(Vec<u8>, &'static str), Error> {
        if self.mock {
            return Ok((mock_cover_svg(image_id).into_bytes(), "svg"));
        }
        if !image_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(Error::validation("Unexpected cover ID"));
        }
        let res = self
            .agent
            .get(&format!("{}/t_cover_big/{image_id}.jpg", self.urls.images))
            .call()
            .map_err(|e| Error::backend(format!("Could not download the cover: {e}")))?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(res.into_reader(), 5 << 20),
            &mut bytes,
        )?;
        Ok((bytes, "jpg"))
    }
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

pub(crate) fn parse_candidate(v: &Value) -> Option<Candidate> {
    let companies = |role: &str| -> Vec<String> {
        v["involved_companies"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|c| c[role].as_bool().unwrap_or(false))
                    .filter_map(|c| c["company"]["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(Candidate {
        id: v["id"].as_i64()?,
        name: v["name"].as_str()?.to_string(),
        summary: v["summary"].as_str().map(String::from),
        release: v["first_release_date"].as_i64(),
        genres: v["genres"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|g| g["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        developers: companies("developer"),
        publishers: companies("publisher"),
        rating: v["total_rating"].as_f64(),
        url: v["url"].as_str().map(String::from),
        cover: v["cover"]["image_id"].as_str().map(String::from),
    })
}

/// Find the Xbox 360 game for a name from the title list: the best-ranked match over the query
/// variants (full name, then without edition words). `None` means nothing was close enough.
pub fn lookup(
    client: &Client,
    creds: &Credentials,
    name: &str,
) -> Result<Option<(Candidate, u32)>, Error> {
    for query in rank::variants(name) {
        let found = client.search(creds, &query)?;
        if let Some((c, score)) = rank::best(&query, &found) {
            return Ok(Some((c.clone(), score)));
        }
    }
    Ok(None)
}

/// Download a cover into `dir` (once) and return its file name.
pub fn save_cover(client: &Client, dir: &std::path::Path, image_id: &str) -> Result<String, Error> {
    // Already have it (any extension)?
    for ext in ["jpg", "svg"] {
        let name = store::cover_file_name(image_id, ext);
        if dir.join(&name).is_file() {
            return Ok(name);
        }
    }
    let (bytes, ext) = client.cover(image_id)?;
    std::fs::create_dir_all(dir)?;
    let name = store::cover_file_name(image_id, ext);
    let tmp = dir.join(format!("{name}.part"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dir.join(&name))?;
    crate::perms::own(&dir.join(&name));
    Ok(name)
}

// ── Mock data for --mock ─────────────────────────────────────────────────────

fn mock_candidates(name: &str) -> Vec<Candidate> {
    let hash: i64 = name
        .bytes()
        .fold(7i64, |h, b| h.wrapping_mul(31).wrapping_add(b as i64))
        .abs();
    vec![Candidate {
        id: 900_000 + hash % 90_000,
        name: name.to_string(),
        summary: Some(format!(
            "A sample description for {name}, shown because RustyBox is in mock mode and not talking to IGDB."
        )),
        release: Some(1_200_000_000 + (hash % 100) * 10_000_000),
        genres: vec!["Action".into(), "Adventure".into()],
        developers: vec!["Sample Studio".into()],
        publishers: vec!["Sample Publisher".into()],
        rating: Some(60.0 + (hash % 35) as f64),
        url: None,
        cover: Some(format!("mock{}", hash % 1000)),
    }]
}

pub(crate) fn mock_cover_svg(id: &str) -> String {
    let hue = id
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
        % 360;
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 264 374"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl({hue},60%,30%)"/><stop offset="1" stop-color="hsl({},70%,55%)"/></linearGradient></defs><rect width="264" height="374" fill="url(#g)"/><circle cx="132" cy="150" r="52" fill="none" stroke="#fff" stroke-opacity=".5" stroke-width="3"/><text x="132" y="330" text-anchor="middle" font-family="sans-serif" font-size="18" fill="#fff" fill-opacity=".8">MOCK COVER</text></svg>"##,
        (hue + 40) % 360
    )
}
