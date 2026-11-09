use super::ParseError;
use serde_json::Value;

/// Date du milieu de période : `2026-Q2` → `2026-05-15`, `2026-09` → `2026-09-15`.
/// `None` si l'étiquette n'a pas l'une de ces deux formes.
fn try_period_to_date(p: &str) -> Option<String> {
    fn year(y: &str) -> Option<u32> {
        if y.len() == 4 && y.bytes().all(|b| b.is_ascii_digit()) {
            y.parse().ok()
        } else {
            None
        }
    }
    if let Some((y, q)) = p.split_once("-Q") {
        let q: u32 = q.parse().ok().filter(|q| (1..=4).contains(q))?;
        return Some(format!("{}-{:02}-15", year(y)?, (q - 1) * 3 + 2));
    }
    let (y, m) = p.split_once('-')?;
    let m: u32 = m.parse().ok().filter(|m| (1..=12).contains(m) && p.len() == 7)?;
    Some(format!("{}-{:02}-15", year(y)?, m))
}

/// Comme `try_period_to_date`, mais renvoie l'étiquette telle quelle si elle est inconnue
/// (l'analyseur, lui, rejette ces étiquettes : voir `parse_jsonstat`). Réservé aux tests.
#[cfg(test)]
pub fn period_to_date(p: &str) -> String {
    try_period_to_date(p).unwrap_or_else(|| p.to_string())
}

fn number(v: &Value, pos: usize) -> Result<Option<f64>, ParseError> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => n
            .as_f64()
            .map(Some)
            .ok_or_else(|| ParseError::Format(format!("valeur illisible en position {pos}"))),
        other => Err(ParseError::Format(format!(
            "valeur non numérique en position {pos} : {other}"
        ))),
    }
}

/// Points `(date, valeur)` d'une réponse JSON-stat Eurostat, triés par position de période ;
/// les valeurs absentes sont ignorées.
pub fn parse_jsonstat(json: &str) -> Result<Vec<(String, f64)>, ParseError> {
    let js: Value =
        serde_json::from_str(json).map_err(|e| ParseError::Format(format!("JSON invalide : {e}")))?;
    let index = js
        .pointer("/dimension/time/category/index")
        .ok_or_else(|| ParseError::Format("dimension temporelle absente".into()))?;
    // `index` : objet {étiquette: position} (ou tableau d'étiquettes dans la norme JSON-stat).
    let mut periods: Vec<(usize, String)> = match index {
        Value::Object(m) => {
            let mut v = Vec::with_capacity(m.len());
            for (label, pos) in m {
                let pos = pos
                    .as_u64()
                    .ok_or_else(|| ParseError::Format(format!("position invalide pour {label}")))?;
                v.push((pos as usize, label.clone()));
            }
            v
        }
        Value::Array(a) => {
            let mut v = Vec::with_capacity(a.len());
            for (pos, label) in a.iter().enumerate() {
                let label = label
                    .as_str()
                    .ok_or_else(|| ParseError::Format("étiquette de période invalide".into()))?;
                v.push((pos, label.to_string()));
            }
            v
        }
        _ => return Err(ParseError::Format("index temporel invalide".into())),
    };
    periods.sort();
    let values = js
        .get("value")
        .ok_or_else(|| ParseError::Format("champ « value » absent".into()))?;
    let mut out = Vec::new();
    for (pos, label) in periods {
        let v = match values {
            Value::Object(m) => m.get(&pos.to_string()),
            Value::Array(a) => a.get(pos),
            _ => return Err(ParseError::Format("champ « value » invalide".into())),
        };
        let Some(v) = v else { continue };
        let Some(v) = number(v, pos)? else { continue };
        let date = try_period_to_date(&label)
            .ok_or_else(|| ParseError::Format(format!("période inconnue : {label}")))?;
        out.push((date, v));
    }
    if out.is_empty() {
        return Err(ParseError::Format("aucune valeur dans la réponse JSON-stat".into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(v: Vec<(String, f64)>) -> Value {
        Value::Array(v.into_iter().map(|(d, x)| serde_json::json!([d, x])).collect())
    }
    fn gold(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn period_to_date_quarters_and_months() {
        assert_eq!(period_to_date("2020-Q1"), "2020-02-15");
        assert_eq!(period_to_date("2020-Q2"), "2020-05-15");
        assert_eq!(period_to_date("2020-Q4"), "2020-11-15");
        assert_eq!(period_to_date("2026-Q2"), "2026-05-15");
        assert_eq!(period_to_date("2021-03"), "2021-03-15");
        assert_eq!(period_to_date("2026-09"), "2026-09-15");
    }

    #[test]
    fn jsonstat_skips_missing_values() {
        // positions 0, 2 absentes ; 3 nulle ; ordre des étiquettes volontairement non trié
        let js = r#"{"dimension":{"time":{"category":{"index":
            {"2020-Q3":2,"2020-Q1":0,"2020-Q2":1,"2020-Q4":3,"2021-Q1":4}}}},
            "value":{"1":10.5,"3":null,"4":12}}"#;
        let v = parse_jsonstat(js).unwrap();
        assert_eq!(
            v,
            vec![("2020-05-15".to_string(), 10.5), ("2021-02-15".to_string(), 12.0)]
        );
        // forme dense (tableau avec null)
        let dense = r#"{"dimension":{"time":{"category":{"index":{"2020-01":0,"2020-02":1,"2020-03":2}}}},
            "value":[null,3.5,4]}"#;
        assert_eq!(
            parse_jsonstat(dense).unwrap(),
            vec![("2020-02-15".to_string(), 3.5), ("2020-03-15".to_string(), 4.0)]
        );
    }

    #[test]
    fn jsonstat_matches_python_golden() {
        let cases = [
            (
                include_str!("../../tests/fixtures/eurostat/chomage.json"),
                include_str!("../../tests/golden/eurostat_chomage.json"),
            ),
            (
                include_str!("../../tests/fixtures/eurostat/pib.json"),
                include_str!("../../tests/golden/eurostat_pib.json"),
            ),
            (
                include_str!("../../tests/fixtures/eurostat/exports.json"),
                include_str!("../../tests/golden/eurostat_exports.json"),
            ),
            (
                include_str!("../../tests/fixtures/eurostat/imports.json"),
                include_str!("../../tests/golden/eurostat_imports.json"),
            ),
        ];
        for (fixture, golden) in cases {
            let got = parse_jsonstat(fixture).unwrap();
            assert!(!got.is_empty());
            // Les listes sont ordonnées : l'égalité de Value::Array l'est aussi.
            assert_eq!(pts(got), gold(golden));
        }
    }

    #[test]
    fn jsonstat_rejects_html() {
        assert!(matches!(parse_jsonstat("<html>502</html>"), Err(ParseError::Format(_))));
        assert!(matches!(parse_jsonstat(""), Err(ParseError::Format(_))));
        assert!(matches!(parse_jsonstat("{}"), Err(ParseError::Format(_))));
    }

    #[test]
    fn jsonstat_rejects_series_without_values() {
        let none = r#"{"dimension":{"time":{"category":{"index":{"2020-01":0,"2020-02":1}}}},"value":{}}"#;
        assert!(matches!(parse_jsonstat(none), Err(ParseError::Format(_))));
        let nulls = r#"{"dimension":{"time":{"category":{"index":{"2020-01":0}}}},"value":[null]}"#;
        assert!(matches!(parse_jsonstat(nulls), Err(ParseError::Format(_))));
        let empty_idx = r#"{"dimension":{"time":{"category":{"index":{}}}},"value":{}}"#;
        assert!(matches!(parse_jsonstat(empty_idx), Err(ParseError::Format(_))));
    }

    #[test]
    fn jsonstat_rejects_bad_value_or_period() {
        let s = r#"{"dimension":{"time":{"category":{"index":{"2020-01":0}}}},"value":{"0":"abc"}}"#;
        assert!(matches!(parse_jsonstat(s), Err(ParseError::Format(_))));
        let p = r#"{"dimension":{"time":{"category":{"index":{"2020-13":0}}}},"value":{"0":1}}"#;
        assert!(matches!(parse_jsonstat(p), Err(ParseError::Format(_))));
    }
}
