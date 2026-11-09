//! Stockage atomique des fichiers de données `window.VAR=...;`.

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Rend unique le nom du fichier temporaire pour chaque appel du processus.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    tmp_name.push(format!(".{}.{}.tmp", std::process::id(), n));
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

    /// Date de modification du fichier de données (`None` s'il est absent ou inconnu).
    pub fn modified(&self, file: &str) -> Option<std::time::SystemTime> {
        if !Self::is_known(file) {
            return None;
        }
        std::fs::metadata(self.dir.join(file)).ok()?.modified().ok()
    }

    /// Âge du fichier de données en secondes (date de modification), `None` s'il est absent.
    pub fn age_seconds(&self, file: &str) -> Option<u64> {
        if !Self::is_known(file) {
            return None;
        }
        let modified = std::fs::metadata(self.dir.join(file)).ok()?.modified().ok()?;
        Some(modified.elapsed().map(|d| d.as_secs()).unwrap_or(0))
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

    #[test]
    fn concurrent_saves_same_file_do_not_collide() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        let values: Vec<String> = (0..8).map(|i| format!("v{}", "x".repeat(200_000 + i))).collect();
        let handles: Vec<_> = values
            .iter()
            .cloned()
            .map(|v| {
                let s = s.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        s.save("data.js", &json!(v)).expect("save ok");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let got = String::from_utf8(s.read("data.js").unwrap()).unwrap();
        assert!(values.iter().any(|v| got == render_js("DATA", &json!(v))));
        let names: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["data.js"], "leftover temp files: {names:?}");
    }

    #[test]
    fn write_atomic_rename_failure_cleans_temp_and_keeps_old() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("old.js");
        write_atomic(&old, b"old").unwrap();
        let target = d.path().join("target_dir");
        std::fs::create_dir(&target).unwrap();
        assert!(write_atomic(&target, b"new").is_err());
        let mut names: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["old.js", "target_dir"]);
        assert_eq!(std::fs::read(&old).unwrap(), b"old");
    }

    #[test]
    fn is_known_is_exact_match() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        for bad in ["../rome.js", "sub/rome.js", "rome.js/", "rome.json", "ROME.JS", "", "..\rome.js"] {
            assert!(!Store::is_known(bad), "{bad:?}");
            assert!(s.read(bad).is_err(), "{bad:?}");
            assert!(s.save(bad, &json!({})).is_err(), "{bad:?}");
        }
        assert!(std::fs::read_dir(d.path()).unwrap().next().is_none());
    }
}
