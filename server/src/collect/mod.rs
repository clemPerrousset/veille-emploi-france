pub mod carte;
pub mod core;
pub mod dares;
pub mod eurostat;
pub mod ft;
pub mod indeed;
pub mod rome;
pub mod tech;
pub mod tension;

/// Erreur d'analyse d'une réponse de source de données.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// Contenu qui n'a pas la forme attendue (page d'erreur, colonnes absentes, nombre invalide…).
    #[error("format inattendu : {0}")]
    Format(String),
    /// Données lisibles mais incohérentes entre elles (dates différentes selon les catégories).
    #[error("données incohérentes : {0}")]
    Mismatch(String),
}

/// Échec d'une collecte. Les messages n'embarquent jamais d'URL avec paramètres ni de secret.
#[derive(Debug, thiserror::Error)]
pub enum CollectError {
    /// Source injoignable, statut HTTP non 2xx ou réponse vide.
    #[error("récupération impossible : {0}")]
    Fetch(String),
    /// Réponse reçue mais inexploitable.
    #[error("analyse impossible : {0}")]
    Parse(#[from] ParseError),
    /// Le fichier de données n'a pas pu être écrit (l'ancien reste en place).
    #[error("écriture impossible : {0}")]
    Write(String),
}

/// Appels à l'API « Marché du travail ». Implémenté par `FtClient` ; les tests de simultanéité
/// utilisent un faux qui compte les appels en cours.
pub trait StatApi: Sync {
    fn call<'a>(
        &'a self,
        path: &'a str,
        body: Option<&'a serde_json::Value>,
    ) -> impl std::future::Future<Output = Result<Option<serde_json::Value>, ft::FtError>> + Send + 'a;
}

impl StatApi for ft::FtClient {
    fn call<'a>(
        &'a self,
        path: &'a str,
        body: Option<&'a serde_json::Value>,
    ) -> impl std::future::Future<Output = Result<Option<serde_json::Value>, ft::FtError>> + Send + 'a
    {
        ft::FtClient::call(self, path, body)
    }
}

/// Convertit un échec d'appel en erreur de collecte nommant la source (le `Display` de `FtError`
/// est fixe : jamais de secret, d'URL ni de corps).
pub(crate) fn ft_error(label: &str, e: ft::FtError) -> CollectError {
    match e {
        ft::FtError::Parse => CollectError::Parse(ParseError::Format(format!(
            "{label} : réponse France Travail illisible"
        ))),
        other => CollectError::Fetch(format!("{label} : {other}")),
    }
}

/// Nombre JSON comme Python l'écrit : `15710.0` devient `15710` (entier), le reste est conservé.
pub(crate) fn num(f: f64) -> serde_json::Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15 {
        serde_json::Value::from(f as i64)
    } else {
        serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null)
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::ft::{Backoff, FtClient, FtEndpoints, RateLimiter};
    use crate::config::FtCreds;
    use crate::store::Store;
    use serde_json::Value;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub const SECRET: &str = "fake-secret-123";
    pub const CLIENT_ID: &str = "PAR_fake_client";
    pub const TOKEN: &str = "fake-token-abc";
    pub const FT_FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ft/");
    pub const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/");

    pub fn day() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 11, 9).unwrap()
    }

    pub fn fixture(name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(format!("{FT_FIX}{name}.json")).unwrap()).unwrap()
    }

    pub fn golden(name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(format!("{GOLDEN}{name}.json")).unwrap()).unwrap()
    }

    /// Serveur simulé avec la route `POST /token` ; l'API est servie sous `/api`.
    pub async fn server() -> MockServer {
        let srv = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"access_token": TOKEN, "expires_in": 1500})),
            )
            .mount(&srv)
            .await;
        srv
    }

    /// Client rapide : pas d'attente entre les essais, limiteur très large.
    pub fn client(srv: &MockServer) -> FtClient {
        FtClient::new(
            reqwest::Client::new(),
            FtCreds {
                client_id: CLIENT_ID.into(),
                client_secret: SECRET.into(),
            },
            FtEndpoints {
                token_url: format!("{}/token?realm=%2Fpartenaire", srv.uri()),
                base_url: format!("{}/api", srv.uri()),
            },
            RateLimiter::new(1000),
        )
        .with_backoff(Backoff::none())
    }

    /// Faux client France Travail : le comportement dépend du point d'accès et du code.
    pub struct FakeApi(pub fn(&str, &str) -> Behavior);

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Behavior {
        /// Une valeur dans la série voulue.
        Data,
        /// Réponse valide sans aucune ligne.
        Empty,
        /// Échec HTTP 500 (après reprise).
        Fail,
    }

    #[allow(clippy::manual_async_fn)] // la borne `Send` du trait exige la forme explicite
    impl super::StatApi for FakeApi {
        fn call<'a>(
            &'a self,
            path: &'a str,
            body: Option<&'a Value>,
        ) -> impl std::future::Future<Output = Result<Option<Value>, super::ft::FtError>> + Send + 'a
        {
            async move {
                let endpoint = path.rsplit('/').next().unwrap_or("");
                let code = body
                    .and_then(|b| b["codeActivite"].as_str())
                    .unwrap_or("")
                    .to_string();
                match (self.0)(endpoint, &code) {
                    Behavior::Fail => Err(super::ft::FtError::Http(500)),
                    Behavior::Empty => Ok(Some(serde_json::json!({"listeValeursParPeriode": []}))),
                    Behavior::Data => {
                        let (nomen, period) = match endpoint {
                            "stat-demandeurs" => ("A", "2022T1"),
                            "stat-embauches" => ("TOUT-TOUTE", "2022T1"),
                            _ => ("PERSPECTIVE", "2024"),
                        };
                        Ok(Some(serde_json::json!({"listeValeursParPeriode": [
                            {"codeNomenclature": nomen, "codePeriode": period,
                             "libActivite": "Lib", "valeurPrincipaleNombre": 3}]})))
                    }
                }
            }
        }
    }

    pub fn with_sentinel(file: &str, var: &str) -> (tempfile::TempDir, Store, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = format!("window.{var}={{\"sentinel\":\"ancien\"}};").into_bytes();
        std::fs::write(dir.path().join(file), &sentinel).unwrap();
        let store = Store::new(dir.path());
        (dir, store, sentinel)
    }

    /// Contenu d'un fichier `window.VAR=...;` écrit par un collecteur.
    pub fn payload(dir: &std::path::Path, file: &str, var: &str) -> Value {
        let s = std::fs::read_to_string(dir.join(file)).unwrap();
        let inner = s
            .strip_prefix(&format!("window.{var}="))
            .unwrap()
            .strip_suffix(';')
            .unwrap();
        serde_json::from_str(inner).unwrap()
    }

    pub fn keys(v: &Value) -> Vec<&str> {
        v.as_object().unwrap().keys().map(String::as_str).collect()
    }
}
