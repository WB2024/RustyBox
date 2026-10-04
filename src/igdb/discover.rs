//! Browsing IGDB's Xbox 360 games: filters, sorting, a game's details, and similar games.

use serde::Serialize;
use serde_json::Value;

use super::{
    Candidate, Client, Credentials, GAME_TYPES, XBOX_360, mock_cover_svg, parse_candidate,
};
use crate::error::Error;

pub const PAGE: u32 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    /// Most rated (the games most people know).
    #[default]
    Popular,
    Rating,
    Newest,
    Oldest,
    Name,
}

#[derive(Debug, Clone, Default)]
pub struct Browse {
    /// Search text. IGDB can't sort a search, so a search is ranked by relevance.
    pub q: Option<String>,
    pub genre: Option<i64>,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub sort: Sort,
    pub page: u32,
}

/// Seconds since the epoch for 1 January of `year`.
fn jan1(year: i32) -> i64 {
    let y = (year - 1) as i64;
    let days = y * 365 + y / 4 - y / 100 + y / 400 - 719_162;
    days * 86_400
}

/// The Apicalypse query for a browse (separate so it can be tested without IGDB).
pub fn browse_body(b: &Browse, now: i64) -> String {
    let mut w = vec![
        format!("platforms = ({XBOX_360})"),
        format!("game_type = ({GAME_TYPES})"),
        "cover != null".to_string(),
    ];
    if let Some(g) = b.genre {
        w.push(format!("genres = ({g})"));
    }
    if let Some(y) = b.year_from {
        w.push(format!("first_release_date >= {}", jan1(y)));
    }
    if let Some(y) = b.year_to {
        w.push(format!("first_release_date < {}", jan1(y + 1)));
    }
    let searching = b.q.as_ref().is_some_and(|q| !q.trim().is_empty());
    let sort = match b.sort {
        Sort::Popular => {
            w.push("total_rating_count >= 3".into());
            "sort total_rating_count desc;"
        }
        Sort::Rating => {
            w.push("total_rating_count >= 8".into());
            "sort total_rating desc;"
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
    }
    body.push_str(&format!(
        "fields {}; where {}; ",
        Client::FIELDS,
        w.join(" & ")
    ));
    if !searching {
        // IGDB refuses `sort` together with `search`, and a search has no popularity floor.
        body.push_str(sort);
        body.push(' ');
    } else {
        // The popularity floor would hide small games a person searched for by name.
        body = body
            .replace(" & total_rating_count >= 3", "")
            .replace(" & total_rating_count >= 8", "");
    }
    body.push_str(&format!(
        "limit {PAGE}; offset {};",
        b.page.saturating_sub(1) * PAGE
    ));
    body
}

/// More about one game.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Detail {
    pub game: Candidate,
    pub storyline: Option<String>,
    pub rating_count: Option<i64>,
    /// IGDB image IDs.
    pub screenshots: Vec<String>,
    pub modes: Vec<String>,
    pub perspectives: Vec<String>,
    pub similar_ids: Vec<i64>,
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

pub fn parse_detail(v: &Value) -> Option<Detail> {
    Some(Detail {
        game: parse_candidate(v)?,
        storyline: v["storyline"].as_str().map(String::from),
        rating_count: v["total_rating_count"].as_i64(),
        screenshots: v["screenshots"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s["image_id"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        modes: names(v, "game_modes"),
        perspectives: names(v, "player_perspectives"),
        similar_ids: v["similar_games"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default(),
    })
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
    pub fn browse(&self, creds: &Credentials, b: &Browse) -> Result<Vec<Candidate>, Error> {
        if self.is_mock() {
            return Ok(mock_browse(b));
        }
        let now = super::now() as i64;
        let v = self.query(creds, "games", &browse_body(b, now))?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(parse_candidate).collect())
            .unwrap_or_default())
    }

    /// The genres Xbox 360 games have (cached after the first call).
    pub fn genres(&self, creds: &Credentials) -> Result<Vec<(i64, String)>, Error> {
        if let Some(g) = self
            .genres
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Ok(g);
        }
        let list: Vec<(i64, String)> = if self.is_mock() {
            MOCK_GENRES
                .iter()
                .map(|(i, n)| (*i, n.to_string()))
                .collect()
        } else {
            let v = self.query(creds, "genres", "fields name; limit 60; sort name asc;")?;
            v.as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|g| Some((g["id"].as_i64()?, g["name"].as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default()
        };
        *self.genres.lock().unwrap_or_else(|e| e.into_inner()) = Some(list.clone());
        Ok(list)
    }

    /// Everything about one game.
    pub fn detail(&self, creds: &Credentials, id: i64) -> Result<Option<Detail>, Error> {
        if self.is_mock() {
            return Ok(mock_catalog()
                .into_iter()
                .find(|g| g.0.id == id)
                .map(|(game, _)| Detail {
                    storyline: Some(format!(
                        "The story of {}: a pretend storyline for mock mode.",
                        game.name
                    )),
                    rating_count: Some(120),
                    screenshots: vec![
                        format!("mockshot{}a", id % 100),
                        format!("mockshot{}b", id % 100),
                        format!("mockshot{}c", id % 100),
                    ],
                    modes: vec!["Single player".into(), "Co-operative".into()],
                    perspectives: vec!["Third person".into()],
                    similar_ids: mock_catalog()
                        .iter()
                        .filter(|g| g.0.id != id && g.0.genres == game.genres)
                        .take(8)
                        .map(|g| g.0.id)
                        .collect(),
                    game,
                }));
        }
        let body = format!(
            "fields {},storyline,total_rating_count,screenshots.image_id,game_modes.name,player_perspectives.name,similar_games; where id = {id};",
            Client::FIELDS
        );
        let v = self.query(creds, "games", &body)?;
        Ok(v.as_array().and_then(|a| a.first()).and_then(parse_detail))
    }

    /// Of these IGDB ids, the ones that are Xbox 360 games (similar games are often other platforms).
    pub fn xbox_games_among(
        &self,
        creds: &Credentials,
        ids: &[i64],
    ) -> Result<Vec<Candidate>, Error> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        if self.is_mock() {
            return Ok(mock_catalog()
                .into_iter()
                .filter(|g| ids.contains(&g.0.id))
                .map(|g| g.0)
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
            Client::FIELDS
        );
        let v = self.query(creds, "games", &body)?;
        Ok(v.as_array()
            .map(|a| a.iter().filter_map(parse_candidate).collect())
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

pub(crate) fn mock_catalog() -> Vec<(Candidate, i64)> {
    MOCK_GAMES
        .iter()
        .enumerate()
        .map(|(i, (name, genre, year, rating))| {
            let gname = MOCK_GENRES.iter().find(|g| g.0 == *genre).map(|g| g.1).unwrap_or("Action");
            (
                Candidate {
                    id: 700_000 + i as i64,
                    name: name.to_string(),
                    summary: Some(format!("A sample description of {name}, shown because RustyBox is in mock mode and not talking to IGDB.")),
                    release: Some(jan1(*year) + 100 * 86_400),
                    genres: vec![gname.to_string()],
                    developers: vec!["Sample Studio".into()],
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

fn mock_browse(b: &Browse) -> Vec<Candidate> {
    let q =
        b.q.as_ref()
            .map(|q| q.trim().to_lowercase())
            .filter(|q| !q.is_empty());
    let mut list: Vec<Candidate> = mock_catalog()
        .into_iter()
        .filter(|(g, gid)| {
            b.genre.is_none_or(|x| x == *gid)
                && q.as_ref().is_none_or(|q| g.name.to_lowercase().contains(q))
                && b.year_from
                    .is_none_or(|y| g.release.unwrap_or(0) >= jan1(y))
                && b.year_to
                    .is_none_or(|y| g.release.unwrap_or(0) < jan1(y + 1))
        })
        .map(|g| g.0)
        .collect();
    if q.is_none() {
        match b.sort {
            Sort::Popular | Sort::Rating => list.sort_by(|a, c| {
                c.rating
                    .partial_cmp(&a.rating)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            Sort::Newest => list.sort_by_key(|g| std::cmp::Reverse(g.release)),
            Sort::Oldest => list.sort_by_key(|g| g.release),
            Sort::Name => list.sort_by(|a, c| a.name.cmp(&c.name)),
        }
    }
    let skip = (b.page.saturating_sub(1) * PAGE) as usize;
    list.into_iter().skip(skip).take(PAGE as usize).collect()
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

    #[test]
    fn queries_filter_to_xbox_360_and_never_sort_a_search() {
        let b = Browse {
            genre: Some(5),
            year_from: Some(2007),
            year_to: Some(2009),
            sort: Sort::Rating,
            page: 3,
            ..Default::default()
        };
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
        assert!(
            q.contains("sort total_rating desc;")
                && q.contains("total_rating_count >= 8")
                && q.ends_with("limit 24; offset 48;"),
            "{q}"
        );
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
            !s.contains("sort ") && !s.contains("total_rating_count"),
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
    fn a_games_details_are_read_from_igdbs_answer() {
        let v: Value = serde_json::json!({"id": 7, "name": "Halo 3", "summary": "s", "storyline": "story", "total_rating_count": 900, "first_release_date": 1190000000,
            "cover": {"image_id": "co1"}, "genres": [{"name": "Shooter"}], "screenshots": [{"image_id": "sc1"}, {"image_id": "sc2"}],
            "game_modes": [{"name": "Single player"}, {"name": "Co-operative"}], "player_perspectives": [{"name": "First person"}], "similar_games": [11, 12, 13]});
        let d = parse_detail(&v).unwrap();
        assert_eq!(
            (d.game.name.as_str(), d.storyline.as_deref(), d.rating_count),
            ("Halo 3", Some("story"), Some(900))
        );
        assert_eq!(
            (d.screenshots.len(), d.modes.len(), d.similar_ids.clone()),
            (2, 2, vec![11, 12, 13])
        );
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
        assert!(all[0].rating >= all[5].rating, "popular first");
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
        let race = c
            .browse(
                &creds,
                &Browse {
                    genre: Some(10),
                    page: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(race.len() == 4 && race.iter().all(|g| g.genres == vec!["Racing"]));
        let y = c
            .browse(
                &creds,
                &Browse {
                    year_from: Some(2010),
                    year_to: Some(2011),
                    page: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            !y.is_empty()
                && y.iter()
                    .all(|g| g.release.unwrap() >= jan1(2010) && g.release.unwrap() < jan1(2012))
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
        let d = c.detail(&creds, all[0].id).unwrap().unwrap();
        let similar = c.xbox_games_among(&creds, &d.similar_ids).unwrap();
        assert!(!similar.is_empty() && similar.iter().all(|g| g.genres == d.game.genres));
        assert!(c.genres(&creds).unwrap().len() >= 5);
        assert!(
            c.image_url("t_cover_big", "co1")
                .unwrap()
                .starts_with("data:image/svg+xml")
        );
        assert!(c.image_url("t_cover_big", "../x").is_none());
    }
}
