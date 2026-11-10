//! Valeurs par défaut : copie des deux fichiers de données manuels (`ai_exposure.js`,
//! `ai_rome.js`) s'ils manquent. Ne touche jamais aux fichiers collectés.

use rust_embed::RustEmbed;
use std::io::Write;
use std::path::Path;

/// Seuls ces deux fichiers du dépôt sont embarqués (jamais d'autre fichier de `data/`, qui peut
/// contenir des données collectées réelles, ni `.env`).
#[derive(RustEmbed)]
#[folder = ".."]
#[include = "data/ai_exposure.js"]
#[include = "data/ai_rome.js"]
struct Defaults;

const DEFAULT_FILES: [&str; 2] = ["ai_exposure.js", "ai_rome.js"];

/// Crée `data_dir` s'il manque et y copie les valeurs par défaut absentes. Renvoie les noms créés.
/// Un fichier existant (même modifié à la main) n'est jamais remplacé.
pub fn seed_defaults(data_dir: &Path) -> std::io::Result<Vec<String>> {
    std::fs::create_dir_all(data_dir)?;
    let mut created = Vec::new();
    for name in DEFAULT_FILES {
        let emb = Defaults::get(&format!("data/{name}")).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "valeur par défaut non embarquée",
            )
        })?;
        if create_if_absent(&data_dir.join(name), emb.data.as_ref())? {
            created.push(name.to_string());
        }
    }
    Ok(created)
}

/// Écrit `bytes` dans `target` seulement si `target` n'existe pas ; `true` si créé.
///
/// Le contenu est d'abord écrit en entier dans un fichier temporaire du même dossier, puis
/// publié par `hard_link`, qui échoue avec `AlreadyExists` si la cible existe (même apparue
/// entre-temps) : aucun écrasement possible, et jamais de fichier à moitié écrit sous le nom
/// final. Si le système de fichiers refuse les liens physiques, repli sur `create_new` (toujours
/// sans écrasement, mais non atomique : un fichier partiel est supprimé en cas d'échec).
fn create_if_absent(target: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target.file_name().unwrap_or_default();
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.seed.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let res = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        match std::fs::hard_link(&tmp, target) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(_) => create_new_fallback(target, bytes),
        }
    })();
    let _ = std::fs::remove_file(&tmp);
    res
}

fn create_new_fallback(target: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    let mut f = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e),
    };
    if let Err(e) = f.write_all(bytes).and_then(|()| f.sync_all()) {
        drop(f);
        let _ = std::fs::remove_file(target);
        return Err(e);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> Vec<u8> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../data/").to_string() + name).unwrap()
    }

    #[test]
    fn seed_creates_missing_ai_files() {
        let dir = tempfile::tempdir().unwrap();
        let created = seed_defaults(dir.path()).unwrap();
        assert_eq!(created, ["ai_exposure.js", "ai_rome.js"]);
        for f in DEFAULT_FILES {
            assert_eq!(std::fs::read(dir.path().join(f)).unwrap(), repo(f), "{f}");
        }
    }

    #[test]
    fn seed_never_overwrites_existing_ai_file() {
        let dir = tempfile::tempdir().unwrap();
        let mine = b"window.AI={\"edite\":\"a la main\"};";
        std::fs::write(dir.path().join("ai_exposure.js"), mine).unwrap();
        let created = seed_defaults(dir.path()).unwrap();
        assert_eq!(created, ["ai_rome.js"]);
        assert_eq!(
            std::fs::read(dir.path().join("ai_exposure.js")).unwrap(),
            mine
        );
        assert_eq!(
            std::fs::read(dir.path().join("ai_rome.js")).unwrap(),
            repo("ai_rome.js")
        );
    }

    #[test]
    fn seed_leaves_collected_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        // Absents : jamais créés.
        seed_defaults(dir.path()).unwrap();
        for f in ["data.js", "rome.js", "tech.js", "tension.js"] {
            assert!(!dir.path().join(f).exists(), "{f} ne doit pas être créé");
        }
        // Présents : octets identiques.
        for (f, body) in [
            ("data.js", &b"window.DATA={\"a\":1};"[..]),
            ("rome.js", b"window.ROME={};"),
            ("tech.js", b"window.TECH={};"),
            ("tension.js", b"window.TENSION={};"),
        ] {
            std::fs::write(dir.path().join(f), body).unwrap();
        }
        seed_defaults(dir.path()).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("data.js")).unwrap(),
            b"window.DATA={\"a\":1};"
        );
        assert_eq!(
            std::fs::read(dir.path().join("rome.js")).unwrap(),
            b"window.ROME={};"
        );
        assert_eq!(
            std::fs::read(dir.path().join("tech.js")).unwrap(),
            b"window.TECH={};"
        );
        assert_eq!(
            std::fs::read(dir.path().join("tension.js")).unwrap(),
            b"window.TENSION={};"
        );
    }

    #[test]
    fn seed_creates_missing_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a").join("b");
        assert_eq!(seed_defaults(&sub).unwrap().len(), 2);
        assert!(sub.join("ai_rome.js").is_file());
    }

    #[test]
    fn seed_is_idempotent_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(seed_defaults(dir.path()).unwrap().len(), 2);
        assert!(seed_defaults(dir.path()).unwrap().is_empty());
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["ai_exposure.js", "ai_rome.js"]);
    }

    #[test]
    fn seed_fails_cleanly_when_data_dir_is_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("fichier");
        std::fs::write(&f, b"x").unwrap();
        assert!(seed_defaults(&f).is_err());
        assert!(seed_defaults(&f.join("sous")).is_err());
    }

    #[test]
    fn embedded_defaults_equal_repo_files_and_nothing_else() {
        let mut names: Vec<String> = Defaults::iter().map(|n| n.to_string()).collect();
        names.sort();
        assert_eq!(names, ["data/ai_exposure.js", "data/ai_rome.js"]);
        for f in DEFAULT_FILES {
            let emb = Defaults::get(&format!("data/{f}")).unwrap();
            assert_eq!(emb.data.as_ref(), repo(f).as_slice(), "{f}");
        }
        for other in ["data/rome.js", "data/tech.js", "data/tension.js", "data/data.js", ".env"] {
            assert!(
                Defaults::get(other).is_none(),
                "{other} ne doit pas être embarqué"
            );
        }
    }
}
