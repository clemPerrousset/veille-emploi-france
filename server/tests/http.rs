use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::response::Response;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;
use veille_emploi::http::{router, AppState};
use veille_emploi::status::StatusBoard;
use veille_emploi::store::Store;

fn app() -> (axum::Router, Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path());
    let state = AppState {
        store: store.clone(),
        status: StatusBoard::new(),
        version: "9.9.9-test",
    };
    (router(state), store, dir)
}

async fn send(app: &axum::Router, method: Method, uri: &str, accept_gzip: bool) -> Response {
    let mut req = Request::builder().method(method).uri(uri);
    if accept_gzip {
        req = req.header(header::ACCEPT_ENCODING, "gzip");
    }
    app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
}

async fn get(app: &axum::Router, uri: &str) -> Response {
    send(app, Method::GET, uri, false).await
}

async fn body_string(res: Response) -> String {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn hdr(res: &Response, name: &str) -> String {
    res.headers()
        .get(name)
        .unwrap_or_else(|| panic!("en-tete {name} absent"))
        .to_str()
        .unwrap()
        .to_string()
}

fn assert_security_headers(res: &Response) {
    assert_eq!(
        hdr(res, "content-security-policy"),
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'"
    );
    assert_eq!(hdr(res, "x-content-type-options"), "nosniff");
    assert_eq!(hdr(res, "referrer-policy"), "no-referrer");
    assert_eq!(hdr(res, "x-frame-options"), "DENY");
}

#[tokio::test]
async fn index_served_with_security_headers() {
    let (app, _s, _d) = app();
    let res = get(&app, "/").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(hdr(&res, "content-type").starts_with("text/html"));
    assert_security_headers(&res);
    assert!(body_string(res).await.to_lowercase().contains("<html"));
}

#[tokio::test]
async fn security_headers_on_404_and_405() {
    let (app, _s, _d) = app();
    let r404 = get(&app, "/nope").await;
    assert_eq!(r404.status(), StatusCode::NOT_FOUND);
    assert_security_headers(&r404);
    let r405 = send(&app, Method::POST, "/", false).await;
    assert_eq!(r405.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_security_headers(&r405);
}

#[tokio::test]
async fn static_assets_have_correct_types() {
    let (app, _s, _d) = app();
    let js = get(&app, "/app.js").await;
    assert_eq!(js.status(), StatusCode::OK);
    assert_eq!(hdr(&js, "content-type"), "application/javascript; charset=utf-8");
    let css = get(&app, "/style.css").await;
    assert_eq!(css.status(), StatusCode::OK);
    assert!(hdr(&css, "content-type").starts_with("text/css"));
    let chart = get(&app, "/vendor/chart.umd.min.js").await;
    assert_eq!(chart.status(), StatusCode::OK);
    assert_eq!(hdr(&chart, "content-type"), "application/javascript; charset=utf-8");
    assert!(!body_string(chart).await.is_empty());
}

#[tokio::test]
async fn data_file_404_when_missing() {
    let (app, _s, _d) = app();
    let res = get(&app, "/data/data.js").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_security_headers(&res);
}

#[tokio::test]
async fn data_file_served_after_save() {
    let (app, store, _d) = app();
    store.save("data.js", &json!({"a": [1, 2]})).unwrap();
    let res = get(&app, "/data/data.js").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(hdr(&res, "content-type"), "application/javascript; charset=utf-8");
    assert_eq!(hdr(&res, "cache-control"), "public, max-age=300");
    assert_security_headers(&res);
    assert_eq!(body_string(res).await, r#"window.DATA={"a":[1,2]};"#);
    // relu a chaque requete
    store.save("data.js", &json!({"a": 3})).unwrap();
    let res = get(&app, "/data/data.js").await;
    assert_eq!(body_string(res).await, r#"window.DATA={"a":3};"#);
}

#[tokio::test]
async fn unknown_data_name_is_404() {
    let (app, _s, dir) = app();
    // Des fichiers non listes dans le dossier de donnees ne doivent jamais sortir.
    std::fs::write(dir.path().join("secret.js"), "SECRET").unwrap();
    std::fs::write(dir.path().join("data.js.bak"), "SECRET").unwrap();
    std::fs::write(dir.path().join(".env"), "FT_CLIENT_SECRET=SECRET").unwrap();
    for uri in [
        "/data/..%2f.env",
        "/data/%2e%2e/.env",
        "/data/secret.js",
        "/data/data.js.bak",
        "/data/..%5c.env",
        "/data/%2e%2e%2fdata.js",
        "/data/",
    ] {
        let res = get(&app, uri).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
        let body = body_string(res).await;
        assert!(!body.contains("SECRET"), "{uri}");
        assert!(!body.contains("FT_CLIENT"), "{uri}");
    }
}

#[tokio::test]
async fn health_has_no_secret_and_lists_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path());
    store.save("rome.js", &json!({})).unwrap();
    let status = StatusBoard::new();
    status.record_err("ft", chrono::Utc::now(), "boom");
    let app = router(AppState { store, status, version: "9.9.9-test" });
    let res = get(&app, "/health").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(hdr(&res, "content-type").starts_with("application/json"));
    assert_security_headers(&res);
    let text = body_string(res).await;
    assert!(!text.contains("FT_CLIENT"));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["version"], "9.9.9-test");
    assert_eq!(v["collectors"]["ft"]["last_error"], "boom");
    assert_eq!(v["files"]["rome.js"]["exists"], true);
    assert!(v["files"]["rome.js"]["age_seconds"].is_number());
    assert_eq!(v["files"]["data.js"]["exists"], false);
    assert!(v["files"]["data.js"]["age_seconds"].is_null());
    assert_eq!(v["files"].as_object().unwrap().len(), 5);
}

#[tokio::test]
async fn post_is_405() {
    let (app, store, _d) = app();
    store.save("data.js", &json!({})).unwrap();
    for uri in ["/", "/data/data.js", "/health", "/app.js"] {
        for m in [Method::POST, Method::PUT, Method::DELETE] {
            let res = send(&app, m.clone(), uri, false).await;
            assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED, "{m} {uri}");
        }
    }
}

#[tokio::test]
async fn unknown_path_is_404() {
    let (app, _s, _d) = app();
    for uri in ["/nope", "/.env", "/server/Cargo.toml", "/vendor/other.js", "/index.html"] {
        let res = get(&app, uri).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn gzip_when_requested() {
    let (app, store, _d) = app();
    store.save("data.js", &json!({"x": "y".repeat(5000)})).unwrap();
    let res = send(&app, Method::GET, "/data/data.js", true).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(hdr(&res, "content-encoding"), "gzip");
    assert_security_headers(&res);
    let plain = get(&app, "/data/data.js").await;
    assert!(plain.headers().get("content-encoding").is_none());
}
