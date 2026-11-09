//! Collecteur `core` : Indeed + Eurostat + DARES → `data.js`.

use super::{dares, eurostat, indeed, CollectError, ParseError};
use crate::store::Store;
use serde_json::{json, Value};

const USER_AGENT: &str = "suivi-offre-emploi/1.0";

/// URLs de base des trois sources (terminées par `/`).
#[derive(Debug, Clone)]
pub struct Sources {
    pub indeed: String,
    pub eurostat: String,
    pub dares: String,
}

impl Sources {
    pub fn production() -> Self {
        Sources {
            indeed: "https://raw.githubusercontent.com/hiring-lab/job_postings_tracker/master/FR/".into(),
            eurostat: "https://ec.europa.eu/eurostat/api/dissemination/statistics/1.0/data/".into(),
            dares: "https://data.dares.travail-emploi.gouv.fr/api/explore/v2.1/catalog/datasets/".into(),
        }
    }
}

/// Récupère un corps texte ; le message d'erreur ne contient que l'hôte et le chemin.
async fn get(http: &reqwest::Client, url: &str) -> Result<String, CollectError> {
    let place = match reqwest::Url::parse(url) {
        Ok(u) => format!("{}{}", u.host_str().unwrap_or("?"), u.path()),
        Err(_) => "URL invalide".to_string(),
    };
    let resp = http
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(|e| CollectError::Fetch(format!("{place} : {}", e.without_url())))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(CollectError::Fetch(format!("HTTP {} sur {place}", status.as_u16())));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| CollectError::Fetch(format!("{place} : {}", e.without_url())))?;
    if body.trim().is_empty() {
        return Err(CollectError::Fetch(format!("réponse vide de {place}")));
    }
    Ok(body)
}

fn points(pts: Vec<(String, f64)>) -> Value {
    Value::Array(pts.into_iter().map(|(d, v)| json!([d, v])).collect())
}

fn series(label: &str, freq: &str, pts: Vec<(String, f64)>) -> Value {
    json!({"label": label, "freq": freq, "points": points(pts)})
}

/// Même requête que `eurostat()` de `fetch_data.py` (ordre des paramètres conservé).
async fn eurostat_points(
    http: &reqwest::Client,
    base: &str,
    dataset: &str,
    params: &[(&str, &str)],
) -> Result<Vec<(String, f64)>, CollectError> {
    let q: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let body = get(http, &format!("{base}{dataset}?{}&sinceTimePeriod=2019-01", q.join("&"))).await?;
    Ok(eurostat::parse_jsonstat(&body)?)
}

async fn dares_csv(http: &reqwest::Client, base: &str, dataset: &str) -> Result<String, CollectError> {
    get(http, &format!("{base}{dataset}/exports/csv?use_labels=true")).await
}

/// Les séries « total » et « nouvelles » doivent couvrir les mêmes dates (sinon fichier tronqué).
fn check_aggregate(agg: &Value) -> Result<(), ParseError> {
    let len = |k: &str| agg.pointer(&format!("/{k}/dates")).and_then(Value::as_array).map(Vec::len);
    if len("total postings") != len("new postings") {
        return Err(ParseError::Mismatch(
            "séries « total postings » et « new postings » de longueurs différentes".into(),
        ));
    }
    Ok(())
}

/// Récupère les trois sources, assemble toute la charge utile en mémoire, puis écrit
/// `data.js` une seule fois. Au moindre échec, le fichier précédent reste intact.
pub async fn run(
    http: &reqwest::Client,
    src: &Sources,
    store: &Store,
    today: chrono::NaiveDate,
) -> Result<(), CollectError> {
    let aggregate = indeed::parse_aggregate(&get(http, &format!("{}aggregate_job_postings_FR.csv", src.indeed)).await?)?;
    check_aggregate(&aggregate)?;
    let sectors = indeed::parse_sectors(&get(http, &format!("{}job_postings_by_sector_FR.csv", src.indeed)).await?)?;

    let es = src.eurostat.as_str();
    let gdp = |item: &'static str| [("geo", "FR"), ("unit", "CLV20_MEUR"), ("s_adj", "SCA"), ("na_item", item)];
    let chomage = eurostat_points(
        http,
        es,
        "une_rt_m",
        &[("geo", "FR"), ("s_adj", "SA"), ("age", "TOTAL"), ("sex", "T"), ("unit", "PC_ACT")],
    )
    .await?;
    let pib = eurostat_points(http, es, "namq_10_gdp", &gdp("B1GQ")).await?;
    let exports = eurostat_points(http, es, "namq_10_gdp", &gdp("P6")).await?;
    let imports = eurostat_points(http, es, "namq_10_gdp", &gdp("P7")).await?;

    let d = src.dares.as_str();
    let offres = dares::parse_offres(&dares_csv(http, d, "dares_offres_collectees_satisfaites_france_travail_cvs-cjo_mens").await?)?;
    let dei = dares::parse_dei(&dares_csv(http, d, "dares_defm_stock_france_cvs").await?)?;
    let vacants = dares::parse_vacants(&dares_csv(http, d, "dares_emploivacants_cvs_emploisvacants").await?)?;

    let data = json!({
        "aggregate": aggregate,
        "sectors": sectors,
        "macro": {
            "chomage": series("Taux de chômage", "mensuel", chomage),
            "pib": series("PIB (volume)", "trimestriel", pib),
            "exports": series("Exportations (volume)", "trimestriel", exports),
            "imports": series("Importations (volume)", "trimestriel", imports),
            "ft_offres": series("Offres France Travail", "mensuel", offres),
            "ft_dei": series("Demandeurs d'emploi (cat. A)", "mensuel", dei),
        },
        "vacants": vacants,
        "updated": today.format("%Y-%m-%d").to_string(),
    });
    store
        .save("data.js", &data)
        .map_err(|e| CollectError::Write(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");
    const SENTINEL: &[u8] = b"window.DATA={\"sentinel\":\"ancien\"};";
    const DARES_OFFRES: &str = "dares_offres_collectees_satisfaites_france_travail_cvs-cjo_mens";
    const DARES_DEI: &str = "dares_defm_stock_france_cvs";
    const DARES_VAC: &str = "dares_emploivacants_cvs_emploisvacants";

    /// (nom, chemin, paramètres de requête exigés, fixture)
    type Endpoint = (&'static str, String, Vec<(&'static str, &'static str)>, &'static str);

    fn endpoints() -> Vec<Endpoint> {
        let es = |item: Option<&'static str>, ds: &str, fix: &'static str, name: &'static str| -> Endpoint {
            let mut q = match item {
                None => vec![("geo", "FR"), ("s_adj", "SA"), ("age", "TOTAL"), ("sex", "T"), ("unit", "PC_ACT")],
                Some(i) => vec![("geo", "FR"), ("unit", "CLV20_MEUR"), ("s_adj", "SCA"), ("na_item", i)],
            };
            q.push(("sinceTimePeriod", "2019-01"));
            (name, format!("/eurostat/{ds}"), q, fix)
        };
        let da = |ds: &str, fix: &'static str, name: &'static str| -> Endpoint {
            (name, format!("/dares/{ds}/exports/csv"), vec![("use_labels", "true")], fix)
        };
        vec![
            ("indeed_aggregate", "/indeed/aggregate_job_postings_FR.csv".into(), vec![], "indeed/aggregate.csv"),
            ("indeed_sectors", "/indeed/job_postings_by_sector_FR.csv".into(), vec![], "indeed/sectors.csv"),
            es(None, "une_rt_m", "eurostat/chomage.json", "eurostat_chomage"),
            es(Some("B1GQ"), "namq_10_gdp", "eurostat/pib.json", "eurostat_pib"),
            es(Some("P6"), "namq_10_gdp", "eurostat/exports.json", "eurostat_exports"),
            es(Some("P7"), "namq_10_gdp", "eurostat/imports.json", "eurostat_imports"),
            da(DARES_OFFRES, "dares/offres.csv", "dares_offres"),
            da(DARES_DEI, "dares/dei.csv", "dares_dei"),
            da(DARES_VAC, "dares/vacants.csv", "dares_vacants"),
        ]
    }

    fn mock_for(e: &Endpoint, resp: ResponseTemplate) -> Mock {
        let mut m = Mock::given(method("GET")).and(path(e.1.clone())).and(header("user-agent", USER_AGENT));
        for (k, v) in &e.2 {
            m = m.and(query_param(*k, *v));
        }
        m.respond_with(resp)
    }

    fn fixture(rel: &str) -> Vec<u8> {
        std::fs::read(format!("{FIX}{rel}")).unwrap()
    }

    /// Serveur simulé : tout répond avec les fixtures, sauf `bad` (préfixe de nom) qui reçoit `bad_resp`.
    async fn server(bad: Option<(&str, ResponseTemplate)>) -> MockServer {
        let srv = MockServer::start().await;
        let mut bad = bad;
        for e in endpoints() {
            let resp = match &bad {
                Some((prefix, _)) if e.0.starts_with(prefix) => bad.take().unwrap().1,
                _ => ResponseTemplate::new(200).set_body_bytes(fixture(e.3)),
            };
            mock_for(&e, resp).mount(&srv).await;
        }
        assert!(bad.is_none(), "nom de source inconnu");
        srv
    }

    fn sources(srv: &MockServer) -> Sources {
        Sources {
            indeed: format!("{}/indeed/", srv.uri()),
            eurostat: format!("{}/eurostat/", srv.uri()),
            dares: format!("{}/dares/", srv.uri()),
        }
    }

    fn day() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 11, 9).unwrap()
    }

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    fn with_sentinel() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.js"), SENTINEL).unwrap();
        let store = Store::new(dir.path());
        (dir, store)
    }

    fn payload(bytes: &[u8]) -> Value {
        let s = std::str::from_utf8(bytes).unwrap();
        let inner = s.strip_prefix("window.DATA=").unwrap().strip_suffix(';').unwrap();
        serde_json::from_str(inner).unwrap()
    }

    fn keys(v: &Value) -> Vec<&str> {
        v.as_object().unwrap().keys().map(String::as_str).collect()
    }

    #[tokio::test]
    async fn run_writes_data_js_matching_python_golden() {
        let srv = MockServer::start().await;
        for e in endpoints() {
            mock_for(&e, ResponseTemplate::new(200).set_body_bytes(fixture(e.3))).expect(1).mount(&srv).await;
        }
        let (dir, store) = with_sentinel();
        run(&client(), &sources(&srv), &store, day()).await.unwrap();
        let written = std::fs::read(dir.path().join("data.js")).unwrap();
        let got = payload(&written);
        let golden: Value = serde_json::from_slice(&std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/data.json")).unwrap()).unwrap();
        assert_eq!(got, golden);
        assert_eq!(keys(&got), ["aggregate", "sectors", "macro", "vacants", "updated"]);
        assert_eq!(keys(&got["macro"]), ["chomage", "pib", "exports", "imports", "ft_offres", "ft_dei"]);
        assert_eq!(keys(&got["macro"]["pib"]), ["label", "freq", "points"]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "pas de fichier temporaire");
    }

    /// Chaque source (et sous-requête) x chaque défaut : le précédent `data.js` reste identique.
    async fn assert_keeps_previous(kind: &str, resp: impl Fn() -> ResponseTemplate, check: impl Fn(&CollectError)) {
        for name in ["indeed_aggregate", "indeed_sectors", "eurostat_chomage", "eurostat_imports", "dares_offres", "dares_dei", "dares_vacants"] {
            let srv = server(Some((name, resp()))).await;
            let (dir, store) = with_sentinel();
            let err = run(&client(), &sources(&srv), &store, day()).await.expect_err(&format!("{kind}/{name}"));
            check(&err);
            let msg = err.to_string();
            assert!(!msg.contains('?') && !msg.contains("sinceTimePeriod"), "{kind}/{name} : URL dans le message : {msg}");
            assert_eq!(std::fs::read(dir.path().join("data.js")).unwrap(), SENTINEL, "{kind}/{name}");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "{kind}/{name} : fichier temporaire");
        }
    }

    #[tokio::test]
    async fn one_source_failing_keeps_previous_file() {
        assert_keeps_previous("500", || ResponseTemplate::new(500).set_body_string("boom"), |e| {
            assert!(matches!(e, CollectError::Fetch(m) if m.contains("500")), "{e}");
        })
        .await;
    }

    #[tokio::test]
    async fn html_error_page_keeps_previous_file() {
        assert_keeps_previous(
            "html",
            || ResponseTemplate::new(200).set_body_string("<!DOCTYPE html><html><body>Service indisponible</body></html>"),
            |e| assert!(matches!(e, CollectError::Parse(_)), "{e}"),
        )
        .await;
    }

    #[tokio::test]
    async fn empty_body_keeps_previous_file() {
        assert_keeps_previous("vide", || ResponseTemplate::new(200).set_body_string(""), |e| {
            assert!(matches!(e, CollectError::Fetch(m) if m.contains("vide")), "{e}");
        })
        .await;
    }

    #[tokio::test]
    async fn network_error_keeps_previous_file() {
        // port libéré aussitôt : plus personne n'écoute
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let src = Sources { indeed: format!("{base}/indeed/"), eurostat: format!("{base}/eurostat/"), dares: format!("{base}/dares/") };
        let (dir, store) = with_sentinel();
        let err = run(&client(), &src, &store, day()).await.unwrap_err();
        assert!(matches!(err, CollectError::Fetch(_)), "{err}");
        assert_eq!(std::fs::read(dir.path().join("data.js")).unwrap(), SENTINEL);
    }

    #[tokio::test]
    async fn truncated_aggregate_keeps_previous_file() {
        let full = String::from_utf8(fixture("indeed/aggregate.csv")).unwrap();
        // retire la dernière ligne « new postings » : les deux séries n'ont plus la même longueur
        let mut lines: Vec<&str> = full.lines().collect();
        let last = lines.iter().rposition(|l| l.contains("new postings")).unwrap();
        lines.remove(last);
        let srv = server(Some(("indeed_aggregate", ResponseTemplate::new(200).set_body_string(lines.join("\n"))))).await;
        let (dir, store) = with_sentinel();
        let err = run(&client(), &sources(&srv), &store, day()).await.unwrap_err();
        assert!(matches!(err, CollectError::Parse(ParseError::Mismatch(_))), "{err}");
        assert_eq!(std::fs::read(dir.path().join("data.js")).unwrap(), SENTINEL);
    }

    #[tokio::test]
    async fn write_failure_keeps_serving_old_file() {
        let srv = server(None).await;
        let (dir, good_store) = with_sentinel();
        let missing = Store::new(dir.path().join("absent"));
        let err = run(&client(), &sources(&srv), &missing, day()).await.unwrap_err();
        assert!(matches!(err, CollectError::Write(_)), "{err}");
        assert!(!dir.path().join("absent").exists());
        // l'ancien fichier du dossier valide est toujours servi tel quel
        assert_eq!(good_store.read("data.js").unwrap(), SENTINEL);
    }

    #[tokio::test]
    async fn updated_uses_given_date() {
        let srv = server(None).await;
        for (y, m, d, want) in [(2027, 1, 5, "2027-01-05"), (2026, 11, 9, "2026-11-09")] {
            let (dir, store) = with_sentinel();
            let date = chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap();
            run(&client(), &sources(&srv), &store, date).await.unwrap();
            let got = payload(&std::fs::read(dir.path().join("data.js")).unwrap());
            assert_eq!(got["updated"], want);
        }
    }
}
