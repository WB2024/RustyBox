//! Discover: browse IGDB's Xbox 360 games, see a game's details and similar games, and send it to
//! the wanted list or straight to a Usenet download.

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S, grabber};
use crate::{
    error::Error,
    grabber::store,
    igdb::discover::{Browse, Card, Sort},
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/discover", get(browse))
        .route("/api/discover/genres", get(genres))
        .route("/api/discover/random", get(random))
        .route("/api/discover/facets", get(facets))
        .route("/api/discover/facets/{kind}/counts", get(facet_counts))
        .route("/api/discover/suggest", get(suggest))
        .route("/api/discover/{id}/more", get(more))
        .route("/api/discover/{id}", get(detail))
        .route("/api/discover/{id}/grab", post(grab))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::backend(e.to_string()))?
}

/// Where each game stands: in a library, being downloaded, on the wanted list, or none of those.
async fn states(st: &AppState, ids: &[i64]) -> Result<HashMap<i64, Value>, Error> {
    let ids = ids.to_vec();
    st.db
        .run(move |c| {
            let wanted = store::list_wanted(c)?;
            let grabs = store::list_grabs(c, 500)?;
            let (have, _) = store::owned(c)?;
            let mut out = HashMap::new();
            for id in ids {
                let w = wanted.iter().find(|w| w.igdb_id == Some(id));
                let grab = w.and_then(|w| grabs.iter().find(|g| g.wanted_id == w.id));
                let live = grab.filter(|g| matches!(g.status.as_str(), "queued" | "downloading" | "completed" | "importing"));
                let state = if have.contains(&id) {
                    "library"
                } else if live.is_some() {
                    "downloading"
                } else if w.is_some() {
                    "wanted"
                } else {
                    "none"
                };
                out.insert(id, json!({"state": state, "wanted_id": w.map(|w| w.id), "progress": live.map(|g| g.progress), "grab_status": live.map(|g| g.status.clone())}));
            }
            Ok(out)
        })
        .await
}

fn card(c: &Card, st: &AppState, state: Option<&Value>) -> Value {
    let g = &c.game;
    json!({
        "id": g.id, "name": g.name, "year": g.release, "genres": g.genres, "rating": g.rating,
        "developers": g.developers, "summary": g.summary,
        "cover": g.cover.as_ref().and_then(|i| st.igdb.image_url("t_cover_big", i)),
        "status": state,
        "modes": c.modes, "themes": c.themes, "rating_count": c.rating_count, "critics": c.critics,
    })
}

/// Every filter the browser can send. Lists are comma-separated IGDB ids.
#[derive(Deserialize, Default)]
struct BrowseQuery {
    q: Option<String>,
    genre: Option<String>,
    theme: Option<String>,
    mode: Option<String>,
    persp: Option<String>,
    series: Option<String>,
    franchise: Option<String>,
    company: Option<String>,
    engine: Option<String>,
    keyword: Option<String>,
    age: Option<String>,
    /// Facet names (`theme,mode`) where a game must have every chosen value.
    all: Option<String>,
    /// A decade or any span: `2005-2009`, or a single year.
    years: Option<String>,
    min_rating: Option<i32>,
    known: Option<String>,
    max_votes: Option<i32>,
    online_coop: Option<String>,
    local_coop: Option<String>,
    online: Option<String>,
    /// `owned`, `wanted` or both: leave out games you already have or want.
    hide: Option<String>,
    sort: Option<Sort>,
    page: Option<u32>,
}

fn years(s: &str) -> Option<(i32, i32)> {
    let (a, b) = s.split_once('-').unwrap_or((s, s));
    let (a, b): (i32, i32) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    ((1990..=2030).contains(&a) && (1990..=2030).contains(&b) && a <= b).then_some((a, b))
}

fn id_list(s: &Option<String>) -> Vec<i64> {
    s.as_deref()
        .unwrap_or("")
        .split(',')
        .filter_map(|x| x.trim().parse::<i64>().ok())
        .filter(|x| *x > 0)
        .take(40)
        .collect()
}

fn flag(s: &Option<String>) -> bool {
    matches!(s.as_deref(), Some("1" | "true" | "on"))
}

async fn to_browse(st: &AppState, q: BrowseQuery) -> Result<Browse, Error> {
    let (from, to) = q
        .years
        .as_deref()
        .and_then(years)
        .map(|(a, b)| (Some(a), Some(b)))
        .unwrap_or((None, None));
    let mut facets = std::collections::BTreeMap::new();
    for (kind, list) in [
        ("genre", &q.genre),
        ("theme", &q.theme),
        ("mode", &q.mode),
        ("persp", &q.persp),
        ("series", &q.series),
        ("franchise", &q.franchise),
        ("company", &q.company),
        ("engine", &q.engine),
        ("keyword", &q.keyword),
        ("age", &q.age),
    ] {
        let v = id_list(list);
        if !v.is_empty() {
            facets.insert(kind.to_string(), v);
        }
    }
    let hide = q.hide.clone().unwrap_or_default();
    let exclude = if hide.is_empty() {
        vec![]
    } else {
        let (owned, wanted) = (hide.contains("owned"), hide.contains("wanted"));
        st.db
            .run(move |c| {
                let mut ids: Vec<i64> = vec![];
                if owned {
                    ids.extend(store::owned(c)?.0);
                }
                if wanted {
                    ids.extend(store::list_wanted(c)?.iter().filter_map(|w| w.igdb_id));
                }
                ids.sort_unstable();
                ids.dedup();
                Ok(ids)
            })
            .await?
    };
    Ok(Browse {
        q: q.q.filter(|s| !s.trim().is_empty()),
        facets,
        all: q
            .all
            .unwrap_or_default()
            .split(',')
            .filter(|k| crate::igdb::discover::facet_field(k).is_some())
            .map(String::from)
            .collect(),
        year_from: from,
        year_to: to,
        min_rating: q.min_rating.filter(|r| (1..=100).contains(r)),
        known: flag(&q.known),
        max_votes: q.max_votes.filter(|m| *m > 0),
        online_coop: flag(&q.online_coop),
        local_coop: flag(&q.local_coop),
        online: flag(&q.online),
        exclude,
        sort: q.sort.unwrap_or_default(),
        page: q.page.unwrap_or(1).clamp(1, 200),
    })
}

async fn browse(State(st): S, Query(q): Query<BrowseQuery>) -> ApiResult<Json<Value>> {
    let creds = grabber::igdb_creds(&st)?;
    let b = to_browse(&st, q).await?;
    let (st2, b2, c2) = (st.clone(), b.clone(), creds.clone());
    let games = blocking(move || st2.igdb.browse(&c2, &b2)).await?;
    // The size of the whole result, asked once per new filter (the first page).
    let total = if b.page == 1 {
        let (st3, b3) = (st.clone(), b.clone());
        blocking(move || st3.igdb.count(&creds, &b3)).await.ok()
    } else {
        None
    };
    let ids: Vec<i64> = games.iter().map(|g| g.game.id).collect();
    let state = states(&st, &ids).await?;
    Ok(Json(json!({
        "games": games.iter().map(|g| card(g, &st, state.get(&g.game.id))).collect::<Vec<_>>(),
        "page": b.page,
        "has_more": games.len() as u32 == crate::igdb::discover::PAGE,
        "relevance": b.q.is_some(),
        "total": total,
    })))
}

/// One random game that fits the filters (the "surprise me" button).
async fn random(State(st): S, Query(q): Query<BrowseQuery>) -> ApiResult<Json<Value>> {
    let creds = grabber::igdb_creds(&st)?;
    let mut b = to_browse(&st, q).await?;
    b.q = None;
    let st2 = st.clone();
    let pick = blocking(move || st2.igdb.random(&creds, &b)).await?;
    Ok(Json(json!({"id": pick.map(|c| c.game.id)})))
}

async fn genres(State(st): S) -> ApiResult<Json<Value>> {
    let creds = grabber::igdb_creds(&st)?;
    let st2 = st.clone();
    let g = blocking(move || st2.igdb.genres(&creds)).await?;
    Ok(Json(
        json!({"genres": g.iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>()}),
    ))
}

/// The fixed filter lists: genres, themes, game modes, perspectives, age ratings.
async fn facets(State(st): S) -> ApiResult<Json<Value>> {
    let creds = grabber::igdb_creds(&st)?;
    let st2 = st.clone();
    let f = blocking(move || st2.igdb.facets(&creds)).await?;
    Ok(Json(json!(f)))
}

/// How many Xbox 360 games each value of one list has. Slow the first time, then cached.
async fn facet_counts(State(st): S, UrlPath(kind): UrlPath<String>) -> ApiResult<Json<Value>> {
    let creds = grabber::igdb_creds(&st)?;
    let st2 = st.clone();
    let c = blocking(move || st2.igdb.facet_counts(&creds, &kind)).await?;
    Ok(Json(json!({"counts": c})))
}

#[derive(Deserialize)]
struct SuggestQuery {
    kind: String,
    q: String,
}

/// Names to pick from while typing (companies, series, franchises, engines, keywords).
async fn suggest(State(st): S, Query(q): Query<SuggestQuery>) -> ApiResult<Json<Value>> {
    let creds = grabber::igdb_creds(&st)?;
    let st2 = st.clone();
    let list = blocking(move || st2.igdb.suggest(&creds, &q.kind, &q.q)).await?;
    Ok(Json(json!({"results": list})))
}

fn images(st: &AppState, size: &str, ids: &[String]) -> Vec<String> {
    ids.iter()
        .filter_map(|i| st.igdb.image_url(size, i))
        .collect()
}

async fn detail(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    let creds = grabber::igdb_creds(&st)?;
    let (st2, c2) = (st.clone(), creds.clone());
    let d = blocking(move || st2.igdb.detail(&c2, id))
        .await?
        .ok_or_else(|| ApiError::not_found("IGDB doesn't know that game"))?;
    // Similar games, narrowed to ones that exist on the Xbox 360.
    let (st3, ids) = (st.clone(), d.similar_ids.clone());
    let similar = blocking(move || st3.igdb.xbox_games_among(&creds, &ids))
        .await
        .unwrap_or_default();
    let mut all_ids = vec![id];
    all_ids.extend(similar.iter().map(|g| g.game.id));
    let state = states(&st, &all_ids).await?;
    let me = Card {
        game: d.game.clone(),
        modes: d.modes.iter().map(|m| m.name.clone()).collect(),
        themes: d.themes.iter().map(|m| m.name.clone()).collect(),
        rating_count: d.rating_count,
        critics: d.critics,
    };
    let mut v = json!(d);
    v["game"] = card(&me, &st, state.get(&id));
    v["publisher_names"] = json!(d.game.publishers);
    v["url"] = json!(d.game.url);
    v["screenshots"] = json!(images(&st, "t_screenshot_big", &d.screenshots));
    v["artworks"] = json!(images(&st, "t_screenshot_big", &d.artworks));
    v["similar"] = json!(
        similar
            .iter()
            .map(|g| card(g, &st, state.get(&g.game.id)))
            .collect::<Vec<_>>()
    );
    Ok(Json(v))
}

#[derive(Deserialize)]
struct MoreQuery {
    series: Option<i64>,
    franchise: Option<i64>,
    company: Option<i64>,
    series_name: Option<String>,
    franchise_name: Option<String>,
    company_name: Option<String>,
}

/// Other games from the same series, franchise and developer, as shelves under a game.
async fn more(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Query(q): Query<MoreQuery>,
) -> ApiResult<Json<Value>> {
    let creds = grabber::igdb_creds(&st)?;
    let mut shelves = Vec::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::from([id]);
    for (kind, fid, label) in [
        ("series", q.series, q.series_name.clone()),
        ("franchise", q.franchise, q.franchise_name.clone()),
        ("company", q.company, q.company_name.clone()),
    ] {
        let Some(fid) = fid else { continue };
        let mut b = Browse {
            page: 1,
            exclude: vec![id],
            ..Default::default()
        };
        b.facets.insert(kind.into(), vec![fid]);
        let (st2, c2) = (st.clone(), creds.clone());
        let games = blocking(move || st2.igdb.browse(&c2, &b))
            .await
            .unwrap_or_default();
        let games: Vec<Card> = games
            .into_iter()
            .filter(|g| seen.insert(g.game.id))
            .take(12)
            .collect();
        if games.is_empty() {
            continue;
        }
        let ids: Vec<i64> = games.iter().map(|g| g.game.id).collect();
        let state = states(&st, &ids).await?;
        shelves.push(json!({
            "kind": kind, "id": fid,
            "name": label.unwrap_or_default().chars().take(80).collect::<String>(),
            "games": games.iter().map(|g| card(g, &st, state.get(&g.game.id))).collect::<Vec<_>>(),
        }));
    }
    Ok(Json(json!({"shelves": shelves})))
}

#[derive(Deserialize)]
struct GrabReq {
    /// `add` puts it on the wanted list; `best` also searches and sends the best release to SABnzbd.
    mode: String,
}

async fn grab(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Json(r): Json<GrabReq>,
) -> ApiResult<Json<Value>> {
    if !["add", "best"].contains(&r.mode.as_str()) {
        return Err(ApiError::bad("mode is add or best"));
    }
    let creds = grabber::igdb_creds(&st)?;
    let existing = st
        .db
        .run(move |c| {
            Ok(store::list_wanted(c)?
                .into_iter()
                .find(|w| w.igdb_id == Some(id))
                .map(|w| w.id))
        })
        .await?;
    let (wanted_id, added) = match existing {
        Some(w) => (w, false),
        None => {
            let st2 = st.clone();
            let cand = blocking(move || {
                st2.igdb
                    .by_id(&creds, id)?
                    .ok_or_else(|| Error::not_found("IGDB doesn't know that game"))
            })
            .await?;
            (grabber::add_candidate(&st, cand).await?, true)
        }
    };
    if r.mode == "add" {
        return Ok(Json(
            json!({"wanted_id": wanted_id, "added": added, "grabbed": null}),
        ));
    }
    let grabbed = grabber::auto_grab(&st, wanted_id).await?;
    Ok(Json(
        json!({"wanted_id": wanted_id, "added": added, "grabbed": grabbed}),
    ))
}
