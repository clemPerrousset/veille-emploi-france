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
            .field("client_id", &"***")
            .field("client_secret", &"***")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub ft: Option<FtCreds>,
    /// `false` si `VE_DISABLE_SCHEDULER` vaut `1` ou `true` : le serveur sert les fichiers mais ne
    /// lance aucune collecte automatique (tests de fumée, exploitation en lecture seule).
    pub scheduler_enabled: bool,
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

        let scheduler_enabled = !matches!(
            get("VE_DISABLE_SCHEDULER").map(str::to_ascii_lowercase).as_deref(),
            Some("1" | "true")
        );

        Ok(Config {
            bind,
            data_dir,
            ft,
            scheduler_enabled,
        })
    }

    /// Variables d'environnement, complétées par `.env` (l'environnement prime).
    pub fn from_env() -> Result<Config, ConfigError> {
        let mut vars = load_dotenv(Path::new(".env"));
        vars.extend(env_map(std::env::vars_os()));
        Config::from_map(&vars)
    }
}

/// Variables d'environnement sous forme de table. Les entrées dont le nom ou la valeur n'est pas
/// de l'Unicode valide sont ignorées (`std::env::vars()` paniquerait).
pub(crate) fn env_map(
    entries: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> HashMap<String, String> {
    entries
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

/// Lit un fichier `KEY=VALUE` (BOM UTF-8 et préfixe `export ` acceptés) ; ignore commentaires,
/// lignes vides et lignes sans nom de variable.
/// Un fichier absent ou illisible donne une table vide.
pub fn load_dotenv(path: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .filter_map(|(k, v)| {
            let k = k.trim();
            let k = k.strip_prefix("export ").map_or(k, str::trim_start);
            if k.is_empty() || k == "export" {
                return None;
            }
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(v);
            Some((k.to_string(), v.to_string()))
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
    fn debug_never_shows_id_nor_secret() {
        let creds = FtCreds {
            client_id: "PAR_fake_client".into(),
            client_secret: "fake-secret-123".into(),
        };
        let s = format!("{creds:?}");
        assert!(!s.contains("PAR_fake_client") && !s.contains("fake-secret-123"), "{s}");
        assert!(s.contains("client_id") && s.contains("client_secret"), "{s}");
        let cfg = Config::from_map(&map(&[
            ("FT_CLIENT_ID", "PAR_fake_client"),
            ("FT_CLIENT_SECRET", "fake-secret-123"),
        ]))
        .unwrap();
        for s in [format!("{cfg:?}"), format!("{cfg:#?}")] {
            assert!(!s.contains("PAR_fake_client") && !s.contains("fake-secret-123"), "{s}");
        }
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

    #[test]
    fn dotenv_strips_bom_so_first_key_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(&p, "\u{feff}FIRST=1\nSECOND=2\n").unwrap();
        assert_eq!(load_dotenv(&p), map(&[("FIRST", "1"), ("SECOND", "2")]));
    }

    #[test]
    fn dotenv_accepts_export_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(&p, "export A=1\nexport   B = \"x y\"\n").unwrap();
        assert_eq!(load_dotenv(&p), map(&[("A", "1"), ("B", "x y")]));
    }

    #[test]
    fn dotenv_skips_lines_with_empty_key() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(&p, "=x\n  =y\nexport =z\nOK=1\n").unwrap();
        assert_eq!(load_dotenv(&p), map(&[("OK", "1")]));
    }

    #[test]
    fn env_map_ignores_non_unicode_entries_without_panicking() {
        use std::ffi::OsString;
        #[cfg(unix)]
        let bad: OsString = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![0x66, 0x80, 0x6f])
        };
        #[cfg(windows)]
        let bad: OsString = {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[0x66, 0xD800, 0x6f])
        };
        assert!(bad.to_str().is_none());
        let entries = vec![
            (OsString::from("GOOD"), OsString::from("1")),
            (OsString::from("BADVAL"), bad.clone()),
            (bad, OsString::from("v")),
        ];
        assert_eq!(env_map(entries.into_iter()), map(&[("GOOD", "1")]));
    }

    #[test]
    fn scheduler_can_be_disabled_by_variable() {
        assert!(Config::from_map(&HashMap::new()).unwrap().scheduler_enabled);
        for v in ["1", "true"] {
            let cfg = Config::from_map(&map(&[("VE_DISABLE_SCHEDULER", v)])).unwrap();
            assert!(!cfg.scheduler_enabled, "{v}");
        }
        let cfg = Config::from_map(&map(&[("VE_DISABLE_SCHEDULER", "0")])).unwrap();
        assert!(cfg.scheduler_enabled);
    }
}
