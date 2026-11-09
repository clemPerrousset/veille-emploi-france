//! Serveur HTTP : ressources statiques embarquées, fichiers de données, `/health`.
//! Lecture seule : aucune route d'écriture.

use crate::status::StatusBoard;
use crate::store::{Store, DATA_FILES};
use axum::extract::{Path, State};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde_json::{json, Map, Value};
use tower_http::compression::CompressionLayer;

const JS: &str = "application/javascript; charset=utf-8";

const SECURITY_HEADERS: [(&str, &str); 4] = [
    (
        "content-security-policy",
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'",
    ),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("x-frame-options", "DENY"),
];

/// Seuls ces quatre fichiers du dépôt sont embarqués (jamais `.env`, `server/`, `data/`).
#[derive(RustEmbed)]
#[folder = ".."]
#[include = "index.html"]
#[include = "app.js"]
#[include = "style.css"]
#[include = "vendor/chart.umd.min.js"]
struct Assets;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub status: StatusBoard,
    pub version: &'static str,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| asset("index.html", "text/html; charset=utf-8")))
        .route("/app.js", get(|| asset("app.js", JS)))
        .route("/style.css", get(|| asset("style.css", "text/css; charset=utf-8")))
        .route(
            "/vendor/chart.umd.min.js",
            get(|| asset("vendor/chart.umd.min.js", JS)),
        )
        .route("/data/{name}", get(data_file))
        .route("/health", get(health))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(axum::middleware::map_response(add_security_headers))
        .layer(CompressionLayer::new().gzip(true))
        .with_state(state)
}

async fn add_security_headers(mut res: Response) -> Response {
    for (k, v) in SECURITY_HEADERS {
        res.headers_mut()
            .insert(HeaderName::from_static(k), HeaderValue::from_static(v));
    }
    res
}

async fn asset(path: &'static str, content_type: &'static str) -> Response {
    match Assets::get(path) {
        Some(f) => ([(header::CONTENT_TYPE, content_type)], f.data.into_owned()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn data_file(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    // Liste blanche exacte avant tout accès disque.
    if !Store::is_known(&name) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match st.store.read(&name) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, JS),
                (header::CACHE_CONTROL, "public, max-age=300"),
            ],
            bytes,
        )
            .into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn health(State(st): State<AppState>) -> Json<Value> {
    let mut files = Map::new();
    for (name, _) in DATA_FILES {
        let age = st.store.age_seconds(name);
        files.insert(
            name.to_string(),
            json!({"exists": age.is_some(), "age_seconds": age}),
        );
    }
    Json(json!({
        "version": st.version,
        "collectors": st.status.snapshot(),
        "files": files,
    }))
}
