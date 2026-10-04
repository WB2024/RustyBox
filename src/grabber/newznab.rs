//! A Newznab client: search an indexer and read what it returns. Prowlarr's per-indexer
//! address (`http://prowlarr:9696/3`) speaks the same protocol, so it works here too.
//!
//! API keys are in the request addresses, so they are scrubbed from every error message.

use std::time::Duration;

use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Newznab's category for Xbox 360 games, and for its DLC.
pub const CAT_XBOX360: u32 = 1050;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Indexer {
    pub id: u32,
    pub name: String,
    /// `https://api.nzbgeek.info`, or a Prowlarr indexer such as `http://192.168.1.110:9696/5`.
    pub url: String,
    pub api_path: String,
    /// Never sent to the browser.
    pub api_key: String,
    pub enabled: bool,
    /// Lower is tried (and preferred) first.
    pub priority: i32,
    /// Categories to search; Xbox 360 games by default.
    pub categories: Vec<u32>,
    /// Set by a test: does this indexer carry Xbox 360 games at all?
    pub has_xbox360: Option<bool>,
    /// `usenet` (Newznab: NZB files for SABnzbd) or `torrent` (Torznab: torrents for qBittorrent).
    pub protocol: String,
}

pub const PROTOCOLS: [&str; 2] = ["usenet", "torrent"];

impl Default for Indexer {
    fn default() -> Self {
        Indexer {
            id: 0,
            name: String::new(),
            url: String::new(),
            api_path: "/api".into(),
            api_key: String::new(),
            enabled: true,
            priority: 25,
            categories: vec![CAT_XBOX360],
            has_xbox360: None,
            protocol: "usenet".into(),
        }
    }
}

impl Indexer {
    pub fn public(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.remove("api_key");
            o.insert("api_key_set".into(), (!self.api_key.is_empty()).into());
        }
        v
    }

    pub fn validate(mut self) -> Result<Indexer, Error> {
        self.name = self.name.trim().to_string();
        self.url = self.url.trim().trim_end_matches('/').to_string();
        self.api_path = format!("/{}", self.api_path.trim().trim_matches('/'));
        if self.name.is_empty() || self.name.len() > 60 {
            return Err(Error::validation("Give the indexer a name"));
        }
        let rest = self
            .url
            .strip_prefix("http://")
            .or_else(|| self.url.strip_prefix("https://"));
        if rest.is_none_or(|r| r.is_empty() || r.contains([' ', '?', '#'])) {
            return Err(Error::validation(
                "The address must start with http:// or https:// (for example https://api.nzbgeek.info)",
            ));
        }
        if self.categories.is_empty() {
            self.categories = vec![CAT_XBOX360];
        }
        if !PROTOCOLS.contains(&self.protocol.as_str()) {
            return Err(Error::validation(
                "An indexer is for usenet or for torrents",
            ));
        }
        Ok(self)
    }
}

/// One search result.
#[derive(Debug, Clone, Serialize)]
pub struct Release {
    pub indexer_id: u32,
    pub indexer: String,
    pub indexer_priority: i32,
    pub guid: String,
    pub title: String,
    /// Where to fetch the NZB. Holds the indexer's key: never sent to the browser.
    #[serde(skip)]
    pub link: String,
    pub size: u64,
    pub categories: Vec<u32>,
    /// Unix time posted.
    pub posted: Option<i64>,
    pub grabs: Option<u32>,
    /// `usenet` or `torrent`.
    pub protocol: String,
    /// Torrents: how many are sharing it (seeders) and fetching it (peers).
    pub seeders: Option<u32>,
    pub peers: Option<u32>,
    /// Torrents: its 40-digit info hash, if the indexer says.
    pub infohash: Option<String>,
    /// Torrents: a magnet link. May carry a tracker passkey, so it never goes to the browser.
    #[serde(skip)]
    pub magnet: Option<String>,
}

fn scrub(msg: String, key: &str) -> String {
    if key.len() >= 6 {
        msg.replace(key, "***")
    } else {
        msg
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(45))
        .user_agent("RustyBox")
        .build()
}

/// `Sat, 19 Sep 2026 12:00:00 +0000` to unix seconds.
pub fn parse_rfc2822(s: &str) -> Option<i64> {
    let s = s.trim();
    let s = s.split_once(',').map(|(_, r)| r).unwrap_or(s).trim();
    let mut it = s.split_whitespace();
    let day: i64 = it.next()?.parse().ok()?;
    let month = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|m| {
        it.clone()
            .next()
            .is_some_and(|x| x.to_lowercase().starts_with(m))
    })? as i64
        + 1;
    it.next();
    let year: i64 = it.next()?.parse().ok()?;
    let mut hms = it.next()?.split(':');
    let (h, mi, sec): (i64, i64, i64) = (
        hms.next()?.parse().ok()?,
        hms.next()?.parse().ok()?,
        hms.next().and_then(|x| x.parse().ok()).unwrap_or(0),
    );
    let tz = it.next().unwrap_or("+0000");
    let off = if tz.len() == 5 && tz.is_ascii() && (tz.starts_with('+') || tz.starts_with('-')) {
        let v: i64 = tz[1..3].parse::<i64>().ok()? * 3600 + tz[3..5].parse::<i64>().ok()? * 60;
        if tz.starts_with('-') { -v } else { v }
    } else {
        0
    };
    // Days from civil (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + sec - off)
}

/// Read a Newznab RSS answer.
pub fn parse_results(xml: &str, ix: &Indexer) -> Result<Vec<Release>, Error> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(true);
    let mut out = Vec::new();
    let mut cur: Option<Release> = None;
    let mut field: Option<String> = None;
    let mut enclosure_len = 0u64;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let attrs = |k: &str| -> Option<String> {
                    e.attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == k.as_bytes())
                        .and_then(|a| a.unescape_value().ok().map(|v| v.to_string()))
                };
                match name.as_str() {
                    "error" => {
                        let code = attrs("code").unwrap_or_default();
                        let d = attrs("description").unwrap_or_default();
                        return Err(match code.as_str() {
                            "100" | "101" | "102" | "103" => Error::coded(
                                401,
                                "INDEXER_LOGIN",
                                format!("{} refused the API key or the account ({d})", ix.name),
                            ),
                            "429" | "500" if d.to_lowercase().contains("limit") => Error::coded(
                                429,
                                "INDEXER_LIMIT",
                                format!("{} says the API limit is used up ({d})", ix.name),
                            ),
                            _ => {
                                Error::backend(format!("{} answered an error {code}: {d}", ix.name))
                            }
                        });
                    }
                    "item" => {
                        cur = Some(Release {
                            indexer_id: ix.id,
                            indexer: ix.name.clone(),
                            indexer_priority: ix.priority,
                            guid: String::new(),
                            title: String::new(),
                            link: String::new(),
                            size: 0,
                            categories: vec![],
                            posted: None,
                            grabs: None,
                            protocol: ix.protocol.clone(),
                            seeders: None,
                            peers: None,
                            infohash: None,
                            magnet: None,
                        });
                        enclosure_len = 0;
                    }
                    "enclosure" if cur.is_some() => {
                        if let Some(c) = cur.as_mut() {
                            if c.link.is_empty() {
                                c.link = attrs("url").unwrap_or_default();
                            }
                            enclosure_len =
                                attrs("length").and_then(|l| l.parse().ok()).unwrap_or(0);
                        }
                    }
                    "newznab:attr" | "torznab:attr" if cur.is_some() => {
                        let (k, v) = (
                            attrs("name").unwrap_or_default(),
                            attrs("value").unwrap_or_default(),
                        );
                        if let Some(c) = cur.as_mut() {
                            match k.as_str() {
                                "size" => c.size = v.parse().unwrap_or(c.size),
                                "category" => {
                                    if let Ok(n) = v.parse() {
                                        c.categories.push(n);
                                    }
                                }
                                "grabs" => c.grabs = v.parse().ok(),
                                "seeders" => c.seeders = v.parse().ok(),
                                "peers" => c.peers = v.parse().ok(),
                                "infohash" if v.len() == 40 => c.infohash = Some(v.to_lowercase()),
                                "magneturl" if v.starts_with("magnet:") => c.magnet = Some(v),
                                "usenetdate" if c.posted.is_none() => c.posted = parse_rfc2822(&v),
                                _ => {}
                            }
                        }
                    }
                    "title" | "guid" | "link" | "pubDate" if cur.is_some() => field = Some(name),
                    _ => field = None,
                }
            }
            Ok(Event::Text(t)) => {
                if let (Some(c), Some(f)) = (cur.as_mut(), field.as_deref()) {
                    let v = t.unescape().map(|v| v.to_string()).unwrap_or_default();
                    match f {
                        "title" => c.title = v,
                        "guid" => c.guid = v,
                        "link" if v.starts_with("magnet:") => {
                            c.magnet = c.magnet.take().or(Some(v))
                        }
                        "link" if c.link.is_empty() => c.link = v,
                        "pubDate" => c.posted = parse_rfc2822(&v).or(c.posted),
                        _ => {}
                    }
                }
            }
            Ok(Event::CData(t)) => {
                if let (Some(c), Some(f)) = (cur.as_mut(), field.as_deref()) {
                    let v = String::from_utf8_lossy(&t).to_string();
                    match f {
                        "title" => c.title = v,
                        "guid" => c.guid = v,
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "item"
                    && let Some(mut c) = cur.take()
                {
                    if c.size == 0 {
                        c.size = enclosure_len;
                    }
                    if c.guid.is_empty() {
                        c.guid = c.link.clone();
                    }
                    // A torrent may come only as a magnet link.
                    if c.link.is_empty()
                        && let Some(m) = &c.magnet
                    {
                        c.link = m.clone();
                    }
                    if c.infohash.is_none()
                        && let Some((h, _)) = c.magnet.as_deref().and_then(super::torrent::magnet)
                    {
                        c.infohash = Some(h);
                    }
                    if !c.title.is_empty() && !c.link.is_empty() {
                        out.push(c);
                    }
                }
                field = None;
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(Error::backend(format!(
                    "{} sent something that isn't a Newznab answer: {e}",
                    ix.name
                )));
            }
            _ => {}
        }
    }
    Ok(out)
}

fn get(ix: &Indexer, query: &[(&str, &str)]) -> Result<String, Error> {
    let url = format!("{}{}", ix.url, ix.api_path);
    let mut req = agent().get(&url).query("apikey", &ix.api_key);
    for (k, v) in query {
        req = req.query(k, v);
    }
    match req.call() {
        Ok(r) => r.into_string().map_err(|e| {
            Error::backend(scrub(
                format!("Couldn't read {}'s answer: {e}", ix.name),
                &ix.api_key,
            ))
        }),
        Err(ureq::Error::Status(401 | 403, _)) => Err(Error::coded(
            401,
            "INDEXER_LOGIN",
            format!("{} refused the API key", ix.name),
        )),
        Err(ureq::Error::Status(429, _)) => Err(Error::coded(
            429,
            "INDEXER_LIMIT",
            format!(
                "{} says too many requests (its API limit is used up for now). Try again later.",
                ix.name
            ),
        )),
        Err(ureq::Error::Status(code, _)) => {
            Err(Error::backend(format!("{} answered {code}", ix.name)))
        }
        Err(ureq::Error::Transport(t)) => Err(Error::coded(
            502,
            "INDEXER_UNREACHABLE",
            scrub(format!("Can't reach {}: {t}", ix.name), &ix.api_key),
        )),
    }
}

/// Search one indexer for `q` in its categories.
pub fn search(ix: &Indexer, q: &str) -> Result<Vec<Release>, Error> {
    let cats = ix
        .categories
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let xml = get(
        ix,
        &[
            ("t", "search"),
            ("q", q),
            ("cat", &cats),
            ("limit", "100"),
            ("extended", "1"),
        ],
    )?;
    parse_results(&xml, ix)
}

#[derive(Debug, Clone, Serialize)]
pub struct Caps {
    pub has_xbox360: bool,
    pub categories: Vec<(u32, String)>,
}

/// What the indexer supports. Whether it carries Xbox 360 games decides if it is searched.
pub fn caps(ix: &Indexer) -> Result<Caps, Error> {
    let xml = get(ix, &[("t", "caps")])?;
    if xml.contains("<error") {
        parse_results(&xml, ix)?; // turns the error into a proper message
    }
    let mut r = Reader::from_str(&xml);
    let mut cats = Vec::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if matches!(e.name().as_ref(), b"category" | b"subcat") =>
            {
                let id = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"id")
                    .and_then(|a| a.unescape_value().ok().and_then(|v| v.parse::<u32>().ok()));
                let name = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"name")
                    .and_then(|a| a.unescape_value().ok().map(|v| v.to_string()));
                if let (Some(id), Some(name)) = (id, name) {
                    cats.push((id, name));
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(Error::backend(format!(
                    "{} sent unreadable capabilities: {e}",
                    ix.name
                )));
            }
            _ => {}
        }
    }
    if cats.is_empty() && !xml.contains("<caps") {
        return Err(Error::backend(format!(
            "{} didn't answer like a Newznab indexer",
            ix.name
        )));
    }
    Ok(Caps {
        has_xbox360: cats.iter().any(|(id, _)| *id == CAT_XBOX360),
        categories: cats
            .into_iter()
            .filter(|(id, _)| (1000..2000).contains(id))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn torznab_results_carry_seeders_and_magnets() {
        let ix = Indexer {
            id: 3,
            name: "Tz".into(),
            protocol: "torrent".into(),
            ..Default::default()
        };
        let xml = r#"<?xml version="1.0"?><rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel>
          <item><title>A</title><guid>g1</guid><link>http://x/a.torrent</link><enclosure url="http://x/a.torrent" length="10"/>
            <torznab:attr name="seeders" value="12"/><torznab:attr name="peers" value="15"/>
            <torznab:attr name="infohash" value="0123456789ABCDEF0123456789ABCDEF01234567"/></item>
          <item><title>B</title><guid>g2</guid><link>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&amp;dn=B</link>
            <torznab:attr name="seeders" value="0"/></item>
          <item><title>C</title><guid>g3</guid><enclosure url="magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"/>
            <torznab:attr name="magneturl" value="magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"/></item>
        </channel></rss>"#;
        let r = parse_results(xml, &ix).unwrap();
        assert_eq!(r.len(), 3, "{r:?}");
        assert_eq!(
            (r[0].protocol.as_str(), r[0].seeders, r[0].peers),
            ("torrent", Some(12), Some(15))
        );
        assert_eq!(
            r[0].infohash.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert!(
            r[1].link.starts_with("magnet:") && r[1].magnet.is_some(),
            "a magnet-only item: {:?}",
            r[1]
        );
        assert_eq!(
            r[1].infohash.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert!(r[2].magnet.is_some());
        // A magnet link with several parameters (an `&` in the XML) and nothing else.
        let xml = r#"<rss><channel><item><title>D</title><guid>g4</guid><link>magnet:?xt=urn:btih:abababababababababababababababababababab&amp;dn=D&amp;tr=udp%3A%2F%2Ft</link></item></channel></rss>"#;
        let r = parse_results(xml, &ix).unwrap();
        assert_eq!(r.len(), 1, "a feed item with only a magnet link: {r:?}");
    }

    #[test]
    fn odd_dates_from_an_indexer_never_panic() {
        // A time zone with a multi-byte character in it, as a hostile feed might send.
        assert!(parse_rfc2822("Sat, 19 Sep 2026 12:00:00 +1é1").is_some());
        assert!(parse_rfc2822("Sat, 19 Sep 2026 12:00:00 +0200").is_some());
        assert!(parse_rfc2822("nonsense é").is_none());
    }

    use super::*;

    const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
<channel><title>Indexer</title>
<item><title>Halo.3.PAL.XBOX360-GAC</title><guid isPermaLink="true">https://x/details/abc</guid>
<link>https://x/getnzb/abc.nzb?apikey=SECRETKEY1</link><pubDate>Sat, 19 Sep 2026 12:00:00 +0000</pubDate>
<enclosure url="https://x/getnzb/abc.nzb?apikey=SECRETKEY1" length="7485000000" type="application/x-nzb"/>
<newznab:attr name="category" value="1000"/><newznab:attr name="category" value="1050"/>
<newznab:attr name="size" value="7485000000"/><newznab:attr name="grabs" value="120"/></item>
<item><title><![CDATA[Gears of War 3 Re-Up xbox360 (ShEkStEr)]]></title><guid>def</guid><link>https://x/getnzb/def.nzb</link>
<enclosure url="https://x/getnzb/def.nzb" length="123" type="x"/></item>
<item><title>No link item</title><guid>zzz</guid></item>
</channel></rss>"#;

    #[test]
    fn results_are_read_with_their_attributes() {
        let ix = Indexer {
            id: 3,
            name: "Geek".into(),
            priority: 10,
            ..Default::default()
        };
        let r = parse_results(RSS, &ix).unwrap();
        assert_eq!(r.len(), 2, "an item with no link is dropped");
        assert_eq!(
            (
                r[0].title.as_str(),
                r[0].size,
                r[0].grabs,
                r[0].categories.clone()
            ),
            (
                "Halo.3.PAL.XBOX360-GAC",
                7_485_000_000,
                Some(120),
                vec![1000, 1050]
            )
        );
        assert_eq!(r[0].posted, Some(1_789_819_200)); // 2026-09-19 12:00 UTC
        assert!(r[0].link.contains("getnzb/abc.nzb"));
        assert_eq!(
            (r[1].title.as_str(), r[1].size, r[1].guid.as_str()),
            ("Gears of War 3 Re-Up xbox360 (ShEkStEr)", 123, "def")
        );
        assert_eq!((r[0].indexer_id, r[0].indexer_priority), (3, 10));
        // The link, which holds the key, is never serialised.
        assert!(!serde_json::to_string(&r[0]).unwrap().contains("SECRETKEY1"));
    }

    #[test]
    fn indexer_errors_are_explained_and_dates_parse() {
        let ix = Indexer {
            name: "Geek".into(),
            ..Default::default()
        };
        let e = parse_results(
            r#"<?xml version="1.0"?><error code="100" description="Incorrect user credentials"/>"#,
            &ix,
        )
        .unwrap_err();
        assert_eq!(e.to_box_error().error, "INDEXER_LOGIN");
        assert!(
            parse_results("<html>nope", &ix)
                .map(|v| v.is_empty())
                .unwrap_or(true)
        );
        assert_eq!(parse_rfc2822("Thu, 01 Jan 1970 00:00:00 +0000"), Some(0));
        assert_eq!(parse_rfc2822("Thu, 01 Jan 1970 01:00:00 +0100"), Some(0));
        assert_eq!(parse_rfc2822("garbage"), None);
        assert!(
            Indexer {
                name: "x".into(),
                url: "ftp://x".into(),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert_eq!(
            Indexer {
                name: "x".into(),
                url: "https://a.b/".into(),
                api_path: "api".into(),
                ..Default::default()
            }
            .validate()
            .unwrap()
            .api_path,
            "/api"
        );
    }
}
