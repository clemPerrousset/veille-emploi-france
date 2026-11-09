//! Assemblage du programme : clients HTTP, validation de l'adresse d'écoute, serveur, commande
//! `once`. Aucun identifiant n'est jamais écrit dans les journaux ni dans les messages.

use crate::collect::ft::{FtClient, FtEndpoints, RateLimiter};
use crate::config::{Config, FtCreds};
use crate::http::{router, AppState};
use crate::schedule::{run_forever, run_job, Ctx, Job, SystemWallClock};
use crate::status::StatusBoard;
use crate::store::Store;
use chrono::{DateTime, Duration as Span, Utc};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const USER_AGENT: &str = "suivi-offre-emploi/1.0";
const CORE_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CORE_REDIRECTS: usize = 3;
/// L'API France Travail autorise 10 appels par seconde ; on s'en tient à 8.
pub const FT_CALLS_PER_SECOND: u32 = 8;

/// Les deux clients HTTP du programme.
pub struct Clients {
    /// Sources publiques (Indeed, Eurostat, DARES).
    pub core: reqwest::Client,
    /// France Travail, présent si les identifiants le sont.
    pub ft: Option<FtClient>,
}

/// Construit les clients avec les URL de production.
pub fn build_clients(ft: Option<&FtCreds>) -> Result<Clients, String> {
    build_clients_with(ft, FtEndpoints::production())
}

/// Comme `build_clients`, avec des URL données (tests).
///
/// SÉCURITÉ : le client France Travail ne suit AUCUNE redirection. Une redirection (307/308)
/// de l'URL de jeton renverrait le corps du formulaire, qui contient le secret client, vers un
/// autre hôte. Les délais d'attente par requête (jeton 30 s, appel 60 s) sont fixés par `ft.rs`.
pub fn build_clients_with(ft: Option<&FtCreds>, endpoints: FtEndpoints) -> Result<Clients, String> {
    let core = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(CORE_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(MAX_CORE_REDIRECTS))
        .build()
        .map_err(|_| "création du client HTTP impossible".to_string())?;
    let ft = match ft {
        None => None,
        Some(creds) => {
            let http = reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(CONNECT_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "création du client France Travail impossible".to_string())?;
            Some(FtClient::new(
                http,
                creds.clone(),
                endpoints,
                RateLimiter::new(FT_CALLS_PER_SECOND),
            ))
        }
    };
    Ok(Clients { core, ft })
}

/// L'adresse d'écoute doit être locale (127.0.0.0/8 ou `::1`) : le serveur n'a pas
/// d'authentification et ne doit pas être joignable depuis le réseau.
pub fn validate_bind(addr: SocketAddr) -> Result<SocketAddr, String> {
    if addr.ip().is_loopback() {
        Ok(addr)
    } else {
        Err(format!(
            "VE_BIND doit être une adresse locale (127.0.0.1 ou [::1]) : {addr} exposerait le \
             serveur au réseau, or il n'a aucune authentification"
        ))
    }
}

/// Reprend la date des fichiers de données déjà présents comme dernier succès connu, pour qu'un
/// redémarrage ne relance pas inutilement une collecte récente (et rattrape un vrai retard).
pub fn seed_status_from_files(store: &Store, status: &StatusBoard, now: DateTime<Utc>) {
    for (job, file) in [
        (Job::Core, "data.js"),
        (Job::Rome, "rome.js"),
        (Job::Tech, "tech.js"),
    ] {
        if let Some(age) = store.age_seconds(file) {
            let age = i64::try_from(age).unwrap_or(0).min(100 * 365 * 86_400);
            status.record_ok(job.name(), now - Span::seconds(age));
        }
    }
}

pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
}

fn build_ctx(cfg: Config) -> Result<Ctx, String> {
    let clients = build_clients(cfg.ft.as_ref())?;
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| {
        format!(
            "dossier de données inutilisable ({}) : {}",
            cfg.data_dir.display(),
            e.kind()
        )
    })?;
    let store = Store::new(cfg.data_dir.clone());
    let status = StatusBoard::new();
    seed_status_from_files(&store, &status, Utc::now());
    Ok(Ctx {
        http: clients.core,
        store,
        status,
        cfg,
        ft: clients.ft,
    })
}

/// Lignes de démarrage (jamais d'identifiants).
pub fn startup_lines(cfg: &Config) -> Vec<String> {
    let mut v = vec![
        format!("veille_emploi {}", env!("CARGO_PKG_VERSION")),
        format!("adresse d'écoute : {}", cfg.bind),
        format!("dossier de données : {}", cfg.data_dir.display()),
    ];
    if !cfg.scheduler_enabled {
        v.push(
            "planificateur désactivé (VE_DISABLE_SCHEDULER) : aucune collecte automatique".into(),
        );
    } else if cfg.ft.is_some() {
        v.push("collecteurs actifs : core, rome, tech".into());
    } else {
        v.push("collecteurs actifs : core".into());
        v.push("identifiants France Travail absents : rome et tech désactivés".into());
    }
    v
}

/// `serve` : serveur HTTP + planificateur. Renvoie le code de sortie.
pub async fn serve() -> i32 {
    match serve_inner().await {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("erreur : {msg}");
            1
        }
    }
}

async fn serve_inner() -> Result<(), String> {
    // `.env` du dossier courant (s'il existe), puis l'environnement du processus.
    let cfg = Config::from_env().map_err(|e| e.to_string())?;
    let bind = validate_bind(cfg.bind)?;
    for l in startup_lines(&cfg) {
        tracing::info!("{l}");
    }
    let scheduler = cfg.scheduler_enabled;
    let ctx = Arc::new(build_ctx(cfg)?);
    let app = router(AppState {
        store: ctx.store.clone(),
        status: ctx.status.clone(),
        version: env!("CARGO_PKG_VERSION"),
    });
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| format!("écoute impossible sur {bind} : {}", e.kind()))?;
    let server = axum::serve(listener, app).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("arrêt demandé (Ctrl-C)");
    });
    // Le planificateur tourne sur cette même tâche (son futur n'est pas `Send`, voir `Runner`) ;
    // il ne se termine jamais, l'arrêt du serveur met donc fin aux deux.
    let res = if scheduler {
        tokio::select! {
            r = server => r,
            () = run_forever(ctx.clone()) => Ok(()),
        }
    } else {
        server.await
    };
    res.map_err(|e| format!("serveur arrêté : {}", e.kind()))
}

/// `once <job>` : lance une collecte, affiche une ligne de résultat. Code 0 si réussie, 1 sinon.
pub async fn once(job: Job) -> i32 {
    let ctx = match Config::from_env()
        .map_err(|e| e.to_string())
        .and_then(build_ctx)
    {
        Ok(c) => c,
        Err(m) => {
            eprintln!("erreur : {m}");
            return 1;
        }
    };
    match run_job(&ctx, &SystemWallClock, &ctx.status, job).await {
        Ok(()) => {
            println!(
                "{} : collecte réussie (données dans {})",
                job.name(),
                ctx.cfg.data_dir.display()
            );
            0
        }
        Err(e) => {
            println!("{} : échec : {e}", job.name());
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::ft::FtError;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SECRET: &str = "fake-secret-123";
    const CLIENT_ID: &str = "PAR_fake_client";

    fn creds() -> FtCreds {
        FtCreds {
            client_id: CLIENT_ID.into(),
            client_secret: SECRET.into(),
        }
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn bind_accepts_loopback_only() {
        for ok in [
            "127.0.0.1:3210",
            "127.0.0.1:0",
            "127.5.5.5:80",
            "[::1]:3210",
        ] {
            assert_eq!(validate_bind(addr(ok)), Ok(addr(ok)), "{ok}");
        }
        for bad in [
            "0.0.0.0:3210",
            "192.168.1.20:3210",
            "10.0.0.1:80",
            "[::]:3210",
            "8.8.8.8:80",
        ] {
            let e = validate_bind(addr(bad)).unwrap_err();
            assert!(e.contains("VE_BIND") && e.contains("locale"), "{bad}: {e}");
        }
    }

    #[test]
    fn startup_lines_never_show_credentials_and_announce_missing_ft() {
        let mut cfg = Config::from_map(&Default::default()).unwrap();
        let text = startup_lines(&cfg).join("\n");
        assert!(text.contains("127.0.0.1:3210") && text.contains("collecteurs actifs : core"));
        assert!(
            text.contains("absents") && text.contains("rome et tech désactivés"),
            "{text}"
        );
        cfg.ft = Some(creds());
        let text = startup_lines(&cfg).join("\n");
        assert!(text.contains("core, rome, tech"), "{text}");
        assert!(
            !text.contains(SECRET) && !text.contains(CLIENT_ID),
            "{text}"
        );
    }

    #[test]
    fn seed_status_uses_file_ages() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.save("data.js", &serde_json::json!({"a": 1})).unwrap();
        let status = StatusBoard::new();
        seed_status_from_files(&store, &status, Utc::now());
        let snap = status.snapshot();
        assert!(snap["core"].last_ok.is_some());
        assert!(!snap.contains_key("rome") && !snap.contains_key("tech"));
    }

    #[tokio::test]
    async fn core_client_sends_user_agent_and_follows_a_few_redirects() {
        let srv = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", "/b"))
            .mount(&srv)
            .await;
        Mock::given(method("GET"))
            .and(path("/b"))
            .and(header("user-agent", "suivi-offre-emploi/1.0"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&srv)
            .await;
        let clients = build_clients(None).unwrap();
        assert!(clients.ft.is_none());
        let body = clients
            .core
            .get(format!("{}/a", srv.uri()))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
    }

    #[tokio::test]
    async fn core_client_stops_after_three_redirects() {
        let srv = MockServer::start().await;
        for (from, to) in [
            ("/r0", "/r1"),
            ("/r1", "/r2"),
            ("/r2", "/r3"),
            ("/r3", "/r4"),
        ] {
            Mock::given(method("GET"))
                .and(path(from))
                .respond_with(ResponseTemplate::new(302).insert_header("Location", to))
                .mount(&srv)
                .await;
        }
        let clients = build_clients(None).unwrap();
        assert!(clients
            .core
            .get(format!("{}/r0", srv.uri()))
            .send()
            .await
            .is_err());
    }

    #[tokio::test]
    async fn ft_client_does_not_follow_a_307_from_the_token_endpoint() {
        let evil = MockServer::start().await; // serveur B
        let origin = MockServer::start().await; // serveur A
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("Location", format!("{}/steal", evil.uri())),
            )
            .mount(&origin)
            .await;
        let clients = build_clients_with(
            Some(&creds()),
            FtEndpoints {
                token_url: format!("{}/token?realm=%2Fpartenaire", origin.uri()),
                base_url: format!("{}/api", origin.uri()),
            },
        )
        .unwrap();
        let ft = clients.ft.expect("client France Travail construit");
        let err = ft.call("/x", None).await.unwrap_err();
        assert!(matches!(err, FtError::Auth | FtError::Http(_)), "{err:?}");
        assert!(
            evil.received_requests().await.unwrap().is_empty(),
            "le serveur B n'aurait dû recevoir aucune requête"
        );
        let text = format!("{err} {err:?} {ft:?}");
        assert!(
            !text.contains(SECRET) && !text.contains(CLIENT_ID),
            "{text}"
        );
    }
}
