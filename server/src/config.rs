//! Configuration du serveur, lue depuis l'environnement et un fichier `.env` optionnel.
//! Aucun secret n'apparaît dans `Debug` ni dans les messages d'erreur.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

const DEFAULT_BIND: &str = "127.0.0.1:3210";
const DEFAULT_DATA_DIR: &str = "./data";

/// Identifiants OAuth France Travail.
#[derive(Clone, PartialEq, Eq)]
pub struct FtCreds {
    pub client_id: String,
    pub client_secret: String,
}

impl fmt::Debug for FtCreds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FtCreds")
            .field("client_id", &self.client_id)
            .field("client_secret", &"***")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub ft: Option<FtCreds>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("VE_BIND invalide : {0}")]
    InvalidBind(String),
    #[error("FT_CLIENT_ID manquant alors que FT_CLIENT_SECRET est défini")]
    MissingFtId,
    #[error("FT_CLIENT_SECRET manquant alors que FT_CLIENT_ID est défini")]
    MissingFtSecret,
}

impl Config {
    pub fn from_map(vars: &HashMap<String, String>) -> Result<Config, ConfigError> {
        let get = |k: &str| vars.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());

        let bind_raw = get("VE_BIND").unwrap_or(DEFAULT_BIND);
        let bind = bind_raw
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::InvalidBind(bind_raw.to_string()))?;
        let data_dir = PathBuf::from(get("VE_DATA_DIR").unwrap_or(DEFAULT_DATA_DIR));

        let ft = match (get("FT_CLIENT_ID"), get("FT_CLIENT_SECRET")) {
            (Some(id), Some(secret)) => Some(FtCreds {
                client_id: id.to_string(),
                client_secret: secret.to_string(),
            }),
            (None, None) => None,
            (Some(_), None) => return Err(ConfigError::MissingFtSecret),
            (None, Some(_)) => return Err(ConfigError::MissingFtId),
        };

        Ok(Config { bind, data_dir, ft })
    }

    /// Variables d'environnement, complétées par `.env` (l'environnement prime).
    pub fn from_env() -> Result<Config, ConfigError> {
        let mut vars = load_dotenv(Path::new(".env"));
        vars.extend(std::env::vars());
        Config::from_map(&vars)
    }
}

/// Lit un fichier `KEY=VALUE` ; ignore commentaires et lignes vides.
/// Un fichier absent ou illisible donne une table vide.
pub fn load_dotenv(path: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| {
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(v);
            (k.trim().to_string(), v.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn defaults_when_map_empty() {
        let cfg = Config::from_map(&HashMap::new()).unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:3210".parse::<SocketAddr>().unwrap());
        assert_eq!(cfg.data_dir, PathBuf::from("./data"));
        assert!(cfg.ft.is_none());
    }

    #[test]
    fn ft_loaded_when_both_set() {
        let cfg = Config::from_map(&map(&[("FT_CLIENT_ID", "id-1"), ("FT_CLIENT_SECRET", "fake-secret-123")])).unwrap();
        let ft = cfg.ft.unwrap();
        assert_eq!(ft.client_id, "id-1");
        assert_eq!(ft.client_secret, "fake-secret-123");
    }

    #[test]
    fn only_id_is_an_error() {
        let err = Config::from_map(&map(&[("FT_CLIENT_ID", "id-1")])).unwrap_err();
        assert_eq!(err, ConfigError::MissingFtSecret);
    }

    #[test]
    fn only_secret_is_an_error() {
        let err = Config::from_map(&map(&[("FT_CLIENT_SECRET", "fake-secret-123")])).unwrap_err();
        assert_eq!(err, ConfigError::MissingFtId);
    }

    #[test]
    fn invalid_bind_is_rejected() {
        let err = Config::from_map(&map(&[("VE_BIND", "pas-une-adresse")])).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidBind(_)));
    }

    #[test]
    fn debug_never_shows_secret() {
        let cfg = Config::from_map(&map(&[("FT_CLIENT_ID", "id-1"), ("FT_CLIENT_SECRET", "fake-secret-123")])).unwrap();
        let s = format!("{:?}", cfg);
        assert!(!s.contains("fake-secret-123"));
        assert!(s.contains("***"));
    }

    #[test]
    fn error_messages_never_contain_secret() {
        let errs = [
            Config::from_map(&map(&[("FT_CLIENT_SECRET", "fake-secret-123")])).unwrap_err(),
            Config::from_map(&map(&[("FT_CLIENT_ID", "id-1"), ("FT_CLIENT_SECRET", "fake-secret-123"), ("VE_BIND", "nope")])).unwrap_err(),
        ];
        for e in errs {
            assert!(!e.to_string().contains("fake-secret-123"));
            assert!(!format!("{:?}", e).contains("fake-secret-123"));
        }
    }

    #[test]
    fn dotenv_ignores_comments_and_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(&p, "# c\nA=1\n\nB=2").unwrap();
        let m = load_dotenv(&p);
        assert_eq!(m, map(&[("A", "1"), ("B", "2")]));
        assert!(load_dotenv(&dir.path().join("absent")).is_empty());
    }
}
