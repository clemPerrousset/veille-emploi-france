//! Collecteur `tension` : demandeurs d'emploi (cat. A, trimestriels) et tension de recrutement
//! (annuelle) des métiers ROME les plus volumineux → `tension.js`. Même résultat que
//! `fetch_tension.py`, lancé par le job `tech` juste après `tech.js`.
//!
//! Sélection : les `TOP_N` codes de `rome.js` (déjà présent dans le dossier de données, lu au moment
//! du job) au plus gros cumul d'offres sur les 4 derniers trimestres ; égalité : ordre du code.
//!
//! Écarts voulus avec le Python, pour ne jamais écraser un bon fichier par un mauvais :
//! - `rome.js` absent, illisible ou sans aucun métier : échec explicite, rien n'est écrit ;
//! - appels : mêmes points d'accès et même reprise que `tech`, mais un code dont un appel échoue
//!   (après les 5 essais) est ignoré, au lieu de donner une série vide comme `fetch_rome.call`
//!   (qui renvoie `None` après ses essais) ; plus de 10 % de codes en échec : échec, rien n'est écrit ;
//! - un code sans aucune donnée n'est pas un échec (il figure avec `t` et `d` vides, comme en Python) ;
//!   mais si aucun code ne fournit de donnée, rien n'est écrit (comme `rome` et `tech`).

use super::rome::MIN_PERIOD;
use super::tech::series;
use super::{CollectError, ParseError, StatApi};
use crate::store::Store;
use chrono::NaiveDate;
use futures::stream::{self, StreamExt};
use serde_json::{json, Map, Value};

/// Nombre de métiers retenus (les plus volumineux).
pub const TOP_N: usize = 300;

/// Part maximale (exclusive) de codes en échec tolérée : 1/10 = 10 % (comme `rome`).
const MAX_FAILURE_RATIO: (usize, usize) = (1, 10);

fn format_error(label: &str, detail: &str) -> CollectError {
    CollectError::Parse(ParseError::Format(format!("{label} : {detail}")))
}

/// Contenu de `rome.js` (déjà collecté) lu par le `Store`. `label` nomme le collecteur dans les
/// messages (`tension`, `carte`).
pub(super) fn read_rome(store: &Store, label: &str) -> Result<Value, CollectError> {
    let bytes = store.read("rome.js").map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CollectError::Fetch(format!(
                "{label} : rome.js absent (la collecte rome doit avoir réussi avant)"
            ))
        } else {
            CollectError::Fetch(format!("{label} : rome.js illisible ({})", e.kind()))
        }
    })?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format_error(label, "rome.js n'est pas de l'UTF-8"))?;
    let inner = text
        .trim()
        .strip_prefix("window.ROME=")
        .ok_or_else(|| format_error(label, "rome.js n'a pas la forme « window.ROME=… »"))?;
    let inner = inner.strip_suffix(';').unwrap_or(inner);
    serde_json::from_str(inner).map_err(|_| format_error(label, "rome.js : JSON illisible"))
}

/// Les `n` codes au plus gros cumul d'offres sur les 4 derniers trimestres (valeurs nulles = 0) ;
/// égalité : ordre du code (comme `top_codes` du Python).
pub fn top_codes(rome: &Value, n: usize) -> Result<Vec<String>, CollectError> {
    top_codes_for(rome, n, "tension")
}

/// Comme `top_codes`, les messages d'erreur portant le nom `label` du collecteur.
pub(super) fn top_codes_for(
    rome: &Value,
    n: usize,
    label: &str,
) -> Result<Vec<String>, CollectError> {
    let items = rome
        .get("items")
        .and_then(Value::as_object)
        .ok_or_else(|| format_error(label, "rome.js : champ « items » absent"))?;
    let mut vol: Vec<(&str, f64)> = Vec::with_capacity(items.len());
    for (code, item) in items {
        let offers = item.get("o").and_then(Value::as_array).ok_or_else(|| {
            format_error(label, &format!("rome.js : métier {code} sans série « o »"))
        })?;
        let mut total = 0.0;
        for x in &offers[offers.len().saturating_sub(4)..] {
            match x {
                Value::Null => {}
                Value::Number(v) => total += v.as_f64().unwrap_or(0.0),
                _ => {
                    return Err(format_error(
                        label,
                        &format!("rome.js : métier {code} avec une valeur non numérique"),
                    ))
                }
            }
        }
        vol.push((code.as_str(), total));
    }
    vol.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    Ok(vol
        .into_iter()
        .take(n)
        .map(|(c, _)| c.to_string())
        .collect())
}

/// `{"t": {AAAA: niveau}, "d": {AAAATn: demandeurs cat. A}}` d'un code ; le moindre appel en échec
/// est une erreur nommant le code et le point d'accès.
async fn fetch_code<C: StatApi>(client: &C, code: &str) -> Result<Value, CollectError> {
    let (d, _) = series(
        client,
        "tension",
        code,
        "stat-demandeurs",
        "TRIMESTRE",
        "CATCAND",
        "A",
    )
    .await?;
    let (t, _) = series(
        client,
        "tension",
        code,
        "stat-perspective-employeur",
        "ANNEE",
        "TYPE_TENSION",
        "PERSPECTIVE",
    )
    .await?;
    let d: Map<String, Value> = d
        .into_iter()
        .filter(|(p, _)| p.as_str() >= MIN_PERIOD)
        .collect();
    Ok(json!({"t": t, "d": d}))
}

/// `tension.js` pour les `TOP_N` premiers métiers de `rome.js` (`workers` codes simultanés au plus).
pub async fn run<C: StatApi>(
    client: &C,
    store: &Store,
    today: NaiveDate,
    workers: usize,
) -> Result<(), CollectError> {
    run_with(client, store, today, TOP_N, workers).await
}

/// Comme `run`, pour `n` métiers (les tests réduisent `n`, comme le module Python).
pub async fn run_with<C: StatApi>(
    client: &C,
    store: &Store,
    today: NaiveDate,
    n: usize,
    workers: usize,
) -> Result<(), CollectError> {
    let codes = top_codes(&read_rome(store, "tension")?, n)?;
    if codes.is_empty() {
        return Err(format_error("tension", "rome.js ne contient aucun métier"));
    }
    let fetched: Vec<(&str, Result<Value, CollectError>)> = stream::iter(&codes)
        .map(|code| async move { (code.as_str(), fetch_code(client, code).await) })
        .buffered(workers.max(1))
        .collect()
        .await;

    let mut items = Map::new();
    let mut failures = 0usize;
    let mut first_failure = None;
    let mut any_data = false;
    for (code, res) in fetched {
        match res {
            Ok(item) => {
                any_data |= item["t"].as_object().is_some_and(|m| !m.is_empty())
                    || item["d"].as_object().is_some_and(|m| !m.is_empty());
                items.insert(code.to_string(), item);
            }
            Err(e) => {
                tracing::warn!(code, error = %e, "tension : métier ignoré");
                failures += 1;
                first_failure.get_or_insert(e);
            }
        }
    }
    // Garde (seuil exclusif, arithmétique entière) : trop d'échecs = ne rien écraser.
    if failures * MAX_FAILURE_RATIO.1 > codes.len() * MAX_FAILURE_RATIO.0 {
        return Err(CollectError::Fetch(format!(
            "tension : {failures} échec(s) sur {} code(s)",
            codes.len()
        )));
    }
    if !any_data {
        return Err(CollectError::Fetch(format!(
            "tension : aucun métier n'a fourni de données ({failures} échec(s) sur {} métiers{})",
            codes.len(),
            first_failure
                .map(|e| format!(", premier échec : {e}"))
                .unwrap_or_default()
        )));
    }
    let out = json!({
        "updated": today.format("%Y-%m-%d").to_string(),
        "n": items.len(),
        "items": items,
    });
    store
        .save("tension.js", &out)
        .map_err(|e| CollectError::Write(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::rome::DEFAULT_WORKERS;
    use super::super::testutil::*;
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Codes de `golden/rome.json` : K1303 > M1805 > A1401 en cumul des 4 derniers trimestres.
    const CODES: [&str; 3] = ["M1805", "A1401", "K1303"];

    /// (point d'accès, préfixe de fixture, nomenclature de requête, type de période)
    const CALLS: [(&str, &str, &str, &str); 2] = [
        ("stat-demandeurs", "demandeurs", "CATCAND", "TRIMESTRE"),
        (
            "stat-perspective-employeur",
            "tension",
            "TYPE_TENSION",
            "ANNEE",
        ),
    ];

    /// Magasin de test : `rome.js` (donné) et un `tension.js` sentinelle.
    fn store_with_rome(rome: &Value) -> (tempfile::TempDir, Store, Vec<u8>) {
        let (dir, store, sentinel) = with_sentinel("tension.js", "TENSION");
        store.save("rome.js", rome).unwrap();
        (dir, store, sentinel)
    }

    async fn tension_server(
        codes: &[&str],
        per_call: impl Fn(&str, &str) -> ResponseTemplate,
    ) -> MockServer {
        let srv = server().await;
        for code in codes {
            for (ep, prefix, nomen, period) in CALLS {
                Mock::given(method("POST"))
                    .and(path(format!("/api/v1/indicateur/{ep}")))
                    .and(body_partial_json(json!({
                        "codeActivite": code, "codeTypeActivite": "ROME",
                        "codeTypeNomenclature": nomen, "codeTypePeriode": period,
                        "codeTypeTerritoire": "NAT", "codeTerritoire": "FR"})))
                    .respond_with(per_call(prefix, code))
                    .mount(&srv)
                    .await;
            }
        }
        srv
    }

    fn fixture_resp(prefix: &str, code: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(fixture(&format!("{prefix}_{code}")))
    }

    /// `rome.js` minimal : `(code, o)`.
    fn rome_of(items: &[(&str, Value)]) -> Value {
        let items: Map<String, Value> = items
            .iter()
            .map(|(c, o)| (c.to_string(), json!({"l": "x", "o": o})))
            .collect();
        json!({"updated": "2026-11-09", "periods": [], "items": items})
    }

    fn no_leftover(dir: &std::path::Path, expected: usize) {
        let names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), expected, "fichiers résiduels : {names:?}");
    }

    // --- sélection ---------------------------------------------------------------------------

    #[test]
    fn top_codes_sorts_by_last_four_quarters_then_code() {
        let rome = golden("rome");
        assert_eq!(
            top_codes(&rome, TOP_N).unwrap(),
            ["K1303", "M1805", "A1401"]
        );
        assert_eq!(top_codes(&rome, 2).unwrap(), ["K1303", "M1805"]);
        assert!(top_codes(&rome, 0).unwrap().is_empty());
        // seuls les 4 derniers trimestres comptent ; moins de 4 valeurs : toutes ; null = 0
        let rome = rome_of(&[
            ("B", json!([1000, 1, 1, 1, 1])),
            ("A", json!([null, 2, 2, 2, null])),
            ("C", json!([9])),
            ("D", json!([])),
        ]);
        // B = 4, A = 4 (égalité : A avant B), C = 9, D = 0
        assert_eq!(top_codes(&rome, 10).unwrap(), ["C", "A", "B", "D"]);
    }

    #[test]
    fn top_codes_breaks_ties_by_code_and_is_stable_on_input_order() {
        let a = rome_of(&[
            ("M2", json!([5])),
            ("M10", json!([5])),
            ("A9", json!([5])),
            ("Z1", json!([6])),
        ]);
        let b = rome_of(&[
            ("Z1", json!([6])),
            ("A9", json!([5])),
            ("M10", json!([5])),
            ("M2", json!([5])),
        ]);
        let want = ["Z1", "A9", "M10", "M2"]; // ordre du texte : « M10 » avant « M2 »
        assert_eq!(top_codes(&a, 4).unwrap(), want);
        assert_eq!(top_codes(&b, 4).unwrap(), want);
        // la coupe à n départage aussi par le code
        assert_eq!(top_codes(&a, 2).unwrap(), ["Z1", "A9"]);
        // décimaux et sommes égales par chemins différents
        let rome = rome_of(&[("B", json!([1.5, 1.5])), ("A", json!([3]))]);
        assert_eq!(top_codes(&rome, 2).unwrap(), ["A", "B"]);
    }

    #[test]
    fn top_codes_rejects_malformed_rome() {
        for bad in [
            json!({}),
            json!({"items": []}),
            json!({"items": {"A": {"l": "x"}}}),
            json!({"items": {"A": {"o": "texte"}}}),
            json!({"items": {"A": {"o": [1, "2"]}}}),
        ] {
            let err = top_codes(&bad, 5).unwrap_err();
            assert!(matches!(err, CollectError::Parse(_)), "{bad}: {err}");
        }
    }

    // --- parité avec le Python ---------------------------------------------------------------

    #[tokio::test]
    async fn tension_matches_python_golden() {
        let srv = tension_server(&CODES, fixture_resp).await;
        let (dir, store, _) = store_with_rome(&golden("rome"));
        run(&client(&srv), &store, day(), DEFAULT_WORKERS)
            .await
            .unwrap();
        let got = payload(dir.path(), "tension.js", "TENSION");
        let gold = golden("tension");
        assert_eq!(got, gold);
        assert_eq!(keys(&got), ["updated", "n", "items"]);
        assert_eq!(got["n"], 3);
        assert_eq!(got["updated"], "2026-11-09");
        // ordre des métiers : volume décroissant (K1303, M1805, A1401), comme Python
        assert_eq!(keys(&got["items"]), ["K1303", "M1805", "A1401"]);
        assert_eq!(keys(&got["items"]["K1303"]), ["t", "d"]);
        // octet pour octet : mêmes nombres, mêmes ordres de clés
        let text = std::fs::read_to_string(dir.path().join("tension.js")).unwrap();
        assert_eq!(text, format!("window.TENSION={};", gold));
        // demandeurs : cat. A seulement, périodes >= 2021T1 ; tension : PERSPECTIVE par année
        let d = got["items"]["M1805"]["d"].as_object().unwrap();
        assert!(!d.is_empty() && d.keys().all(|p| p.as_str() >= MIN_PERIOD));
        assert!(got["items"]["M1805"]["t"]
            .as_object()
            .unwrap()
            .values()
            .all(|v| v.is_u64()));
        no_leftover(dir.path(), 2); // rome.js + tension.js
                                    // deux appels par métier, sans aucun appel pour les codes non retenus
        let posts = srv.received_requests().await.unwrap();
        let posts: Vec<_> = posts
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .collect();
        let api = posts
            .iter()
            .filter(|r| r.url.path().starts_with("/api/"))
            .count();
        assert_eq!(api, 6);
    }

    #[tokio::test]
    async fn tension_with_reduced_n_matches_python_golden() {
        // A1401 est écarté : aucune route pour lui (un appel donnerait un 404, donc un trou visible)
        let srv = tension_server(&["M1805", "K1303"], fixture_resp).await;
        let (dir, store, _) = store_with_rome(&golden("rome"));
        run_with(&client(&srv), &store, day(), 2, DEFAULT_WORKERS)
            .await
            .unwrap();
        let got = payload(dir.path(), "tension.js", "TENSION");
        let gold = golden("tension_top2");
        assert_eq!(got, gold);
        assert_eq!(keys(&got["items"]), ["K1303", "M1805"]);
        let text = std::fs::read_to_string(dir.path().join("tension.js")).unwrap();
        assert_eq!(text, format!("window.TENSION={};", gold));
        let n_api = srv
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().starts_with("/api/"))
            .count();
        assert_eq!(n_api, 4);
    }

    #[tokio::test]
    async fn code_without_data_is_kept_empty_like_python() {
        // A1215 (réponses réelles) : aucun demandeur, mais une tension annuelle
        let rome = rome_of(&[("M1805", json!([5])), ("A1215", json!([1]))]);
        let srv = tension_server(&["M1805", "A1215"], fixture_resp).await;
        let (dir, store, _) = store_with_rome(&rome);
        run(&client(&srv), &store, day(), DEFAULT_WORKERS)
            .await
            .unwrap();
        let got = payload(dir.path(), "tension.js", "TENSION");
        assert_eq!(got["n"], 2);
        assert_eq!(got["items"]["A1215"]["d"], json!({}));
        assert_eq!(got["items"]["A1215"]["t"]["2025"], 2);
        assert_eq!(got["items"]["M1805"], golden("tension")["items"]["M1805"]);
        // sans aucune donnée : un 404 / 204 (réponse « vide ») n'est pas non plus un échec
        let srv = tension_server(&["M1805", "A1215"], |prefix, code| {
            if code == "A1215" {
                ResponseTemplate::new(404)
            } else {
                fixture_resp(prefix, code)
            }
        })
        .await;
        let (dir, store, _) = store_with_rome(&rome);
        run(&client(&srv), &store, day(), DEFAULT_WORKERS)
            .await
            .unwrap();
        let got = payload(dir.path(), "tension.js", "TENSION");
        assert_eq!(got["items"]["A1215"], json!({"t": {}, "d": {}}));
    }

    #[tokio::test]
    async fn periods_before_2021t1_are_dropped_from_demandeurs_only() {
        let rome = rome_of(&[("M1805", json!([5]))]);
        let srv = tension_server(&["M1805"], |prefix, _| {
            let (nomen, rows) = if prefix == "demandeurs" {
                ("A", &[("2020T4", 1), ("2021T1", 2), ("2022T1", 3)][..])
            } else {
                ("PERSPECTIVE", &[("2015", 4), ("2025", 5)][..])
            };
            let l: Vec<Value> = rows
                .iter()
                .map(|(p, v)| {
                    json!({"codeNomenclature": nomen, "codePeriode": p,
                           "valeurPrincipaleNombre": v})
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"listeValeursParPeriode": l}))
        })
        .await;
        let (dir, store, _) = store_with_rome(&rome);
        run(&client(&srv), &store, day(), 1).await.unwrap();
        let got = payload(dir.path(), "tension.js", "TENSION");
        assert_eq!(
            got["items"]["M1805"],
            json!({"t": {"2015": 4, "2025": 5}, "d": {"2021T1": 2, "2022T1": 3}})
        );
    }

    // --- rome.js absent ou illisible ---------------------------------------------------------

    #[tokio::test]
    async fn missing_or_unreadable_rome_fails_explicitly_without_calls() {
        let cases: [(&str, Option<&[u8]>); 6] = [
            ("absent", None),
            ("vide", Some(b"")),
            ("html", Some(b"<html>erreur</html>")),
            ("json cassé", Some(b"window.ROME={\"items\":;")),
            ("sans items", Some(b"window.ROME={\"periods\":[]};")),
            ("pas utf-8", Some(&[0xff, 0xfe, 0x00])),
        ];
        for (name, content) in cases {
            let srv = server().await;
            let (dir, store, sentinel) = with_sentinel("tension.js", "TENSION");
            if let Some(c) = content {
                std::fs::write(dir.path().join("rome.js"), c).unwrap();
            }
            let err = run(&client(&srv), &store, day(), DEFAULT_WORKERS)
                .await
                .unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("tension") && msg.contains("rome.js"),
                "{name} : {msg}"
            );
            assert_eq!(
                std::fs::read(dir.path().join("tension.js")).unwrap(),
                sentinel,
                "{name}"
            );
            assert!(
                srv.received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .all(|r| r.url.path() == "/token"),
                "{name} : aucun appel d'API attendu"
            );
            no_leftover(dir.path(), if content.is_some() { 2 } else { 1 });
        }
        // rome.js sans aucun métier : échec aussi
        let srv = server().await;
        let (dir, store, sentinel) = store_with_rome(&rome_of(&[]));
        let err = run(&client(&srv), &store, day(), 4).await.unwrap_err();
        assert!(err.to_string().contains("aucun métier"), "{err}");
        assert_eq!(
            std::fs::read(dir.path().join("tension.js")).unwrap(),
            sentinel
        );
    }

    #[tokio::test]
    async fn missing_tension_js_is_created_and_no_temp_file_remains() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.save("rome.js", &golden("rome")).unwrap();
        let api = FakeApi(|_, _| Behavior::Data);
        run(&api, &store, day(), 2).await.unwrap();
        assert!(dir.path().join("tension.js").is_file());
        no_leftover(dir.path(), 2);
    }

    // --- échecs d'appels ---------------------------------------------------------------------

    /// `rome.js` de 100 codes `Z0000`… tous de volume différent, et un faux client où les `fail`
    /// premiers codes (l'un ou l'autre appel) échouent, les `empty` suivants sont sans donnée.
    fn rome100() -> Value {
        let items: Vec<(String, Value)> = (0..100)
            .map(|i| (format!("Z{i:04}"), json!([1000 - i])))
            .collect();
        let refs: Vec<(&str, Value)> = items.iter().map(|(c, o)| (c.as_str(), o.clone())).collect();
        rome_of(&refs)
    }

    fn index_of(code: &str) -> usize {
        code[1..].parse().unwrap()
    }

    #[tokio::test]
    async fn ten_percent_of_codes_failing_still_writes_the_rest() {
        // 10 codes sur 100 en échec (la moitié sur chaque point d'accès) : toléré
        let api = FakeApi(|ep, code| {
            let i = index_of(code);
            if i < 10 && i.is_multiple_of(2) == (ep == "stat-demandeurs") {
                Behavior::Fail
            } else {
                Behavior::Data
            }
        });
        let (dir, store, _) = store_with_rome(&rome100());
        run_with(&api, &store, day(), 100, 4).await.unwrap();
        let got = payload(dir.path(), "tension.js", "TENSION");
        assert_eq!(got["n"], 90);
        let items = got["items"].as_object().unwrap();
        assert_eq!(items.len(), 90);
        assert!(!items.contains_key("Z0000") && items.contains_key("Z0010"));
        // l'ordre (volume décroissant) est conservé malgré la simultanéité
        let order: Vec<&str> = keys(&got["items"]);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted);
        no_leftover(dir.path(), 2);
    }

    #[tokio::test]
    async fn more_than_ten_percent_failing_keeps_previous_file() {
        let api = FakeApi(|_, code| {
            if index_of(code) < 11 {
                Behavior::Fail
            } else {
                Behavior::Data
            }
        });
        let (dir, store, sentinel) = store_with_rome(&rome100());
        let err = run_with(&api, &store, day(), 100, 4).await.unwrap_err();
        assert!(matches!(err, CollectError::Fetch(_)), "{err}");
        assert!(
            err.to_string()
                .contains("tension : 11 échec(s) sur 100 code(s)"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("tension.js")).unwrap(),
            sentinel
        );
        no_leftover(dir.path(), 2);
    }

    #[tokio::test]
    async fn empty_responses_are_not_failures_but_all_empty_writes_nothing() {
        // 15 % de codes sans donnée : pas des échecs
        let api = FakeApi(|_, code| {
            if index_of(code) < 15 {
                Behavior::Empty
            } else {
                Behavior::Data
            }
        });
        let (dir, store, _) = store_with_rome(&rome100());
        run_with(&api, &store, day(), 100, 4).await.unwrap();
        let got = payload(dir.path(), "tension.js", "TENSION");
        assert_eq!(got["n"], 100);
        assert_eq!(got["items"]["Z0000"], json!({"t": {}, "d": {}}));
        // aucune donnée nulle part : rien n'est écrit, l'ancien fichier reste
        let api = FakeApi(|_, _| Behavior::Empty);
        let (dir, store, sentinel) = store_with_rome(&rome100());
        let err = run_with(&api, &store, day(), 100, 4).await.unwrap_err();
        assert!(err.to_string().contains("aucun métier"), "{err}");
        assert_eq!(
            std::fs::read(dir.path().join("tension.js")).unwrap(),
            sentinel
        );
    }

    #[tokio::test]
    async fn default_run_keeps_the_300_biggest_of_a_larger_rome() {
        let items: Vec<(String, Value)> =
            (0..450).map(|i| (format!("Z{i:04}"), json!([i]))).collect();
        let refs: Vec<(&str, Value)> = items.iter().map(|(c, o)| (c.as_str(), o.clone())).collect();
        let (dir, store, _) = store_with_rome(&rome_of(&refs));
        let api = FakeApi(|_, _| Behavior::Data);
        run(&api, &store, day(), DEFAULT_WORKERS).await.unwrap();
        let got = payload(dir.path(), "tension.js", "TENSION");
        assert_eq!(got["n"], 300);
        let order = keys(&got["items"]);
        assert_eq!(order.first(), Some(&"Z0449"));
        assert_eq!(order.last(), Some(&"Z0150"));
    }

    #[tokio::test]
    async fn failing_calls_over_http_keep_file_and_name_code_without_secrets() {
        // HTML : réponse illisible pour M1805 (1 code sur 3 : plus de 10 %, échec de la collecte)
        for failing in ["demandeurs", "tension"] {
            let srv = tension_server(&CODES, |prefix, code| {
                if prefix == failing && code == "M1805" {
                    ResponseTemplate::new(200).set_body_string("<html>erreur</html>")
                } else {
                    fixture_resp(prefix, code)
                }
            })
            .await;
            let (dir, store, sentinel) = store_with_rome(&golden("rome"));
            let err = run(&client(&srv), &store, day(), DEFAULT_WORKERS)
                .await
                .unwrap_err();
            for msg in [format!("{err}"), format!("{err:?}")] {
                assert!(msg.contains("tension : 1 échec(s) sur 3"), "{msg}");
                for secret in [SECRET, CLIENT_ID, TOKEN] {
                    assert!(!msg.contains(secret), "{msg}");
                }
            }
            assert_eq!(
                std::fs::read(dir.path().join("tension.js")).unwrap(),
                sentinel
            );
            no_leftover(dir.path(), 2);
        }
        // tout en 500 / jeton refusé : même garantie
        for bad in [ResponseTemplate::new(500), ResponseTemplate::new(401)] {
            let srv = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/token"))
                .respond_with(bad.set_body_string(format!("{SECRET} {TOKEN}")))
                .mount(&srv)
                .await;
            let (dir, store, sentinel) = store_with_rome(&golden("rome"));
            let err = run(&client(&srv), &store, day(), DEFAULT_WORKERS)
                .await
                .unwrap_err();
            for s in [format!("{err}"), format!("{err:?}")] {
                assert!(s.contains("tension"), "{s}");
                for secret in [SECRET, CLIENT_ID, TOKEN] {
                    assert!(!s.contains(secret), "{s}");
                }
            }
            assert_eq!(
                std::fs::read(dir.path().join("tension.js")).unwrap(),
                sentinel
            );
        }
    }

    #[tokio::test]
    async fn at_most_the_worker_limit_of_codes_run_at_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Probe {
            now: AtomicUsize,
            max: AtomicUsize,
        }
        #[allow(clippy::manual_async_fn)]
        impl StatApi for Probe {
            fn call<'a>(
                &'a self,
                _path: &'a str,
                _body: Option<&'a Value>,
            ) -> impl std::future::Future<Output = Result<Option<Value>, super::super::ft::FtError>>
                   + Send
                   + 'a {
                async move {
                    let cur = self.now.fetch_add(1, Ordering::SeqCst) + 1;
                    self.max.fetch_max(cur, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                    self.now.fetch_sub(1, Ordering::SeqCst);
                    Ok(Some(json!({"listeValeursParPeriode": [
                        {"codeNomenclature": "A", "codePeriode": "2022T1",
                         "valeurPrincipaleNombre": 1}]})))
                }
            }
        }
        for workers in [1usize, 3] {
            let probe = Probe {
                now: AtomicUsize::new(0),
                max: AtomicUsize::new(0),
            };
            let (_dir, store, _) = store_with_rome(&rome100());
            run_with(&probe, &store, day(), 12, workers).await.unwrap();
            assert_eq!(probe.max.load(Ordering::SeqCst), workers);
        }
    }
}
