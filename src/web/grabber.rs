//! The wanted list, indexers, SABnzbd, searching and grabbing, tracking downloads, and importing
//! what finishes. Radarr-style, for Xbox 360 games, with IGDB as the metadata.

use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query, State},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, ApiResult, AppState, S};
use crate::{
    error::Error,
    extras::notify,
    grabber::{
        config::Config,
        engine::{self, Judged},
        newznab::{self, Indexer},
        qbit::{AddSource, Phase, Qbit},
        release,
        sab::Sab,
        store::{self, NewGrab, NewWanted, Wanted},
        torrent::{self, Fetched},
        unpack,
    },
    igdb::{self, store as igdb_store},
    jobs::now,
    library,
    transfer::plan::{self as tplan, Mode, Request, Source},
};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/grab/config", get(get_config).put(put_config))
        .route("/api/grab/indexers", post(add_indexer))
        .route("/api/grab/indexers/test", post(test_indexer))
        .route(
            "/api/grab/indexers/{id}",
            put(update_indexer).delete(delete_indexer),
        )
        .route("/api/grab/prowlarr", post(import_prowlarr))
        .route("/api/grab/sab/test", post(test_sab))
        .route("/api/grab/sab/category", post(make_category))
        .route("/api/grab/qbit/test", post(test_qbit))
        .route("/api/grab/qbit/category", post(make_qbit_category))
        .route("/api/wanted", get(list_wanted).post(add_wanted))
        .route("/api/wanted/lookup", get(lookup))
        .route("/api/wanted/search-all", post(search_all))
        .route("/api/wanted/{id}", put(update_wanted).delete(remove_wanted))
        .route("/api/wanted/{id}/search", post(search_one))
        .route("/api/wanted/{id}/grab", post(grab_one))
        .route("/api/wanted/{id}/auto", post(auto_one))
        .route("/api/grab/activity", get(activity))
        .route(
            "/api/grab/activity/{id}",
            axum::routing::delete(remove_grab),
        )
        .route("/api/grab/activity/{id}/import", post(retry_import))
        .route("/api/grab/blocklist", get(get_blocklist))
        .route("/api/grab/blocklist/{id}", axum::routing::delete(unblock))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::backend(e.to_string()))?
}

// ── Configuration ────────────────────────────────────────────────────────────

async fn get_config(State(st): S) -> Json<Value> {
    Json(st.grabber.get().public())
}

/// Everything except indexers (those have their own routes) and secrets that are left blank.
#[derive(Deserialize)]
struct ConfigReq {
    profile: Option<release::Profile>,
    path_maps: Option<Vec<crate::grabber::config::PathMap>>,
    search_every_hours: Option<u32>,
    auto_grab: Option<bool>,
    auto_import: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    import_library: Option<Option<(i64, i64)>>,
    #[serde(default, deserialize_with = "double_option")]
    also_drive: Option<Option<(i64, i64)>>,
    #[serde(default, deserialize_with = "double_option")]
    also_console: Option<Option<crate::grabber::config::AlsoConsole>>,
    import_mode: Option<String>,
    convert_iso: Option<bool>,
    sab: Option<SabReq>,
    qbit: Option<QbitReq>,
    torrent_dirs: Option<Vec<String>>,
    torrent_import_mode: Option<String>,
    torrent_after_import: Option<String>,
}

#[derive(Deserialize)]
struct QbitReq {
    url: Option<String>,
    username: Option<String>,
    /// Blank or missing keeps the saved password.
    password: Option<String>,
    category: Option<String>,
}

/// Tell "not given" (None) from an explicit null (Some(None)), so a setting can be cleared.
fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

#[derive(Deserialize)]
struct SabReq {
    url: Option<String>,
    /// Blank or missing keeps the saved key.
    api_key: Option<String>,
    category: Option<String>,
}

async fn put_config(State(st): S, Json(r): Json<ConfigReq>) -> ApiResult<Json<Value>> {
    if let Some((lib, pid)) = r.import_library.flatten() {
        let lp = st.db.run(move |c| library::get_path(c, lib, pid)).await?;
        if !lp.writable {
            return Err(ApiError::bad(
                "Finished games can only go to a writable library folder",
            ));
        }
    }
    if let Some((lib, pid)) = r.also_drive.flatten() {
        let lp = st.db.run(move |c| library::get_path(c, lib, pid)).await?;
        if !lp.writable {
            return Err(ApiError::bad(
                "Games can only be copied on to a writable folder",
            ));
        }
    }
    if let Some(ac) = r.also_console.clone().flatten() {
        st.consoles.get(ac.console_id)?;
    }
    st.grabber.update(|c| {
        if let Some(d) = r.also_drive {
            c.also_drive = d;
        }
        if let Some(d) = r.also_console {
            c.also_console = d;
        }
        if let Some(p) = r.profile {
            c.profile = p;
        }
        if let Some(m) = r.path_maps {
            c.path_maps = m;
        }
        if let Some(h) = r.search_every_hours {
            c.search_every_hours = h;
        }
        if let Some(a) = r.auto_grab {
            c.auto_grab = a;
        }
        if let Some(a) = r.auto_import {
            c.auto_import = a;
        }
        if let Some(l) = r.import_library {
            c.import_library = l;
        }
        if let Some(m) = r.import_mode {
            c.import_mode = m;
        }
        if let Some(v) = r.convert_iso {
            c.convert_iso = v;
        }
        if let Some(q) = r.qbit {
            if let Some(u) = q.url {
                c.qbit.url = u;
            }
            if let Some(u) = q.username {
                c.qbit.username = u;
            }
            if let Some(p) = q.password.filter(|p| !p.is_empty()) {
                c.qbit.password = p;
            }
            if let Some(cat) = q.category {
                c.qbit.category = cat;
            }
        }
        if let Some(d) = r.torrent_dirs {
            c.torrent_dirs = d;
        }
        if let Some(m) = r.torrent_import_mode {
            c.torrent_import_mode = m;
        }
        if let Some(m) = r.torrent_after_import {
            c.torrent_after_import = m;
        }
        if let Some(s) = r.sab {
            if let Some(u) = s.url {
                c.sab.url = u;
            }
            if let Some(k) = s.api_key.filter(|k| !k.trim().is_empty()) {
                c.sab.api_key = k.trim().to_string();
            }
            if let Some(cat) = s.category {
                c.sab.category = cat;
            }
        }
        Ok(())
    })?;
    Ok(Json(st.grabber.get().public()))
}

// ── Indexers ─────────────────────────────────────────────────────────────────

async fn add_indexer(State(st): S, Json(ix): Json<Indexer>) -> ApiResult<Json<Value>> {
    let ix = ix.validate()?;
    if ix.api_key.trim().is_empty() {
        return Err(ApiError::bad("Enter the indexer's API key"));
    }
    let added = st.grabber.update(|c| {
        let id = c.indexers.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        if c.indexers
            .iter()
            .any(|i| i.name.eq_ignore_ascii_case(&ix.name))
        {
            return Err(Error::conflict(format!(
                "There is already an indexer called {}",
                ix.name
            )));
        }
        let ix = Indexer { id, ..ix };
        c.indexers.push(ix.clone());
        Ok(ix)
    })?;
    Ok(Json(added.public()))
}

async fn update_indexer(
    State(st): S,
    UrlPath(id): UrlPath<u32>,
    Json(ix): Json<Indexer>,
) -> ApiResult<Json<Value>> {
    let done = st.grabber.update(|c| {
        let slot = c
            .indexers
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or_else(|| Error::not_found(format!("No indexer #{id}")))?;
        let key = if ix.api_key.trim().is_empty() {
            slot.api_key.clone()
        } else {
            ix.api_key.clone()
        };
        let next = Indexer {
            id,
            api_key: key,
            ..ix
        }
        .validate()?;
        *slot = next.clone();
        Ok(next)
    })?;
    Ok(Json(done.public()))
}

async fn delete_indexer(State(st): S, UrlPath(id): UrlPath<u32>) -> ApiResult<Json<Value>> {
    st.grabber.update(|c| {
        let n = c.indexers.len();
        c.indexers.retain(|i| i.id != id);
        if c.indexers.len() == n {
            Err(Error::not_found(format!("No indexer #{id}")))
        } else {
            Ok(())
        }
    })?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct TestIndexer {
    /// A saved indexer (its stored key is used if none is typed)...
    id: Option<u32>,
    #[serde(flatten)]
    ix: Indexer,
}

/// Check an indexer: does it answer, accept the key, and carry Xbox 360 games? A saved indexer
/// remembers the answer.
async fn test_indexer(State(st): S, Json(r): Json<TestIndexer>) -> ApiResult<Json<Value>> {
    let mut ix = r.ix;
    if let Some(id) = r.id {
        let saved = st
            .grabber
            .get()
            .indexers
            .into_iter()
            .find(|i| i.id == id)
            .ok_or_else(|| Error::not_found(format!("No indexer #{id}")))?;
        if ix.api_key.trim().is_empty() {
            ix.api_key = saved.api_key;
        }
        if ix.name.trim().is_empty() {
            ix.name = saved.name;
        }
        if ix.url.trim().is_empty() {
            ix.url = saved.url;
            ix.api_path = saved.api_path;
        }
    }
    let ix = ix.validate()?;
    let probe = ix.clone();
    let caps = blocking(move || newznab::caps(&probe)).await?;
    if let Some(id) = r.id {
        let has = caps.has_xbox360;
        let _ = st.grabber.update(|c| {
            if let Some(i) = c.indexers.iter_mut().find(|i| i.id == id) {
                i.has_xbox360 = Some(has);
            }
            Ok(())
        });
    }
    Ok(Json(
        json!({"ok": true, "has_xbox360": caps.has_xbox360, "categories": caps.categories}),
    ))
}

#[derive(Deserialize)]
struct ProwlarrReq {
    url: String,
    api_key: String,
}

/// Add Prowlarr's usenet and torrent indexers. Each is searched through Prowlarr's own Newznab or
/// Torznab address for it (`<prowlarr>/<id>/api`), so only Prowlarr's key is needed, not each indexer's.
async fn import_prowlarr(State(st): S, Json(r): Json<ProwlarrReq>) -> ApiResult<Json<Value>> {
    let url = r.url.trim().trim_end_matches('/').to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(ApiError::bad(
            "Prowlarr's address must look like http://192.168.1.110:9696",
        ));
    }
    let key = r.api_key.trim().to_string();
    let (u2, k2) = (url.clone(), key.clone());
    let list: Vec<(u32, String, String)> = blocking(move || {
        let v: Value = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(8))
            .timeout_read(Duration::from_secs(30))
            .build()
            .get(&format!("{u2}/api/v1/indexer"))
            .set("X-Api-Key", &k2)
            .call()
            .map_err(|e| match e {
                ureq::Error::Status(401 | 403, _) => Error::coded(
                    401,
                    "PROWLARR_LOGIN",
                    "Prowlarr refused that API key (Settings → General in Prowlarr)",
                ),
                ureq::Error::Status(c, _) => Error::backend(format!("Prowlarr answered {c}")),
                ureq::Error::Transport(t) => Error::coded(
                    502,
                    "PROWLARR_UNREACHABLE",
                    format!("Can't reach Prowlarr: {t}"),
                ),
            })?
            .into_json()
            .map_err(|e| Error::backend(format!("Unexpected answer from Prowlarr: {e}")))?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter(|i| {
                (i["protocol"] == "usenet" || i["protocol"] == "torrent") && i["enable"] == true
            })
            .filter_map(|i| {
                Some((
                    i["id"].as_u64()? as u32,
                    i["name"].as_str()?.to_string(),
                    i["protocol"].as_str()?.to_string(),
                ))
            })
            .collect())
    })
    .await?;
    let mut added = Vec::new();
    st.grabber.update(|c| {
        for (pid, name, protocol) in &list {
            let full = format!("{name} (via Prowlarr)");
            if c.indexers
                .iter()
                .any(|i| i.name.eq_ignore_ascii_case(&full))
            {
                continue;
            }
            let id = c.indexers.iter().map(|i| i.id).max().unwrap_or(0) + 1;
            c.indexers.push(Indexer {
                id,
                name: full.clone(),
                url: format!("{url}/{pid}"),
                api_key: key.clone(),
                protocol: protocol.clone(),
                ..Default::default()
            });
            added.push(full);
        }
        Ok(())
    })?;
    // Find out which of them carry Xbox 360 games (in the background of this request).
    let ixs = st.grabber.get().indexers;
    let checked: Vec<(u32, Option<bool>)> = blocking(move || {
        Ok(std::thread::scope(|s| {
            let hs: Vec<_> = ixs
                .iter()
                .filter(|i| i.has_xbox360.is_none())
                .map(|i| s.spawn(move || (i.id, newznab::caps(i).ok().map(|c| c.has_xbox360))))
                .collect();
            hs.into_iter().filter_map(|h| h.join().ok()).collect()
        }))
    })
    .await?;
    st.grabber.update(|c| {
        for (id, has) in checked {
            if let Some(i) = c.indexers.iter_mut().find(|i| i.id == id) {
                i.has_xbox360 = has.or(i.has_xbox360);
            }
        }
        Ok(())
    })?;
    Ok(Json(
        json!({"added": added, "indexers": st.grabber.get().public()["indexers"]}),
    ))
}

// ── SABnzbd ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TestSab {
    url: Option<String>,
    api_key: Option<String>,
}

fn sab_from(st: &AppState, r: &TestSab) -> Sab {
    let saved = st.grabber.get().sab;
    Sab {
        url: r
            .url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or(saved.url.clone()),
        api_key: r
            .api_key
            .clone()
            .filter(|k| !k.trim().is_empty())
            .unwrap_or(saved.api_key.clone()),
        category: saved.category,
    }
}

async fn test_sab(State(st): S, Json(r): Json<TestSab>) -> ApiResult<Json<Value>> {
    let sab = sab_from(&st, &r).validate()?;
    if !sab.configured() {
        return Err(ApiError::bad("Enter SABnzbd's address and API key"));
    }
    let (version, cats) = blocking(move || Ok((sab.version()?, sab.categories()?))).await?;
    let want = st.grabber.get().sab.category;
    Ok(Json(
        json!({"ok": true, "version": version, "categories": cats, "category_exists": want.is_empty() || cats.contains(&want)}),
    ))
}

#[derive(Deserialize)]
struct CatReq {
    name: String,
}

/// Make the category in SABnzbd (so downloads get their own folder).
async fn make_category(State(st): S, Json(r): Json<CatReq>) -> ApiResult<Json<Value>> {
    let sab = st.grabber.get().sab;
    if !sab.configured() {
        return Err(ApiError::bad("Set up SABnzbd first"));
    }
    let name = r.name.trim().to_string();
    if name.is_empty()
        || name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    {
        return Err(ApiError::bad(
            "A category name is letters, digits, dashes and underscores",
        ));
    }
    blocking(move || sab.create_category(&name)).await?;
    Ok(Json(json!({"ok": true})))
}

// ── qBittorrent ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TestQbit {
    url: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

fn qbit_from(st: &AppState, r: &TestQbit) -> Qbit {
    let saved = st.grabber.get().qbit;
    Qbit {
        url: r
            .url
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or(saved.url.clone()),
        username: r.username.clone().unwrap_or(saved.username.clone()),
        password: r
            .password
            .clone()
            .filter(|p| !p.is_empty())
            .unwrap_or(saved.password.clone()),
        category: saved.category,
    }
}

async fn test_qbit(State(st): S, Json(r): Json<TestQbit>) -> ApiResult<Json<Value>> {
    let q = qbit_from(&st, &r).validate()?;
    if !q.configured() {
        return Err(ApiError::bad("Enter qBittorrent's address"));
    }
    let (version, cats) = blocking(move || Ok((q.version()?, q.categories()?))).await?;
    let want = st.grabber.get().qbit.category;
    Ok(Json(
        json!({"ok": true, "version": version, "categories": cats, "category_exists": want.is_empty() || cats.contains(&want)}),
    ))
}

/// Make the category in qBittorrent (so downloads are grouped under it).
async fn make_qbit_category(State(st): S, Json(r): Json<CatReq>) -> ApiResult<Json<Value>> {
    let q = st.grabber.get().qbit;
    if !q.configured() {
        return Err(ApiError::bad("Set up qBittorrent first"));
    }
    let name = r.name.trim().to_string();
    if name.is_empty()
        || name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    {
        return Err(ApiError::bad(
            "A category name is letters, digits, dashes and underscores",
        ));
    }
    blocking(move || q.create_category(&name)).await?;
    Ok(Json(json!({"ok": true})))
}

// ── The wanted list ──────────────────────────────────────────────────────────

fn names_for(w: &Wanted) -> Vec<String> {
    igdb::rank::variants(&w.name)
}

async fn list_wanted(State(st): S) -> ApiResult<Json<Value>> {
    let (wanted, grabs, (igdb_have, tid_have)) = st
        .db
        .run(|c| {
            Ok((
                store::list_wanted(c)?,
                store::list_grabs(c, 500)?,
                store::owned(c)?,
            ))
        })
        .await?;
    let cfg = st.grabber.get();
    let items: Vec<Value> = wanted
        .iter()
        .map(|w| {
            let have = w.igdb_id.is_some_and(|i| igdb_have.contains(&i))
                || w.title_id
                    .as_deref()
                    .is_some_and(|t| tid_have.contains(&t.to_uppercase()));
            let grab = grabs.iter().find(|g| g.wanted_id == w.id);
            json!({"wanted": w, "have": have, "grab": grab})
        })
        .collect();
    Ok(Json(json!({
        "items": items,
        "ready": (cfg.sab.configured() || cfg.qbit.configured()) && !engine::usable(&cfg).is_empty(),
        "indexers": engine::usable(&cfg).len(),
        "sab": cfg.sab.configured(),
        "qbit": cfg.qbit.configured(),
    })))
}

#[derive(Deserialize)]
struct LookupQuery {
    q: String,
}

pub(super) fn igdb_creds(st: &AppState) -> Result<igdb::Credentials, ApiError> {
    if st.igdb.is_mock() {
        return Ok(igdb::Credentials {
            client_id: "mock".into(),
            client_secret: "mock".into(),
        });
    }
    super::igdb::resolve(st).map(|(c, _)| c).ok_or_else(|| {
        ApiError::bad("IGDB isn't set up yet. Add your Twitch Client ID and Secret in Settings.")
    })
}

/// Xbox 360 games matching a name on IGDB, to pick one to add.
async fn lookup(State(st): S, Query(q): Query<LookupQuery>) -> ApiResult<Json<Value>> {
    if q.q.trim().len() < 2 {
        return Err(ApiError::bad("Type at least two letters"));
    }
    let creds = igdb_creds(&st)?;
    let st2 = st.clone();
    let name = q.q.clone();
    let found = blocking(move || st2.igdb.search(&creds, &name)).await?;
    let existing: std::collections::HashSet<i64> = st
        .db
        .run(|c| {
            Ok(store::list_wanted(c)?
                .into_iter()
                .filter_map(|w| w.igdb_id)
                .collect())
        })
        .await?;
    let covers: Vec<Option<String>> = found
        .iter()
        .map(|c| c.cover.as_ref().and_then(|i| st.igdb.thumb_url(i)))
        .collect();
    Ok(Json(
        json!({"games": found.iter().zip(covers).map(|(c, thumb)| json!({"candidate": c, "thumb": thumb, "wanted": existing.contains(&c.id)})).collect::<Vec<_>>()}),
    ))
}

#[derive(Deserialize)]
struct AddWanted {
    igdb_id: i64,
    /// The result the user picked from the lookup, so IGDB isn't asked again.
    candidate: Option<igdb::Candidate>,
}

/// Put a game on the wanted list (with its cover). Fails with a conflict if it is already there.
pub(super) async fn add_candidate(st: &Arc<AppState>, cand: igdb::Candidate) -> ApiResult<i64> {
    let (st2, dir) = (st.clone(), igdb_store::covers_dir(&st.cfg.config_dir));
    let c2 = cand.clone();
    let cover = blocking(move || {
        Ok(c2
            .cover
            .as_ref()
            .and_then(|i| igdb::save_cover(&st2.igdb, &dir, i).ok()))
    })
    .await?;
    Ok(st
        .db
        .run(move |c| {
            store::add_wanted(
                c,
                &NewWanted {
                    igdb_id: Some(cand.id),
                    name: &cand.name,
                    release: cand.release,
                    summary: cand.summary.as_deref(),
                    cover: cover.as_deref(),
                    title_id: None,
                },
            )
        })
        .await?)
}

async fn add_wanted(State(st): S, Json(r): Json<AddWanted>) -> ApiResult<Json<Value>> {
    let creds = igdb_creds(&st)?;
    let (st2, id, picked) = (st.clone(), r.igdb_id, r.candidate);
    let cand = blocking(move || {
        Ok(match picked {
            Some(c) if c.id == id => c,
            _ => st2
                .igdb
                .by_id(&creds, id)?
                .ok_or_else(|| Error::not_found("IGDB doesn't know that game"))?,
        })
    })
    .await?;
    let name = cand.name.clone();
    let new_id = add_candidate(&st, cand).await?;
    Ok(Json(json!({"id": new_id, "name": name})))
}

#[derive(Deserialize)]
struct UpdateWanted {
    monitored: Option<bool>,
    note: Option<String>,
}

async fn update_wanted(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Json(r): Json<UpdateWanted>,
) -> ApiResult<Json<Value>> {
    st.db
        .run(move |c| {
            let w = store::get_wanted(c, id)?;
            store::set_monitored(c, id, r.monitored.unwrap_or(w.monitored), r.note.as_deref())
        })
        .await?;
    Ok(Json(json!({"ok": true})))
}

async fn remove_wanted(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    st.db.run(move |c| store::delete_wanted(c, id)).await?;
    st.grab_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    Ok(Json(json!({"ok": true})))
}

// ── Searching and grabbing ───────────────────────────────────────────────────

fn need_ready(cfg: &Config) -> Result<(), ApiError> {
    if engine::usable(cfg).is_empty() {
        return Err(ApiError::bad(
            "No indexers are set up (or none carries Xbox 360 games). Add them under Setup.",
        ));
    }
    Ok(())
}

async fn search(st: &Arc<AppState>, id: i64) -> ApiResult<(Wanted, engine::Outcome)> {
    let cfg = st.grabber.get();
    need_ready(&cfg)?;
    let (w, blocked) = st
        .db
        .run(move |c| Ok((store::get_wanted(c, id)?, store::blocked_keys(c)?)))
        .await?;
    let names = names_for(&w);
    let out = blocking(move || Ok(engine::search_game(&cfg, &names, &blocked))).await?;
    let summary = out.summary();
    st.db
        .run(move |c| store::touch_search(c, id, &summary))
        .await?;
    st.grab_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, out.results.clone());
    Ok((w, out))
}

async fn search_one(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    let (w, out) = search(&st, id).await?;
    Ok(Json(
        json!({"wanted": w, "searched": out.searched, "errors": out.errors, "results": out.results}),
    ))
}

#[derive(Deserialize)]
struct GrabReq {
    /// The release's guid, from the last search of this game.
    guid: String,
    indexer_id: u32,
    /// Grab even though it was rejected (you looked at why and want it anyway).
    #[serde(default)]
    force: bool,
}

/// Hand a torrent to qBittorrent. Returns its info hash, which names it from then on.
fn send_torrent(q: &Qbit, r: &crate::grabber::newznab::Release) -> Result<String, Error> {
    // The indexer's link first (it may give the .torrent file, or lead to a magnet); its magnet
    // link if the link doesn't work out.
    let fetched = match torrent::fetch(&r.link) {
        Ok(f) => f,
        Err(e) => match &r.magnet {
            Some(m) => Fetched::Magnet(m.clone()),
            None => return Err(e),
        },
    };
    let (hash, add) = match &fetched {
        Fetched::File(bytes) => {
            let t = torrent::parse(bytes)?;
            (
                t.info_hash,
                AddSource::File {
                    name: "release.torrent",
                    bytes,
                },
            )
        }
        Fetched::Magnet(m) => {
            let (h, _) = torrent::magnet(m)
                .ok_or_else(|| Error::backend("The indexer's magnet link has no usable hash"))?;
            (h, AddSource::Magnet(m))
        }
    };
    // Already in qBittorrent (a second grab of the same release): just follow it.
    if q.info(std::slice::from_ref(&hash))?.is_empty() {
        q.add(add, false)?;
    }
    Ok(hash)
}

/// Send a release to its download client (SABnzbd for usenet, qBittorrent for torrents) and record it.
async fn grab(st: &Arc<AppState>, w: &Wanted, j: &Judged) -> Result<i64, ApiError> {
    let cfg = st.grabber.get();
    let torrent = j.release.protocol == "torrent";
    if torrent && !cfg.qbit.configured() {
        return Err(ApiError::bad(
            "qBittorrent isn't set up yet. Add it under Setup.",
        ));
    }
    if !torrent && !cfg.sab.configured() {
        return Err(ApiError::bad(
            "SABnzbd isn't set up yet. Add it under Setup.",
        ));
    }
    if st
        .db
        .run({
            let id = w.id;
            move |c| store::has_open_grab(c, id)
        })
        .await?
    {
        return Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "ALREADY_DOWNLOADING",
            format!("{} is already downloading", w.name),
            true,
        ));
    }
    let client_id = if torrent {
        let (q, r) = (cfg.qbit.clone(), j.release.clone());
        blocking(move || send_torrent(&q, &r)).await?
    } else {
        let (sab, link, name) = (
            cfg.sab.clone(),
            j.release.link.clone(),
            j.release.title.clone(),
        );
        blocking(move || sab.add_url(&link, &name)).await?
    };
    let (wid, guid, title, ix, size, score) = (
        w.id,
        j.release.guid.clone(),
        j.release.title.clone(),
        j.release.indexer.clone(),
        j.release.size,
        j.verdict.score,
    );
    let protocol = if torrent { "torrent" } else { "usenet" };
    let id = st
        .db
        .run(move |c| {
            store::add_grab(
                c,
                &NewGrab {
                    wanted_id: wid,
                    guid: &guid,
                    title: &title,
                    indexer: &ix,
                    size,
                    score,
                    sab_id: &client_id,
                    protocol,
                },
            )
        })
        .await?;
    Ok(id)
}

async fn grab_one(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Json(r): Json<GrabReq>,
) -> ApiResult<Json<Value>> {
    let j = st
        .grab_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .and_then(|v| {
            v.iter()
                .find(|j| j.release.guid == r.guid && j.release.indexer_id == r.indexer_id)
                .cloned()
        })
        .ok_or_else(|| ApiError::bad("That result is no longer in memory. Search again."))?;
    if !j.verdict.rejected.is_empty() && !r.force {
        return Err(ApiError::bad(format!(
            "This release was rejected: {}",
            j.verdict.rejected.join("; ")
        )));
    }
    let w = st.db.run(move |c| store::get_wanted(c, id)).await?;
    let grab_id = grab(&st, &w, &j).await?;
    Ok(Json(json!({"grab": grab_id, "title": j.release.title})))
}

/// Search and grab the best acceptable release, if there is one.
pub async fn auto_grab(st: &Arc<AppState>, id: i64) -> ApiResult<Option<String>> {
    let (w, out) = search(st, id).await?;
    let min = st.grabber.get().profile.min_score;
    match out.best(min) {
        Some(j) => {
            grab(st, &w, j).await?;
            Ok(Some(j.release.title.clone()))
        }
        None => Ok(None),
    }
}

async fn auto_one(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"grabbed": auto_grab(&st, id).await?})))
}

/// Search every monitored game that isn't already downloading or in a library (a job).
async fn search_all(State(st): S) -> ApiResult<Json<Value>> {
    need_ready(&st.grabber.get())?;
    let job = st
        .jobs
        .create(
            "grab",
            "Search for all wanted games",
            vec!["grab-search".into()],
        )
        .map_err(|b| ApiError::busy(&b))?;
    let id = job.id;
    let st2 = st.clone();
    st.jobs.spawn(job, move |job| async move {
        let n = run_search_round(&st2, |m| job.step(m)).await?;
        job.log(format!("{n} game(s) grabbed"));
        Ok(())
    });
    Ok(Json(json!({"job": id})))
}

/// One round over the wanted list. Returns how many games were grabbed.
pub async fn run_search_round(
    st: &Arc<AppState>,
    mut say: impl FnMut(String),
) -> Result<usize, Error> {
    let cfg = st.grabber.get();
    let (wanted, (igdb_have, tid_have)) = st
        .db
        .run(|c| Ok((store::list_wanted(c)?, store::owned(c)?)))
        .await?;
    let mut grabbed = 0;
    for w in wanted.iter().filter(|w| w.monitored) {
        let have = w.igdb_id.is_some_and(|i| igdb_have.contains(&i))
            || w.title_id
                .as_deref()
                .is_some_and(|t| tid_have.contains(&t.to_uppercase()));
        let wid = w.id;
        if have || st.db.run(move |c| store::has_open_grab(c, wid)).await? {
            continue;
        }
        say(format!("Searching for {}", w.name));
        let found = if cfg.auto_grab {
            auto_grab(st, w.id).await.map(|g| g.is_some())
        } else {
            search(st, w.id)
                .await
                .map(|(_, o)| o.best(cfg.profile.min_score).is_some())
        };
        match found {
            Ok(true) if cfg.auto_grab => grabbed += 1,
            Ok(_) => {}
            Err(e) => say(format!("{}: {}", w.name, e.message())),
        }
    }
    Ok(grabbed)
}

// ── Activity ─────────────────────────────────────────────────────────────────

async fn activity(State(st): S) -> ApiResult<Json<Value>> {
    let (grabs, wanted) = st
        .db
        .run(|c| Ok((store::list_grabs(c, 100)?, store::list_wanted(c)?)))
        .await?;
    let names: HashMap<i64, &Wanted> = wanted.iter().map(|w| (w.id, w)).collect();
    Ok(Json(
        json!({"grabs": grabs.iter().map(|g| json!({"grab": g, "game": names.get(&g.wanted_id).map(|w| &w.name), "cover": names.get(&g.wanted_id).and_then(|w| w.cover.as_ref())})).collect::<Vec<_>>()}),
    ))
}

#[derive(Deserialize)]
struct RemoveGrab {
    /// Also put the release on the blocklist (a bad one).
    #[serde(default)]
    blocklist: bool,
}

async fn remove_grab(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Query(q): Query<RemoveGrab>,
) -> ApiResult<Json<Value>> {
    let cfg = st.grabber.get();
    let g = st.db.run(move |c| store::get_grab(c, id)).await?;
    if matches!(g.status.as_str(), "queued" | "downloading") {
        if g.protocol == "torrent" {
            // Stop it and take the part-downloaded files away with it.
            if let (true, Some(hash)) = (cfg.qbit.configured(), g.sab_id.clone()) {
                let q = cfg.qbit.clone();
                let _ = blocking(move || q.remove(&hash, true)).await;
            }
        } else if let (true, Some(nzo)) = (cfg.sab.configured(), g.sab_id.clone()) {
            let sab = cfg.sab.clone();
            let _ = blocking(move || sab.delete(&nzo)).await;
        }
    }
    let g2 = g.clone();
    st.db
        .run(move |c| {
            if q.blocklist {
                store::block(
                    c,
                    Some(g2.wanted_id),
                    &g2.guid,
                    &g2.title,
                    &g2.indexer,
                    "Removed by you",
                )?;
            }
            store::delete_grab(c, id)?;
            if !store::has_open_grab(c, g2.wanted_id)? {
                store::set_wanted_status(c, g2.wanted_id, "wanted")?;
            }
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok": true})))
}

async fn retry_import(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    let g = st.db.run(move |c| store::get_grab(c, id)).await?;
    if g.path.is_none() {
        return Err(ApiError::bad(
            "This download has no finished folder to import yet",
        ));
    }
    // A second import of the same download while the first runs would fail on files the first
    // has already moved, and wrongly mark it as failed.
    if matches!(g.status.as_str(), "importing" | "unpacking")
        && g.import_job
            .and_then(|j| st.jobs.get(j as u64))
            .is_some_and(|j| !j.status().is_terminal())
    {
        return Err(ApiError::bad("This download is already being imported"));
    }
    let job = begin_import(&st, &g).await?;
    Ok(Json(json!({"job": job})))
}

/// Start importing a grab and record which job is doing it. A failure is recorded on the grab too.
async fn begin_import(st: &Arc<AppState>, g: &store::Grab) -> ApiResult<i64> {
    let gid = g.id;
    match import_grab(st, g).await {
        Ok(started) => {
            let (status, job) = match started {
                Started::Import(j) => ("importing", j),
                Started::Unpack(j) => ("unpacking", j),
            };
            st.db
                .run(move |c| store::update_grab(c, gid, status, None, None, None, Some(job)))
                .await?;
            Ok(job)
        }
        Err(e) => {
            let why = e.message();
            let w2 = why.clone();
            let _ = st
                .db
                .run(move |c| {
                    store::update_grab(c, gid, "import_failed", None, None, Some(&w2), None)
                })
                .await;
            Err(e)
        }
    }
}

async fn get_blocklist(State(st): S) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"blocked": st.db.run(|c| store::blocklist(c)).await?}),
    ))
}

async fn unblock(State(st): S, UrlPath(id): UrlPath<i64>) -> ApiResult<Json<Value>> {
    st.db.run(move |c| store::unblock(c, id)).await?;
    Ok(Json(json!({"ok": true})))
}

// ── Importing what finishes ──────────────────────────────────────────────────

/// What starting an import did: an import job, or first a job that opens zips.
enum Started {
    Import(i64),
    Unpack(i64),
}

/// Where a grab's zips are opened to: a hidden folder next to the download, so the disc images
/// come out on the same disk (and an import of them is a rename, not a copy).
const UNPACK_PREFIX: &str = ".rustybox-unpack-";

fn unpack_dir(download: &str, grab_id: i64) -> std::path::PathBuf {
    let p = std::path::Path::new(download);
    p.parent()
        .unwrap_or(p)
        .join(format!("{UNPACK_PREFIX}{grab_id}"))
}

fn is_unpack_dir(path: &str) -> bool {
    std::path::Path::new(path)
        .file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with(UNPACK_PREFIX))
}

/// Put one downloaded file in a folder of its own (a hardlink, or a copy across disks).
fn stage_file(file: &str, grab_id: i64) -> Result<String, Error> {
    let src = std::path::Path::new(file);
    let name = src
        .file_name()
        .ok_or_else(|| Error::validation("That download has no file name"))?;
    let dir = unpack_dir(file, grab_id);
    std::fs::create_dir_all(&dir)?;
    let to = dir.join(name);
    if !to.exists() && std::fs::hard_link(src, &to).is_err() {
        std::fs::copy(src, &to)?;
    }
    Ok(dir.to_string_lossy().to_string())
}

/// Hardlink when the download and the library are on one disk (instant, and the torrent keeps
/// seeding); copy when they are not, or the library is on a drive agent.
fn torrent_mode(cfg: &Config, download: &str, dest: &library::LibPath) -> Mode {
    use std::os::unix::fs::MetadataExt;
    match cfg.torrent_import_mode.as_str() {
        "move" => Mode::Move,
        "copy" => Mode::Copy,
        _ => {
            if dest.is_remote() {
                return Mode::Copy;
            }
            let dev = |p: &str| std::fs::metadata(p).map(|m| m.dev()).ok();
            match (dev(download), dev(&dest.path)) {
                (Some(a), Some(b)) if a == b => Mode::Hardlink,
                _ => Mode::Copy,
            }
        }
    }
}

/// Import a finished download into the chosen library. If it holds zipped disc images instead of
/// games, they are opened first (a job of its own, after which the import follows).
async fn import_grab(st: &Arc<AppState>, g: &store::Grab) -> ApiResult<Started> {
    let cfg = st.grabber.get();
    let (lib_id, path_id) = cfg
        .import_library
        .ok_or_else(|| ApiError::bad("Choose which library finished games go to under Setup"))?;
    let raw = g
        .path
        .clone()
        .ok_or_else(|| ApiError::bad("No finished folder"))?;
    // A folder RustyBox made itself is already a path it can see; the client's paths are mapped.
    let mut path = if is_unpack_dir(&raw) {
        raw.clone()
    } else {
        cfg.map_path(&raw)
    };
    // A one-file download (qBittorrent reports the file itself): import it from a folder of its own,
    // so the rest of what is next to it isn't swept up. The file is hardlinked in (a copy if the
    // disk differs) and that link is what gets moved on.
    if std::path::Path::new(&path).is_file() && !path.to_lowercase().ends_with(".zip") {
        let (file, id) = (path.clone(), g.id);
        path = blocking(move || stage_file(&file, id)).await?;
    }
    let (roots, source) = (st.cfg.roots.clone(), Source::Folder { path: path.clone() });
    let resolved = st.db.run(move |c| tplan::resolve(c, &source, &roots)).await.map_err(|e| ApiError::bad(format!("RustyBox can't see the download at {path}: {e}. Check the path mapping under Setup.")))?;
    let found = blocking(move || tplan::discover(&resolved)).await?;
    let wanted_games: Vec<_> = found
        .into_iter()
        .filter(|c| c.content_kind == "game" && c.kind != "file")
        .collect();
    let title_ids: Vec<String> = wanted_games
        .iter()
        .filter_map(|c| c.title_id.clone())
        .collect();
    let games: Vec<String> = wanted_games.into_iter().map(|c| c.id).collect();
    if games.is_empty() {
        // Disc images in zips (Redump's collection is one zip per game): open them first.
        let local = std::path::PathBuf::from(&path);
        let plans = blocking({
            let local = local.clone();
            move || Ok(unpack::find(&local))
        })
        .await?;
        if !plans.is_empty() {
            return start_unpack(st, g, &path, plans).await.map(Started::Unpack);
        }
        return Err(ApiError::bad(format!(
            "No ISO or Games on Demand folder found in {}. SABnzbd may have left it packed, or the path mapping may be wrong.",
            raw
        )));
    }
    let dest_lp = st
        .db
        .run(move |c| library::get_path(c, lib_id, path_id))
        .await?;
    let kind = st
        .db
        .run(move |c| Ok(library::get_library(c, lib_id)?.kind))
        .await?;
    let unpacked = is_unpack_dir(&path);
    let mode = if unpacked {
        // Our own unpacked copy: it can simply be moved, which leaves nothing behind.
        Mode::Move
    } else if g.protocol == "torrent" {
        torrent_mode(&cfg, &path, &dest_lp)
    } else if cfg.import_mode == "copy" {
        Mode::Copy
    } else {
        Mode::Move
    };
    let req = Request {
        source: Source::Folder { path: path.clone() },
        select: games,
        mode,
        dest: tplan::DestRef {
            library_id: lib_id,
            path_id,
        },
        layout: None,
        convert_iso: cfg.convert_iso && kind == "god",
        overwrite: false,
        replace: false,
        also_to: cfg.also_drive.map(|(library_id, path_id)| tplan::DestRef {
            library_id,
            path_id,
        }),
    };
    let (gid, ids) = (g.id, title_ids.join(","));
    let _ = st.db.run(move |c| store::set_title_ids(c, gid, &ids)).await;
    super::import::start_import(st, req)
        .await
        .map(|id| Started::Import(id as i64))
}

/// Open the zips of a finished download into a hidden folder next to it (a job).
async fn start_unpack(
    st: &Arc<AppState>,
    g: &store::Grab,
    download: &str,
    plans: Vec<unpack::ZipPlan>,
) -> ApiResult<i64> {
    let dest = unpack_dir(download, g.id);
    let bytes: u64 = plans.iter().map(unpack::ZipPlan::bytes).sum();
    let job = st.jobs.create_queued(
        "unpack",
        &format!("Unpack {}", g.title),
        vec![
            format!("unpack:{}", g.id),
            format!(
                "unpack-disk:{}",
                dest.parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
            ),
        ],
    );
    let job_id = job.id as i64;
    st.jobs.spawn(job, move |job| async move {
        job.log(format!(
            "{} zip(s) with disc images, about {} MB, going to {}",
            plans.len(),
            bytes / 1_048_576,
            dest.display()
        ));
        let (j, token) = (job.clone(), job.cancel_token());
        tokio::task::spawn_blocking(move || {
            let cancelled = || token.is_cancelled();
            let progress = |f: f32, what: &str| {
                j.progress(f * 100.0);
                j.step(what.to_string());
            };
            unpack::extract(
                &plans,
                &dest,
                &crate::convert::Ctl {
                    cancelled: &cancelled,
                    progress: &progress,
                },
            )
            .map(|_| ())
        })
        .await
        .map_err(|e| Error::backend(e.to_string()))?
    });
    Ok(job_id)
}

/// After an import: send the games it brought in on to the console, if that is asked for.
async fn send_to_console(st: &Arc<AppState>, g: &store::Grab) {
    let cfg = st.grabber.get();
    let (Some(ac), Some((lib, _))) = (cfg.also_console.clone(), cfg.import_library) else {
        return;
    };
    let titles: Vec<String> = g
        .title_ids
        .clone()
        .unwrap_or_default()
        .split(',')
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect();
    if titles.is_empty() {
        return;
    }
    let ids = st
        .db
        .run(move |c| store::items_with_titles(c, lib, &titles))
        .await
        .unwrap_or_default();
    if ids.is_empty() {
        tell(
            st,
            format!("{}: couldn't send to the console", g.title),
            "The imported game wasn't found in the library.".into(),
            "failed",
        );
        return;
    }
    let items = ids
        .into_iter()
        .map(|item_id| crate::convert::plan::ItemRef {
            library_id: lib,
            item_id,
        })
        .collect();
    let req = super::console::SendReq::games(items, ac.dest.clone());
    let gid = g.id;
    match super::console::start_send(st, ac.console_id, req).await {
        Ok(job) => {
            let _ = st
                .db
                .run(move |c| store::set_extra_job(c, gid, job as i64))
                .await;
        }
        Err(e) => {
            tell(
                st,
                format!("{}: couldn't send to the console", g.title),
                e.message(),
                "failed",
            );
        }
    }
}

// ── Background: following SABnzbd, and searching on a schedule ───────────────

fn tell(st: &Arc<AppState>, title: String, msg: String, status: &'static str) {
    let s = st.settings.get();
    if s.notify_url.is_empty() || s.notify_on == "off" || (status == "done" && s.notify_on != "all")
    {
        return;
    }
    let url = s.notify_url;
    tokio::task::spawn_blocking(move || notify::send(&url, &title, &msg, status, None));
}

/// A download has finished: note where it is, say so, and import it if that is automatic.
async fn download_done(st: &Arc<AppState>, cfg: &Config, g: &store::Grab, path: String) {
    let gid = g.id;
    let p2 = path.clone();
    let _ = st
        .db
        .run(move |c| store::update_grab(c, gid, "completed", Some(100.0), Some(&p2), None, None))
        .await;
    tell(
        st,
        format!("{} downloaded", g.title),
        if cfg.auto_import {
            "Importing it next."
        } else {
            "It's ready to import from the Wanted page."
        }
        .into(),
        "done",
    );
    if cfg.auto_import {
        let mut done = g.clone();
        done.path = Some(path);
        let _ = begin_import(st, &done).await; // a failure is recorded on the download
    }
}

/// Follow the job importing (or unpacking) a download, and move the download on when it ends.
async fn follow_job(st: &Arc<AppState>, cfg: &Config, g: &store::Grab) {
    let gid = g.id;
    let Some(job) = g.import_job.and_then(|j| st.jobs.get(j as u64)) else {
        // The job is gone: RustyBox was restarted while it ran. Don't wait for it for
        // ever; say so, and let the user press Import to try again.
        let _ = st
            .db
            .run(move |c| {
                store::update_grab(
                    c,
                    gid,
                    "import_failed",
                    None,
                    None,
                    Some("The import was interrupted (RustyBox restarted). Press Import to try again."),
                    None,
                )
            })
            .await;
        return;
    };
    if !job.status().is_terminal() {
        return;
    }
    let sum = job.summary();
    let unpacking = g.status == "unpacking";
    match sum.status {
        crate::jobs::Status::Done if unpacking => {
            // The disc images are out: import them from where they were put.
            let Some(download) = g.path.clone().map(|p| cfg.map_path(&p)) else {
                return;
            };
            let dir = unpack_dir(&download, gid).to_string_lossy().to_string();
            let mut next = g.clone();
            next.path = Some(dir.clone());
            let d2 = dir.clone();
            let _ = st
                .db
                .run(move |c| store::update_grab(c, gid, "completed", None, Some(&d2), None, None))
                .await;
            let _ = begin_import(st, &next).await;
        }
        crate::jobs::Status::Done => {
            let wid = g.wanted_id;
            let _ = st
                .db
                .run(move |c| {
                    store::update_grab(c, gid, "imported", Some(100.0), None, None, None)?;
                    store::set_wanted_status(c, wid, "downloaded")
                })
                .await;
            tell(
                st,
                format!("{} imported", g.title),
                "It's in your library now.".into(),
                "done",
            );
            clean_up_after_import(cfg, g).await;
            send_to_console(st, g).await;
        }
        _ => {
            let why = sum
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "The import was cancelled".into());
            let w2 = why.clone();
            let _ = st
                .db
                .run(move |c| {
                    store::update_grab(c, gid, "import_failed", None, None, Some(&w2), None)
                })
                .await;
            tell(
                st,
                format!(
                    "{} {} failed",
                    if unpacking { "Unpacking" } else { "Importing" },
                    g.title
                ),
                why,
                "failed",
            );
        }
    }
}

/// After an import: take away what RustyBox made for it (opened zips), and, if asked, the torrent.
async fn clean_up_after_import(cfg: &Config, g: &store::Grab) {
    // What RustyBox made for this download (opened zips, a staged one-file download).
    if let Some(p) = g.path.as_deref() {
        let mut dirs = vec![];
        if is_unpack_dir(p) {
            dirs.push(p.to_string());
        } else {
            dirs.push(
                unpack_dir(&cfg.map_path(p), g.id)
                    .to_string_lossy()
                    .to_string(),
            );
        }
        for d in dirs {
            if is_unpack_dir(&d) {
                let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(d)).await;
            }
        }
    }
    if g.protocol == "torrent" && cfg.torrent_after_import == "remove" && cfg.qbit.configured() {
        let Some(hash) = g.sab_id.clone() else { return };
        // After a move there are no files of the torrent left to delete.
        let files = cfg.torrent_import_mode != "move";
        let q = cfg.qbit.clone();
        let _ = blocking(move || q.remove(&hash, files)).await;
    }
}

/// Look at the download clients for every open download, and move each along.
pub async fn poll_downloads(st: &Arc<AppState>) {
    let cfg = st.grabber.get();
    let Ok(open) = st.db.run(|c| store::open_grabs(c)).await else {
        return;
    };
    if open.is_empty() {
        return;
    }
    for g in open
        .iter()
        .filter(|g| matches!(g.status.as_str(), "importing" | "unpacking"))
    {
        follow_job(st, &cfg, g).await;
    }
    let waiting: Vec<_> = open
        .into_iter()
        .filter(|g| !matches!(g.status.as_str(), "importing" | "unpacking"))
        .collect();
    let (torrents, usenet): (Vec<_>, Vec<_>) =
        waiting.into_iter().partition(|g| g.protocol == "torrent");
    poll_usenet(st, &cfg, usenet).await;
    poll_torrents(st, &cfg, torrents).await;
}

async fn poll_torrents(st: &Arc<AppState>, cfg: &Config, grabs: Vec<store::Grab>) {
    if grabs.is_empty() || !cfg.qbit.configured() {
        return;
    }
    let hashes: Vec<String> = grabs.iter().filter_map(|g| g.sab_id.clone()).collect();
    let q = cfg.qbit.clone();
    let Ok(infos) = blocking(move || q.info(&hashes)).await else {
        return;
    };
    let t = now() as i64;
    for g in grabs {
        let gid = g.id;
        let hash = g.sab_id.clone().unwrap_or_default().to_lowercase();
        match infos.iter().find(|i| i.hash == hash) {
            Some(i) => match i.phase() {
                Phase::Queued | Phase::Downloading => {
                    let status = if i.phase() == Phase::Downloading {
                        "downloading"
                    } else {
                        "queued"
                    };
                    let p = i.percent as f64;
                    let _ = st
                        .db
                        .run(move |c| store::update_grab(c, gid, status, Some(p), None, None, None))
                        .await;
                }
                Phase::Done => {
                    if g.status == "completed" {
                        continue; // already waiting for a manual import
                    }
                    download_done(st, cfg, &g, i.content_path.clone()).await;
                }
                Phase::Failed => {
                    let why = format!("qBittorrent reports “{}”", i.state);
                    let (g2, w2) = (g.clone(), why.clone());
                    let _ = st
                        .db
                        .run(move |c| {
                            store::update_grab(c, gid, "failed", None, None, Some(&w2), None)?;
                            store::block(
                                c,
                                Some(g2.wanted_id),
                                &g2.guid,
                                &g2.title,
                                &g2.indexer,
                                &w2,
                            )?;
                            store::set_wanted_status(c, g2.wanted_id, "wanted")
                        })
                        .await;
                    tell(
                        st,
                        format!("{} failed", g.title),
                        format!("{why}. It's on the blocklist; the next search will skip it."),
                        "failed",
                    );
                }
            },
            None if t - g.updated > 900 && t - g.added > 900 => {
                let _ = st
                    .db
                    .run(move |c| {
                        store::update_grab(
                            c,
                            gid,
                            "failed",
                            None,
                            None,
                            Some("qBittorrent no longer has this torrent"),
                            None,
                        )?;
                        store::set_wanted_status(c, g.wanted_id, "wanted")
                    })
                    .await;
            }
            None => {}
        }
    }
}

async fn poll_usenet(st: &Arc<AppState>, cfg: &Config, open: Vec<store::Grab>) {
    if open.is_empty() || !cfg.sab.configured() {
        return;
    }
    let sab = cfg.sab.clone();
    let Ok((queue, history)) = blocking(move || Ok((sab.queue()?, sab.history(100)?))).await else {
        return;
    };
    let t = now() as i64;
    for g in open {
        let gid = g.id;
        let nzo = g.sab_id.clone().unwrap_or_default();
        if let Some(q) = queue.iter().find(|q| q.nzo_id == nzo) {
            let status = if q.status.eq_ignore_ascii_case("downloading") {
                "downloading"
            } else {
                "queued"
            };
            let p = q.percent as f64;
            let _ = st
                .db
                .run(move |c| store::update_grab(c, gid, status, Some(p), None, None, None))
                .await;
        } else if let Some(h) = history.iter().find(|h| h.nzo_id == nzo) {
            match h.status.as_str() {
                "Completed" => {
                    if g.status == "completed" {
                        continue; // already waiting for a manual import
                    }
                    download_done(st, cfg, &g, h.storage.clone()).await;
                }
                "Failed" => {
                    let why = if h.fail_message.is_empty() {
                        "SABnzbd couldn't finish it".to_string()
                    } else {
                        h.fail_message.clone()
                    };
                    let (g2, w2) = (g.clone(), why.clone());
                    let _ = st
                        .db
                        .run(move |c| {
                            store::update_grab(c, gid, "failed", None, None, Some(&w2), None)?;
                            store::block(
                                c,
                                Some(g2.wanted_id),
                                &g2.guid,
                                &g2.title,
                                &g2.indexer,
                                &w2,
                            )?;
                            store::set_wanted_status(c, g2.wanted_id, "wanted")
                        })
                        .await;
                    tell(
                        st,
                        format!("{} failed", g.title),
                        format!("{why}. It's on the blocklist; the next search will skip it."),
                        "failed",
                    );
                }
                _ => {} // extracting, verifying... still going
            }
        } else if t - g.updated > 900 && t - g.added > 900 {
            let _ = st
                .db
                .run(move |c| {
                    store::update_grab(
                        c,
                        gid,
                        "failed",
                        None,
                        None,
                        Some("SABnzbd no longer has this download"),
                        None,
                    )?;
                    store::set_wanted_status(c, g.wanted_id, "wanted")
                })
                .await;
        }
    }
}

/// Start the download follower and the scheduled search. Called by `serve`, not by tests.
pub fn spawn_background(st: Arc<AppState>) {
    let s1 = st.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            poll_downloads(&s1).await;
        }
    });
    tokio::spawn(async move {
        let mut last = std::time::Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let every = st.grabber.get().search_every_hours as u64;
            if every == 0 || last.elapsed() < Duration::from_secs(every * 3600) {
                continue;
            }
            last = std::time::Instant::now();
            if let Ok(job) = st.jobs.create(
                "grab",
                "Scheduled search for wanted games",
                vec!["grab-search".into()],
            ) {
                let st2 = st.clone();
                st.jobs.spawn(job, move |job| async move {
                    let n = run_search_round(&st2, |m| job.step(m)).await?;
                    job.log(format!("{n} game(s) grabbed"));
                    Ok(())
                });
            }
        }
    });
}
