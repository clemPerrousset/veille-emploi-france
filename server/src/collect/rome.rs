//! Collecteur `rome` : offres par métier ROME et par trimestre → `rome.js`.
//! Même résultat que `fetch_rome.py`.

use super::{ft_error, num, CollectError, ParseError, StatApi};
use crate::store::Store;
use chrono::NaiveDate;
use futures::stream::{self, StreamExt};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Premier trimestre conservé (comparaison de texte, comme en Python).
pub const MIN_PERIOD: &str = "2021T1";
/// Appels simultanés par défaut (l'application est limitée à 10 appels/seconde).
pub const DEFAULT_WORKERS: usize = 4;

const LIST_PATH: &str = "/v1/referentiel/activites/ROME";
const OFFERS_PATH: &str = "/v1/indicateur/stat-offres";

/// Part maximale (exclusive) de codes en échec tolérée : 1/10 = 10 %.
const MAX_FAILURE_RATIO: (usize, usize) = (1, 10);

type Fetched = Result<BTreeMap<String, f64>, CollectError>;

/// Offres « Toutes offres » (`TOFF`) par trimestre depuis `2021T1`, valeurs non nulles seulement.
pub fn parse_offers(resp: &Value) -> BTreeMap<String, f64> {
    resp.get("listeValeursParPeriode")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|x| x.get("codeNomenclature").and_then(Value::as_str) == Some("TOFF"))
        .filter_map(|x| {
            let period = x.get("codePeriode").and_then(Value::as_str).unwrap_or("");
            if period < MIN_PERIOD {
                return None;
            }
            let value = x.get("valeurPrincipaleNombre").and_then(Value::as_f64)?;
            Some((period.to_string(), value))
        })
        .collect()
}

/// Assemble la charge utile de `rome.js`. `acts` : (code, libellé) dans l'ordre de la liste.
/// Les codes sans aucune donnée sont omis.
pub fn assemble(
    acts: &[(String, String)],
    results: &BTreeMap<String, BTreeMap<String, f64>>,
    today: NaiveDate,
) -> Value {
    let periods: Vec<&String> = results
        .values()
        .flat_map(BTreeMap::keys)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut items = Map::new();
    for (code, label) in acts {
        let Some(r) = results.get(code).filter(|r| !r.is_empty()) else {
            continue;
        };
        let o: Vec<Value> = periods
            .iter()
            .map(|p| r.get(*p).map_or(Value::Null, |v| num(*v)))
            .collect();
        items.insert(code.clone(), json!({"l": label, "o": o}));
    }
    json!({
        "updated": today.format("%Y-%m-%d").to_string(),
        "periods": periods,
        "items": items,
    })
}

/// Liste ROME : (code, libellé) dans l'ordre de l'API. Fatal si elle est inexploitable.
async fn fetch_activities<C: StatApi>(client: &C) -> Result<Vec<(String, String)>, CollectError> {
    let label = "france travail liste ROME";
    let resp = client
        .call(LIST_PATH, None)
        .await
        .map_err(|e| ft_error(label, e))?
        .ok_or_else(|| CollectError::Fetch(format!("{label} : réponse vide")))?;
    let list = resp
        .get("activites")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CollectError::Parse(ParseError::Format(format!(
                "{label} : champ « activites » absent"
            )))
        })?;
    list.iter()
        .map(|a| {
            let field = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string);
            match (field("codeActivite"), field("libelleActivite")) {
                (Some(c), Some(l)) => Ok((c, l)),
                _ => Err(CollectError::Parse(ParseError::Format(format!(
                    "{label} : activité sans code ou sans libellé"
                )))),
            }
        })
        .collect()
}

/// Récupère la liste ROME puis les offres de chaque métier (`workers` appels simultanés au plus).
/// Un métier en échec est ignoré ; si aucun métier ne fournit de donnée, rien n'est écrit.
pub async fn run<C: StatApi>(
    client: &C,
    store: &Store,
    today: NaiveDate,
    workers: usize,
) -> Result<(), CollectError> {
    let acts = fetch_activities(client).await?;
    let fetched: Vec<(&str, Fetched)> = stream::iter(&acts)
        .map(|(code, _)| async move {
            let body = json!({"codeTypeTerritoire": "NAT", "codeTerritoire": "FR",
                "codeTypeActivite": "ROME", "codeActivite": code,
                "codeTypePeriode": "TRIMESTRE", "codeTypeNomenclature": "ORIGINEOFF"});
            let res = client
                .call(OFFERS_PATH, Some(&body))
                .await
                .map_err(|e| ft_error(&format!("france travail stat-offres {code}"), e))
                .map(|r| r.map(|v| parse_offers(&v)).unwrap_or_default());
            (code.as_str(), res)
        })
        .buffer_unordered(workers.max(1))
        .collect()
        .await;

    let mut results = BTreeMap::new();
    let mut failures = 0usize;
    let mut first_failure = None;
    for (code, res) in fetched {
        match res {
            Ok(r) => {
                results.insert(code.to_string(), r);
            }
            Err(e) => {
                tracing::warn!(code, error = %e, "ROME : métier ignoré");
                failures += 1;
                first_failure.get_or_insert(e);
            }
        }
    }
    // Garde (seuil exclusif, arithmétique entière) : trop d'échecs = ne rien écraser.
    if failures * MAX_FAILURE_RATIO.1 > acts.len() * MAX_FAILURE_RATIO.0 {
        return Err(CollectError::Fetch(format!(
            "rome : {failures} échec(s) sur {} code(s)",
            acts.len()
        )));
    }
    if results.values().all(BTreeMap::is_empty) {
        return Err(CollectError::Fetch(format!(
            "rome : aucun métier n'a fourni de données ({failures} échec(s) sur {} métiers{})",
            acts.len(),
            first_failure
                .map(|e| format!(", premier échec : {e}"))
                .unwrap_or_default()
        )));
    }
    store
        .save("rome.js", &assemble(&acts, &results, today))
        .map_err(|e| CollectError::Write(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ROME_CODES: [&str; 4] = ["M1805", "A1401", "K1303", "A1215"];

    fn acts() -> Vec<(String, String)> {
        fixture("activites")["activites"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["codeActivite"].as_str().unwrap().to_string(),
                    a["libelleActivite"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn real_results() -> BTreeMap<String, BTreeMap<String, f64>> {
        ROME_CODES
            .iter()
            .map(|c| {
                (
                    c.to_string(),
                    parse_offers(&fixture(&format!("offres_{c}"))),
                )
            })
            .collect()
    }

    /// Ligne réelle `TOFF` 2026T2 de M1805, nomenclature/période/valeur modifiables : jeux DÉRIVÉS.
    fn derived_row(nomenclature: &str, period: &str, value: Value) -> Value {
        let real = fixture("offres_M1805");
        let mut row = real["listeValeursParPeriode"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["codeNomenclature"] == "TOFF" && r["codePeriode"] == "2026T2")
            .unwrap()
            .clone();
        row["codeNomenclature"] = json!(nomenclature);
        row["codePeriode"] = json!(period);
        row["valeurPrincipaleNombre"] = value;
        row
    }

    #[test]
    fn parse_offers_keeps_only_toff_from_2021t1() {
        // Les données réelles n'ont aucune période avant 2021T1 : ce jeu est DÉRIVÉ d'une ligne réelle
        // dont `codePeriode` (et la nomenclature, pour PE et CUMUL) est modifié.
        let resp = json!({"listeValeursParPeriode": [
            derived_row("TOFF", "2020T4", json!(111)),
            derived_row("TOFF", "2021T1", json!(222)),
            derived_row("TOFF", "2022T3", json!(333)),
            derived_row("PE", "2025T1", json!(444)),
            derived_row("TOFF-CUMUL12MOIS", "2025T2", json!(555)),
        ]});
        let got = parse_offers(&resp);
        let want: BTreeMap<String, f64> =
            [("2021T1".to_string(), 222.0), ("2022T3".to_string(), 333.0)].into();
        assert_eq!(got, want);
        // données réelles : uniquement TOFF, toutes les périodes >= 2021T1
        let real = parse_offers(&fixture("offres_M1805"));
        assert_eq!(real.len(), 22);
        assert!(real.keys().all(|p| p.as_str() >= MIN_PERIOD));
        assert_eq!(real["2026T2"], 11840.0);
    }

    #[test]
    fn parse_offers_skips_masked_values() {
        let mut absent = derived_row("TOFF", "2022T1", json!(1));
        absent
            .as_object_mut()
            .unwrap()
            .remove("valeurPrincipaleNombre");
        let resp = json!({"listeValeursParPeriode": [
            absent,
            derived_row("TOFF", "2022T2", Value::Null),
            derived_row("TOFF", "2022T3", json!(0)),
            derived_row("TOFF", "2022T4", json!(7)),
        ]});
        let got = parse_offers(&resp);
        assert!(!got.contains_key("2022T1"), "valeur absente : pas de 0");
        assert!(!got.contains_key("2022T2"), "valeur nulle : pas de 0");
        // comme Python, un vrai zéro (non masqué) est conservé
        assert_eq!(got.get("2022T3"), Some(&0.0));
        assert_eq!(got.get("2022T4"), Some(&7.0));
        assert_eq!(got.len(), 2);
        // réponses vides ou sans liste
        assert!(parse_offers(&fixture("offres_A1215")).is_empty());
        assert!(parse_offers(&json!({})).is_empty());
    }

    #[test]
    fn assemble_matches_python_golden() {
        let out = assemble(&acts(), &real_results(), day());
        let gold = golden("rome");
        assert_eq!(out, gold);
        assert_eq!(keys(&out), ["updated", "periods", "items"]);
        assert_eq!(keys(&out["items"]), ["M1805", "A1401", "K1303"]);
        assert_eq!(keys(&out["items"]["M1805"]), ["l", "o"]);
        // sérialisation identique à Python : entiers sans « .0 »
        assert_eq!(out.to_string(), gold.to_string());
        assert!(out["items"]["M1805"]["o"][0].is_i64());
    }

    #[test]
    fn assemble_omits_codes_without_data() {
        let out = assemble(&acts(), &real_results(), day());
        assert!(out["items"].get("A1215").is_none());
        assert_eq!(out["items"].as_object().unwrap().len(), 3);
        // trous : période absente pour un code -> null
        let a = vec![
            ("X1".to_string(), "x".to_string()),
            ("X2".to_string(), "y".to_string()),
        ];
        let mut r = BTreeMap::new();
        r.insert("X1".to_string(), [("2022T1".to_string(), 5.0)].into());
        r.insert("X2".to_string(), [("2021T4".to_string(), 6.5)].into());
        let out = assemble(&a, &r, day());
        assert_eq!(out["periods"], json!(["2021T4", "2022T1"]));
        assert_eq!(out["items"]["X1"]["o"], json!([null, 5]));
        assert_eq!(out["items"]["X2"]["o"], json!([6.5, null]));
        assert_eq!(out["updated"], "2026-11-09");
    }

    /// Monte la liste et une réponse par code (requête reconnue par son corps JSON).
    /// La liste est complétée par 16 codes « bouchons » sans donnée (réponse vide, pas un échec)
    /// pour que l'échec d'un seul code reste sous le seuil de 10 % de la garde.
    async fn rome_server(per_code: impl Fn(&str) -> ResponseTemplate) -> MockServer {
        let srv = server().await;
        let mut list = fixture("activites");
        for i in 0..16 {
            list["activites"]
                .as_array_mut()
                .unwrap()
                .push(json!({"codeActivite": format!("Y{i:02}"), "libelleActivite": "bouchon"}));
        }
        Mock::given(method("GET"))
            .and(path("/api/v1/referentiel/activites/ROME"))
            .respond_with(ResponseTemplate::new(200).set_body_json(list))
            .mount(&srv)
            .await;
        for c in ROME_CODES {
            Mock::given(method("POST"))
                .and(path("/api/v1/indicateur/stat-offres"))
                .and(body_partial_json(json!({
                    "codeActivite": c, "codeTypeActivite": "ROME", "codeTypeNomenclature": "ORIGINEOFF",
                    "codeTypePeriode": "TRIMESTRE", "codeTypeTerritoire": "NAT", "codeTerritoire": "FR"})))
                .respond_with(per_code(c))
                .mount(&srv)
                .await;
        }
        Mock::given(method("POST"))
            .and(path("/api/v1/indicateur/stat-offres"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"listeValeursParPeriode": []})),
            )
            .mount(&srv)
            .await;
        srv
    }

    fn fixture_resp(c: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(fixture(&format!("offres_{c}")))
    }

    #[tokio::test]
    async fn run_writes_rome_js_matching_python_golden() {
        let srv = rome_server(fixture_resp).await;
        let (dir, store, _) = with_sentinel("rome.js", "ROME");
        run(&client(&srv), &store, day(), DEFAULT_WORKERS)
            .await
            .unwrap();
        let got = payload(dir.path(), "rome.js", "ROME");
        assert_eq!(got, golden("rome"));
        assert_eq!(keys(&got), ["updated", "periods", "items"]);
        assert_eq!(keys(&got["items"]), ["M1805", "A1401", "K1303"]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        // un seul POST par code
        let posts = srv
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("stat-offres"))
            .count();
        assert_eq!(posts, 20);
    }

    #[tokio::test]
    async fn one_code_failing_does_not_block_others() {
        let srv = rome_server(|c| {
            if c == "M1805" {
                ResponseTemplate::new(500).set_body_string("boom")
            } else {
                fixture_resp(c)
            }
        })
        .await;
        let (dir, store, _) = with_sentinel("rome.js", "ROME");
        run(&client(&srv), &store, day(), 2).await.unwrap();
        let got = payload(dir.path(), "rome.js", "ROME");
        assert_eq!(keys(&got["items"]), ["A1401", "K1303"]);
        assert_eq!(got["items"]["K1303"], golden("rome")["items"]["K1303"]);
    }

    #[tokio::test]
    async fn all_codes_failing_keeps_previous_file() {
        let srv = rome_server(|_| ResponseTemplate::new(500)).await;
        let (dir, store, sentinel) = with_sentinel("rome.js", "ROME");
        let err = run(&client(&srv), &store, day(), 4).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("rome") && msg.contains('4'), "{msg}");
        assert_eq!(std::fs::read(dir.path().join("rome.js")).unwrap(), sentinel);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

        // aucun code n'a de donnée (réponses vides réelles) : rien n'est écrit non plus
        let srv = rome_server(|_| fixture_resp("A1215")).await;
        let (dir, store, sentinel) = with_sentinel("rome.js", "ROME");
        run(&client(&srv), &store, day(), 4).await.unwrap_err();
        assert_eq!(std::fs::read(dir.path().join("rome.js")).unwrap(), sentinel);
    }

    #[tokio::test]
    async fn html_response_is_skipped_not_fatal_for_one_code_but_fatal_if_list_fails() {
        let html = "<!DOCTYPE html><html><body>Service indisponible</body></html>";
        // un code répond du HTML : ignoré
        let srv = rome_server(|c| {
            if c == "A1401" {
                ResponseTemplate::new(200).set_body_string(html)
            } else {
                fixture_resp(c)
            }
        })
        .await;
        let (dir, store, _) = with_sentinel("rome.js", "ROME");
        run(&client(&srv), &store, day(), 4).await.unwrap();
        let got = payload(dir.path(), "rome.js", "ROME");
        assert_eq!(keys(&got["items"]), ["M1805", "K1303"]);

        // la liste est inexploitable : fatal, fichier précédent intact
        for resp in [
            ResponseTemplate::new(200).set_body_string(html),
            ResponseTemplate::new(500).set_body_string("boom"),
            ResponseTemplate::new(200).set_body_json(json!({"autre": 1})),
            ResponseTemplate::new(404),
        ] {
            let srv = server().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/referentiel/activites/ROME"))
                .respond_with(resp)
                .mount(&srv)
                .await;
            let (dir, store, sentinel) = with_sentinel("rome.js", "ROME");
            let err = run(&client(&srv), &store, day(), 4).await.unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains("liste ROME"), "{msg}");
            assert_eq!(std::fs::read(dir.path().join("rome.js")).unwrap(), sentinel);
        }
    }

    #[tokio::test]
    async fn errors_never_contain_secrets() {
        for bad in [ResponseTemplate::new(500), ResponseTemplate::new(401)] {
            let srv = server().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/referentiel/activites/ROME"))
                .respond_with(bad)
                .mount(&srv)
                .await;
            let (_dir, store, _) = with_sentinel("rome.js", "ROME");
            let err = run(&client(&srv), &store, day(), 4).await.unwrap_err();
            for s in [format!("{err}"), format!("{err:?}")] {
                for secret in [SECRET, CLIENT_ID, TOKEN] {
                    assert!(!s.contains(secret), "{s}");
                }
            }
        }
    }

    /// Faux client : compte les appels simultanés.
    struct Probe {
        now: AtomicUsize,
        max: AtomicUsize,
        n: usize,
    }

    #[allow(clippy::manual_async_fn)] // la borne `Send` du trait exige la forme explicite
    impl StatApi for Probe {
        fn call<'a>(
            &'a self,
            path: &'a str,
            body: Option<&'a Value>,
        ) -> impl std::future::Future<Output = Result<Option<Value>, super::super::ft::FtError>>
               + Send
               + 'a {
            async move {
                if path == LIST_PATH {
                    let acts: Vec<Value> = (0..self.n)
                        .map(
                            |i| json!({"codeActivite": format!("Z{i:04}"), "libelleActivite": "x"}),
                        )
                        .collect();
                    return Ok(Some(json!({"activites": acts})));
                }
                let cur = self.now.fetch_add(1, Ordering::SeqCst) + 1;
                self.max.fetch_max(cur, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(15)).await;
                self.now.fetch_sub(1, Ordering::SeqCst);
                let _ = body;
                Ok(Some(json!({"listeValeursParPeriode": [
                    {"codeNomenclature": "TOFF", "codePeriode": "2022T1",
                     "valeurPrincipaleNombre": 1}]})))
            }
        }
    }

    /// Faux client : les `fail` premiers codes échouent (500), les `empty` suivants répondent
    /// sans donnée, les autres ont une valeur.
    struct Scripted {
        n: usize,
        fail: usize,
        empty: usize,
    }

    #[allow(clippy::manual_async_fn)] // la borne `Send` du trait exige la forme explicite
    impl StatApi for Scripted {
        fn call<'a>(
            &'a self,
            path: &'a str,
            body: Option<&'a Value>,
        ) -> impl std::future::Future<Output = Result<Option<Value>, super::super::ft::FtError>>
               + Send
               + 'a {
            async move {
                if path == LIST_PATH {
                    let acts: Vec<Value> = (0..self.n)
                        .map(
                            |i| json!({"codeActivite": format!("Z{i:04}"), "libelleActivite": "x"}),
                        )
                        .collect();
                    return Ok(Some(json!({"activites": acts})));
                }
                let code = body.unwrap()["codeActivite"].as_str().unwrap();
                let i: usize = code[1..].parse().unwrap();
                if i < self.fail {
                    Err(super::super::ft::FtError::Http(500))
                } else if i < self.fail + self.empty {
                    Ok(Some(json!({"listeValeursParPeriode": []})))
                } else {
                    Ok(Some(json!({"listeValeursParPeriode": [
                        {"codeNomenclature": "TOFF", "codePeriode": "2022T1",
                         "valeurPrincipaleNombre": 1}]})))
                }
            }
        }
    }

    #[tokio::test]
    async fn ten_percent_failures_still_writes() {
        let api = Scripted {
            n: 100,
            fail: 10,
            empty: 0,
        };
        let (dir, store, _) = with_sentinel("rome.js", "ROME");
        run(&api, &store, day(), 4).await.unwrap();
        let got = payload(dir.path(), "rome.js", "ROME");
        assert_eq!(got["items"].as_object().unwrap().len(), 90);
    }

    #[tokio::test]
    async fn more_than_ten_percent_failures_keeps_previous_file() {
        let api = Scripted {
            n: 100,
            fail: 11,
            empty: 0,
        };
        let (dir, store, sentinel) = with_sentinel("rome.js", "ROME");
        let err = run(&api, &store, day(), 4).await.unwrap_err();
        assert!(matches!(err, CollectError::Fetch(_)), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("rome : 11 échec(s) sur 100 code(s)"), "{msg}");
        assert_eq!(std::fs::read(dir.path().join("rome.js")).unwrap(), sentinel);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn empty_responses_do_not_count_as_failures() {
        let api = Scripted {
            n: 100,
            fail: 0,
            empty: 15,
        };
        let (dir, store, _) = with_sentinel("rome.js", "ROME");
        run(&api, &store, day(), 4).await.unwrap();
        let got = payload(dir.path(), "rome.js", "ROME");
        assert_eq!(got["items"].as_object().unwrap().len(), 85);
    }

    #[tokio::test]
    async fn rome_never_exceeds_worker_limit() {
        for workers in [1usize, 3, 4] {
            let probe = Probe {
                now: AtomicUsize::new(0),
                max: AtomicUsize::new(0),
                n: 24,
            };
            let (dir, store, _) = with_sentinel("rome.js", "ROME");
            run(&probe, &store, day(), workers).await.unwrap();
            let max = probe.max.load(Ordering::SeqCst);
            assert!(max <= workers, "workers={workers} max={max}");
            assert_eq!(max, workers, "le parallélisme doit être réellement utilisé");
            let got = payload(dir.path(), "rome.js", "ROME");
            assert_eq!(got["items"].as_object().unwrap().len(), 24);
        }
    }
}
