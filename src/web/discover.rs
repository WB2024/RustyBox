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
    igdb::{
        Candidate,
        discover::{Browse, Sort},
    },
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/discover", get(browse))
        .route("/api/discover/genres", get(genres))
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

fn card(c: &Candidate, st: &AppState, state: Option<&Value>) -> Value {
    json!({
        "id": c.id, "name": c.name, "year": c.release, "genres": c.genres, "rating": c.rating,
        "developers": c.developers, "summary": c.summary,
        "cover": c.cover.as_ref().and_then(|i| st.igdb.image_url("t_cover_big", i)),
        "status": state,
    })
}

#[derive(Deserialize)]
struct BrowseQuery {
    q: Option<String>,
    genre: Option<i64>,
    /// A decade or any span: `2005-2009`, or a single year.
    years: Option<String>,
    sort: Option<Sort>,
    page: Option<u32>,
}

fn years(s: &str) -> Option<(i32, i32)> {
    let (a, b) = s.split_once('-').unwrap_or((s, s));
    let (a, b): (i32, i32) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    ((1990..=2030).contains(&a) && (1990..=2030).contains(&b) && a <= b).then_some((a, b))
}

async fn browse(State(st): S, Query(q): Query<BrowseQuery>) -> ApiResult<Json<Value>> {
    let creds = grabber::igdb_creds(&st)?;
    let (from, to) = q
        .years
        .as_deref()
        .and_then(years)
        .map(|(a, b)| (Some(a), Some(b)))
        .unwrap_or((None, None));
    let b = Browse {
        q: q.q.filter(|s| !s.trim().is_empty()),
        genre: q.genre,
        year_from: from,
        year_to: to,
        sort: q.sort.unwrap_or_default(),
        page: q.page.unwrap_or(1).clamp(1, 200),
    };
    let (st2, b2) = (st.clone(), b.clone());
    let games = blocking(move || st2.igdb.browse(&creds, &b2)).await?;
    let ids: Vec<i64> = games.iter().map(|g| g.id).collect();
    let state = states(&st, &ids).await?;
    Ok(Json(json!({
        "games": games.iter().map(|g| card(g, &st, state.get(&g.id))).collect::<Vec<_>>(),
        "page": b.page,
        "has_more": games.len() as u32 == crate::igdb::discover::PAGE,
        "relevance": b.q.is_some(),
    })))
}

async fn genres(State(st): S) -> ApiResult<Json<Value>> {
    let creds = grabber::igdb_creds(&st)?;
    let st2 = st.clone();
    let g = blocking(move || st2.igdb.genres(&creds)).await?;
    Ok(Json(
        json!({"genres": g.iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>()}),
    ))
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
    all_ids.extend(similar.iter().map(|g| g.id));
    let state = states(&st, &all_ids).await?;
    Ok(Json(json!({
        "game": card(&d.game, &st, state.get(&id)),
        "storyline": d.storyline,
        "rating_count": d.rating_count,
        "publishers": d.game.publishers,
        "url": d.game.url,
        "modes": d.modes,
        "perspectives": d.perspectives,
        "screenshots": d.screenshots.iter().filter_map(|i| st.igdb.image_url("t_screenshot_big", i)).collect::<Vec<_>>(),
        "similar": similar.iter().map(|g| card(g, &st, state.get(&g.id))).collect::<Vec<_>>(),
    })))
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
