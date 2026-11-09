//! Client de l'API France Travail « Marché du travail » : jeton OAuth mis en cache, reprise sur
//! erreurs transitoires, limitation de débit.
//!
//! Règle de sécurité : ni le secret client, ni le jeton, ni l'identifiant client n'apparaissent
//! jamais dans une erreur (`Display`/`Debug`), un journal `tracing` ou un `Debug` de structure.
//! Les erreurs ne portent aucune donnée issue du réseau (ni URL, ni corps, ni message `reqwest`).
//! Le secret ne circule que dans le corps du formulaire de la requête de jeton.

use crate::config::FtCreds;
use serde_json::Value;
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

const SCOPE: &str = "api_stats-offres-demandes-emploiv1 offresetdemandesemploi";
const TRIES: u32 = 5;
/// Le jeton est renouvelé 60 s avant son expiration (comme `fetch_rome.py`).
const SAFETY_MARGIN: Duration = Duration::from_secs(60);
const DEFAULT_EXPIRES_IN: u64 = 1400;
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// URLs du service d'authentification et de l'API (sans `/` final pour `base_url`).
#[derive(Debug, Clone)]
pub struct FtEndpoints {
    pub token_url: String,
    pub base_url: String,
}

impl FtEndpoints {
    pub fn production() -> Self {
        FtEndpoints {
            token_url:
                "https://entreprise.francetravail.fr/connexion/oauth2/access_token?realm=%2Fpartenaire"
                    .into(),
            base_url: "https://api.francetravail.io/partenaire/stats-offres-demandes-emploi".into(),
        }
    }
}

/// Échec d'un appel. Aucune variante ne porte de donnée sensible : le `Display` est fixe.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FtError {
    #[error("authentification France Travail refusée")]
    Auth,
    #[error("France Travail a répondu HTTP {0}")]
    Http(u16),
    #[error("France Travail injoignable (erreur réseau)")]
    Network,
    #[error("réponse France Travail illisible")]
    Parse,
}

impl FtError {
    fn retryable(&self) -> bool {
        matches!(
            self,
            FtError::Network | FtError::Http(429) | FtError::Http(500..)
        )
    }
}

/// Source de temps monotone du jeton (durée écoulée depuis un point d'origine arbitraire).
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

/// Horloge réelle.
pub struct SystemClock(std::time::Instant);

impl SystemClock {
    pub fn new() -> Self {
        SystemClock(std::time::Instant::now())
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Attente entre deux essais : `base × numéro de l'essai` (1,5 s, 3 s, 4,5 s… en production).
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub base: Duration,
}

impl Backoff {
    pub fn production() -> Self {
        Backoff {
            base: Duration::from_millis(1500),
        }
    }
    pub fn none() -> Self {
        Backoff {
            base: Duration::ZERO,
        }
    }
}

/// Limiteur à fenêtre glissante : jamais plus de `per_second` acquisitions sur une seconde.
pub struct RateLimiter {
    per_second: usize,
    stamps: Mutex<VecDeque<Instant>>,
}

impl RateLimiter {
    pub fn new(per_second: u32) -> Self {
        RateLimiter {
            per_second: per_second.max(1) as usize,
            stamps: Mutex::new(VecDeque::new()),
        }
    }

    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut s = self.stamps.lock().await;
                let now = Instant::now();
                while s
                    .front()
                    .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(1))
                {
                    s.pop_front();
                }
                if s.len() < self.per_second {
                    s.push_back(now);
                    return;
                }
                // Plein : on attend que la plus ancienne sorte de la fenêtre.
                (s[0] + Duration::from_secs(1)).saturating_duration_since(now)
            };
            tokio::time::sleep(wait).await;
        }
    }
}

struct Token {
    value: String,
    expires_at: Duration,
}

pub struct FtClient {
    http: reqwest::Client,
    creds: FtCreds,
    ep: FtEndpoints,
    limiter: RateLimiter,
    backoff: Backoff,
    clock: Arc<dyn Clock>,
    token: Mutex<Option<Token>>,
}

// Pas de `derive(Debug)` : jeton et secret ne doivent jamais pouvoir être affichés.
impl fmt::Debug for FtClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FtClient")
            .field("ep", &self.ep)
            .finish_non_exhaustive()
    }
}

impl FtClient {
    pub fn new(
        http: reqwest::Client,
        creds: FtCreds,
        ep: FtEndpoints,
        limiter: RateLimiter,
    ) -> Self {
        FtClient {
            http,
            creds,
            ep,
            limiter,
            backoff: Backoff::production(),
            clock: Arc::new(SystemClock::new()),
            token: Mutex::new(None),
        }
    }

    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Jeton valide, en le renouvelant si besoin. Le verrou est tenu pendant le renouvellement :
    /// plusieurs appelants simultanés ne déclenchent qu'une seule requête de jeton.
    async fn token(&self) -> Result<String, FtError> {
        let mut guard = self.token.lock().await;
        let now = self.clock.now();
        if let Some(t) = guard.as_ref() {
            if now + SAFETY_MARGIN < t.expires_at {
                return Ok(t.value.clone());
            }
        }
        tracing::debug!("France Travail : demande d'un nouveau jeton");
        let fresh = self.fetch_token().await?;
        let value = fresh.value.clone();
        *guard = Some(fresh);
        Ok(value)
    }

    async fn fetch_token(&self) -> Result<Token, FtError> {
        let resp = self
            .http
            .post(&self.ep.token_url)
            .timeout(TOKEN_TIMEOUT)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.creds.client_id.as_str()),
                ("client_secret", self.creds.client_secret.as_str()),
                ("scope", SCOPE),
            ])
            .send()
            .await
            .map_err(|_| {
                tracing::warn!("France Travail : jeton, erreur réseau");
                FtError::Network
            })?;
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(
                status = status.as_u16(),
                "France Travail : jeton refusé ou indisponible"
            );
            return Err(match status.as_u16() {
                400 | 401 | 403 => FtError::Auth,
                c => FtError::Http(c),
            });
        }
        let body: Value = resp.json().await.map_err(|_| {
            tracing::warn!("France Travail : réponse de jeton illisible");
            FtError::Parse
        })?;
        let value = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                tracing::warn!("France Travail : réponse de jeton sans access_token");
                FtError::Parse
            })?
            .to_string();
        let expires_in = body
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_EXPIRES_IN);
        tracing::debug!(expires_in, "France Travail : jeton obtenu");
        Ok(Token {
            value,
            expires_at: self.clock.now() + Duration::from_secs(expires_in),
        })
    }

    /// Oublie le jeton s'il est toujours celui qui vient d'être refusé.
    async fn invalidate(&self, rejected: &str) {
        let mut guard = self.token.lock().await;
        if guard.as_ref().is_some_and(|t| t.value == rejected) {
            *guard = None;
        }
    }

    /// Appelle l'API (POST JSON si `body`, sinon GET). `Ok(None)` sur 204 ou 404.
    /// Reprise (5 essais) sur 429, 5xx et erreur réseau ; un 401 déclenche un seul renouvellement
    /// du jeton puis un nouvel essai.
    pub async fn call(&self, path: &str, body: Option<&Value>) -> Result<Option<Value>, FtError> {
        let payload = match body.map(serde_json::to_vec) {
            Some(Ok(p)) => Some(p),
            Some(Err(_)) => return Err(FtError::Parse),
            None => None,
        };
        let url = format!("{}{}", self.ep.base_url, path);
        let mut renewed = false;
        let mut last = FtError::Network;
        for attempt in 1..=TRIES {
            if attempt > 1 {
                tokio::time::sleep(self.backoff.base * (attempt - 1)).await;
            }
            let token = match self.token().await {
                Ok(t) => t,
                Err(e) if e.retryable() => {
                    tracing::warn!(
                        attempt,
                        tries = TRIES,
                        "France Travail : jeton indisponible, nouvel essai"
                    );
                    last = e;
                    continue;
                }
                Err(e) => return Err(e),
            };
            self.limiter.acquire().await;
            let req = match &payload {
                Some(p) => self.http.post(&url).body(p.clone()),
                None => self.http.get(&url),
            };
            let sent = req
                .timeout(CALL_TIMEOUT)
                .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await;
            let resp = match sent {
                Ok(r) => r,
                Err(_) => {
                    tracing::warn!(
                        attempt,
                        tries = TRIES,
                        "France Travail : erreur réseau, nouvel essai"
                    );
                    last = FtError::Network;
                    continue;
                }
            };
            let status = resp.status().as_u16();
            match status {
                204 | 404 => return Ok(None),
                401 => {
                    if renewed {
                        tracing::warn!(
                            status,
                            attempt,
                            "France Travail : refus persistant après renouvellement du jeton"
                        );
                        return Err(FtError::Auth);
                    }
                    renewed = true;
                    tracing::warn!(
                        status,
                        attempt,
                        "France Travail : refus, renouvellement du jeton puis nouvel essai"
                    );
                    self.invalidate(&token).await;
                    last = FtError::Auth;
                    continue;
                }
                429 | 500.. => {
                    tracing::warn!(
                        status,
                        attempt,
                        tries = TRIES,
                        "France Travail : erreur temporaire, nouvel essai"
                    );
                    last = FtError::Http(status);
                    continue;
                }
                s if !(200..300).contains(&s) => {
                    tracing::warn!(status = s, attempt, "France Travail : statut inattendu");
                    return Err(FtError::Http(s));
                }
                _ => {}
            }
            match resp.json::<Value>().await {
                Ok(v) => return Ok(Some(v)),
                Err(e) if e.is_decode() => {
                    tracing::warn!(attempt, "France Travail : JSON invalide");
                    return Err(FtError::Parse);
                }
                Err(_) => {
                    tracing::warn!(
                        attempt,
                        tries = TRIES,
                        "France Travail : lecture interrompue, nouvel essai"
                    );
                    last = FtError::Network;
                }
            }
        }
        tracing::warn!(
            tries = TRIES,
            "France Travail : abandon après tous les essais"
        );
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use tracing_subscriber::fmt::MakeWriter;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    const SECRET: &str = "fake-secret-123";
    const CLIENT_ID: &str = "PAR_fake_client";
    const TOKEN: &str = "fake-token-abc";

    fn creds() -> FtCreds {
        FtCreds {
            client_id: CLIENT_ID.into(),
            client_secret: SECRET.into(),
        }
    }

    /// Horloge contrôlable.
    struct FakeClock(AtomicU64);
    impl FakeClock {
        fn new() -> Arc<Self> {
            Arc::new(FakeClock(AtomicU64::new(0)))
        }
        fn advance(&self, secs: u64) {
            self.0.fetch_add(secs * 1000, Ordering::SeqCst);
        }
    }
    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            Duration::from_millis(self.0.load(Ordering::SeqCst))
        }
    }

    /// Réponses de jeton successives : `fake-token-abc-1`, `-2`, …
    struct SeqToken {
        n: AtomicUsize,
        expires_in: u64,
        delay: Duration,
    }
    impl SeqToken {
        fn new(expires_in: u64) -> Self {
            SeqToken {
                n: AtomicUsize::new(0),
                expires_in,
                delay: Duration::ZERO,
            }
        }
    }
    impl Respond for SeqToken {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            let n = self.n.fetch_add(1, Ordering::SeqCst) + 1;
            ResponseTemplate::new(200)
                .set_delay(self.delay)
                .set_body_json(json!({
                    "access_token": format!("{TOKEN}-{n}"),
                    "expires_in": self.expires_in,
                    "token_type": "Bearer",
                }))
        }
    }

    fn tok(n: usize) -> String {
        format!("Bearer {TOKEN}-{n}")
    }

    fn endpoints(srv: &MockServer) -> FtEndpoints {
        FtEndpoints {
            token_url: format!("{}/token?realm=%2Fpartenaire", srv.uri()),
            base_url: format!("{}/api", srv.uri()),
        }
    }

    fn client_with(ep: FtEndpoints, clock: Arc<FakeClock>) -> FtClient {
        FtClient::new(reqwest::Client::new(), creds(), ep, RateLimiter::new(1000))
            .with_backoff(Backoff::none())
            .with_clock(clock)
    }

    fn client(srv: &MockServer) -> (FtClient, Arc<FakeClock>) {
        let clock = FakeClock::new();
        (client_with(endpoints(srv), clock.clone()), clock)
    }

    async fn mount_token(srv: &MockServer, r: SeqToken) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(r)
            .mount(srv)
            .await;
    }

    async fn count(srv: &MockServer, p: &str) -> usize {
        srv.received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == p)
            .count()
    }

    fn ok_json() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({"ok": true}))
    }

    #[tokio::test]
    async fn token_requested_once_and_cached() {
        let srv = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(query_realm())
            .and(body_string_contains("grant_type=client_credentials"))
            .and(body_string_contains(format!("client_id={CLIENT_ID}")))
            .and(body_string_contains(format!("client_secret={SECRET}")))
            .and(body_string_contains(
                "scope=api_stats-offres-demandes-emploiv1+offresetdemandesemploi",
            ))
            .respond_with(SeqToken::new(1400))
            .expect(1)
            .mount(&srv)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/x"))
            .and(header("authorization", tok(1).as_str()))
            .and(header("content-type", "application/json"))
            .and(header("accept", "application/json"))
            .respond_with(ok_json())
            .expect(3)
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        for _ in 0..3 {
            let r = c.call("/v1/x", Some(&json!({"a": 1}))).await.unwrap();
            assert_eq!(r, Some(json!({"ok": true})));
        }
    }

    fn query_realm() -> wiremock::matchers::QueryParamExactMatcher {
        wiremock::matchers::query_param("realm", "/partenaire")
    }

    #[tokio::test]
    async fn get_is_used_without_body() {
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/ref"))
            .respond_with(ok_json())
            .expect(1)
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        assert!(c.call("/v1/ref", None).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn token_refreshed_when_close_to_expiry() {
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(120)).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/x"))
            .respond_with(ok_json())
            .mount(&srv)
            .await;
        let (c, clock) = client(&srv);
        c.call("/v1/x", None).await.unwrap();
        clock.advance(59); // reste 61 s > marge de 60 s : on garde
        c.call("/v1/x", None).await.unwrap();
        assert_eq!(count(&srv, "/token").await, 1);
        clock.advance(2); // reste 59 s < 60 s : renouvelé
        c.call("/v1/x", None).await.unwrap();
        assert_eq!(count(&srv, "/token").await, 2);
        let last = srv.received_requests().await.unwrap();
        let last = last.iter().rfind(|r| r.url.path() == "/api/v1/x").unwrap();
        assert_eq!(last.headers.get("authorization").unwrap(), tok(2).as_str());
    }

    #[tokio::test]
    async fn token_expired_mid_run_is_renewed_after_401() {
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/x"))
            .and(header("authorization", tok(1).as_str()))
            .respond_with(ResponseTemplate::new(401).set_body_string("expired"))
            .expect(1)
            .mount(&srv)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/x"))
            .and(header("authorization", tok(2).as_str()))
            .respond_with(ok_json())
            .expect(1)
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        let r = c.call("/v1/x", None).await.unwrap();
        assert_eq!(r, Some(json!({"ok": true})));
        assert_eq!(count(&srv, "/token").await, 2);
    }

    #[tokio::test]
    async fn persistent_401_renews_once_then_fails_with_auth() {
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/x"))
            .respond_with(ResponseTemplate::new(401))
            .expect(2)
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        assert_eq!(c.call("/v1/x", None).await.unwrap_err(), FtError::Auth);
        assert_eq!(count(&srv, "/token").await, 2);
    }

    #[tokio::test]
    async fn retries_on_429_then_succeeds() {
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/x"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(2)
            .expect(2)
            .mount(&srv)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/x"))
            .respond_with(ok_json())
            .expect(1)
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        assert_eq!(
            c.call("/v1/x", None).await.unwrap(),
            Some(json!({"ok": true}))
        );
        assert_eq!(count(&srv, "/token").await, 1);
    }

    #[tokio::test]
    async fn retries_on_5xx_then_succeeds() {
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/x"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&srv)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/x"))
            .respond_with(ok_json())
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        assert!(c.call("/v1/x", None).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn gives_up_after_five_attempts() {
        for (status, want) in [(429u16, FtError::Http(429)), (500, FtError::Http(500))] {
            let srv = MockServer::start().await;
            mount_token(&srv, SeqToken::new(1400)).await;
            Mock::given(method("GET"))
                .and(path("/api/v1/x"))
                .respond_with(ResponseTemplate::new(status))
                .expect(5)
                .mount(&srv)
                .await;
            let (c, _) = client(&srv);
            assert_eq!(c.call("/v1/x", None).await.unwrap_err(), want);
        }
    }

    #[tokio::test]
    async fn backoff_waits_base_times_attempt() {
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/x"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&srv)
            .await;
        let clock = FakeClock::new();
        let c = client_with(endpoints(&srv), clock).with_backoff(Backoff {
            base: Duration::from_millis(20),
        });
        let t = std::time::Instant::now();
        c.call("/v1/x", None).await.unwrap_err();
        // 20 + 40 + 60 + 80 ms entre les 5 essais
        assert!(
            t.elapsed() >= Duration::from_millis(200),
            "{:?}",
            t.elapsed()
        );
    }

    #[tokio::test]
    async fn no_content_and_not_found_give_none() {
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        for (p, s) in [("/api/a", 204u16), ("/api/b", 404)] {
            Mock::given(method("GET"))
                .and(path(p))
                .respond_with(ResponseTemplate::new(s))
                .expect(1)
                .mount(&srv)
                .await;
        }
        let (c, _) = client(&srv);
        assert_eq!(c.call("/a", None).await.unwrap(), None);
        assert_eq!(c.call("/b", None).await.unwrap(), None);
    }

    #[tokio::test]
    async fn other_client_error_is_not_retried() {
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        Mock::given(method("GET"))
            .and(path("/api/x"))
            .respond_with(ResponseTemplate::new(400))
            .expect(1)
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        assert_eq!(c.call("/x", None).await.unwrap_err(), FtError::Http(400));
    }

    #[tokio::test]
    async fn invalid_json_is_parse_error() {
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        Mock::given(method("GET"))
            .and(path("/api/x"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>pas du json"))
            .expect(1)
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        assert_eq!(c.call("/x", None).await.unwrap_err(), FtError::Parse);
    }

    #[tokio::test]
    async fn token_response_without_access_token_is_parse_error() {
        let srv = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"nope": 1})))
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        assert_eq!(c.call("/x", None).await.unwrap_err(), FtError::Parse);
    }

    #[tokio::test]
    async fn concurrent_callers_trigger_a_single_token_request() {
        let srv = MockServer::start().await;
        let mut slow = SeqToken::new(1400);
        slow.delay = Duration::from_millis(100);
        mount_token(&srv, slow).await;
        Mock::given(method("GET"))
            .and(path("/api/x"))
            .respond_with(ok_json())
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        let c = &c;
        let rs = futures::future::join_all((0..8).map(|_| c.call("/x", None))).await;
        assert!(rs.iter().all(|r| matches!(r, Ok(Some(_)))));
        assert_eq!(count(&srv, "/token").await, 1);
    }

    #[test]
    fn production_endpoints_are_the_documented_ones() {
        let e = FtEndpoints::production();
        assert_eq!(
            e.token_url,
            "https://entreprise.francetravail.fr/connexion/oauth2/access_token?realm=%2Fpartenaire"
        );
        assert_eq!(
            e.base_url,
            "https://api.francetravail.io/partenaire/stats-offres-demandes-emploi"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limiter_allows_at_most_n_per_second() {
        let lim = RateLimiter::new(8);
        let start = Instant::now();
        let mut stamps = vec![];
        for _ in 0..24 {
            lim.acquire().await;
            stamps.push(Instant::now());
        }
        // les 8 premières passent sans attendre
        assert!(stamps[7].duration_since(start) < Duration::from_millis(10));
        // jamais 9 acquisitions dans une même seconde
        for i in 0..stamps.len() - 8 {
            assert!(
                stamps[i + 8].duration_since(stamps[i]) >= Duration::from_secs(1),
                "9 acquisitions en moins d'une seconde à partir de {i}"
            );
        }
        // 24 acquisitions à 8/s : au moins 2 s
        assert!(stamps[23].duration_since(start) >= Duration::from_secs(2));
    }

    // ---- secret ----

    #[derive(Clone, Default)]
    struct LogBuf(Arc<StdMutex<Vec<u8>>>);
    struct LogWriter(Arc<StdMutex<Vec<u8>>>);
    impl io::Write for LogWriter {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for LogBuf {
        type Writer = LogWriter;
        fn make_writer(&'a self) -> LogWriter {
            LogWriter(self.0.clone())
        }
    }
    impl LogBuf {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    fn assert_clean(what: &str, text: &str) {
        let forbidden = [
            SECRET,
            CLIENT_ID,
            TOKEN,
            "Bearer",
            "Authorization",
            "authorization",
        ];
        for f in forbidden {
            assert!(!text.contains(f), "{what} contient « {f} » : {text}");
        }
    }

    /// Serveur TCP qui accepte puis coupe chaque connexion : erreur réseau immédiate.
    async fn dropping_server() -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((sock, _)) = l.accept().await {
                drop(sock);
            }
        });
        port
    }

    #[tokio::test]
    async fn secret_never_appears_in_errors_or_logs() {
        let logs = LogBuf::default();
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(logs.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(sub);

        // Les corps d'erreur du serveur répètent le secret : ils ne doivent rien laisser fuiter.
        let echo = format!("invalid client {SECRET} {CLIENT_ID} {TOKEN}");
        let mut errors: Vec<(&str, FtError)> = vec![];

        // 1. 401 sur le jeton
        let srv = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(401).set_body_string(echo.clone()))
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        errors.push(("token 401", c.call("/x", None).await.unwrap_err()));
        let debug_client = format!("{c:?}");
        assert_clean("Debug du client", &debug_client);

        // 2. 401 sur l'API (jeton valide, refusé deux fois)
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        Mock::given(method("GET"))
            .and(path("/api/x"))
            .respond_with(ResponseTemplate::new(401).set_body_string(echo.clone()))
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        errors.push(("api 401", c.call("/x", None).await.unwrap_err()));
        assert_clean("Debug du client après appel", &format!("{c:?}"));

        // 3. 500 sur l'API
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        Mock::given(method("GET"))
            .and(path("/api/x"))
            .respond_with(ResponseTemplate::new(500).set_body_string(echo.clone()))
            .mount(&srv)
            .await;
        let (c, _) = client(&srv);
        errors.push(("api 500", c.call("/x", None).await.unwrap_err()));

        // 4. réseau : le serveur coupe chaque connexion aussitôt (un port fermé met ~2 s par
        // tentative sous Windows, ce qui rendrait le test lent)
        let port = dropping_server().await;
        let ep = FtEndpoints {
            token_url: format!("http://127.0.0.1:{port}/token?realm=%2Fpartenaire"),
            base_url: format!("http://127.0.0.1:{port}/api"),
        };
        let c = client_with(ep, FakeClock::new());
        errors.push(("réseau", c.call("/x", None).await.unwrap_err()));

        // 5. jeton obtenu, puis API injoignable
        let srv = MockServer::start().await;
        mount_token(&srv, SeqToken::new(1400)).await;
        let ep = FtEndpoints {
            token_url: format!("{}/token", srv.uri()),
            base_url: format!("http://127.0.0.1:{port}/api"),
        };
        let c = client_with(ep, FakeClock::new());
        errors.push(("api réseau", c.call("/x", None).await.unwrap_err()));

        assert_eq!(
            errors.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>(),
            [
                FtError::Auth,
                FtError::Auth,
                FtError::Http(500),
                FtError::Network,
                FtError::Network
            ]
        );
        for (what, e) in &errors {
            assert_clean(&format!("{what} (Display)"), &format!("{e}"));
            assert_clean(&format!("{what} (Debug)"), &format!("{e:?}"));
        }
        let text = logs.text();
        assert!(!text.is_empty(), "les journaux capturés sont vides");
        assert!(
            text.contains("essai") && text.contains("status"),
            "journaux attendus absents : {text}"
        );
        assert_clean("journaux", &text);
    }
}
