//! Stockage atomique des fichiers de données `window.VAR=...;`.

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DATA_FILES: [(&str, &str); 5] = [
    ("data.js", "DATA"),
    ("rome.js", "ROME"),
    ("tech.js", "TECH"),
    ("ai_exposure.js", "AI"),
    ("ai_rome.js", "AIROME"),
];

/// Produit `window.VAR=<json compact>;` (JSON UTF-8, accents conservés).
pub fn render_js(var: &str, value: &Value) -> String {
    format!("window.{}={};", var, value)
}

/// Écrit via un fichier temporaire du même dossier puis `rename`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "chemin sans nom"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Store { dir: dir.into() }
    }

    pub fn is_known(file: &str) -> bool {
        DATA_FILES.iter().any(|(f, _)| *f == file)
    }

    fn var_of(file: &str) -> Option<&'static str> {
        DATA_FILES.iter().find(|(f, _)| *f == file).map(|(_, v)| *v)
    }

    pub fn save(&self, file: &str, value: &Value) -> std::io::Result<()> {
        let var = Self::var_of(file).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "fichier inconnu")
        })?;
        write_atomic(&self.dir.join(file), render_js(var, value).as_bytes())
    }

    pub fn read(&self, file: &str) -> std::io::Result<Vec<u8>> {
        if !Self::is_known(file) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "fichier inconnu",
            ));
        }
        std::fs::read(self.dir.join(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_js_is_compact_and_terminated() {
        assert_eq!(render_js("DATA", &json!({"a":[1,2]})), r#"window.DATA={"a":[1,2]};"#);
    }

    #[test]
    fn render_js_keeps_accents() {
        let s = render_js("DATA", &json!({"k":"é"}));
        assert!(s.contains('é'));
        assert!(!s.contains(r"\u00e9"));
    }

    #[test]
    fn write_atomic_replaces_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.js");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn write_atomic_failure_keeps_old_file() {
        let d = tempfile::tempdir().unwrap();
        let ok = d.path().join("x.js");
        write_atomic(&ok, b"old").unwrap();
        let bad = d.path().join("missing").join("x.js");
        assert!(write_atomic(&bad, b"new").is_err());
        assert_eq!(std::fs::read(&ok).unwrap(), b"old");
    }

    #[test]
    fn store_rejects_unknown_file_name() {
        assert!(!Store::is_known("../.env"));
        assert!(Store::is_known("rome.js"));
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        assert!(s.read("../.env").is_err());
        assert!(s.save("evil.js", &json!({})).is_err());
    }

    #[test]
    fn store_save_and_read_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        s.save("tech.js", &json!({"a":1})).unwrap();
        assert_eq!(s.read("tech.js").unwrap(), br#"window.TECH={"a":1};"#);
    }

    #[test]
    fn save_to_missing_dir_errs_and_creates_nothing() {
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("absent");
        let s = Store::new(&missing);
        assert!(s.save("data.js", &json!({})).is_err());
        assert!(!missing.exists());
    }
}
