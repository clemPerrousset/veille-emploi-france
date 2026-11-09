use super::ParseError;
use serde_json::{json, Map, Value};
use std::cmp::Ordering;

/// Lit un CSV DARES (séparateur `;`, BOM éventuellement doublé) : positions des colonnes
/// requises et lignes. Une page d'erreur ou un mauvais séparateur donne `Format`.
fn read(csv_text: &str, required: &[&str]) -> Result<(Vec<usize>, Vec<csv::StringRecord>), ParseError> {
    let mut rdr = csv::ReaderBuilder::new().delimiter(b';').from_reader(csv_text.as_bytes());
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

/// `AAAA-MM` → `AAAA-MM-15` ; `AAAA-Tn` → milieu de trimestre (`AAAA-{3n-1}-15`).
fn dares_date(p: &str) -> Result<String, ParseError> {
    let bad = || ParseError::Format(format!("date DARES invalide : {p:?}"));
    let year_ok = |y: &str| y.len() == 4 && y.bytes().all(|b| b.is_ascii_digit());
    if let Some((y, q)) = p.split_once("-T") {
        let q: u32 = q.parse().ok().filter(|q| (1..=4).contains(q)).ok_or_else(bad)?;
        if !year_ok(y) {
            return Err(bad());
        }
        return Ok(format!("{y}-{:02}-15", (q - 1) * 3 + 2));
    }
    let (y, m) = p.split_once('-').ok_or_else(bad)?;
    let m_ok = m.len() == 2 && m.parse::<u32>().is_ok_and(|m| (1..=12).contains(&m));
    if !year_ok(y) || !m_ok {
        return Err(bad());
    }
    Ok(format!("{p}-15"))
}

fn float(s: &str, what: &str, date: &str) -> Result<f64, ParseError> {
    s.trim()
        .parse::<f64>()
        .ok()
        .filter(|f| f.is_finite())
        .ok_or_else(|| ParseError::Format(format!("{what} non numérique {s:?} ({date})")))
}

fn sort_points(pts: &mut [(String, f64)]) {
    pts.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal)));
}

/// Offres d'emploi collectées (France Travail), France métropolitaine, mensuelles, ≥ 2019-01.
pub fn parse_offres(csv: &str) -> Result<Vec<(String, f64)>, ParseError> {
    let cols = [
        "Date",
        "Code région",
        "Type d'emploi",
        "Qualification",
        "Type d'offre d'emploi",
        "Nombre d'offres d'emploi",
    ];
    let (i, rows) = read(csv, &cols)?;
    let mut out = Vec::new();
    for r in &rows {
        if &r[i[1]] == "Total France métropolitaine"
            && &r[i[2]] == "Total"
            && &r[i[3]] == "Total"
            && r[i[4]].starts_with("Offres d'emploi collect")
            && &r[i[0]] >= "2019-01"
        {
            out.push((dares_date(&r[i[0]])?, float(&r[i[5]], "nombre d'offres", &r[i[0]])?));
        }
    }
    if out.is_empty() {
        return Err(ParseError::Format("aucune ligne retenue pour les offres (filtres ou format modifiés ?)".into()));
    }
    sort_points(&mut out);
    Ok(out)
}

/// Demandeurs d'emploi catégorie A, France métropolitaine, tous autres critères `Total`.
pub fn parse_dei(csv: &str) -> Result<Vec<(String, f64)>, ParseError> {
    let cols = [
        "Date",
        "Champ",
        "Catégorie",
        "Sexe",
        "Tranche d'âge",
        "Tranche d'heures travaillées",
        "Ancienneté",
        "Nombre de demandeurs d'emploi",
    ];
    let (i, rows) = read(csv, &cols)?;
    let mut out = Vec::new();
    for r in &rows {
        if &r[i[1]] == "France métropolitaine"
            && &r[i[2]] == "A"
            && (3..=6).all(|k| &r[i[k]] == "Total")
            && &r[i[0]] >= "2019-01"
        {
            out.push((dares_date(&r[i[0]])?, float(&r[i[7]], "nombre de demandeurs", &r[i[0]])?));
        }
    }
    if out.is_empty() {
        return Err(ParseError::Format("aucune ligne retenue pour les demandeurs d'emploi (filtres ou format modifiés ?)".into()));
    }
    sort_points(&mut out);
    Ok(out)
}

type Sector = (String, String, Vec<(String, f64)>);

/// `{code NAF: {"label": …, "points": [[date, taux]]}}` (A10, plus de 10 salariés, ≥ 2019-T1) ;
/// les secteurs gardent l'ordre de première apparition, un taux vide ignore la ligne.
pub fn parse_vacants(csv: &str) -> Result<Value, ParseError> {
    let cols = [
        "Date",
        "Champ",
        "Nomenclature",
        "Code NAF",
        "Libellé NAF",
        "Taux d'emplois vacants (en %)",
    ];
    let (i, rows) = read(csv, &cols)?;
    let mut sectors: Vec<Sector> = Vec::new();
    for r in &rows {
        if &r[i[2]] != "A10" || &r[i[1]] != "Plus de 10 salariés" || &r[i[0]] < "2019-T1" {
            continue;
        }
        let rate = r[i[5]].trim();
        if rate.is_empty() {
            continue; // taux non publié (secret statistique)
        }
        let point = (dares_date(&r[i[0]])?, float(rate, "taux", &r[i[0]])?);
        let code = &r[i[3]];
        match sectors.iter_mut().find(|s| s.0 == *code) {
            Some(s) => s.2.push(point),
            None => sectors.push((code.to_string(), r[i[4]].to_string(), vec![point])),
        }
    }
    if sectors.is_empty() {
        return Err(ParseError::Format("aucun secteur retenu (filtres ou format modifiés ?)".into()));
    }
    let mut out = Map::new();
    for (code, label, mut pts) in sectors {
        sort_points(&mut pts);
        let points: Vec<Value> = pts.into_iter().map(|(d, v)| json!([d, v])).collect();
        out.insert(code, json!({"label": label, "points": points}));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFRES: &str = include_str!("../../tests/fixtures/dares/offres.csv");
    const DEI: &str = include_str!("../../tests/fixtures/dares/dei.csv");
    const VAC: &str = include_str!("../../tests/fixtures/dares/vacants.csv");

    fn pts(v: Vec<(String, f64)>) -> Value {
        Value::Array(v.into_iter().map(|(d, x)| json!([d, x])).collect())
    }
    fn gold(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn dares_strips_double_bom() {
        assert!(VAC.starts_with("\u{feff}\u{feff}"), "la fixture doit garder le BOM doublé");
        assert!(VAC.contains("\r\n"), "la fixture doit garder les fins de ligne CRLF");
        assert!(parse_vacants(VAC).is_ok());
        let triple = format!("\u{feff}{VAC}");
        assert_eq!(parse_vacants(&triple).unwrap(), parse_vacants(VAC).unwrap());
        let bare = VAC.trim_start_matches('\u{feff}');
        assert_eq!(parse_vacants(bare).unwrap(), parse_vacants(VAC).unwrap());
        // CRLF ou LF : même résultat
        assert_eq!(parse_vacants(&VAC.replace("\r\n", "\n")).unwrap(), parse_vacants(VAC).unwrap());
    }

    #[test]
    fn dares_offres_matches_golden() {
        let got = parse_offres(OFFRES).unwrap();
        assert_eq!(got.len(), 9); // 6 + 3 retenues ; les distractors sont écartés
        assert_eq!(pts(got), gold(include_str!("../../tests/golden/dares_offres.json")));
    }

    #[test]
    fn dares_dei_matches_golden() {
        let got = parse_dei(DEI).unwrap();
        assert_eq!(got.len(), 9);
        assert_eq!(pts(got), gold(include_str!("../../tests/golden/dares_dei.json")));
    }

    #[test]
    fn vacants_match_golden() {
        let golden_text = include_str!("../../tests/golden/dares_vacants.json");
        let got = parse_vacants(VAC).unwrap();
        assert_eq!(got, gold(golden_text));
        // le golden Python suit l'ordre de première apparition : on compare aussi l'ordre des clés
        let gold_order: Vec<String> = gold(golden_text).as_object().unwrap().keys().cloned().collect();
        let got_order: Vec<String> = got.as_object().unwrap().keys().cloned().collect();
        assert_eq!(got_order, gold_order);
    }

    #[test]
    fn vacants_skip_empty_rate() {
        let csv = "Date;Champ;Nomenclature;Code NAF;Libellé NAF;Type de données;Nombre d'emplois vacants;Taux d'emplois vacants (en %)\n\
2019-T2;Plus de 10 salariés;A10;Z;Zèbre;cvs-cjo;5;1.5\n\
2019-T1;Plus de 10 salariés;A10;Z;Zèbre;cvs-cjo;;\n\
2019-T1;Plus de 10 salariés;A10;B;Beta;cvs-cjo;7;2.5\n\
2018-T4;Plus de 10 salariés;A10;B;Beta;cvs-cjo;7;9.9\n\
2019-T1;Ensemble;A10;B;Beta;cvs-cjo;7;8.8\n\
2019-T1;Plus de 10 salariés;A4;B;Beta;cvs-cjo;7;7.7\n\
2019-T3;Plus de 10 salariés;A10;B;Beta;cvs-cjo;7;2.0\n";
        let v = parse_vacants(csv).unwrap();
        assert_eq!(v["Z"]["points"], json!([["2019-05-15", 1.5]])); // pas de point à 0
        assert_eq!(v["B"]["points"], json!([["2019-02-15", 2.5], ["2019-08-15", 2.0]]));
        assert_eq!(v["Z"]["label"], "Zèbre");
        // ordre explicite : Z avant B (première apparition), clés internes label puis points
        let order: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(order, ["Z", "B"]);
        let inner: Vec<&str> = v["Z"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(inner, ["label", "points"]);
    }

    #[test]
    fn dares_rejects_wrong_separator() {
        assert!(matches!(parse_offres(&OFFRES.replace(';', ",")), Err(ParseError::Format(_))));
        assert!(matches!(parse_dei(&DEI.replace(';', ",")), Err(ParseError::Format(_))));
        assert!(matches!(parse_vacants(&VAC.replace(';', ",")), Err(ParseError::Format(_))));
    }

    #[test]
    fn dares_rejects_html_and_empty() {
        for f in [parse_offres, parse_dei] {
            assert!(matches!(f("<html>502</html>"), Err(ParseError::Format(_))));
            assert!(matches!(f(""), Err(ParseError::Format(_))));
        }
        assert!(matches!(parse_vacants("<html>502</html>"), Err(ParseError::Format(_))));
        assert!(matches!(parse_vacants(""), Err(ParseError::Format(_))));
    }

    #[test]
    fn dares_rejects_bad_number_or_date() {
        let head = "Date;Champ;Type de données;Catégorie;Sexe;Tranche d'âge;Tranche d'heures travaillées;Ancienneté;Nombre de demandeurs d'emploi\n";
        let row = |d: &str, n: &str| {
            format!("{head}{d};France métropolitaine;cvs-cjo;A;Total;Total;Total;Total;{n}\n")
        };
        assert!(matches!(parse_dei(&row("2019-01", "abc")), Err(ParseError::Format(_))));
        assert!(matches!(parse_dei(&row("2019-01", "")), Err(ParseError::Format(_))));
        assert!(matches!(parse_dei(&row("2019-13", "5")), Err(ParseError::Format(_))));
        assert!(parse_dei(&row("2019-01", "5")).is_ok());
    }

    #[test]
    fn dares_rejects_empty_retained_set() {
        let header_of = |s: &str| format!("{}\n", s.lines().next().unwrap());
        // en-tête seul
        assert!(matches!(parse_offres(&header_of(OFFRES)), Err(ParseError::Format(_))));
        assert!(matches!(parse_dei(&header_of(DEI)), Err(ParseError::Format(_))));
        assert!(matches!(parse_vacants(&header_of(VAC)), Err(ParseError::Format(_))));
        // lignes présentes mais toutes écartées (libellé amont renommé)
        let renamed = OFFRES.replace("Total France métropolitaine", "France métropolitaine (total)");
        assert!(matches!(parse_offres(&renamed), Err(ParseError::Format(_))));
        let renamed = DEI.replace("France métropolitaine", "Métropole");
        assert!(matches!(parse_dei(&renamed), Err(ParseError::Format(_))));
        let renamed = VAC.replace("Plus de 10 salariés", "11 salariés et plus");
        assert!(matches!(parse_vacants(&renamed), Err(ParseError::Format(_))));
    }

    #[test]
    fn dares_rejects_truncated_row() {
        let row = OFFRES.lines().nth(1).unwrap();
        let cut = row.rsplit_once(';').unwrap().0;
        let csv = format!("{}\n{cut}\n", OFFRES.lines().next().unwrap());
        assert!(matches!(parse_offres(&csv), Err(ParseError::Format(_))));
    }

    #[test]
    fn dares_quarter_label_to_mid_quarter_date() {
        assert_eq!(dares_date("2026-T2").unwrap(), "2026-05-15");
        assert_eq!(dares_date("2019-T1").unwrap(), "2019-02-15");
        assert_eq!(dares_date("2019-T4").unwrap(), "2019-11-15");
        assert_eq!(dares_date("2026-09").unwrap(), "2026-09-15");
        assert!(dares_date("2019-T5").is_err());
    }
}
