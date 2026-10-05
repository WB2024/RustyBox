//! Browsing IGDB's Xbox 360 games: filters (genre, theme, game mode, perspective, series,
//! franchise, company, engine, keyword, age rating, years, ratings, multiplayer), sorting, a
//! game's full details, related games and facet counts.
//!
//! Every filter is one `Browse` value. It becomes an IGDB query (`browse_body`, `count_body`) or,
//! in `--mock` mode, is evaluated over a built-in catalogue, so both behave the same.

use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::Value;

use super::{
    Candidate, Client, Credentials, GAME_TYPES, XBOX_360, mock_cover_svg, parse_candidate,
};
use crate::error::Error;

pub const PAGE: u32 = 24;

/// What can be filtered on: the name the browser uses, and the IGDB field it means.
pub const FACETS: &[(&str, &str)] = &[
    ("genre", "genres"),
    ("theme", "themes"),
    ("mode", "game_modes"),
    ("persp", "player_perspectives"),
    ("series", "collections"),
    ("franchise", "franchises"),
    ("company", "involved_companies.company"),
    ("engine", "game_engines"),
    ("keyword", "keywords"),
    ("age", "age_ratings.rating_category"),
];

pub fn facet_field(kind: &str) -> Option<&'static str> {
    FACETS.iter().find(|(k, _)| *k == kind).map(|(_, f)| *f)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    /// Most rated (the games most people know).
    #[default]
    Popular,
    /// Best rated by players.
    Rating,
    /// Best rated by critics.
    Critics,
    Newest,
    Oldest,
    Name,
}

#[derive(Debug, Clone, Default)]
pub struct Browse {
    /// Search text. IGDB can't sort a search, so a search is ranked by relevance.
    pub q: Option<String>,
    /// Chosen values per facet (`FACETS` kinds), as IGDB ids.
    pub facets: BTreeMap<String, Vec<i64>>,
    /// Facets where a game must have all of the chosen values, not just one.
    pub all: Vec<String>,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    /// At least this player rating (0-100).
    pub min_rating: Option<i32>,
    /// Only games that enough people have rated to mean something.
    pub known: bool,
    /// At most this many ratings (with a high rating: games few people know).
    pub max_votes: Option<i32>,
    pub online_coop: bool,
    pub local_coop: bool,
    pub online: bool,
    /// Games to leave out (the ones you have or want).
    pub exclude: Vec<i64>,
    pub sort: Sort,
    pub page: u32,
}

impl Browse {
    fn has_facets(&self) -> bool {
        self.facets.values().any(|v| !v.is_empty())
            || self.year_from.is_some()
            || self.year_to.is_some()
            || self.min_rating.is_some()
            || self.max_votes.is_some()
            || self.online_coop
            || self.local_coop
            || self.online
    }
}

/// Seconds since the epoch for 1 January of `year`.
fn jan1(year: i32) -> i64 {
    let y = (year - 1) as i64;
    let days = y * 365 + y / 4 - y / 100 + y / 400 - 719_162;
    days * 86_400
}

fn ids(v: &[i64]) -> String {
    v.iter()
        .take(400)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// The `where` conditions for a browse, without the popularity floors that depend on sorting.
fn conditions(b: &Browse) -> Vec<String> {
    let mut w = vec![
        format!("platforms = ({XBOX_360})"),
        format!("game_type = ({GAME_TYPES})"),
        "cover != null".to_string(),
    ];
    for (kind, field) in FACETS {
        let Some(v) = b.facets.get(*kind).filter(|v| !v.is_empty()) else {
            continue;
        };
        if b.all.iter().any(|a| a == kind) {
            w.push(format!("{field} = [{}]", ids(v)));
        } else {
            w.push(format!("{field} = ({})", ids(v)));
        }
    }
    if let Some(y) = b.year_from {
        w.push(format!("first_release_date >= {}", jan1(y)));
    }
    if let Some(y) = b.year_to {
        w.push(format!("first_release_date < {}", jan1(y + 1)));
    }
    if let Some(r) = b.min_rating {
        w.push(format!("total_rating >= {}", r.clamp(0, 100)));
        w.push("total_rating_count >= 3".into());
    }
    if b.known {
        w.push("total_rating_count >= 10".into());
    }
    if let Some(m) = b.max_votes {
        w.push(format!("total_rating_count <= {}", m.clamp(1, 100_000)));
    }
    if b.online_coop {
        w.push("multiplayer_modes.onlinecoop = true".into());
    }
    if b.local_coop {
        w.push(
            "(multiplayer_modes.offlinecoop = true | multiplayer_modes.splitscreen = true)".into(),
        );
    }
    if b.online {
        w.push("multiplayer_modes.onlinemax >= 2".into());
    }
    if !b.exclude.is_empty() {
        w.push(format!("id != ({})", ids(&b.exclude)));
    }
    w
}

/// The fields a result card needs.
fn card_fields() -> String {
    format!(
        "{},game_modes.name,themes.name,total_rating_count,aggregated_rating",
        Client::FIELDS
    )
}

/// The Apicalypse query for a page of a browse (separate so it can be tested without IGDB).
pub fn browse_body(b: &Browse, now: i64) -> String {
    let mut w = conditions(b);
    let searching = b.q.as_ref().is_some_and(|q| !q.trim().is_empty());
    // A floor on how many people rated a game keeps a bare browse to games people know; once
    // anything is chosen (a series, a theme...) small games are what is being looked for.
    let floor = !b.has_facets();
    let sort = match b.sort {
        Sort::Popular => {
            if floor {
                w.push("total_rating_count >= 3".into());
            }
            "sort total_rating_count desc;"
        }
        Sort::Rating => {
            w.push(format!(
                "total_rating_count >= {}",
                if floor { 8 } else { 3 }
            ));
            "sort total_rating desc;"
        }
        Sort::Critics => {
            w.push(format!(
                "aggregated_rating_count >= {}",
                if floor { 5 } else { 2 }
            ));
            "sort aggregated_rating desc;"
        }
        Sort::Newest => {
            w.push(format!(
                "first_release_date != null & first_release_date < {now}"
            ));
            "sort first_release_date desc;"
        }
        Sort::Oldest => {
            w.push("first_release_date != null".into());
            "sort first_release_date asc;"
        }
        Sort::Name => "sort name asc;",
    };
    let mut body = String::new();
    if searching {
        let q = b.q.clone().unwrap_or_default().replace(['\\', '"'], " ");
        body.push_str(&format!("search \"{}\"; ", q.trim()));
        // IGDB refuses `sort` together with `search`, and a search has no popularity floor.
        w.retain(|c| {
            !c.starts_with("total_rating_count >= 3") && !c.starts_with("total_rating_count >= 8")
        });
    }
    body.push_str(&format!(
        "fields {}; where {}; ",
        card_fields(),
        w.join(" & ")
    ));
    if !searching {
        body.push_str(sort);
        body.push(' ');
    }
    body.push_str(&format!(
        "limit {PAGE}; offset {};",
        b.page.saturating_sub(1) * PAGE
    ));
    body
}

/// The query that counts what a browse matches (no sort, no paging).
pub fn count_body(b: &Browse) -> String {
    let mut w = conditions(b);
    let searching = b.q.as_ref().is_some_and(|q| !q.trim().is_empty());
    let mut body = String::new();
    if searching {
        body.push_str(&format!(
            "search \"{}\"; ",
            b.q.clone()
                .unwrap_or_default()
                .replace(['\\', '"'], " ")
                .trim()
        ));
    } else if b.sort == Sort::Critics {
        w.push(format!(
            "aggregated_rating_count >= {}",
            if b.has_facets() { 2 } else { 5 }
        ));
    }
    body.push_str(&format!("where {};", w.join(" & ")));
    body
}

// ── What a result card and a game's page hold ────────────────────────────────

/// A game in a list: what the main query returns.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Card {
    #[serde(flatten)]
    pub game: Candidate,
    pub modes: Vec<String>,
    pub themes: Vec<String>,
    pub rating_count: Option<i64>,
    pub critics: Option<f64>,
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|g| g["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_card(v: &Value) -> Option<Card> {
    Some(Card {
        game: parse_candidate(v)?,
        modes: names(v, "game_modes"),
        themes: names(v, "themes"),
        rating_count: v["total_rating_count"].as_i64(),
        critics: v["aggregated_rating"].as_f64(),
    })
}

/// Something that can be filtered on or opened: an IGDB id and its name.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Named {
    pub id: i64,
    pub name: String,
}

fn named(v: &Value, key: &str) -> Vec<Named> {
    v[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|g| {
                    Some(Named {
                        id: g["id"].as_i64()?,
                        name: g["name"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgeRating {
    pub org: String,
    pub rating: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Language {
    pub name: String,
    /// Which of `Audio`, `Subtitles`, `Interface` it has.
    pub kinds: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct Multiplayer {
    pub online_coop: bool,
    pub offline_coop: bool,
    pub split_screen: bool,
    pub lan: bool,
    pub campaign_coop: bool,
    pub drop_in: bool,
    pub online_max: i64,
    pub offline_max: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReleaseDate {
    pub region: String,
    pub date: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Video {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Site {
    pub kind: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Related {
    pub kind: String,
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct TimeToBeat {
    /// Seconds.
    pub hastily: Option<i64>,
    pub normally: Option<i64>,
    pub completely: Option<i64>,
    pub count: Option<i64>,
}

/// More about one game.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Detail {
    pub game: Candidate,
    pub storyline: Option<String>,
    pub rating_count: Option<i64>,
    pub critics: Option<f64>,
    pub critics_count: Option<i64>,
    /// IGDB image IDs.
    pub screenshots: Vec<String>,
    pub artworks: Vec<String>,
    pub genres: Vec<Named>,
    pub themes: Vec<Named>,
    pub modes: Vec<Named>,
    pub perspectives: Vec<Named>,
    pub series: Vec<Named>,
    pub franchises: Vec<Named>,
    pub engines: Vec<Named>,
    pub keywords: Vec<Named>,
    pub developers: Vec<Named>,
    pub publishers: Vec<Named>,
    pub porters: Vec<Named>,
    pub age_ratings: Vec<AgeRating>,
    pub languages: Vec<Language>,
    pub multiplayer: Option<Multiplayer>,
    pub releases: Vec<ReleaseDate>,
    pub videos: Vec<Video>,
    pub sites: Vec<Site>,
    pub also_called: Vec<String>,
    pub related: Vec<Related>,
    pub similar_ids: Vec<i64>,
    pub time_to_beat: Option<TimeToBeat>,
}

fn region_name(r: &str) -> &str {
    match r {
        "europe" => "Europe",
        "north_america" => "North America",
        "australia" => "Australia",
        "new_zealand" => "New Zealand",
        "japan" => "Japan",
        "china" => "China",
        "asia" => "Asia",
        "worldwide" => "Worldwide",
        "korea" => "Korea",
        "brazil" => "Brazil",
        other => other,
    }
}

fn company_list(v: &Value, role: &str) -> Vec<Named> {
    v["involved_companies"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|c| c[role].as_bool().unwrap_or(false))
                .filter_map(|c| {
                    Some(Named {
                        id: c["company"]["id"].as_i64()?,
                        name: c["company"]["name"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn images(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s["image_id"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_detail(v: &Value) -> Option<Detail> {
    let game = parse_candidate(v)?;
    let mut age_ratings: Vec<AgeRating> = v["age_ratings"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    Some(AgeRating {
                        org: r["organization"]["name"].as_str()?.to_string(),
                        rating: r["rating_category"]["rating"].as_str()?.to_string(),
                        reasons: r["rating_content_descriptions"]
                            .as_array()
                            .map(|d| {
                                d.iter()
                                    .filter_map(|x| x["description"].as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // ESRB and PEGI first: the ones people know.
    age_ratings.sort_by_key(|r| match r.org.as_str() {
        "ESRB" => 0,
        "PEGI" => 1,
        _ => 2,
    });
    let mut languages: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for l in v["language_supports"].as_array().into_iter().flatten() {
        if let Some(n) = l["language"]["name"].as_str() {
            let kind = l["language_support_type"]["name"]
                .as_str()
                .unwrap_or("Interface");
            let e = languages.entry(n.to_string()).or_default();
            if !e.iter().any(|k| k == kind) {
                e.push(kind.to_string());
            }
        }
    }
    // Xbox 360 details first; if IGDB has none for this console, any platform's.
    let mp = v["multiplayer_modes"].as_array().and_then(|a| {
        let pick: Vec<&Value> = a
            .iter()
            .filter(|m| m["platform"].as_i64() == Some(XBOX_360 as i64))
            .collect();
        let pick = if pick.is_empty() {
            a.iter().collect()
        } else {
            pick
        };
        pick.into_iter().fold(None, |acc: Option<Multiplayer>, m| {
            let mut x = acc.unwrap_or_default();
            let b = |k: &str| m[k].as_bool().unwrap_or(false);
            x.online_coop |= b("onlinecoop");
            x.offline_coop |= m["offlinecoopmax"].as_i64().unwrap_or(0) > 1 || b("offlinecoop");
            x.split_screen |= b("splitscreen");
            x.lan |= b("lancoop");
            x.campaign_coop |= b("campaigncoop");
            x.drop_in |= b("dropin");
            x.online_max = x.online_max.max(m["onlinemax"].as_i64().unwrap_or(0));
            x.offline_max = x
                .offline_max
                .max(m["offlinemax"].as_i64().unwrap_or(0))
                .max(m["offlinecoopmax"].as_i64().unwrap_or(0));
            Some(x)
        })
    });
    let mut releases: Vec<ReleaseDate> = v["release_dates"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|r| r["platform"].as_i64() == Some(XBOX_360 as i64))
                .filter_map(|r| {
                    Some(ReleaseDate {
                        region: region_name(r["release_region"]["region"].as_str()?).to_string(),
                        date: r["date"].as_i64()?,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    releases.sort_by_key(|r| r.date);
    let mut related = Vec::new();
    for (key, label) in [
        ("parent_game", "Base game"),
        ("version_parent", "Edition of"),
        ("remakes", "Remake"),
        ("remasters", "Remaster"),
        ("ports", "Port"),
        ("expansions", "Expansion"),
        ("standalone_expansions", "Standalone expansion"),
        ("dlcs", "Add-on"),
        ("bundles", "Bundle"),
    ] {
        let list: Vec<Named> = match v[key].as_array() {
            Some(_) => named(v, key),
            None => v[key]
                .as_object()
                .and_then(|o| {
                    Some(vec![Named {
                        id: o.get("id")?.as_i64()?,
                        name: o.get("name")?.as_str()?.to_string(),
                    }])
                })
                .unwrap_or_default(),
        };
        related.extend(list.into_iter().take(12).map(|n| Related {
            kind: label.to_string(),
            id: n.id,
            name: n.name,
        }));
    }
    Some(Detail {
        storyline: v["storyline"].as_str().map(String::from),
        rating_count: v["total_rating_count"].as_i64(),
        critics: v["aggregated_rating"].as_f64(),
        critics_count: v["aggregated_rating_count"].as_i64(),
        screenshots: images(v, "screenshots"),
        artworks: images(v, "artworks"),
        genres: named(v, "genres"),
        themes: named(v, "themes"),
        modes: named(v, "game_modes"),
        perspectives: named(v, "player_perspectives"),
        series: named(v, "collections"),
        franchises: named(v, "franchises"),
        engines: named(v, "game_engines"),
        keywords: named(v, "keywords").into_iter().take(24).collect(),
        developers: company_list(v, "developer"),
        publishers: company_list(v, "publisher"),
        porters: company_list(v, "porting"),
        age_ratings,
        languages: languages
            .into_iter()
            .map(|(name, kinds)| Language { name, kinds })
            .collect(),
        multiplayer: mp,
        releases,
        videos: v["videos"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        let id = x["video_id"].as_str()?;
                        // A YouTube id: letters, digits, dash, underscore.
                        (id.len() <= 20
                            && id
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
                        .then(|| Video {
                            id: id.to_string(),
                            name: x["name"].as_str().unwrap_or("Video").to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        sites: v["websites"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        let url = x["url"].as_str()?;
                        let kind = x["type"]["type"].as_str().unwrap_or("Link");
                        // Only web links: nothing else is ever put in an `href`.
                        ((url.starts_with("https://") || url.starts_with("http://"))
                            && !kind.starts_with("App Store")
                            && kind != "Google Play"
                            && kind != "Itch"
                            && kind != "Android")
                            .then(|| Site {
                                kind: kind.to_string(),
                                url: url.to_string(),
                            })
                    })
                    .take(10)
                    .collect()
            })
            .unwrap_or_default(),
        also_called: names(v, "alternative_names")
            .into_iter()
            .filter(|n| n.is_ascii())
            .take(8)
            .collect(),
        related,
        similar_ids: v["similar_games"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default(),
        time_to_beat: None,
        game,
    })
}

const DETAIL_FIELDS: &str = "storyline,total_rating_count,aggregated_rating,aggregated_rating_count,screenshots.image_id,artworks.image_id,genres.name,themes.name,game_modes.name,player_perspectives.name,collections.name,franchises.name,game_engines.name,keywords.name,involved_companies.company.name,involved_companies.developer,involved_companies.publisher,involved_companies.porting,age_ratings.organization.name,age_ratings.rating_category.rating,age_ratings.rating_content_descriptions.description,language_supports.language.name,language_supports.language_support_type.name,multiplayer_modes.onlinecoop,multiplayer_modes.offlinecoop,multiplayer_modes.offlinecoopmax,multiplayer_modes.offlinemax,multiplayer_modes.onlinemax,multiplayer_modes.splitscreen,multiplayer_modes.lancoop,multiplayer_modes.campaigncoop,multiplayer_modes.dropin,multiplayer_modes.platform,release_dates.date,release_dates.platform,release_dates.release_region.region,videos.video_id,videos.name,websites.url,websites.type.type,alternative_names.name,dlcs.name,expansions.name,standalone_expansions.name,remakes.name,remasters.name,ports.name,bundles.name,parent_game.name,version_parent.name,similar_games";

// ── What the filter lists hold ───────────────────────────────────────────────

/// The fixed choices IGDB has: genres, themes, game modes, perspectives and age ratings.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Facets {
    pub genre: Vec<Named>,
    pub theme: Vec<Named>,
    pub mode: Vec<Named>,
    pub persp: Vec<Named>,
    pub age: Vec<Named>,
}

#[derive(Default)]
pub struct Cache {
    facets: Option<Facets>,
    /// (when, id → how many Xbox 360 games) per facet kind.
    counts: HashMap<String, (Instant, HashMap<i64, i64>)>,
}

const COUNTS_FOR: Duration = Duration::from_secs(12 * 3600);

/// Make something safe to put in a quoted Apicalypse string.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '"' | '\\' | '*' | ';' | '\n' | '\r'))
        .collect::<String>()
        .trim()
        .chars()
        .take(60)
        .collect()
}

impl Client {
    /// A picture from IGDB in one of its sizes (`t_cover_big`, `t_screenshot_big`, `t_thumb`...).
    /// In mock mode the picture is an inline sample.
    pub fn image_url(&self, size: &str, image_id: &str) -> Option<String> {
        if !image_id.bytes().all(|b| b.is_ascii_alphanumeric())
            || !size.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return None;
        }
        if self.is_mock() {
            let svg = mock_cover_svg(image_id);
            return Some(format!(
                "data:image/svg+xml;utf8,{}",
                svg.replace('#', "%23").replace('"', "'")
            ));
        }
        Some(format!("{}/{size}/{image_id}.jpg", self.urls.images))
    }

    /// A page of Xbox 360 games.
    pub fn browse(&self, creds: &Credentials, b: &Browse) -> Result<Vec<Card>, Error> {
        if self.is_mock() {
            return Ok(mock_browse(b));
        }
        let now = super::now() as i64;
        let v = self.query(creds, "games", &browse_body(b, now))?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(parse_card).collect())
            .unwrap_or_default())
    }

    /// How many games a browse matches in all.
    pub fn count(&self, creds: &Credentials, b: &Browse) -> Result<i64, Error> {
        if self.is_mock() {
            let mut all = b.clone();
            all.page = 1;
            return Ok(mock_filtered(&all).len() as i64);
        }
        let v = self.query(creds, "games/count", &count_body(b))?;
        Ok(v["count"].as_i64().unwrap_or(0))
    }

    /// The genres Xbox 360 games have (cached after the first call).
    pub fn genres(&self, creds: &Credentials) -> Result<Vec<(i64, String)>, Error> {
        Ok(self
            .facets(creds)?
            .genre
            .into_iter()
            .map(|n| (n.id, n.name))
            .collect())
    }

    /// The fixed filter lists, cached after the first call.
    pub fn facets(&self, creds: &Credentials) -> Result<Facets, Error> {
        if let Some(f) = self
            .disc
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .facets
            .clone()
        {
            return Ok(f);
        }
        let f = if self.is_mock() {
            mock_facets()
        } else {
            let list = |ep: &str, extra: &str| -> Result<Vec<Named>, Error> {
                let v = self.query(
                    creds,
                    ep,
                    &format!("fields name; {extra} limit 100; sort name asc;"),
                )?;
                Ok(named(&serde_json::json!({"x": v}), "x"))
            };
            // Age ratings come as categories of an organisation: show the two people know.
            let cats = self.query(
                creds,
                "age_rating_categories",
                "fields rating,organization.name; where organization = (1,2); limit 100;",
            )?;
            let mut age: Vec<(String, Named)> = cats
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|c| {
                            let org = c["organization"]["name"].as_str()?;
                            let r = c["rating"].as_str()?;
                            Some((
                                org.to_string(),
                                Named {
                                    id: c["id"].as_i64()?,
                                    name: format!("{org} {r}"),
                                },
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            age.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.id.cmp(&b.1.id)));
            Facets {
                genre: list("genres", "")?,
                theme: list("themes", "")?,
                mode: list("game_modes", "")?,
                persp: list("player_perspectives", "")?,
                age: age.into_iter().map(|a| a.1).collect(),
            }
        };
        self.disc.lock().unwrap_or_else(|e| e.into_inner()).facets = Some(f.clone());
        Ok(f)
    }

    /// How many Xbox 360 games each value of a fixed facet has (cached for hours). Asked ten at a
    /// time through IGDB's multi-query.
    pub fn facet_counts(
        &self,
        creds: &Credentials,
        kind: &str,
    ) -> Result<HashMap<i64, i64>, Error> {
        let Some(field) = facet_field(kind) else {
            return Err(Error::validation("Unknown filter"));
        };
        if let Some((at, c)) = self
            .disc
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .counts
            .get(kind)
            && at.elapsed() < COUNTS_FOR
        {
            return Ok(c.clone());
        }
        let f = self.facets(creds)?;
        let list = match kind {
            "genre" => f.genre,
            "theme" => f.theme,
            "mode" => f.mode,
            "persp" => f.persp,
            "age" => f.age,
            _ => return Err(Error::validation("That filter has no fixed list")),
        };
        let mut out = HashMap::new();
        if self.is_mock() {
            for n in &list {
                let mut b = Browse::default();
                b.facets.insert(kind.to_string(), vec![n.id]);
                out.insert(n.id, mock_filtered(&b).len() as i64);
            }
        } else {
            for chunk in list.chunks(10) {
                let body = chunk
                    .iter()
                    .map(|n| {
                        format!(
                            "query games/count \"{}\" {{ where platforms = ({XBOX_360}) & game_type = ({GAME_TYPES}) & {field} = ({}); }};",
                            n.id, n.id
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let v = self.query(creds, "multiquery", &body)?;
                for r in v.as_array().into_iter().flatten() {
                    if let (Some(id), Some(c)) = (
                        r["name"].as_str().and_then(|s| s.parse::<i64>().ok()),
                        r["count"].as_i64(),
                    ) {
                        out.insert(id, c);
                    }
                }
            }
        }
        self.disc
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .counts
            .insert(kind.to_string(), (Instant::now(), out.clone()));
        Ok(out)
    }

    /// Names to pick from as a person types: companies, series, franchises, engines, keywords.
    pub fn suggest(
        &self,
        creds: &Credentials,
        kind: &str,
        text: &str,
    ) -> Result<Vec<Named>, Error> {
        let text = clean(text);
        if text.len() < 2 {
            return Ok(vec![]);
        }
        if self.is_mock() {
            let l = text.to_lowercase();
            return Ok(mock_names(kind)
                .into_iter()
                .filter(|n| n.name.to_lowercase().contains(&l))
                .collect());
        }
        let (ep, extra) = match kind {
            "company" => ("companies", ""),
            "series" => (
                "collections",
                &format!("games.platforms = ({XBOX_360}) & ")[..],
            ),
            "franchise" => (
                "franchises",
                &format!("games.platforms = ({XBOX_360}) & ")[..],
            ),
            "engine" => ("game_engines", ""),
            "keyword" => ("keywords", ""),
            _ => return Err(Error::validation("That filter can't be searched")),
        };
        let v = self.query(
            creds,
            ep,
            &format!("fields name; where {extra}name ~ *\"{text}\"*; limit 12;"),
        )?;
        Ok(named(&serde_json::json!({"x": v}), "x"))
    }

    /// One random game that fits a browse.
    pub fn random(&self, creds: &Credentials, b: &Browse) -> Result<Option<Card>, Error> {
        let n = self.count(creds, b)?;
        if n == 0 {
            return Ok(None);
        }
        let pick = (super::now_nanos() % n as u128) as u32;
        let mut one = b.clone();
        one.sort = Sort::Name;
        one.q = None;
        if self.is_mock() {
            return Ok(mock_filtered(&one).into_iter().nth(pick as usize));
        }
        let mut body = browse_body(&one, super::now() as i64);
        // The page size is 24: take the one game at position `pick` instead.
        body = body
            .rsplit_once("limit ")
            .map(|(a, _)| a.to_string())
            .unwrap_or(body);
        body.push_str(&format!("limit 1; offset {pick};"));
        let v = self.query(creds, "games", &body)?;
        Ok(v.as_array().and_then(|a| a.first()).and_then(parse_card))
    }

    /// Everything about one game.
    pub fn detail(&self, creds: &Credentials, id: i64) -> Result<Option<Detail>, Error> {
        if self.is_mock() {
            return Ok(mock_detail(id));
        }
        let body = format!(
            "fields {},{DETAIL_FIELDS}; where id = {id};",
            Client::FIELDS
        );
        let v = self.query(creds, "games", &body)?;
        let mut d = v.as_array().and_then(|a| a.first()).and_then(parse_detail);
        if let Some(d) = d.as_mut() {
            // How long it takes to finish, from players (a small second query; never fatal).
            if let Ok(t) = self.query(
                creds,
                "game_time_to_beats",
                &format!(
                    "fields hastily,normally,completely,count; where game_id = {id}; limit 1;"
                ),
            ) && let Some(t) = t.as_array().and_then(|a| a.first())
            {
                let tt = TimeToBeat {
                    hastily: t["hastily"].as_i64(),
                    normally: t["normally"].as_i64(),
                    completely: t["completely"].as_i64(),
                    count: t["count"].as_i64(),
                };
                if tt.hastily.is_some() || tt.normally.is_some() || tt.completely.is_some() {
                    d.time_to_beat = Some(tt);
                }
            }
        }
        Ok(d)
    }

    /// Of these IGDB ids, the ones that are Xbox 360 games (similar games are often other platforms).
    pub fn xbox_games_among(&self, creds: &Credentials, ids: &[i64]) -> Result<Vec<Card>, Error> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        if self.is_mock() {
            return Ok(mock_catalog_cards()
                .into_iter()
                .filter(|g| ids.contains(&g.game.id))
                .collect());
        }
        let list = ids
            .iter()
            .take(30)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let body = format!(
            "fields {}; where id = ({list}) & platforms = ({XBOX_360}) & game_type = ({GAME_TYPES}) & cover != null; limit 12;",
            card_fields()
        );
        let v = self.query(creds, "games", &body)?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(parse_card).collect())
            .unwrap_or_default())
    }
}

// ── Mock data ────────────────────────────────────────────────────────────────

const MOCK_GENRES: &[(i64, &str)] = &[
    (5, "Shooter"),
    (10, "Racing"),
    (12, "Role-playing (RPG)"),
    (31, "Adventure"),
    (8, "Platform"),
    (4, "Fighting"),
    (14, "Sport"),
    (15, "Strategy"),
];

const MOCK_THEMES: &[(i64, &str)] = &[
    (1, "Action"),
    (17, "Fantasy"),
    (18, "Science fiction"),
    (19, "Horror"),
    (21, "Survival"),
    (23, "Stealth"),
    (27, "Comedy"),
    (38, "Open world"),
];
const MOCK_MODES: &[(i64, &str)] = &[
    (1, "Single player"),
    (2, "Multiplayer"),
    (3, "Co-operative"),
    (4, "Split screen"),
];
const MOCK_PERSP: &[(i64, &str)] = &[
    (1, "First person"),
    (2, "Third person"),
    (3, "Bird view / Isometric"),
    (4, "Side view"),
];
const MOCK_AGE: &[(i64, &str)] = &[
    (3, "ESRB E"),
    (4, "ESRB E10+"),
    (5, "ESRB T"),
    (6, "ESRB M"),
];
const MOCK_COMPANIES: &[(i64, &str)] = &[
    (10, "Microsoft Studios"),
    (11, "Bethesda Softworks"),
    (12, "Rockstar Games"),
    (99, "Sample Studio"),
];
const MOCK_SERIES: &[(i64, &str)] = &[
    (1, "Halo"),
    (2, "Gears of War"),
    (3, "Call of Duty"),
    (4, "Mass Effect"),
    (5, "Forza"),
    (6, "Need for Speed"),
    (7, "Fallout"),
    (8, "Grand Theft Auto"),
];
const MOCK_ENGINES: &[(i64, &str)] = &[(351, "Unreal Engine 3"), (7, "id Tech 4")];

fn pairs(l: &[(i64, &str)]) -> Vec<Named> {
    l.iter()
        .map(|(id, name)| Named {
            id: *id,
            name: name.to_string(),
        })
        .collect()
}

fn mock_facets() -> Facets {
    Facets {
        genre: pairs(MOCK_GENRES),
        theme: pairs(MOCK_THEMES),
        mode: pairs(MOCK_MODES),
        persp: pairs(MOCK_PERSP),
        age: pairs(MOCK_AGE),
    }
}

fn mock_names(kind: &str) -> Vec<Named> {
    match kind {
        "company" => pairs(MOCK_COMPANIES),
        "series" | "franchise" => pairs(MOCK_SERIES),
        "engine" => pairs(MOCK_ENGINES),
        "keyword" => pairs(&[(5, "zombies"), (6, "post-apocalyptic")]),
        _ => vec![],
    }
}

/// (name, genre id, year, rating)
const MOCK_GAMES: &[(&str, i64, i32, f64)] = &[
    ("Halo 3", 5, 2007, 88.0),
    ("Halo: Reach", 5, 2010, 86.0),
    ("Gears of War", 5, 2006, 87.0),
    ("Gears of War 2", 5, 2008, 88.0),
    ("Gears of War 3", 5, 2011, 85.0),
    ("Call of Duty 4: Modern Warfare", 5, 2007, 90.0),
    ("Bioshock", 5, 2007, 89.0),
    ("Left 4 Dead 2", 5, 2009, 85.0),
    ("Forza Motorsport 4", 10, 2011, 88.0),
    ("Forza Motorsport 3", 10, 2009, 86.0),
    ("Need for Speed: Most Wanted", 10, 2005, 80.0),
    ("Burnout Paradise", 10, 2008, 87.0),
    ("Fable II", 12, 2008, 84.0),
    ("Mass Effect 2", 12, 2010, 91.0),
    ("Mass Effect", 12, 2007, 87.0),
    ("The Elder Scrolls IV: Oblivion", 12, 2006, 86.0),
    ("Fallout 3", 12, 2008, 87.0),
    ("Alan Wake", 31, 2010, 80.0),
    ("Red Dead Redemption", 31, 2010, 93.0),
    ("Grand Theft Auto IV", 31, 2008, 92.0),
    ("Assassin's Creed II", 31, 2009, 87.0),
    ("Castle Crashers", 8, 2008, 82.0),
    ("Ninja Gaiden II", 4, 2008, 78.0),
    ("Street Fighter IV", 4, 2009, 87.0),
    ("Fight Night Round 4", 14, 2009, 84.0),
    ("Halo Wars", 15, 2009, 75.0),
    ("The Simpsons Game", 31, 2007, 68.0),
    ("Dead Space", 5, 2008, 88.0),
    ("Dead Rising", 31, 2006, 75.0),
    ("Viva Pinata", 15, 2006, 76.0),
];

/// What the mock catalogue says about a game for each filter, as the ids a real game would have.
struct MockMeta {
    facets: BTreeMap<&'static str, Vec<i64>>,
    online_coop: bool,
    local_coop: bool,
    online: bool,
}

fn mock_meta(i: usize, name: &str, genre: i64) -> MockMeta {
    let mut f: BTreeMap<&'static str, Vec<i64>> = BTreeMap::new();
    f.insert("genre", vec![genre]);
    let mut modes = vec![1];
    let shooter = genre == 5;
    let racing = genre == 10;
    if shooter || racing || genre == 4 {
        modes.push(2);
    }
    if i.is_multiple_of(3) {
        modes.push(3);
    }
    if racing || genre == 8 {
        modes.push(4);
    }
    f.insert("mode", modes.clone());
    f.insert(
        "theme",
        match genre {
            5 => vec![1, 18],
            10 => vec![1],
            12 => vec![17, 38],
            31 => vec![1, 38],
            8 => vec![27],
            15 => vec![18],
            _ => vec![1],
        },
    );
    if name.starts_with("Dead") || name.contains("Left 4") || name.contains("Alan") {
        if let Some(t) = f.get_mut("theme") {
            t.push(19);
        }
    }
    f.insert(
        "persp",
        vec![if shooter {
            1
        } else if genre == 8 {
            4
        } else {
            2
        }],
    );
    f.insert(
        "age",
        vec![if shooter || name.contains("Dead") {
            6
        } else if genre == 14 || racing {
            3
        } else {
            5
        }],
    );
    let series = MOCK_SERIES
        .iter()
        .find(|(_, s)| {
            name.starts_with(s) || (*s == "Grand Theft Auto" && name.contains("Grand Theft"))
        })
        .map(|(id, _)| *id);
    if let Some(s) = series {
        f.insert("series", vec![s]);
        f.insert("franchise", vec![s]);
    }
    let company = if name.starts_with("Halo")
        || name.starts_with("Gears")
        || name.starts_with("Forza")
        || name.starts_with("Fable")
    {
        10
    } else if name.starts_with("Fallout") || name.contains("Elder Scrolls") {
        11
    } else if name.contains("Grand Theft") || name.contains("Red Dead") {
        12
    } else {
        99
    };
    f.insert("company", vec![company]);
    if name.starts_with("Gears") || name.starts_with("Bioshock") {
        f.insert("engine", vec![351]);
    }
    MockMeta {
        facets: f,
        online_coop: modes.contains(&3) && shooter,
        local_coop: modes.contains(&4) || (modes.contains(&3) && !shooter),
        online: modes.contains(&2),
    }
}

fn mock_names_for(list: &[(i64, &str)], ids: &[i64]) -> Vec<String> {
    ids.iter()
        .filter_map(|i| list.iter().find(|x| x.0 == *i).map(|x| x.1.to_string()))
        .collect()
}

pub(crate) fn mock_catalog() -> Vec<(Candidate, i64)> {
    MOCK_GAMES
        .iter()
        .enumerate()
        .map(|(i, (name, genre, year, rating))| {
            let gname = MOCK_GENRES
                .iter()
                .find(|g| g.0 == *genre)
                .map(|g| g.1)
                .unwrap_or("Action");
            let m = mock_meta(i, name, *genre);
            let dev = m.facets["company"][0];
            (
                Candidate {
                    id: 700_000 + i as i64,
                    name: name.to_string(),
                    summary: Some(format!("A sample description of {name}, shown because RustyBox is in mock mode and not talking to IGDB.")),
                    release: Some(jan1(*year) + 100 * 86_400),
                    genres: vec![gname.to_string()],
                    developers: vec![MOCK_COMPANIES.iter().find(|c| c.0 == dev).map(|c| c.1).unwrap_or("Sample Studio").to_string()],
                    publishers: vec!["Sample Publisher".into()],
                    rating: Some(*rating),
                    url: None,
                    cover: Some(format!("mock{}", 100 + i)),
                },
                *genre,
            )
        })
        .collect()
}

fn mock_catalog_cards() -> Vec<Card> {
    mock_catalog()
        .into_iter()
        .enumerate()
        .map(|(i, (game, genre))| {
            let m = mock_meta(i, &game.name, genre);
            Card {
                modes: mock_names_for(MOCK_MODES, &m.facets["mode"]),
                themes: mock_names_for(MOCK_THEMES, &m.facets["theme"]),
                rating_count: Some(50 + game.rating.unwrap_or(0.0) as i64 * 3 + (40 - i as i64)),
                critics: game.rating.map(|r| r - 2.0),
                game,
            }
        })
        .collect()
}

/// The mock catalogue narrowed by a browse, in the order the browse asks for.
fn mock_filtered(b: &Browse) -> Vec<Card> {
    let q =
        b.q.as_ref()
            .map(|q| q.trim().to_lowercase())
            .filter(|q| !q.is_empty());
    let mut list: Vec<Card> = mock_catalog_cards()
        .into_iter()
        .enumerate()
        .filter(|(i, c)| {
            let genre = MOCK_GAMES[*i].1;
            let m = mock_meta(*i, &c.game.name, genre);
            let facets_ok = b.facets.iter().all(|(kind, want)| {
                if want.is_empty() {
                    return true;
                }
                let have = m.facets.get(kind.as_str()).cloned().unwrap_or_default();
                if b.all.iter().any(|a| a == kind) {
                    want.iter().all(|w| have.contains(w))
                } else {
                    want.iter().any(|w| have.contains(w))
                }
            });
            facets_ok
                && q.as_ref()
                    .is_none_or(|q| c.game.name.to_lowercase().contains(q))
                && b.year_from
                    .is_none_or(|y| c.game.release.unwrap_or(0) >= jan1(y))
                && b.year_to
                    .is_none_or(|y| c.game.release.unwrap_or(0) < jan1(y + 1))
                && b.min_rating
                    .is_none_or(|r| c.game.rating.unwrap_or(0.0) >= r as f64)
                && (!b.known || c.rating_count.unwrap_or(0) >= 10)
                && b.max_votes
                    .is_none_or(|m| c.rating_count.unwrap_or(0) <= m as i64)
                && (!b.online_coop || m.online_coop)
                && (!b.local_coop || m.local_coop)
                && (!b.online || m.online)
                && !b.exclude.contains(&c.game.id)
        })
        .map(|(_, c)| c)
        .collect();
    if q.is_none() {
        match b.sort {
            Sort::Popular => list.sort_by_key(|c| std::cmp::Reverse(c.rating_count)),
            Sort::Rating => list.sort_by(|a, c| {
                c.game
                    .rating
                    .partial_cmp(&a.game.rating)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            Sort::Critics => list.sort_by(|a, c| {
                c.critics
                    .partial_cmp(&a.critics)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            Sort::Newest => list.sort_by_key(|c| std::cmp::Reverse(c.game.release)),
            Sort::Oldest => list.sort_by_key(|c| c.game.release),
            Sort::Name => list.sort_by(|a, c| a.game.name.cmp(&c.game.name)),
        }
    }
    list
}

fn mock_browse(b: &Browse) -> Vec<Card> {
    let skip = (b.page.saturating_sub(1) * PAGE) as usize;
    mock_filtered(b)
        .into_iter()
        .skip(skip)
        .take(PAGE as usize)
        .collect()
}

fn mock_detail(id: i64) -> Option<Detail> {
    let (i, (game, genre)) = mock_catalog()
        .into_iter()
        .enumerate()
        .find(|(_, g)| g.0.id == id)?;
    let m = mock_meta(i, &game.name, genre);
    let named_from = |list: &[(i64, &str)], key: &str| -> Vec<Named> {
        m.facets
            .get(key)
            .map(|ids| {
                ids.iter()
                    .filter_map(|i| {
                        list.iter().find(|x| x.0 == *i).map(|x| Named {
                            id: x.0,
                            name: x.1.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let similar_ids = mock_catalog()
        .iter()
        .filter(|g| g.0.id != id && g.0.genres == game.genres)
        .take(8)
        .map(|g| g.0.id)
        .collect();
    let developer = named_from(MOCK_COMPANIES, "company");
    Some(Detail {
        storyline: Some(format!(
            "The story of {}: a pretend storyline for mock mode.",
            game.name
        )),
        rating_count: Some(120),
        critics: game.rating.map(|r| r - 3.0),
        critics_count: Some(18),
        screenshots: (0..3)
            .map(|n| format!("mockshot{}{}", id % 100, ["a", "b", "c"][n]))
            .collect(),
        artworks: vec![],
        genres: named_from(MOCK_GENRES, "genre"),
        themes: named_from(MOCK_THEMES, "theme"),
        modes: named_from(MOCK_MODES, "mode"),
        perspectives: named_from(MOCK_PERSP, "persp"),
        series: named_from(MOCK_SERIES, "series"),
        franchises: named_from(MOCK_SERIES, "franchise"),
        engines: named_from(MOCK_ENGINES, "engine"),
        keywords: vec![Named {
            id: 5,
            name: "sample".into(),
        }],
        developers: developer.clone(),
        publishers: developer,
        porters: vec![],
        age_ratings: named_from(MOCK_AGE, "age")
            .into_iter()
            .map(|n| AgeRating {
                org: "ESRB".into(),
                rating: n.name.trim_start_matches("ESRB ").to_string(),
                reasons: vec!["Sample content".into()],
            })
            .collect(),
        languages: vec![
            Language {
                name: "English".into(),
                kinds: vec!["Audio".into(), "Subtitles".into(), "Interface".into()],
            },
            Language {
                name: "French".into(),
                kinds: vec!["Subtitles".into()],
            },
        ],
        multiplayer: Some(Multiplayer {
            online_coop: m.online_coop,
            offline_coop: m.local_coop,
            split_screen: m.local_coop,
            online_max: if m.online { 8 } else { 0 },
            offline_max: if m.local_coop { 2 } else { 0 },
            ..Default::default()
        }),
        releases: game
            .release
            .map(|d| {
                vec![
                    ReleaseDate {
                        region: "North America".into(),
                        date: d,
                    },
                    ReleaseDate {
                        region: "Europe".into(),
                        date: d + 5 * 86_400,
                    },
                ]
            })
            .unwrap_or_default(),
        videos: vec![Video {
            id: "dQw4w9WgXcQ".into(),
            name: "Trailer".into(),
        }],
        sites: vec![Site {
            kind: "Official Website".into(),
            url: "https://example.com/".into(),
        }],
        also_called: vec![],
        related: if game.name == "Halo 3" {
            vec![Related {
                kind: "Add-on".into(),
                id: 700_001,
                name: "Halo: Reach".into(),
            }]
        } else {
            vec![]
        },
        similar_ids,
        time_to_beat: Some(TimeToBeat {
            hastily: Some(9 * 3600),
            normally: Some(12 * 3600),
            completely: Some(20 * 3600),
            count: Some(30),
        }),
        game,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jan_first_is_computed_right() {
        assert_eq!(jan1(1970), 0);
        assert_eq!(jan1(2000), 946_684_800);
        assert_eq!(jan1(2008), 1_199_145_600);
    }

    fn with(kind: &str, ids: &[i64]) -> Browse {
        let mut b = Browse {
            page: 1,
            ..Default::default()
        };
        b.facets.insert(kind.into(), ids.to_vec());
        b
    }

    #[test]
    fn queries_filter_to_xbox_360_and_never_sort_a_search() {
        let mut b = with("genre", &[5]);
        b.year_from = Some(2007);
        b.year_to = Some(2009);
        b.sort = Sort::Rating;
        b.page = 3;
        let q = browse_body(&b, 1_700_000_000);
        assert!(
            q.contains("platforms = (12)")
                && q.contains("game_type = (0,8,9,10,11)")
                && q.contains("cover != null"),
            "{q}"
        );
        assert!(
            q.contains("genres = (5)")
                && q.contains(&format!("first_release_date >= {}", jan1(2007)))
                && q.contains(&format!("first_release_date < {}", jan1(2010))),
            "{q}"
        );
        // Something is chosen, so the floor for best-rated is the lower one.
        assert!(
            q.contains("sort total_rating desc;")
                && q.contains("total_rating_count >= 3")
                && q.ends_with("limit 24; offset 48;"),
            "{q}"
        );
        // A bare best-rated list keeps the higher floor.
        let bare = browse_body(
            &Browse {
                sort: Sort::Rating,
                page: 1,
                ..Default::default()
            },
            0,
        );
        assert!(bare.contains("total_rating_count >= 8"), "{bare}");
        // A search is relevance-ranked: no sort, no popularity floor, and quotes can't break out.
        let s = browse_body(
            &Browse {
                q: Some("halo \"3\"".into()),
                sort: Sort::Popular,
                page: 1,
                ..Default::default()
            },
            0,
        );
        assert!(
            s.starts_with("search \"halo  3 \"") || s.starts_with("search \"halo  3\""),
            "{s}"
        );
        assert!(
            !s.contains("sort ") && !s.contains("total_rating_count >="),
            "{s}"
        );
        // Newest never lists games that aren't out yet.
        assert!(
            browse_body(
                &Browse {
                    sort: Sort::Newest,
                    ..Default::default()
                },
                1_700_000_000
            )
            .contains("first_release_date < 1700000000")
        );
    }

    #[test]
    fn every_filter_becomes_a_condition() {
        let mut b = with("theme", &[19, 17]);
        b.facets.insert("mode".into(), vec![2, 3]);
        b.all = vec!["mode".into()];
        b.facets.insert("series".into(), vec![847]);
        b.facets.insert("company".into(), vec![365]);
        b.facets.insert("age".into(), vec![6]);
        b.min_rating = Some(75);
        b.known = true;
        b.online_coop = true;
        b.local_coop = true;
        b.online = true;
        b.exclude = vec![1, 2];
        let c = count_body(&b);
        for want in [
            "themes = (19,17)",
            "game_modes = [2,3]",
            "collections = (847)",
            "involved_companies.company = (365)",
            "age_ratings.rating_category = (6)",
            "total_rating >= 75",
            "total_rating_count >= 10",
            "multiplayer_modes.onlinecoop = true",
            "(multiplayer_modes.offlinecoop = true | multiplayer_modes.splitscreen = true)",
            "multiplayer_modes.onlinemax >= 2",
            "id != (1,2)",
        ] {
            assert!(c.contains(want), "{want} missing from {c}");
        }
        assert!(
            c.starts_with("where ") && !c.contains("sort") && !c.contains("limit"),
            "{c}"
        );
        // Quotes and wildcards in typed text can't leave the string.
        assert_eq!(clean("a\"b*c;\\d"), "abcd");
    }

    #[test]
    fn a_games_details_are_read_from_igdbs_answer() {
        let v: Value = serde_json::json!({"id": 7, "name": "Halo 3", "summary": "s", "storyline": "story", "total_rating_count": 900, "aggregated_rating": 91.5, "aggregated_rating_count": 30, "first_release_date": 1190000000,
            "cover": {"image_id": "co1"}, "genres": [{"id": 5, "name": "Shooter"}], "themes": [{"id": 18, "name": "Science fiction"}],
            "screenshots": [{"image_id": "sc1"}, {"image_id": "sc2"}],
            "game_modes": [{"id": 1, "name": "Single player"}, {"id": 3, "name": "Co-operative"}], "player_perspectives": [{"id": 1, "name": "First person"}],
            "collections": [{"id": 9, "name": "Halo"}], "franchises": [{"id": 137, "name": "Halo"}], "game_engines": [{"id": 3, "name": "Blam!"}],
            "involved_companies": [{"company": {"id": 1, "name": "Bungie"}, "developer": true, "publisher": false, "porting": false}, {"company": {"id": 2, "name": "Microsoft"}, "developer": false, "publisher": true}],
            "age_ratings": [{"organization": {"name": "PEGI"}, "rating_category": {"rating": "16"}}, {"organization": {"name": "ESRB"}, "rating_category": {"rating": "M"}, "rating_content_descriptions": [{"description": "Blood"}]}],
            "language_supports": [{"language": {"name": "English"}, "language_support_type": {"name": "Audio"}}, {"language": {"name": "English"}, "language_support_type": {"name": "Subtitles"}}],
            "multiplayer_modes": [{"platform": 6, "onlinemax": 2}, {"platform": 12, "onlinecoop": true, "onlinemax": 16, "offlinecoopmax": 4, "splitscreen": true}],
            "release_dates": [{"platform": 12, "date": 1190000000, "release_region": {"region": "north_america"}}, {"platform": 6, "date": 1, "release_region": {"region": "europe"}}],
            "videos": [{"video_id": "abc_-1", "name": "Trailer"}, {"video_id": "bad id\"", "name": "x"}],
            "websites": [{"url": "https://halo.example", "type": {"type": "Official Website"}}, {"url": "javascript:alert(1)", "type": {"type": "Official Website"}}],
            "dlcs": [{"id": 70, "name": "Map pack"}], "parent_game": {"id": 6, "name": "Base"},
            "similar_games": [11, 12, 13]});
        let d = parse_detail(&v).unwrap();
        assert_eq!(
            (d.game.name.as_str(), d.storyline.as_deref(), d.rating_count),
            ("Halo 3", Some("story"), Some(900))
        );
        assert_eq!(
            (d.screenshots.len(), d.modes.len(), d.similar_ids.clone()),
            (2, 2, vec![11, 12, 13])
        );
        assert_eq!(
            (
                d.series[0].id,
                d.franchises[0].name.as_str(),
                d.engines[0].id
            ),
            (9, "Halo", 3)
        );
        assert_eq!(
            (d.developers[0].name.as_str(), d.publishers[0].id),
            ("Bungie", 2)
        );
        assert_eq!(d.age_ratings[0].org, "ESRB", "ESRB first");
        assert_eq!(d.age_ratings[0].reasons, vec!["Blood"]);
        assert_eq!(d.languages[0].kinds, vec!["Audio", "Subtitles"]);
        let mp = d.multiplayer.unwrap();
        assert!(mp.online_coop && mp.split_screen && mp.offline_coop);
        assert_eq!(
            (mp.online_max, mp.offline_max),
            (16, 4),
            "this console's modes, not the PC's"
        );
        assert_eq!(d.releases.len(), 1, "only Xbox 360 release dates");
        assert_eq!(d.releases[0].region, "North America");
        assert_eq!(d.videos.len(), 1, "an id with odd characters is dropped");
        assert_eq!(d.sites.len(), 1, "only web links");
        let kinds: Vec<&str> = d.related.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(kinds, vec!["Base game", "Add-on"]);
        assert!(parse_detail(&serde_json::json!({"name": "no id"})).is_none());
    }

    #[test]
    fn the_mock_catalog_filters_sorts_and_pages() {
        let c = Client::new(super::super::Urls::default(), true);
        let creds = Credentials {
            client_id: "m".into(),
            client_secret: "m".into(),
        };
        let all = c
            .browse(
                &creds,
                &Browse {
                    page: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(all.len(), PAGE as usize);
        assert!(all[0].rating_count >= all[5].rating_count, "popular first");
        let page2 = c
            .browse(
                &creds,
                &Browse {
                    page: 2,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(all.len() + page2.len(), MOCK_GAMES.len());
        let race = c.browse(&creds, &with("genre", &[10])).unwrap();
        assert!(race.len() == 4 && race.iter().all(|g| g.game.genres == vec!["Racing"]));
        let mut y = Browse {
            year_from: Some(2010),
            year_to: Some(2011),
            page: 1,
            ..Default::default()
        };
        y.sort = Sort::Newest;
        let y = c.browse(&creds, &y).unwrap();
        assert!(
            !y.is_empty()
                && y.iter().all(|g| g.game.release.unwrap() >= jan1(2010)
                    && g.game.release.unwrap() < jan1(2012))
        );
        assert_eq!(
            c.browse(
                &creds,
                &Browse {
                    q: Some("gears".into()),
                    page: 1,
                    ..Default::default()
                }
            )
            .unwrap()
            .len(),
            3
        );
        // Facets combine, "all" narrows "any", and the counts agree with the lists.
        let mut horror_or_fantasy = with("theme", &[19, 17]);
        let any = c.count(&creds, &horror_or_fantasy).unwrap();
        horror_or_fantasy.all = vec!["theme".into()];
        let both = c.count(&creds, &horror_or_fantasy).unwrap();
        assert!(any > 0 && both <= any);
        let series = c.browse(&creds, &with("series", &[2])).unwrap();
        assert!(series.len() == 3 && series.iter().all(|g| g.game.name.starts_with("Gears")));
        let mut coop = Browse {
            page: 1,
            ..Default::default()
        };
        coop.local_coop = true;
        assert!(!c.browse(&creds, &coop).unwrap().is_empty());
        let mut hide = with("series", &[2]);
        hide.exclude = vec![series[0].game.id];
        assert_eq!(c.count(&creds, &hide).unwrap(), 2);
        let counts = c.facet_counts(&creds, "theme").unwrap();
        assert_eq!(counts[&19], c.count(&creds, &with("theme", &[19])).unwrap());
        assert!(c.facet_counts(&creds, "nonsense").is_err());
        assert!(!c.suggest(&creds, "series", "gea").unwrap().is_empty());
        assert!(c.random(&creds, &with("genre", &[10])).unwrap().is_some());
        // A game's page and its similar games.
        let d = c.detail(&creds, all[0].game.id).unwrap().unwrap();
        let similar = c.xbox_games_among(&creds, &d.similar_ids).unwrap();
        assert!(!similar.is_empty() && similar.iter().all(|g| g.game.genres == d.game.genres));
        assert!(c.genres(&creds).unwrap().len() >= 5);
        assert!(
            c.image_url("t_cover_big", "co1")
                .unwrap()
                .starts_with("data:image/svg+xml")
        );
        assert!(c.image_url("t_cover_big", "../x").is_none());
    }
}
