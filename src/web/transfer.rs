//! Moving files between the browser and a library.
//!
//! Uploads are chunked and resumable: the browser sends a file in pieces at byte offsets; the
//! server writes them to `<name>.part`, and renames it to the real name when the last byte
//! arrives. Nothing half-written is ever visible under the final name. Downloads support HTTP
//! Range, so big files can be resumed too.

use std::{path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path as UrlPath, Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use super::{ApiError, ApiResult, AppState, S, safe_join};
use crate::{
    fsops,
    library::{self, LibPath, health},
};

/// The largest piece the browser may send in one request.
pub const MAX_CHUNK: usize = fsops::MAX_CHUNK;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/libraries/{id}/upload",
            get(upload_status)
                .put(upload_chunk)
                .layer(axum::extract::DefaultBodyLimit::max(MAX_CHUNK)),
        )
        .route("/api/libraries/{id}/items/{item}/download", get(download))
}

// ── Upload ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct UploadQuery {
    path_id: i64,
    /// Where the file goes, relative to the library folder, e.g. `Halo 3/Halo 3.iso`.
    rel: String,
    offset: Option<u64>,
    total: Option<u64>,
    overwrite: Option<bool>,
}

/// Where an upload goes: a folder here, or a drive on another machine.
enum Target {
    Local(PathBuf),
    Remote(crate::remote::Remote),
}

/// The writable library folder for an upload. The chunk handling itself is `fsops::write_chunk`.
async fn target_root(st: &AppState, library_id: i64, path_id: i64) -> ApiResult<Target> {
    let lp: LibPath = st
        .db
        .run(move |c| library::get_path(c, library_id, path_id))
        .await?;
    if !lp.writable {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "READ_ONLY",
            "That folder is read-only. Turn on writing for it in the library's folder list.",
            true,
        ));
    }
    let h = health::check_path(&lp, std::time::Duration::from_secs(3)).await;
    if !h.online {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "PATH_OFFLINE",
            format!("That folder is offline: {}", h.problem.unwrap_or_default()),
            true,
        ));
    }
    Ok(match lp.remote() {
        Some(r) => Target::Remote(r),
        None => Target::Local(PathBuf::from(&lp.path)),
    })
}

async fn upload_status(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Query(q): Query<UploadQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(match target_root(&st, id, q.path_id).await? {
        Target::Local(root) => json!(fsops::write_status(&root, &q.rel).await?),
        Target::Remote(r) => {
            let rel = q.rel.clone();
            json!(
                tokio::task::spawn_blocking(move || r.write_status(&rel))
                    .await
                    .map_err(|e| crate::error::Error::backend(e.to_string()))??
            )
        }
    }))
}

async fn upload_chunk(
    State(st): S,
    UrlPath(id): UrlPath<i64>,
    Query(q): Query<UploadQuery>,
    body: Body,
) -> ApiResult<Json<Value>> {
    let total = q
        .total
        .ok_or_else(|| ApiError::bad("Missing total (the file's full size)"))?;
    let offset = q.offset.unwrap_or(0);
    let overwrite = q.overwrite.unwrap_or(false);
    match target_root(&st, id, q.path_id).await? {
        Target::Local(root) => {
            let r = fsops::write_chunk(
                &st.uploading,
                &root,
                &q.rel,
                offset,
                total,
                overwrite,
                body.into_data_stream(),
            )
            .await?;
            Ok(Json(json!({"done": r.done, "offset": r.offset})))
        }
        Target::Remote(remote) => {
            // The piece is passed on to the drive agent, which does the same checks and writes it.
            let data = axum::body::to_bytes(body, fsops::MAX_CHUNK)
                .await
                .map_err(|e| ApiError::bad(format!("Upload interrupted: {e}")))?;
            let rel = q.rel.clone();
            let (done, now) = tokio::task::spawn_blocking(move || {
                remote.write_chunk(&rel, offset, total, overwrite, &data)
            })
            .await
            .map_err(|e| crate::error::Error::backend(e.to_string()))??;
            Ok(Json(json!({"done": done, "offset": now})))
        }
    }
}

// ── Download ─────────────────────────────────────────────────────────────────

fn disposition(name: &str) -> HeaderValue {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_control() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

/// A file on a drive agent, passed through to the browser piece by piece (with Range, so it can resume).
async fn download_remote(
    remote: crate::remote::Remote,
    rel: &str,
    name: &str,
    range_header: Option<String>,
) -> ApiResult<Response> {
    use crate::error::Error;
    let (r2, rel2) = (remote.clone(), rel.to_string());
    let stat = tokio::task::spawn_blocking(move || r2.stat(&rel2))
        .await
        .map_err(|e| Error::backend(e.to_string()))??;
    if !stat.exists || stat.is_dir {
        return Err(ApiError::not_found("That file isn't available"));
    }
    let size = stat.size;
    // A single `bytes=a-b`, `bytes=a-` or `bytes=-n` range; anything else gets the whole file.
    let range = range_header
        .as_deref()
        .and_then(|v| v.strip_prefix("bytes="))
        .filter(|v| !v.contains(','))
        .and_then(|v| v.split_once('-'))
        .map(
            |(a, b)| match (a.parse::<u64>().ok(), b.parse::<u64>().ok()) {
                (Some(s), Some(e)) => (s, e.min(size.saturating_sub(1))),
                (Some(s), None) => (s, size.saturating_sub(1)),
                (None, Some(n)) => (size.saturating_sub(n), size.saturating_sub(1)),
                (None, None) => (0, size.saturating_sub(1)),
            },
        );
    let (start, end, partial) = match range {
        Some((s, e)) if size > 0 && s <= e && s < size => (s, e, true),
        Some(_) if size > 0 => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{size}"))],
            )
                .into_response());
        }
        _ => (0, size.saturating_sub(1), false),
    };
    let len = if size == 0 { 0 } else { end - start + 1 };

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(4);
    let rel3 = rel.to_string();
    tokio::task::spawn_blocking(move || {
        if len == 0 {
            return;
        }
        let mut reader = match remote.read(&rel3, start, Some(len)) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.blocking_send(Err(std::io::Error::other(e.to_string())));
                return;
            }
        };
        let mut left = len;
        let mut buf = vec![0u8; 256 * 1024];
        while left > 0 {
            let want = (left as usize).min(buf.len());
            match std::io::Read::read(&mut reader, &mut buf[..want]) {
                Ok(0) => break,
                Ok(n) => {
                    left -= n as u64;
                    if tx
                        .blocking_send(Ok(axum::body::Bytes::copy_from_slice(&buf[..n])))
                        .is_err()
                    {
                        return; // the browser went away
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let mut res = Response::new(Body::from_stream(stream));
    *res.status_mut() = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let h = res.headers_mut();
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::CONTENT_DISPOSITION, disposition(name));
    if partial && let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")) {
        h.insert(header::CONTENT_RANGE, v);
    }
    Ok(res)
}

async fn download(
    State(st): S,
    UrlPath((id, item_id)): UrlPath<(i64, i64)>,
    req: Request,
) -> ApiResult<Response> {
    let (item, lp) = st
        .db
        .run(move |c| library::get_item(c, id, item_id))
        .await?;
    if item.kind == "god" {
        return Err(ApiError::bad(
            "This is a folder of many files; downloading folders isn't supported yet",
        ));
    }
    if let Some(remote) = lp.remote() {
        let range = req
            .headers()
            .get(header::RANGE)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        return download_remote(remote, &item.relpath, &item.name, range).await;
    }
    let root = PathBuf::from(&lp.path);
    let full = safe_join(&root, &item.relpath)?;
    // Resolve symlinks and make sure the real file is still inside the library folder.
    let (canon_root, canon_full) = (
        tokio::fs::canonicalize(&root).await,
        tokio::fs::canonicalize(&full).await,
    );
    let (Ok(canon_root), Ok(canon_full)) = (canon_root, canon_full) else {
        return Err(ApiError::not_found(
            "That file isn't available right now (is the disk or share mounted?)",
        ));
    };
    // A link imported from elsewhere on the server may point outside this folder, but only to
    // somewhere RustyBox is allowed to use.
    let allowed =
        canon_full.starts_with(&canon_root) || library::is_within_roots(&st.cfg.roots, &canon_full);
    if !allowed || !canon_full.is_file() {
        return Err(ApiError::not_found("That file isn't available"));
    }
    let name = canon_full
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| item.name.clone());
    let mut res = ServeFile::new(&canon_full)
        .oneshot(req)
        .await
        .unwrap_or_else(|e| match e {})
        .into_response();
    res.headers_mut()
        .insert(header::CONTENT_DISPOSITION, disposition(&name));
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_names_are_quoted_safely() {
        let v = disposition("Halo \"3\" é.iso");
        let s = v.to_str().unwrap();
        assert!(
            s.contains("filename=\"Halo _3_ _.iso\"")
                && s.contains("filename*=UTF-8''Halo%20%223%22%20%C3%A9.iso"),
            "{s}"
        );
    }
}
