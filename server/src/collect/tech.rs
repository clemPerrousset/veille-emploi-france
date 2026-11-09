//! Collecteur `tech` : demandeurs, embauches et tension des métiers du numérique → `tech.js`.
//! Même résultat que `fetch_tech.py`.

use super::rome::MIN_PERIOD;
use super::{ft_error, CollectError, StatApi};
use crate::store::Store;
use chrono::NaiveDate;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

pub const TECH_CODES: [&str; 18] = [
    "M1805", "M1855", "M1861", "M1853", "M1827", "M1811", "M1419", "M1405", "M1856", "M1884",
    "M1802", "M1801", "M1810", "M1806", "M1867", "M1803", "I1404", "I1401",
];

/// Série d'une réponse : valeurs non nulles de la nomenclature voulue par période (ordre de la
/// réponse), et libellé de la dernière ligne retenue.
pub fn parse_series(resp: &Value, wanted: &str) -> (Map<String, Value>, Option<String>) {
    let mut out = Map::new();
    let mut label = None;
    for x in resp
        .get("listeValeursParPeriode")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if x.get("codeNomenclature").and_then(Value::as_str) != Some(wanted) {
            continue;
        }
        let (Some(period), Some(v)) = (
            x.get("codePeriode").and_then(Value::as_str),
            x.get("valeurPrincipaleNombre").filter(|v| v.is_number()),
        ) else {
            continue;
        };
        out.insert(period.to_string(), v.clone());
        label = x
            .get("libActivite")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    (out, label)
}

type Series = (Map<String, Value>, Option<String>);

/// Une série de l'API ; tout échec nomme le code et le point d'accès.
async fn series<C: StatApi>(
    client: &C,
    code: &str,
    endpoint: &str,
    period: &str,
    nomenclature: &str,
    wanted: &str,
) -> Result<Series, CollectError> {
    let body = json!({"codeTypeTerritoire": "NAT", "codeTerritoire": "FR",
        "codeTypeActivite": "ROME", "codeActivite": code,
        "codeTypePeriode": period, "codeTypeNomenclature": nomenclature});
    let path = format!("/v1/indicateur/{endpoint}");
    let resp = client
        .call(&path, Some(&body))
        .await
        .map_err(|e| ft_error(&format!("france travail tech {code} {endpoint}"), e))?;
    Ok(resp.map_or_else(|| (Map::new(), None), |r| parse_series(&r, wanted)))
}

/// `tech.js` pour les codes de `TECH_CODES`.
pub async fn run<C: StatApi>(
    client: &C,
    store: &Store,
    today: NaiveDate,
) -> Result<(), CollectError> {
    run_with_codes(client, store, today, &TECH_CODES).await
}

/// Comme `run`, pour une liste de codes donnée (les tests utilisent les deux codes enregistrés).
/// Le moindre appel en échec est fatal : le fichier précédent reste intact.
pub async fn run_with_codes<C: StatApi>(
    client: &C,
    store: &Store,
    today: NaiveDate,
    codes: &[&str],
) -> Result<(), CollectError> {
    let mut raw = Vec::new();
    for code in codes {
        let (d, l1) = series(client, code, "stat-demandeurs", "TRIMESTRE", "CATCAND", "A").await?;
        let (e, l2) = series(
            client,
            code,
            "stat-embauches",
            "TRIMESTRE",
            "CATCANDxDUREEEMP",
            "TOUT-TOUTE",
        )
        .await?;
        let (t, l3) = series(
            client,
            code,
            "stat-perspective-employeur",
            "ANNEE",
            "TYPE_TENSION",
            "PERSPECTIVE",
        )
        .await?;
        let label = l1.or(l2).or(l3).unwrap_or_else(|| code.to_string());
        raw.push((code.to_string(), label, d, e, t));
    }
    if raw
        .iter()
        .all(|(_, _, d, e, t)| d.is_empty() && e.is_empty() && t.is_empty())
    {
        return Err(CollectError::Fetch(
            "france travail tech : aucune donnée reçue".into(),
        ));
    }
    let periods: BTreeSet<&String> = raw
        .iter()
        .flat_map(|(_, _, d, e, _)| d.keys().chain(e.keys()))
        .filter(|p| p.as_str() >= MIN_PERIOD)
        .collect();
    let aligned = |m: &Map<String, Value>| -> Vec<Value> {
        periods
            .iter()
            .map(|p| m.get(*p).cloned().unwrap_or(Value::Null))
            .collect()
    };
    let mut items = Map::new();
    for (code, label, d, e, t) in &raw {
        items.insert(
            code.clone(),
            json!({"l": label, "d": aligned(d), "e": aligned(e), "t": t}),
        );
    }
    let out = json!({
        "updated": today.format("%Y-%m-%d").to_string(),
        "periods": periods,
        "items": items,
    });
    store
        .save("tech.js", &out)
        .map_err(|e| CollectError::Write(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CODES: [&str; 2] = ["M1805", "M1856"];

    /// (point d'accès, préfixe de fixture, nomenclature de requête, type de période)
    const CALLS: [(&str, &str, &str, &str); 3] = [
        ("stat-demandeurs", "demandeurs", "CATCAND", "TRIMESTRE"),
        (
            "stat-embauches",
            "embauches",
            "CATCANDxDUREEEMP",
            "TRIMESTRE",
        ),
        (
            "stat-perspective-employeur",
            "tension",
            "TYPE_TENSION",
            "ANNEE",
        ),
    ];

    async fn tech_server(per_call: impl Fn(&str, &str) -> ResponseTemplate) -> MockServer {
        let srv = server().await;
        for code in CODES {
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

    #[tokio::test]
    async fn tech_matches_python_golden() {
        let srv = tech_server(fixture_resp).await;
        let (dir, store, _) = with_sentinel("tech.js", "TECH");
        run_with_codes(&client(&srv), &store, day(), &CODES)
            .await
            .unwrap();
        let got = payload(dir.path(), "tech.js", "TECH");
        let gold = golden("tech");
        assert_eq!(got, gold);
        assert_eq!(keys(&got), ["updated", "periods", "items"]);
        assert_eq!(keys(&got["items"]), ["M1805", "M1856"]);
        assert_eq!(keys(&got["items"]["M1805"]), ["l", "d", "e", "t"]);
        // octet pour octet : mêmes nombres, même ordre des années de `t`
        let text = std::fs::read_to_string(dir.path().join("tech.js")).unwrap();
        assert_eq!(text, format!("window.TECH={};", gold));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(
            TECH_CODES,
            [
                "M1805", "M1855", "M1861", "M1853", "M1827", "M1811", "M1419", "M1405", "M1856",
                "M1884", "M1802", "M1801", "M1810", "M1806", "M1867", "M1803", "I1404", "I1401"
            ]
        );
    }

    /// Réponse minimale d'une série.
    fn rows(nomen: &str, vals: &[(&str, Option<i64>)]) -> ResponseTemplate {
        let l: Vec<Value> = vals
            .iter()
            .map(|(p, v)| {
                json!({"codeNomenclature": nomen, "codePeriode": p, "libActivite": "Lib",
                       "valeurPrincipaleNombre": v})
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({"listeValeursParPeriode": l}))
    }

    #[tokio::test]
    async fn tech_periods_are_union_of_demandeurs_and_embauches_from_2021t1() {
        // jeux construits à la main (la tension ne compte pas dans les périodes)
        let srv = tech_server(|prefix, _| match prefix {
            "demandeurs" => rows(
                "A",
                &[
                    ("2020T4", Some(1)),
                    ("2021T1", Some(2)),
                    ("2021T3", None),
                    ("2022T1", Some(4)),
                ],
            ),
            "embauches" => rows(
                "TOUT-TOUTE",
                &[
                    ("2019T1", Some(9)),
                    ("2021T2", Some(5)),
                    ("2022T1", Some(6)),
                ],
            ),
            _ => rows("PERSPECTIVE", &[("2015", Some(3))]),
        })
        .await;
        let (dir, store, _) = with_sentinel("tech.js", "TECH");
        run_with_codes(&client(&srv), &store, day(), &["M1805"])
            .await
            .unwrap();
        let got = payload(dir.path(), "tech.js", "TECH");
        assert_eq!(got["periods"], json!(["2021T1", "2021T2", "2022T1"]));
        assert_eq!(got["items"]["M1805"]["d"], json!([2, null, 4]));
        assert_eq!(got["items"]["M1805"]["e"], json!([null, 5, 6]));
        assert_eq!(got["items"]["M1805"]["l"], "Lib");
        // la tension garde ses années, même avant 2021
        assert_eq!(got["items"]["M1805"]["t"], json!({"2015": 3}));
    }

    #[tokio::test]
    async fn tech_tension_is_year_to_level_map() {
        let srv = tech_server(fixture_resp).await;
        let (dir, store, _) = with_sentinel("tech.js", "TECH");
        run_with_codes(&client(&srv), &store, day(), &CODES)
            .await
            .unwrap();
        let got = payload(dir.path(), "tech.js", "TECH");
        let t = &got["items"]["M1805"]["t"];
        assert_eq!(
            t,
            &json!({"2025": 4, "2024": 5, "2023": 5, "2022": 5, "2021": 4,
                    "2020": 5, "2019": 5, "2018": 5, "2017": 4, "2016": 4})
        );
        assert_eq!(keys(t)[0], "2025");
        assert!(t.as_object().unwrap().values().all(|v| v.is_u64()));
    }

    fn endpoint_of(prefix: &str) -> &'static str {
        CALLS.iter().find(|c| c.1 == prefix).unwrap().0
    }

    #[tokio::test]
    async fn tech_failures_are_fatal_and_keep_previous_file() {
        for failing in ["demandeurs", "embauches", "tension"] {
            let srv = tech_server(|prefix, code| {
                if prefix == failing && code == "M1856" {
                    ResponseTemplate::new(200).set_body_string("<html>erreur</html>")
                } else {
                    fixture_resp(prefix, code)
                }
            })
            .await;
            let (dir, store, sentinel) = with_sentinel("tech.js", "TECH");
            let err = run_with_codes(&client(&srv), &store, day(), &CODES)
                .await
                .unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("tech M1856") && msg.contains(endpoint_of(failing)),
                "{msg}"
            );
            for secret in [SECRET, CLIENT_ID, TOKEN] {
                assert!(!msg.contains(secret));
            }
            assert_eq!(std::fs::read(dir.path().join("tech.js")).unwrap(), sentinel);
        }
        // aucune donnée du tout : rien n'est écrit
        let srv = tech_server(|_, _| ResponseTemplate::new(404)).await;
        let (dir, store, sentinel) = with_sentinel("tech.js", "TECH");
        run_with_codes(&client(&srv), &store, day(), &CODES)
            .await
            .unwrap_err();
        assert_eq!(std::fs::read(dir.path().join("tech.js")).unwrap(), sentinel);
    }
}
