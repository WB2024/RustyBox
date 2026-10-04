//! The web UI, embedded in the binary from `web/` (plain HTML, CSS and JS modules, no build step).

use axum::{
    extract::Path,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/"]
struct Assets;

fn serve(path: &str, req: &HeaderMap) -> Response {
    let Some(file) = Assets::get(path) else {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    };
    let etag = format!(
        "\"{}\"",
        file.metadata
            .sha256_hash()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    if req.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    let mime = mime_for(path);
    // `no-cache` means "revalidate with the ETag", so an upgrade is picked up straight away.
    (
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::CACHE_CONTROL, "no-cache".to_string()),
            (header::ETAG, etag),
        ],
        file.data,
    )
        .into_response()
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

pub async fn index(headers: HeaderMap) -> Response {
    serve("index.html", &headers)
}

pub async fn asset(Path(path): Path<String>, headers: HeaderMap) -> Response {
    serve(&path, &headers)
}
