//! Collecteur `carte` : offres et demandeurs d'emploi par métier et par département, pour la carte de
//! France → `carte.js`. Même résultat que `fetch_carte.py`.
//!
//! Pour les `TOP_N` métiers de `rome.js` (puis les métiers tech de `TECH_CODES` présents dans `rome.js`
//! et absents de ce top) (même tri que `tension` : cumul d'offres des 4 derniers
//! trimestres, égalité : ordre du code) et pour chacun des 96 départements métropolitains, deux appels
//! (niveau « DEP ») : offres enregistrées (`TOFF`, nomenclature `ORIGINEOFF`) cumulées sur les 4 derniers
//! trimestres, et demandeurs de catégorie A (`CATCAND`) au dernier trimestre. Soit ~19 200 appels, environ
//! une heure : le planificateur ne lance ce job qu'une fois par mois (voir `schedule`).
//!
//! Écarts voulus avec le Python, pour ne jamais écraser un bon fichier par un mauvais :
//! - `rome.js` absent, illisible ou sans aucun métier : échec explicite, rien n'est écrit ;
//! - une cellule (métier x département) dont un appel échoue après les 5 essais est « en échec » (le
//!   Python, lui, prend `None` après ses essais comme une absence de donnée) ; ses valeurs sont `null` ;
//!   plus de 10 % de cellules en échec (seuil exclusif, arithmétique entière) : échec, rien n'est écrit,
//!   l'ancien `carte.js` reste en place ;
//! - une réponse vide ou un 404 n'est PAS un échec : la valeur est `null`, comme en Python ; mais si aucune
//!   cellule ne fournit la moindre donnée, rien n'est écrit (comme `rome`, `tech` et `tension`) ;
//! - pas de fichier de reprise (`carte_cache.json`) : une collecte interrompue repart de zéro.

use super::tech::TECH_CODES;
use super::tension::{read_rome, top_codes_for};
use super::{ft_error, CollectError, ParseError, StatApi};
use crate::store::Store;
use chrono::NaiveDate;
use futures::stream::{self, StreamExt};
use serde_json::{json, Map, Number, Value};
use std::collections::BTreeMap;

/// Nombre de métiers retenus (les plus volumineux).
pub const TOP_N: usize = 100;
/// Appels simultanés (l'application est limitée à 10 appels/seconde ; le limiteur du client veille).
pub const WORKERS: usize = 6;

/// Départements métropolitains : 01 à 19, 2A, 2B, 21 à 95 (même liste et même ordre que `DEPS` du Python).
pub const DEPS: [&str; 96] = [
    "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12", "13", "14", "15", "16",
    "17", "18", "19", "2A", "2B", "21", "22", "23", "24", "25", "26", "27", "28", "29", "30", "31",
    "32", "33", "34", "35", "36", "37", "38", "39", "40", "41", "42", "43", "44", "45", "46", "47",
    "48", "49", "50", "51", "52", "53", "54", "55", "56", "57", "58", "59", "60", "61", "62", "63",
    "64", "65", "66", "67", "68", "69", "70", "71", "72", "73", "74", "75", "76", "77", "78", "79",
    "80", "81", "82", "83", "84", "85", "86", "87", "88", "89", "90", "91", "92", "93", "94", "95",
];

/// Part maximale (exclusive) de cellules en échec tolérée : 1/10 = 10 % (comme `rome` et `tension`).
const MAX_FAILURE_RATIO: (usize, usize) = (1, 10);

const OFFERS_PATH: &str = "/v1/indicateur/stat-offres";
const APPLICANTS_PATH: &str = "/v1/indicateur/stat-demandeurs";
const LABEL: &str = "carte";

/// Lignes d'une réponse pour la nomenclature `wanted` : valeurs numériques par période, la dernière ligne
/// d'une période répétée l'emporte (comme le dictionnaire du Python). Ordre = ordre des périodes (texte).
fn rows_of(resp: Option<&Value>, wanted: &str) -> BTreeMap<String, Number> {
    resp.and_then(|r| r.get("listeValeursParPeriode"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|x| x.get("codeNomenclature").and_then(Value::as_str) == Some(wanted))
        .filter_map(|x| {
            let period = x.get("codePeriode").and_then(Value::as_str)?;
            match x.get("valeurPrincipaleNombre") {
                Some(Value::Number(v)) => Some((period.to_string(), v.clone())),
                _ => None,
            }
        })
        .collect()
}

/// Somme de nombres JSON comme Python : entière tant que toutes les valeurs le sont, sinon décimale.
fn sum_numbers<'a>(values: impl Iterator<Item = &'a Number>) -> Value {
    let mut int_sum: Option<i64> = Some(0);
    let mut float_sum = 0.0;
    for v in values {
        float_sum += v.as_f64().unwrap_or(0.0);
        int_sum = match (int_sum, v.as_i64()) {
            (Some(s), Some(i)) => s.checked_add(i),
            _ => None,
        };
    }
    match int_sum {
        Some(s) => Value::from(s),
        None => Number::from_f64(float_sum).map_or(Value::Null, Value::Number),
    }
}

/// Cumul des 4 derniers trimestres d'offres `TOFF` ; `Null` s'il y a moins de 4 trimestres avec une valeur.
pub fn offers_4q(resp: Option<&Value>) -> Value {
    let rows = rows_of(resp, "TOFF");
    if rows.len() < 4 {
        return Value::Null;
    }
    sum_numbers(rows.values().skip(rows.len() - 4))
}

/// Demandeurs de catégorie A au dernier trimestre : (valeur, période), `(Null, None)` sans donnée.
pub fn applicants_last(resp: Option<&Value>) -> (Value, Option<String>) {
    match rows_of(resp, "A").into_iter().next_back() {
        Some((period, v)) => (Value::Number(v), Some(period)),
        None => (Value::Null, None),
    }
}

/// Une cellule : offres sur 4 trimestres, demandeurs, période de ces demandeurs.
struct Cell {
    offers: Value,
    applicants: Value,
    period: Option<String>,
}

async fn fetch_cell<C: StatApi>(client: &C, code: &str, dep: &str) -> Result<Cell, CollectError> {
    let body = |nomenclature: &str| {
        json!({"codeTypeTerritoire": "DEP", "codeTypeActivite": "ROME",
            "codeTerritoire": dep, "codeActivite": code,
            "codeTypePeriode": "TRIMESTRE", "codeTypeNomenclature": nomenclature})
    };
    let offers = client
        .call(OFFERS_PATH, Some(&body("ORIGINEOFF")))
        .await
        .map_err(|e| {
            ft_error(
                &format!("france travail {LABEL} {code} {dep} stat-offres"),
                e,
            )
        })?;
    let applicants = client
        .call(APPLICANTS_PATH, Some(&body("CATCAND")))
        .await
        .map_err(|e| {
            ft_error(
                &format!("france travail {LABEL} {code} {dep} stat-demandeurs"),
                e,
            )
        })?;
    let (applicants, period) = applicants_last(applicants.as_ref());
    Ok(Cell {
        offers: offers_4q(offers.as_ref()),
        applicants,
        period,
    })
}

/// Sélection : les `n` métiers les plus volumineux de `rome.js`, PUIS les codes de `extras` présents dans
/// `rome.js` et absents de ce top, dans l'ordre de la liste (comme `selection` du Python).
pub fn selection(rome: &Value, n: usize, extras: &[&str]) -> Result<Vec<String>, CollectError> {
    let mut codes = top_codes_for(rome, n, LABEL)?;
    let items = rome.get("items").and_then(Value::as_object);
    for extra in extras {
        if items.is_some_and(|i| i.contains_key(*extra)) && !codes.iter().any(|c| c == extra) {
            codes.push((*extra).to_string());
        }
    }
    Ok(codes)
}

/// `carte.js` pour les `TOP_N` premiers métiers de `rome.js` plus les métiers tech x les 96 départements.
pub async fn run<C: StatApi>(
    client: &C,
    store: &Store,
    today: NaiveDate,
) -> Result<(), CollectError> {
    run_with_extras(client, store, today, TOP_N, &TECH_CODES, &DEPS, WORKERS).await
}

/// Comme `run`, pour `n` métiers, sans métier supplémentaire, et la liste de départements donnée.
pub async fn run_with<C: StatApi>(
    client: &C,
    store: &Store,
    today: NaiveDate,
    n: usize,
    deps: &[&str],
    workers: usize,
) -> Result<(), CollectError> {
    run_with_extras(client, store, today, n, &[], deps, workers).await
}

/// Comme `run_with`, avec les codes supplémentaires `extras` (les tests injectent `n`, `extras` et `deps`,
/// comme le module Python).
pub async fn run_with_extras<C: StatApi>(
    client: &C,
    store: &Store,
    today: NaiveDate,
    n: usize,
    extras: &[&str],
    deps: &[&str],
    workers: usize,
) -> Result<(), CollectError> {
    let codes = selection(&read_rome(store, LABEL)?, n, extras)?;
    if codes.is_empty() || deps.is_empty() {
        return Err(CollectError::Parse(ParseError::Format(format!(
            "{LABEL} : rien à collecter ({} métier(s), {} département(s))",
            codes.len(),
            deps.len()
        ))));
    }
    let total = codes.len() * deps.len();
    tracing::info!(
        metiers = codes.len(),
        departements = deps.len(),
        cellules = total,
        "carte : début de la collecte"
    );
    let mut stream = stream::iter((0..total).map(|i| (i / deps.len(), i % deps.len())))
        .map(|(c, d)| {
            let (code, dep) = (codes[c].as_str(), deps[d]);
            async move { (c, d, fetch_cell(client, code, dep).await) }
        })
        .buffer_unordered(workers.max(1));

    let mut cells: Vec<Option<Cell>> = (0..total).map(|_| None).collect();
    let mut failures = 0usize;
    let mut first_failure = None;
    let mut done = 0usize;
    while let Some((c, d, res)) = stream.next().await {
        match res {
            Ok(cell) => cells[c * deps.len() + d] = Some(cell),
            Err(e) => {
                tracing::warn!(error = %e, "carte : cellule ignorée");
                failures += 1;
                first_failure.get_or_insert(e);
            }
        }
        done += 1;
        if done.is_multiple_of(1000) {
            tracing::info!(done, total, echecs = failures, "carte : progression");
        }
    }
    drop(stream);

    // Garde (seuil exclusif, arithmétique entière) : trop d'échecs = ne rien écraser.
    if failures * MAX_FAILURE_RATIO.1 > total * MAX_FAILURE_RATIO.0 {
        return Err(CollectError::Fetch(format!(
            "{LABEL} : {failures} échec(s) sur {total} cellule(s){}",
            first_failure
                .map(|e| format!(", premier échec : {e}"))
                .unwrap_or_default()
        )));
    }
    let any_data = cells
        .iter()
        .flatten()
        .any(|c| !c.offers.is_null() || !c.applicants.is_null());
    if !any_data {
        return Err(CollectError::Fetch(format!(
            "{LABEL} : aucune cellule n'a fourni de données ({failures} échec(s) sur {total} cellules{})",
            first_failure
                .map(|e| format!(", premier échec : {e}"))
                .unwrap_or_default()
        )));
    }

    let mut quarter: Option<&str> = None;
    let mut items = Map::new();
    for (c, code) in codes.iter().enumerate() {
        let (mut offers, mut applicants) = (Vec::new(), Vec::new());
        for cell in &cells[c * deps.len()..(c + 1) * deps.len()] {
            match cell {
                Some(x) => {
                    offers.push(x.offers.clone());
                    applicants.push(x.applicants.clone());
                    if let Some(p) = x.period.as_deref() {
                        if quarter.is_none_or(|q| p > q) {
                            quarter = Some(p);
                        }
                    }
                }
                None => {
                    offers.push(Value::Null);
                    applicants.push(Value::Null);
                }
            }
        }
        items.insert(code.clone(), json!({"o": offers, "d": applicants}));
    }
    let out = json!({
        "updated": today.format("%Y-%m-%d").to_string(),
        "quarter": quarter,
        "deps": deps,
        "items": items,
    });
    store
        .save("carte.js", &out)
        .map_err(|e| CollectError::Write(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Codes de `golden/rome.json` par volume décroissant, et départements de `golden/carte.json`.
    const CODES: [&str; 3] = ["K1303", "M1805", "A1401"];
    const GOLD_DEPS: [&str; 4] = ["2A", "48", "75", "90"];

    /// Magasin de test : `rome.js` (donné) et un `carte.js` sentinelle.
    fn store_with_rome(rome: &Value) -> (tempfile::TempDir, Store, Vec<u8>) {
        let (dir, store, sentinel) = with_sentinel("carte.js", "CARTE");
        store.save("rome.js", rome).unwrap();
        (dir, store, sentinel)
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

    /// Réponse minimale : une ligne par `(période, valeur)`.
    fn rows(nomen: &str, vals: &[(&str, Option<i64>)]) -> Value {
        let l: Vec<Value> = vals
            .iter()
            .map(|(p, v)| {
                json!({"codeNomenclature": nomen, "codePeriode": p, "libActivite": "Lib",
                       "valeurPrincipaleNombre": v})
            })
            .collect();
        json!({"listeValeursParPeriode": l})
    }

    /// Serveur simulé : `answer(endpoint, code, dep)` donne la réponse de chaque appel. Le corps de
    /// requête est vérifié (territoire `DEP`, métier ROME, trimestres, nomenclature du point d'accès).
    async fn carte_server(
        codes: &[&str],
        deps: &[&str],
        answer: impl Fn(&str, &str, &str) -> ResponseTemplate,
    ) -> MockServer {
        let srv = server().await;
        for code in codes {
            for dep in deps {
                for (ep, nomen) in [
                    ("stat-offres", "ORIGINEOFF"),
                    ("stat-demandeurs", "CATCAND"),
                ] {
                    Mock::given(method("POST"))
                        .and(path(format!("/api/v1/indicateur/{ep}")))
                        .and(body_partial_json(json!({
                            "codeTypeTerritoire": "DEP", "codeTerritoire": dep,
                            "codeTypeActivite": "ROME", "codeActivite": code,
                            "codeTypePeriode": "TRIMESTRE", "codeTypeNomenclature": nomen})))
                        .respond_with(answer(ep, code, dep))
                        .mount(&srv)
                        .await;
                }
            }
        }
        srv
    }

    /// Fixtures réelles : absentes = 404 (comme l'API pour une réponse vide).
    fn fixture_answer(ep: &str, code: &str, dep: &str) -> ResponseTemplate {
        let prefix = if ep == "stat-offres" {
            "offres"
        } else {
            "demandeurs"
        };
        let file = format!("{FT_FIX}carte_{prefix}_{code}_{dep}.json");
        match std::fs::read(&file) {
            Ok(bytes) => ResponseTemplate::new(200)
                .set_body_json(serde_json::from_slice::<Value>(&bytes).unwrap()),
            Err(_) => ResponseTemplate::new(404),
        }
    }

    fn ok(body: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(body)
    }

    /// Quatre trimestres pour les offres (cumul 10) et un trimestre pour les demandeurs (valeur 7).
    fn simple_answer(ep: &str, _: &str, _: &str) -> ResponseTemplate {
        if ep == "stat-offres" {
            ok(rows(
                "TOFF",
                &[
                    ("2025T4", Some(1)),
                    ("2026T1", Some(2)),
                    ("2026T2", Some(3)),
                    ("2026T3", Some(4)),
                ],
            ))
        } else {
            ok(rows("A", &[("2026T2", Some(7))]))
        }
    }

    // --- liste des départements et sélection -------------------------------------------------

    #[test]
    fn production_departments_are_the_96_metropolitan_ones_in_python_order() {
        let python: Vec<String> = (1..20)
            .map(|i| format!("{i:02}"))
            .chain(["2A".to_string(), "2B".to_string()])
            .chain((21..96).map(|i| format!("{i:02}")))
            .collect();
        assert_eq!(DEPS.to_vec(), python);
        assert_eq!(DEPS.len(), 96);
        assert_eq!(
            (DEPS[0], DEPS[18], DEPS[19], DEPS[20], DEPS[21]),
            ("01", "19", "2A", "2B", "21")
        );
        assert_eq!(DEPS[95], "95");
        assert!(!DEPS.contains(&"20") && !DEPS.contains(&"971") && !DEPS.contains(&"96"));
        let mut unique = DEPS.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 96);
        assert_eq!((TOP_N, WORKERS), (100, 6));
    }

    #[test]
    fn selection_uses_the_same_sort_as_tension() {
        let rome = golden("rome");
        assert_eq!(
            top_codes_for(&rome, TOP_N, LABEL).unwrap(),
            ["K1303", "M1805", "A1401"]
        );
        assert_eq!(top_codes_for(&rome, 2, LABEL).unwrap(), ["K1303", "M1805"]);
        // volume sur les 4 derniers trimestres, null = 0, égalité : ordre du code
        let rome = rome_of(&[
            ("B", json!([1000, 1, 1, 1, 1])),
            ("A", json!([null, 2, 2, 2, null])),
            ("C", json!([9])),
        ]);
        assert_eq!(top_codes_for(&rome, 10, LABEL).unwrap(), ["C", "A", "B"]);
        // les erreurs portent le nom du collecteur carte
        let err = top_codes_for(&json!({}), 5, LABEL).unwrap_err().to_string();
        assert!(err.contains("carte :") && !err.contains("tension"), "{err}");
    }

    // --- calculs d'une cellule ---------------------------------------------------------------

    #[test]
    fn offers_are_the_sum_of_the_last_four_quarters() {
        let r = rows(
            "TOFF",
            &[
                ("2026T2", Some(40)), // désordre voulu : on trie par période
                ("2025T1", Some(1000)),
                ("2025T4", Some(10)),
                ("2026T1", Some(30)),
                ("2025T3", Some(5)),
                ("2025T2", None), // valeur nulle : la ligne ne compte pas
            ],
        );
        // 2025T1 (1000), 2025T3 (5), 2025T4 (10), 2026T1 (30), 2026T2 (40) : les 4 dernières = 85
        assert_eq!(offers_4q(Some(&r)), json!(85));
        // exactement 4 trimestres : tous comptent
        let r = rows(
            "TOFF",
            &[
                ("2025T3", Some(1)),
                ("2025T4", Some(2)),
                ("2026T1", Some(3)),
                ("2026T2", Some(4)),
            ],
        );
        assert_eq!(offers_4q(Some(&r)), json!(10));
        // zéro offre sur 4 trimestres : 0, pas null
        let r = rows(
            "TOFF",
            &[
                ("2025T3", Some(0)),
                ("2025T4", Some(0)),
                ("2026T1", Some(0)),
                ("2026T2", Some(0)),
            ],
        );
        assert_eq!(offers_4q(Some(&r)), json!(0));
    }

    #[test]
    fn offers_are_null_with_fewer_than_four_quarters() {
        let three = rows(
            "TOFF",
            &[
                ("2026T1", Some(3)),
                ("2026T2", Some(4)),
                ("2025T4", Some(2)),
                ("2025T3", None), // la valeur nulle ne compte pas : 3 trimestres seulement
            ],
        );
        assert_eq!(offers_4q(Some(&three)), Value::Null);
        assert_eq!(
            offers_4q(Some(&json!({"listeValeursParPeriode": []}))),
            Value::Null
        );
        assert_eq!(offers_4q(Some(&json!({}))), Value::Null);
        assert_eq!(offers_4q(None), Value::Null);
    }

    #[test]
    fn offers_ignore_other_nomenclatures_and_applicants_ignore_other_categories() {
        let mut r = rows(
            "TOFF",
            &[
                ("2025T4", Some(1)),
                ("2026T1", Some(1)),
                ("2026T2", Some(1)),
            ],
        );
        // quatre lignes « PE » et « B » (pièges) ne comptent pas
        for p in ["2025T1", "2025T2", "2025T3", "2026T2"] {
            r["listeValeursParPeriode"].as_array_mut().unwrap().push(
                json!({"codeNomenclature": "PE", "codePeriode": p, "valeurPrincipaleNombre": 99}),
            );
            r["listeValeursParPeriode"].as_array_mut().unwrap().push(
                json!({"codeNomenclature": "B", "codePeriode": p, "valeurPrincipaleNombre": 99}),
            );
        }
        assert_eq!(offers_4q(Some(&r)), Value::Null);
        // demandeurs : seule la catégorie A compte, même si B ou PE ont une période plus récente
        let mut r = rows("A", &[("2025T4", Some(5))]);
        for p in ["2026T1", "2026T2"] {
            for nomen in ["B", "ABC", "PE"] {
                r["listeValeursParPeriode"].as_array_mut().unwrap().push(
                    json!({"codeNomenclature": nomen, "codePeriode": p, "valeurPrincipaleNombre": 99}),
                );
            }
        }
        assert_eq!(
            applicants_last(Some(&r)),
            (json!(5), Some("2025T4".to_string()))
        );
        // et sans ligne A, il n'y a rien (les offres ne se confondent pas avec les demandeurs)
        let r = rows("TOFF", &[("2025T4", Some(5))]);
        assert_eq!(applicants_last(Some(&r)), (Value::Null, None));
        let r = rows("A", &[("2025T4", Some(5))]);
        assert_eq!(offers_4q(Some(&r)), Value::Null);
    }

    #[test]
    fn applicants_are_the_last_quarter_with_a_value() {
        let r = rows(
            "A",
            &[
                ("2025T4", Some(50)),
                ("2026T2", Some(70)),
                ("2026T1", Some(60)),
                ("2026T3", None), // valeur nulle : ni la valeur ni la période ne comptent
            ],
        );
        assert_eq!(
            applicants_last(Some(&r)),
            (json!(70), Some("2026T2".to_string()))
        );
        // un zéro est une valeur
        let r = rows("A", &[("2026T1", Some(0))]);
        assert_eq!(
            applicants_last(Some(&r)),
            (json!(0), Some("2026T1".to_string()))
        );
        assert_eq!(applicants_last(None), (Value::Null, None));
        assert_eq!(
            applicants_last(Some(&json!({"listeValeursParPeriode": []}))),
            (Value::Null, None)
        );
    }

    #[test]
    fn sums_stay_integers_unless_a_value_is_decimal() {
        let r = json!({"listeValeursParPeriode": [
            {"codeNomenclature": "TOFF", "codePeriode": "2026T1", "valeurPrincipaleNombre": 1},
            {"codeNomenclature": "TOFF", "codePeriode": "2026T2", "valeurPrincipaleNombre": 2},
            {"codeNomenclature": "TOFF", "codePeriode": "2026T3", "valeurPrincipaleNombre": 3},
            {"codeNomenclature": "TOFF", "codePeriode": "2026T4", "valeurPrincipaleNombre": 4.5}]});
        let v = offers_4q(Some(&r));
        assert_eq!(v, json!(10.5));
        assert_eq!(v.to_string(), "10.5");
        let r = json!({"listeValeursParPeriode": [
            {"codeNomenclature": "TOFF", "codePeriode": "2026T1", "valeurPrincipaleNombre": 1.0},
            {"codeNomenclature": "TOFF", "codePeriode": "2026T2", "valeurPrincipaleNombre": 2},
            {"codeNomenclature": "TOFF", "codePeriode": "2026T3", "valeurPrincipaleNombre": 3},
            {"codeNomenclature": "TOFF", "codePeriode": "2026T4", "valeurPrincipaleNombre": 4}]});
        // Python : 1.0 + 2 + 3 + 4 = 10.0 (décimal)
        assert_eq!(offers_4q(Some(&r)).to_string(), "10.0");
    }

    // --- parité avec le Python ---------------------------------------------------------------

    #[tokio::test]
    async fn carte_matches_python_golden() {
        let srv = carte_server(&CODES, &GOLD_DEPS, fixture_answer).await;
        let (dir, store, sentinel) = store_with_rome(&golden("rome"));
        run_with(&client(&srv), &store, day(), 3, &GOLD_DEPS, WORKERS)
            .await
            .unwrap();
        let got = payload(dir.path(), "carte.js", "CARTE");
        let gold = golden("carte");
        assert_eq!(got, gold);
        assert_eq!(keys(&got), ["updated", "quarter", "deps", "items"]);
        assert_eq!(keys(&got["items"]), ["K1303", "M1805", "A1401"]);
        assert_eq!(keys(&got["items"]["K1303"]), ["o", "d"]);
        assert_eq!(got["deps"], json!(["2A", "48", "75", "90"]));
        assert_eq!(got["quarter"], "2026T2");
        // les deux cas limites du jeu de référence : A1401 sans offre à Belfort (réponse sans ligne TOFF)
        assert_eq!(got["items"]["A1401"]["o"][3], Value::Null);
        assert_eq!(got["items"]["A1401"]["d"][3], json!(0));
        // octet pour octet : mêmes nombres, mêmes séparateurs (le JSON compact du Python en diffère
        // seulement par les espaces après `:` et `,`)
        let text = std::fs::read_to_string(dir.path().join("carte.js")).unwrap();
        assert_eq!(text, format!("window.CARTE={};", gold));
        assert_ne!(
            std::fs::read(dir.path().join("carte.js")).unwrap(),
            sentinel
        );
        no_leftover(dir.path(), 2); // rome.js, carte.js
    }

    #[tokio::test]
    async fn extras_are_appended_after_the_top_without_duplicates_and_match_golden() {
        // N = 2 : K1303, M1805 ; A1401 ajouté par les extras ; K1303 déjà présent ; ZZ999 absent de rome.js
        let extras = ["A1401", "K1303", "ZZ999"];
        let rome = golden("rome");
        assert_eq!(
            selection(&rome, 2, &extras).unwrap(),
            ["K1303", "M1805", "A1401"]
        );
        assert_eq!(selection(&rome, 2, &[]).unwrap(), ["K1303", "M1805"]);
        // ordre de la liste des extras, pas celui du volume ; le top reste en tête
        assert_eq!(
            selection(&rome, 1, &["A1401", "M1805"]).unwrap(),
            ["K1303", "A1401", "M1805"]
        );
        assert_eq!(selection(&rome, 0, &["M1805"]).unwrap(), ["M1805"]);
        let srv = carte_server(&CODES, &GOLD_DEPS, fixture_answer).await;
        let (dir, store, _) = store_with_rome(&rome);
        run_with_extras(
            &client(&srv),
            &store,
            day(),
            2,
            &extras,
            &GOLD_DEPS,
            WORKERS,
        )
        .await
        .unwrap();
        let got = payload(dir.path(), "carte.js", "CARTE");
        assert_eq!(got, golden("carte"));
        assert_eq!(keys(&got["items"]), ["K1303", "M1805", "A1401"]);
        // 3 métiers x 4 départements x 2 appels, aucun pour ZZ999 ni en double
        let calls = srv
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path().contains("indicateur"))
            .count();
        assert_eq!(calls, 3 * 4 * 2);
    }

    #[test]
    fn production_extras_are_the_tech_codes_from_tech_rs() {
        assert_eq!(TECH_CODES.len(), 18);
        let tech_in_rome = ["M1805", "M1856", "I1401"];
        let mut items: Vec<(&str, Value)> = (0..5)
            .map(|i| (["A", "B", "C", "D", "E"][i], json!([100 - i as i64])))
            .collect();
        for c in tech_in_rome {
            items.push((c, json!([1])));
        }
        let rome = rome_of(&items);
        // top 2 = A, B ; extras dans l'ordre de TECH_CODES (M1805, M1856, I1401), seulement ceux de rome.js
        assert_eq!(
            selection(&rome, 2, &TECH_CODES).unwrap(),
            ["A", "B", "M1805", "M1856", "I1401"]
        );
    }

    #[tokio::test]
    async fn failure_ratio_counts_extra_cells_too() {
        // 1 métier du top + 1 extra = 2 x 5 = 10 cellules ; 1 échec toléré (10 %), 2 refusés
        let deps = ["01", "02", "03", "04", "05"];
        for (bad, ok_expected) in [(1usize, true), (2, false)] {
            let srv = carte_server(&["A", "X"], &deps, |ep, code, dep| {
                let n = deps.iter().position(|d| *d == dep).unwrap();
                if code == "X" && n < bad {
                    ResponseTemplate::new(503)
                } else {
                    simple_answer(ep, code, dep)
                }
            })
            .await;
            let (dir, store, _) =
                store_with_rome(&rome_of(&[("A", json!([9])), ("X", json!([1]))]));
            let res = run_with_extras(&client(&srv), &store, day(), 1, &["X"], &deps, 3).await;
            assert_eq!(res.is_ok(), ok_expected, "{bad} : {res:?}");
            if ok_expected {
                let got = payload(dir.path(), "carte.js", "CARTE");
                assert_eq!(keys(&got["items"]), ["A", "X"]);
                assert_eq!(got["items"]["X"]["o"], json!([null, 10, 10, 10, 10]));
            }
        }
    }

    #[tokio::test]
    async fn n_reduces_the_selection_and_calls_exactly_two_per_cell() {
        let srv = carte_server(&CODES, &GOLD_DEPS, fixture_answer).await;
        let (dir, store, _) = store_with_rome(&golden("rome"));
        run_with(&client(&srv), &store, day(), 2, &GOLD_DEPS, WORKERS)
            .await
            .unwrap();
        let got = payload(dir.path(), "carte.js", "CARTE");
        assert_eq!(keys(&got["items"]), ["K1303", "M1805"]);
        // mêmes valeurs que le jeu complet pour les métiers gardés
        let gold = golden("carte");
        assert_eq!(got["items"]["K1303"], gold["items"]["K1303"]);
        assert_eq!(got["items"]["M1805"], gold["items"]["M1805"]);
        let calls = srv
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path().contains("indicateur"))
            .count();
        assert_eq!(calls, 2 * 2 * 4);
    }

    // --- cas limites -------------------------------------------------------------------------

    #[tokio::test]
    async fn not_found_and_empty_answers_are_null_without_failure() {
        let deps = ["01", "02"];
        let srv = carte_server(&["A"], &deps, |ep, _, dep| match (ep, dep) {
            ("stat-offres", "01") => ResponseTemplate::new(404),
            ("stat-demandeurs", "01") => ResponseTemplate::new(204),
            ("stat-offres", _) => ok(json!({"listeValeursParPeriode": []})),
            _ => ok(rows("A", &[("2026T2", Some(7))])),
        })
        .await;
        let (dir, store, _) = store_with_rome(&rome_of(&[("A", json!([1]))]));
        run_with(&client(&srv), &store, day(), 1, &deps, 2)
            .await
            .unwrap();
        let got = payload(dir.path(), "carte.js", "CARTE");
        assert_eq!(got["items"]["A"]["o"], json!([null, null]));
        assert_eq!(got["items"]["A"]["d"], json!([null, 7]));
        assert_eq!(got["quarter"], "2026T2");
    }

    #[tokio::test]
    async fn fewer_than_four_quarters_gives_null_offers_but_keeps_applicants() {
        let deps = ["01"];
        let srv = carte_server(&["A"], &deps, |ep, _, _| {
            if ep == "stat-offres" {
                ok(rows(
                    "TOFF",
                    &[
                        ("2026T1", Some(3)),
                        ("2026T2", Some(4)),
                        ("2025T4", Some(2)),
                    ],
                ))
            } else {
                ok(rows("A", &[("2026T1", Some(9)), ("2026T2", Some(8))]))
            }
        })
        .await;
        let (dir, store, _) = store_with_rome(&rome_of(&[("A", json!([1]))]));
        run_with(&client(&srv), &store, day(), 1, &deps, 1)
            .await
            .unwrap();
        let got = payload(dir.path(), "carte.js", "CARTE");
        assert_eq!(got["items"]["A"], json!({"o": [null], "d": [8]}));
    }

    #[tokio::test]
    async fn quarter_is_the_latest_applicants_period_and_null_without_any() {
        // le trimestre est le plus récent des demandeurs, pas celui des offres ; il peut venir d'un métier
        // ou d'un département quelconque
        let deps = ["01", "02"];
        let srv = carte_server(&["A", "B"], &deps, |ep, code, dep| {
            if ep == "stat-offres" {
                simple_answer(ep, code, dep) // offres jusqu'en 2026T3
            } else {
                let p = match (code, dep) {
                    ("B", "02") => "2025T4",
                    ("B", _) => "2026T1",
                    _ => "2025T3",
                };
                ok(rows("A", &[(p, Some(1)), ("2024T1", Some(5))]))
            }
        })
        .await;
        let rome = rome_of(&[("A", json!([2])), ("B", json!([1]))]);
        let (dir, store, _) = store_with_rome(&rome);
        run_with(&client(&srv), &store, day(), 2, &deps, 2)
            .await
            .unwrap();
        assert_eq!(
            payload(dir.path(), "carte.js", "CARTE")["quarter"],
            "2026T1"
        );
        // aucun demandeur nulle part (offres seulement) : quarter = null
        let srv = carte_server(&["A"], &deps, |ep, code, dep| {
            if ep == "stat-offres" {
                simple_answer(ep, code, dep)
            } else {
                ResponseTemplate::new(404)
            }
        })
        .await;
        let (dir, store, _) = store_with_rome(&rome_of(&[("A", json!([1]))]));
        run_with(&client(&srv), &store, day(), 1, &deps, 2)
            .await
            .unwrap();
        let got = payload(dir.path(), "carte.js", "CARTE");
        assert_eq!(got["quarter"], Value::Null);
        assert_eq!(got["items"]["A"]["d"], json!([null, null]));
        assert_eq!(got["items"]["A"]["o"], json!([10, 10]));
    }

    #[tokio::test]
    async fn output_order_follows_selection_and_departments_not_completion() {
        // les premières cellules répondent le plus lentement : l'ordre de sortie reste celui des listes
        let deps = ["2A", "21", "01"];
        let srv = carte_server(&["Z", "A"], &deps, |ep, code, dep| {
            let rank = deps.iter().position(|d| *d == dep).unwrap() as u64
                + if code == "Z" { 0 } else { 3 };
            let v = rank as i64 + 1;
            let delay = std::time::Duration::from_millis(60 - 10 * rank);
            let body = if ep == "stat-offres" {
                rows(
                    "TOFF",
                    &[
                        ("2025T3", Some(v)),
                        ("2025T4", Some(0)),
                        ("2026T1", Some(0)),
                        ("2026T2", Some(0)),
                    ],
                )
            } else {
                rows("A", &[("2026T2", Some(100 + v))])
            };
            ok(body).set_delay(delay)
        })
        .await;
        let (dir, store, _) = store_with_rome(&rome_of(&[("A", json!([1])), ("Z", json!([9]))]));
        run_with(&client(&srv), &store, day(), 2, &deps, 6)
            .await
            .unwrap();
        let got = payload(dir.path(), "carte.js", "CARTE");
        assert_eq!(got["deps"], json!(["2A", "21", "01"]));
        assert_eq!(keys(&got["items"]), ["Z", "A"]);
        assert_eq!(got["items"]["Z"]["o"], json!([1, 2, 3]));
        assert_eq!(got["items"]["A"]["d"], json!([104, 105, 106]));
    }

    // --- politique d'échec -------------------------------------------------------------------

    /// 20 départements pour un seul métier = 20 cellules : 2 échecs = 10 % (toléré), 3 = 15 %.
    fn twenty_deps() -> Vec<String> {
        (1..=20).map(|i| format!("{i:02}")).collect()
    }

    async fn failing_cells(failing: usize, endpoint: &'static str) -> MockServer {
        let deps = twenty_deps();
        let deps: Vec<&str> = deps.iter().map(String::as_str).collect();
        let failing: Vec<String> = deps[..failing].iter().map(|d| d.to_string()).collect();
        carte_server(&["A"], &deps, move |ep, code, dep| {
            if ep == endpoint && failing.iter().any(|f| f == dep) {
                ResponseTemplate::new(500)
            } else {
                simple_answer(ep, code, dep)
            }
        })
        .await
    }

    #[tokio::test]
    async fn up_to_ten_percent_failed_cells_are_null_and_the_file_is_written() {
        // une cellule en échec sur l'une OU l'autre de ses deux requêtes : toute la cellule est null
        for endpoint in ["stat-offres", "stat-demandeurs"] {
            let srv = failing_cells(2, endpoint).await; // 2 / 20 = 10 % exactement : toléré
            let (dir, store, _) = store_with_rome(&rome_of(&[("A", json!([1]))]));
            let deps = twenty_deps();
            let deps: Vec<&str> = deps.iter().map(String::as_str).collect();
            run_with(&client(&srv), &store, day(), 1, &deps, 6)
                .await
                .unwrap_or_else(|e| panic!("{endpoint} : {e}"));
            let got = payload(dir.path(), "carte.js", "CARTE");
            let o = got["items"]["A"]["o"].as_array().unwrap();
            let d = got["items"]["A"]["d"].as_array().unwrap();
            assert_eq!((o.len(), d.len()), (20, 20));
            assert!(o[..2].iter().all(Value::is_null), "{endpoint}");
            assert!(d[..2].iter().all(Value::is_null), "{endpoint}");
            assert!(o[2..].iter().all(|v| *v == json!(10)), "{endpoint}");
            assert!(d[2..].iter().all(|v| *v == json!(7)), "{endpoint}");
            no_leftover(dir.path(), 2);
        }
    }

    #[tokio::test]
    async fn more_than_ten_percent_failed_cells_keep_the_previous_file() {
        for endpoint in ["stat-offres", "stat-demandeurs"] {
            let srv = failing_cells(3, endpoint).await; // 3 / 20 = 15 % : refusé
            let (dir, store, sentinel) = store_with_rome(&rome_of(&[("A", json!([1]))]));
            let deps = twenty_deps();
            let deps: Vec<&str> = deps.iter().map(String::as_str).collect();
            let err = run_with(&client(&srv), &store, day(), 1, &deps, 6)
                .await
                .unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("3 échec(s) sur 20 cellule(s)") && msg.contains("carte"),
                "{msg}"
            );
            assert!(
                msg.contains("carte A "),
                "le premier échec est nommé : {msg}"
            );
            for secret in [SECRET, CLIENT_ID, TOKEN] {
                assert!(!msg.contains(secret));
            }
            assert_eq!(
                std::fs::read(dir.path().join("carte.js")).unwrap(),
                sentinel,
                "{endpoint}"
            );
            no_leftover(dir.path(), 2);
        }
    }

    #[tokio::test]
    async fn failure_threshold_is_exclusive_with_integer_arithmetic() {
        // 10 cellules (1 métier x 10 départements) : 1 échec = 10 % toléré, 2 échecs refusés
        let deps: Vec<String> = (1..=10).map(|i| format!("{i:02}")).collect();
        let deps: Vec<&str> = deps.iter().map(String::as_str).collect();
        for (failing, ok_expected) in [(0usize, true), (1, true), (2, false)] {
            let bad: Vec<&str> = deps[..failing].to_vec();
            let srv = carte_server(&["A"], &deps, |ep, code, dep| {
                if bad.contains(&dep) {
                    ResponseTemplate::new(503)
                } else {
                    simple_answer(ep, code, dep)
                }
            })
            .await;
            let (_dir, store, _) = store_with_rome(&rome_of(&[("A", json!([1]))]));
            let res = run_with(&client(&srv), &store, day(), 1, &deps, 3).await;
            assert_eq!(res.is_ok(), ok_expected, "{failing} échec(s) : {res:?}");
        }
    }

    #[tokio::test]
    async fn all_cells_without_data_write_nothing() {
        let deps = ["01", "02"];
        let srv = carte_server(&["A"], &deps, |_, _, _| ResponseTemplate::new(404)).await;
        let (dir, store, sentinel) = store_with_rome(&rome_of(&[("A", json!([1]))]));
        let err = run_with(&client(&srv), &store, day(), 1, &deps, 2)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("aucune cellule"), "{err}");
        assert_eq!(
            std::fs::read(dir.path().join("carte.js")).unwrap(),
            sentinel
        );
        no_leftover(dir.path(), 2);
    }

    #[tokio::test]
    async fn missing_or_unreadable_rome_js_is_an_explicit_failure() {
        let srv = server().await;
        // absent
        let (dir, store, sentinel) = with_sentinel("carte.js", "CARTE");
        let err = run_with(&client(&srv), &store, day(), 1, &["01"], 1)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("carte : rome.js absent"), "{err}");
        assert_eq!(
            std::fs::read(dir.path().join("carte.js")).unwrap(),
            sentinel
        );
        // illisible : pas la forme window.ROME=…, JSON invalide, sans « items », sans métier
        for bad in [
            "n'importe quoi",
            "window.ROME={pas du json};",
            "window.ROME={};",
            r#"window.ROME={"items":{}};"#,
        ] {
            let (dir, store, sentinel) = with_sentinel("carte.js", "CARTE");
            std::fs::write(dir.path().join("rome.js"), bad).unwrap();
            let err = run_with(&client(&srv), &store, day(), 1, &["01"], 1)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("carte :"), "{bad} : {err}");
            assert_eq!(
                std::fs::read(dir.path().join("carte.js")).unwrap(),
                sentinel
            );
        }
        // aucun appel n'a été fait
        assert!(srv
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.url.path() == "/token"));
        // aucun département : rien à collecter
        let (_dir, store, _) = store_with_rome(&rome_of(&[("A", json!([1]))]));
        run_with(&client(&srv), &store, day(), 1, &[], 1)
            .await
            .unwrap_err();
    }

    #[tokio::test]
    async fn unreadable_json_answers_are_failed_cells_not_nulls() {
        let deps = ["01"];
        let srv = carte_server(&["A"], &deps, |_, _, _| {
            ResponseTemplate::new(200).set_body_string("<html>erreur</html>")
        })
        .await;
        let (dir, store, sentinel) = store_with_rome(&rome_of(&[("A", json!([1]))]));
        let err = run_with(&client(&srv), &store, day(), 1, &deps, 1)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("carte"), "{err}");
        assert_eq!(
            std::fs::read(dir.path().join("carte.js")).unwrap(),
            sentinel
        );
    }

    #[tokio::test]
    async fn retries_transient_errors_before_giving_up_on_a_cell() {
        // 429 puis données : la cellule réussit grâce aux reprises du client (5 essais)
        let deps = ["01"];
        let srv = server().await;
        for (ep, body) in [
            (
                "stat-offres",
                rows(
                    "TOFF",
                    &[
                        ("2025T3", Some(1)),
                        ("2025T4", Some(1)),
                        ("2026T1", Some(1)),
                        ("2026T2", Some(1)),
                    ],
                ),
            ),
            ("stat-demandeurs", rows("A", &[("2026T2", Some(5))])),
        ] {
            Mock::given(method("POST"))
                .and(path(format!("/api/v1/indicateur/{ep}")))
                .respond_with(ResponseTemplate::new(429))
                .up_to_n_times(2)
                .mount(&srv)
                .await;
            Mock::given(method("POST"))
                .and(path(format!("/api/v1/indicateur/{ep}")))
                .respond_with(ok(body))
                .mount(&srv)
                .await;
        }
        let (dir, store, _) = store_with_rome(&rome_of(&[("A", json!([1]))]));
        run_with(&client(&srv), &store, day(), 1, &deps, 1)
            .await
            .unwrap();
        let got = payload(dir.path(), "carte.js", "CARTE");
        assert_eq!(got["items"]["A"], json!({"o": [4], "d": [5]}));
    }

    // --- simultanéité ------------------------------------------------------------------------

    #[tokio::test]
    async fn at_most_six_calls_run_at_once_in_production_settings() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counting {
            active: AtomicUsize,
            peak: AtomicUsize,
            calls: AtomicUsize,
        }
        #[allow(clippy::manual_async_fn)]
        impl StatApi for Counting {
            fn call<'a>(
                &'a self,
                _path: &'a str,
                body: Option<&'a Value>,
            ) -> impl std::future::Future<Output = Result<Option<Value>, super::super::ft::FtError>>
                   + Send
                   + 'a {
                async move {
                    assert_eq!(body.unwrap()["codeTypeTerritoire"], "DEP");
                    let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                    self.peak.fetch_max(n, Ordering::SeqCst);
                    self.calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    self.active.fetch_sub(1, Ordering::SeqCst);
                    Ok(Some(rows("A", &[("2026T2", Some(1))])))
                }
            }
        }
        let api = Counting {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
        };
        let (_dir, store, _) = store_with_rome(&rome_of(&[("A", json!([1])), ("B", json!([2]))]));
        // 2 métiers x 96 départements (liste de production) = 192 cellules = 384 appels, 2 par cellule
        run_with(&api, &store, day(), TOP_N, &DEPS, WORKERS)
            .await
            .unwrap();
        assert_eq!(api.calls.load(Ordering::SeqCst), 2 * 96 * 2);
        let peak = api.peak.load(Ordering::SeqCst);
        assert!((2..=6).contains(&peak), "pic de simultanéité : {peak}");
        // `run` (constantes de production) = run_with(TOP_N, DEPS, WORKERS)
        let api2 = Counting {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
        };
        run(&api2, &store, day()).await.unwrap();
        assert_eq!(api2.calls.load(Ordering::SeqCst), 2 * 96 * 2);
    }
}
