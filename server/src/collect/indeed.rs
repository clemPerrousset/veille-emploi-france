use super::ParseError;
use serde_json::{Map, Value};

/// Lit l'en-tête, vérifie les colonnes requises et renvoie leurs positions avec les lignes.
fn read(csv_text: &str, required: &[&str]) -> Result<(Vec<usize>, Vec<csv::StringRecord>), ParseError> {
    let mut rdr = csv::Reader::from_reader(csv_text.as_bytes());
    let headers = rdr
        .headers()
        .map_err(|e| ParseError::Format(format!("en-tête illisible : {e}")))?
        .clone();
    let mut idx = Vec::with_capacity(required.len());
    for col in required {
        let pos = headers
            .iter()
            .position(|h| h.trim_start_matches('\u{feff}') == *col)
            .ok_or_else(|| ParseError::Format(format!("colonne absente : {col}")))?;
        idx.push(pos);
    }
    let mut rows = Vec::new();
    for (n, rec) in rdr.records().enumerate() {
        rows.push(rec.map_err(|e| ParseError::Format(format!("ligne {} invalide : {e}", n + 2)))?);
    }
    Ok((idx, rows))
}

fn number(s: &str, line_hint: &str) -> Result<Value, ParseError> {
    let f: f64 = s
        .trim()
        .parse()
        .map_err(|_| ParseError::Format(format!("indice non numérique {s:?} ({line_hint})")))?;
    serde_json::Number::from_f64(f)
        .map(Value::Number)
        .ok_or_else(|| ParseError::Format(format!("indice non fini {s:?} ({line_hint})")))
}

/// `{"total postings":{"dates":[..],"values":[..]},"new postings":{..}}`
pub fn parse_aggregate(csv: &str) -> Result<Value, ParseError> {
    let (idx, rows) = read(csv, &["date", "variable", "indeed_job_postings_index_SA"])?;
    let mut out = Map::new();
    for variable in ["total postings", "new postings"] {
        let mut dates = Vec::new();
        let mut values = Vec::new();
        for r in rows.iter().filter(|r| &r[idx[1]] == variable) {
            dates.push(Value::String(r[idx[0]].to_string()));
            values.push(number(&r[idx[2]], &r[idx[0]])?);
        }
        if dates.is_empty() {
            return Err(ParseError::Format(format!("aucune ligne « {variable} »")));
        }
        let mut series = Map::new();
        series.insert("dates".into(), Value::Array(dates));
        series.insert("values".into(), Value::Array(values));
        out.insert(variable.into(), Value::Object(series));
    }
    Ok(Value::Object(out))
}

type Category = (String, Vec<String>, Vec<Value>);

/// `{"dates":[..],"series":{catégorie:[..]}}` (lignes « total postings » uniquement).
pub fn parse_sectors(csv: &str) -> Result<Value, ParseError> {
    let (idx, rows) = read(
        csv,
        &["date", "variable", "display_name", "indeed_job_postings_index"],
    )?;
    // Ordre d'apparition conservé.
    let mut cats: Vec<Category> = Vec::new();
    for r in rows.iter().filter(|r| &r[idx[1]] == "total postings") {
        let name = &r[idx[2]];
        let pos = match cats.iter().position(|c| c.0 == name) {
            Some(p) => p,
            None => {
                cats.push((name.to_string(), Vec::new(), Vec::new()));
                cats.len() - 1
            }
        };
        cats[pos].1.push(r[idx[0]].to_string());
        cats[pos].2.push(number(&r[idx[3]], &r[idx[0]])?);
    }
    // Référence = première catégorie la plus longue (comme `max` de Python).
    let mut reference: Option<&Vec<String>> = None;
    for c in &cats {
        if reference.is_none_or(|r| c.1.len() > r.len()) {
            reference = Some(&c.1);
        }
    }
    let reference = reference
        .ok_or_else(|| ParseError::Format("aucune ligne « total postings »".into()))?
        .clone();
    for (name, dates, _) in &cats {
        if *dates != reference {
            return Err(ParseError::Mismatch(format!("dates différentes pour {name}")));
        }
    }
    let mut series = Map::new();
    for (name, _, values) in cats {
        series.insert(name, Value::Array(values));
    }
    let mut out = Map::new();
    out.insert(
        "dates".into(),
        Value::Array(reference.into_iter().map(Value::String).collect()),
    );
    out.insert("series".into(), Value::Object(series));
    Ok(Value::Object(out))
}
#[cfg(test)]
mod tests {
    use super::*;

    const AGG: &str = include_str!("../../tests/fixtures/indeed/aggregate.csv");
    const SEC: &str = include_str!("../../tests/fixtures/indeed/sectors.csv");
    const AGG_GOLD: &str = include_str!("../../tests/golden/indeed_aggregate.json");
    const SEC_GOLD: &str = include_str!("../../tests/golden/indeed_sectors.json");

    fn json(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn aggregate_matches_python_golden() {
        assert_eq!(parse_aggregate(AGG).unwrap(), json(AGG_GOLD));
    }

    #[test]
    fn sectors_matches_python_golden() {
        assert_eq!(parse_sectors(SEC).unwrap(), json(SEC_GOLD));
    }

    #[test]
    fn sectors_keep_first_seen_order() {
        let csv = "date,jobcountry,indeed_job_postings_index,variable,display_name\n\
2020-02-01,FR,1,total postings,Zeta\n\
2020-02-02,FR,2,total postings,Zeta\n\
2020-02-01,FR,3,total postings,Alpha\n\
2020-02-02,FR,4,total postings,Alpha\n\
2020-02-01,FR,5,total postings,\"B, C\"\n\
2020-02-02,FR,6,total postings,\"B, C\"\n";
        let v = parse_sectors(csv).unwrap();
        let keys: Vec<&str> = v["series"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["Zeta", "Alpha", "B, C"]);
        let top: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(top, ["dates", "series"]);
    }

    #[test]
    fn aggregate_keeps_python_key_order() {
        let v = parse_aggregate(AGG).unwrap();
        let top: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(top, ["total postings", "new postings"]);
        for k in top {
            let inner: Vec<&str> = v[k].as_object().unwrap().keys().map(String::as_str).collect();
            assert_eq!(inner, ["dates", "values"]);
        }
    }

    #[test]
    fn bom_and_crlf_give_same_output() {
        let bom = format!("\u{feff}{AGG}");
        assert_eq!(parse_aggregate(&bom).unwrap(), parse_aggregate(AGG).unwrap());
        let crlf = AGG.replace('\n', "\r\n");
        assert_eq!(parse_aggregate(&crlf).unwrap(), parse_aggregate(AGG).unwrap());
        let crlf = SEC.replace('\n', "\r\n");
        assert_eq!(parse_sectors(&crlf).unwrap(), parse_sectors(SEC).unwrap());
    }

    #[test]
    fn sectors_reject_misaligned_dates() {
        let mut lines: Vec<&str> = SEC.lines().collect();
        // retire la dernière ligne « total postings » (dernière catégorie)
        let idx = lines.iter().rposition(|l| l.contains("total postings")).unwrap();
        lines.remove(idx);
        let csv = lines.join("\n");
        assert!(matches!(parse_sectors(&csv), Err(ParseError::Mismatch(_))));
    }

    #[test]
    fn parse_rejects_html_error_page() {
        assert!(matches!(parse_aggregate("<html>502</html>"), Err(ParseError::Format(_))));
        assert!(matches!(parse_sectors("<html>502</html>"), Err(ParseError::Format(_))));
    }

    #[test]
    fn parse_rejects_empty_body() {
        assert!(matches!(parse_aggregate(""), Err(ParseError::Format(_))));
        assert!(matches!(parse_sectors(""), Err(ParseError::Format(_))));
    }

    #[test]
    fn parse_rejects_truncated_row() {
        let a = "date,jobcountry,indeed_job_postings_index_SA,indeed_job_postings_index_NSA,variable\n2020-02-01,FR\n";
        assert!(matches!(parse_aggregate(a), Err(ParseError::Format(_))));
        let s = "date,jobcountry,indeed_job_postings_index,variable,display_name\n2020-02-01,FR,100\n";
        assert!(matches!(parse_sectors(s), Err(ParseError::Format(_))));
    }

    #[test]
    fn non_numeric_index_is_an_error_not_zero() {
        let a = "date,jobcountry,indeed_job_postings_index_SA,indeed_job_postings_index_NSA,variable\n2020-02-01,FR,abc,1,total postings\n";
        assert!(matches!(parse_aggregate(a), Err(ParseError::Format(_))));
        let s = "date,jobcountry,indeed_job_postings_index,variable,display_name\n2020-02-01,FR,,total postings,X\n";
        assert!(matches!(parse_sectors(s), Err(ParseError::Format(_))));
        let n = "date,jobcountry,indeed_job_postings_index,variable,display_name\n2020-02-01,FR,NaN,total postings,X\n";
        assert!(matches!(parse_sectors(n), Err(ParseError::Format(_))));
    }
}
