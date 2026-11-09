//! Intégration : collecte `core` contre un serveur simulé, puis serveur HTTP sur le même dossier.
//! Aucun accès réseau réel.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use veille_emploi::collect::core::{run, Sources};
use veille_emploi::collect::CollectError;
use veille_emploi::http::{router, AppState};
use veille_emploi::schedule::{run_job, Job, Runner, WallClock};
use veille_emploi::seed::seed_defaults;
use veille_emploi::status::StatusBoard;
use veille_emploi::store::Store;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");
const DARES: [(&str, &str); 3] = [
    (
        "dares_offres_collectees_satisfaites_france_travail_cvs-cjo_mens",
        "dares/offres.csv",
    ),
    ("dares_defm_stock_france_cvs", "dares/dei.csv"),
    (
        "dares_emploivacants_cvs_emploisvacants",
        "dares/vacants.csv",
    ),
];

fn fixture(rel: &str) -> Vec<u8> {
    std::fs::read(format!("{FIX}{rel}")).unwrap()
}

fn ok(rel: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_bytes(fixture(rel))
}

/// Serveur simulé des 9 requêtes de `core` ; `fail_chomage` fait échouer Eurostat (chômage).
async fn server(fail_chomage: bool) -> MockServer {
    let srv = MockServer::start().await;
    for (p, f) in [
        (
            "/indeed/aggregate_job_postings_FR.csv",
            "indeed/aggregate.csv",
        ),
        (
            "/indeed/job_postings_by_sector_FR.csv",
            "indeed/sectors.csv",
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ok(f))
            .mount(&srv)
            .await;
    }
    let chomage = if fail_chomage {
        ResponseTemplate::new(503).set_body_string("secret-corps-http")
    } else {
        ok("eurostat/chomage.json")
    };
    Mock::given(method("GET"))
        .and(path("/eurostat/une_rt_m"))
        .respond_with(chomage)
        .mount(&srv)
        .await;
    for (item, f) in [
        ("B1GQ", "eurostat/pib.json"),
        ("P6", "eurostat/exports.json"),
        ("P7", "eurostat/imports.json"),
    ] {
        Mock::given(method("GET"))
            .and(path("/eurostat/namq_10_gdp"))
            .and(query_param("na_item", item))
            .respond_with(ok(f))
            .mount(&srv)
            .await;
    }
    for (ds, f) in DARES {
        Mock::given(method("GET"))
            .and(path(format!("/dares/{ds}/exports/csv")))
            .respond_with(ok(f))
            .mount(&srv)
            .await;
    }
    srv
}

fn sources(srv: &MockServer) -> Sources {
    Sources {
        indeed: format!("{}/indeed/", srv.uri()),
        eurostat: format!("{}/eurostat/", srv.uri()),
        dares: format!("{}/dares/", srv.uri()),
    }
}

/// Même chemin que le planificateur : `run_job` collecte puis consigne dans le `StatusBoard`.
struct CoreRunner {
    http: reqwest::Client,
    src: Sources,
    store: Store,
}

impl Runner for CoreRunner {
    async fn run(&self, job: Job) -> Result<(), CollectError> {
        assert_eq!(job, Job::Core);
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        run(&self.http, &self.src, &self.store, today).await
    }
}

struct FixedClock(DateTime<Utc>);
impl WallClock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

fn clock_at(rfc3339: &str) -> FixedClock {
    FixedClock(rfc3339.parse().unwrap())
}

async fn get(app: &axum::Router, uri: &str) -> Response {
    app.clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn bytes(res: Response) -> Vec<u8> {
    res.into_body().collect().await.unwrap().to_bytes().to_vec()
}

fn app_for(store: &Store, status: &StatusBoard) -> axum::Router {
    router(AppState {
        store: store.clone(),
        status: status.clone(),
        version: "test",
    })
}

fn runner(srv: &MockServer, store: &Store) -> CoreRunner {
    CoreRunner {
        http: reqwest::Client::new(),
        src: sources(srv),
        store: store.clone(),
    }
}

#[tokio::test]
async fn integration_server_serves_everything() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(seed_defaults(dir.path()).unwrap().len(), 2);
    let srv = server(false).await;
    let store = Store::new(dir.path());
    let status = StatusBoard::new();
    run_job(
        &runner(&srv, &store),
        &clock_at("2026-11-09T10:00:00Z"),
        &status,
        Job::Core,
    )
    .await
    .unwrap();
    let app = app_for(&store, &status);

    for uri in ["/", "/app.js", "/style.css", "/vendor/chart.umd.min.js"] {
        let res = get(&app, uri).await;
        assert_eq!(res.status(), StatusCode::OK, "{uri}");
        assert!(!bytes(res).await.is_empty(), "{uri}");
    }
    let res = get(&app, "/data/data.js").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(bytes(res).await.starts_with(b"window.DATA="));
    let res = get(&app, "/data/ai_rome.js").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        bytes(res).await,
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../data/ai_rome.js")).unwrap()
    );
    assert_eq!(
        get(&app, "/data/rome.js").await.status(),
        StatusCode::NOT_FOUND
    );

    let res = get(&app, "/health").await;
    assert_eq!(res.status(), StatusCode::OK);
    let h: Value = serde_json::from_slice(&bytes(res).await).unwrap();
    assert_eq!(h["collectors"]["core"]["last_ok"], "2026-11-09T10:00:00Z");
    assert!(h["collectors"]["core"]["last_error"].is_null());
    assert_eq!(h["files"]["data.js"]["exists"], true);
    assert_eq!(h["files"]["rome.js"]["exists"], false);
}

#[tokio::test]
async fn integration_failure_visible_in_health_and_old_data_served() {
    let dir = tempfile::tempdir().unwrap();
    seed_defaults(dir.path()).unwrap();
    let store = Store::new(dir.path());
    let status = StatusBoard::new();
    let good = server(false).await;
    run_job(
        &runner(&good, &store),
        &clock_at("2026-11-09T10:00:00Z"),
        &status,
        Job::Core,
    )
    .await
    .unwrap();
    let app = app_for(&store, &status);
    let before = bytes(get(&app, "/data/data.js").await).await;
    assert!(before.starts_with(b"window.DATA="));

    let bad = server(true).await;
    let err = run_job(
        &runner(&bad, &store),
        &clock_at("2026-11-09T11:00:00Z"),
        &status,
        Job::Core,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("eurostat une_rt_m"), "{err}");

    let res = get(&app, "/data/data.js").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        bytes(res).await,
        before,
        "l'ancien data.js doit rester servi"
    );

    let raw = bytes(get(&app, "/health").await).await;
    let text = String::from_utf8(raw.clone()).unwrap();
    let h: Value = serde_json::from_slice(&raw).unwrap();
    let core = &h["collectors"]["core"];
    assert!(core["last_error"]
        .as_str()
        .unwrap()
        .contains("eurostat une_rt_m"));
    assert_eq!(core["last_ok"], "2026-11-09T10:00:00Z");
    assert_eq!(core["last_attempt"], "2026-11-09T11:00:00Z");
    assert!(
        !text.contains("secret-corps-http") && !text.contains('?') && !text.contains("http://"),
        "{text}"
    );
}
