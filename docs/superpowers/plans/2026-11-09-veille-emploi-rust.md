# Veille emploi France : portage Rust, implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remplacer les trois scripts Python et le serveur de fichiers par un unique binaire Rust (`veille-emploi`) qui sert l'interface JavaScript existante, collecte seul ses données et se déploie derrière nginx en HTTPS.

**Architecture:** Un crate `server/` (axum + tokio). `index.html`, `app.js`, `style.css` et Chart.js restent à la racine du dépôt et sont embarqués dans le binaire ; les 5 fichiers de données (`window.X=…;`) sont lus dans `DATA_DIR`. Trois collecteurs (`core`, `rome`, `tech`) portent les scripts Python et sont lancés par un planificateur interne. L'écriture est atomique, le dernier fichier valide reste servi en cas d'échec.

**Tech Stack:** Rust (edition 2021), tokio, axum, tower-http, reqwest (rustls), serde_json (`preserve_order`), csv, chrono, rust-embed, tracing ; en test : wiremock, tempfile, tower (`util`).

**Spec:** `docs/superpowers/specs/2026-11-09-veille-emploi-rust-design.md`

## Global Constraints

- Le service écoute sur **127.0.0.1 uniquement** ; port par défaut **3210** (variable `VE_BIND`, à vérifier libre au déploiement).
- Secrets : `FT_CLIENT_ID` et `FT_CLIENT_SECRET` **lus dans l'environnement** ; jamais dans le dépôt, les logs, `/health`, les messages d'erreur ni `Debug`.
- Fichiers de données **au même format que le prototype** : `window.DATA`, `window.ROME`, `window.TECH`, `window.AI`, `window.AIROME`, JSON compact (`separators=(",",":")`, UTF-8), terminés par `;`. Noms : `data.js`, `rome.js`, `tech.js`, `ai_exposure.js`, `ai_rome.js`.
- Routes : `GET /`, `GET /app.js`, `GET /style.css`, `GET /vendor/chart.umd.min.js`, `GET /data/<nom>.js` (liste blanche des 5 noms), `GET /health`. **Aucune route d'écriture.**
- API France Travail : limite de **10 appels/seconde** (rester à 8 au plus) ; droits `api_stats-offres-demandes-emploiv1 offresetdemandesemploi` ; jeton d'environ 25 minutes ; base `https://api.francetravail.io/partenaire/stats-offres-demandes-emploi` ; jeton via `https://entreprise.francetravail.fr/connexion/oauth2/access_token?realm=%2Fpartenaire`. On n'utilise **pas** l'API « Offres d'emploi ».
- Rythmes : `core` (Indeed + Eurostat + DARES) une fois par jour ; France Travail (`rome` puis `tech`) une fois par semaine. Un échec ne remplace jamais le dernier fichier valide.
- En-têtes de sécurité : `Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`.
- `cargo test` doit passer en entier avant tout déploiement ; tests écrits avant le code ; aucun test n'accède au réseau.
- Compatibilité : la version de `rustc` du serveur est inconnue : ne pas exiger plus que **Rust 1.80** (le guide de déploiement vérifie et met à jour si besoin).
- Dépôt public sous licence MIT : rien de secret dans les commits. Travail sur la branche `rust-port`.

## Review Focus

Cas que la conception implique sans que le code de base les exerce, du plus probable au moins probable. Chacun a son test dans la tâche indiquée.

1. **L'amont renvoie autre chose que prévu** (page HTML d'erreur, corps vide, CSV tronqué) : la collecte échoue proprement, l'ancien fichier reste intact. → Tâches 5, 7, 9.
2. **Valeurs manquantes** (taux vide DARES, trou Eurostat, valeur masquée France Travail) : ignorées, jamais `0` ni `NaN`. → Tâches 5, 6, 9.
3. **Limitation et expiration du jeton France Travail** (429 en rafale, jeton expiré en cours de collecte) : nouvelle tentative, renouvellement du jeton, secret absent des messages. → Tâche 8.
4. **Écriture impossible** (dossier absent, disque plein, droits) : erreur enregistrée dans `/health`, fichier précédent intact, le service continue de répondre. → Tâches 2, 7.
5. **Redémarrage après une longue panne** : les collectes en retard repartent tout de suite, sans doubler ni attendre le prochain créneau. → Tâche 10.

Et côté HTTP : un nom de fichier hors liste blanche (`../.env`, `%2e%2e/`) répond 404. → Tâche 4.

---

## File Structure

```
server/Cargo.toml, server/src/{main,lib,config,store,status,http}.rs
server/src/collect/{mod,indeed,eurostat,dares,core,ft,rome,tech}.rs
server/src/schedule.rs
server/tests/{http.rs,fixtures/**,golden/**}
tools/{make_fixtures.py,make_golden.py}
index.html (allégé), app.js, style.css           # extraits du prototype
tests/ui/matrix.js                                # parcours de non-régression de l'interface
docs/deploy/DEPLOIEMENT.md                        # guide pas à pas pour l'auteur
```

Chaque fichier a une seule responsabilité ; les analyseurs (`indeed`, `eurostat`, `dares`) sont des fonctions pures sans réseau.

---

### Task 1: Crate, configuration et secrets

**Files:**
- Create: `server/Cargo.toml`, `server/src/lib.rs`, `server/src/main.rs`, `server/src/config.rs`
- Modify: `.gitignore` (ajouter `server/target/`)
- Test: tests dans `server/src/config.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `pub struct FtCreds { pub client_id: String, pub client_secret: String }` (`Debug` manuel qui masque le secret) ; `pub struct Config { pub bind: SocketAddr, pub data_dir: PathBuf, pub ft: Option<FtCreds> }` ; `Config::from_map(&HashMap<String,String>) -> Result<Config, ConfigError>` ; `Config::from_env() -> Result<Config, ConfigError>` ; `pub fn load_dotenv(path: &Path) -> HashMap<String,String>` ; `pub enum ConfigError { InvalidBind(String), MissingFtId, MissingFtSecret }`. Défauts : `VE_BIND=127.0.0.1:3210`, `VE_DATA_DIR=./data`.

- [ ] **Step 1: Créer la branche et le crate**

Run: `git switch -c rust-port && cargo new --lib server --name veille_emploi` puis dépendances : `cargo add tokio axum tower-http reqwest serde serde_json csv chrono rust-embed tracing tracing-subscriber thiserror futures` (reqwest : `default-features=false`, features `rustls-tls json gzip` ; serde_json : feature `preserve_order` ; tokio : `full`) et en dev : `wiremock tempfile tower` (feature `util`).

- [ ] **Step 2: Écrire les tests de `config.rs`**

Tests : `defaults_when_map_empty` (bind `127.0.0.1:3210`, data_dir `./data`, `ft` = None) ; `ft_loaded_when_both_set` ; `only_id_is_an_error` → `MissingFtSecret` ; `only_secret_is_an_error` → `MissingFtId` ; `invalid_bind_is_rejected` (`VE_BIND=pas-une-adresse` → `InvalidBind`) ; `debug_never_shows_secret` (`format!("{:?}", cfg)` ne contient pas la valeur du secret) ; `error_messages_never_contain_secret` ; `dotenv_ignores_comments_and_blank_lines` (fichier temporaire `# c\nA=1\n\nB=2` → `{A:1,B:2}`).

- [ ] **Step 3: Lancer les tests, vérifier l'échec**

Run: `cargo test -p veille_emploi config` — Expected: erreurs de compilation (symboles absents).

- [ ] **Step 4: Implémenter `config.rs`**

`Debug` écrit `client_secret: "***"`. `ConfigError` n'embarque jamais le secret. `main.rs` provisoire : affiche la version.

- [ ] **Step 5: Lancer les tests**

Run: `cargo test -p veille_emploi` — Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add .gitignore server && git commit -m "feat(server): crate Rust et configuration sans fuite de secret"
```

---

### Task 2: Stockage atomique et état des collectes

**Files:**
- Create: `server/src/store.rs`, `server/src/status.rs`
- Modify: `server/src/lib.rs` (déclarer les modules)
- Test: tests unitaires dans les deux fichiers

**Interfaces:**
- Produces:
  - `pub const DATA_FILES: [(&str, &str); 5]` = `[("data.js","DATA"),("rome.js","ROME"),("tech.js","TECH"),("ai_exposure.js","AI"),("ai_rome.js","AIROME")]`
  - `pub fn render_js(var: &str, value: &serde_json::Value) -> String` → `window.VAR=<json compact>;`
  - `pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()>` (fichier temporaire dans le même dossier puis `rename`)
  - `pub struct Store` : `Store::new(dir: impl Into<PathBuf>)`, `save(&self, file: &str, value: &Value) -> io::Result<()>` (déduit la variable de `DATA_FILES`), `read(&self, file: &str) -> io::Result<Vec<u8>>`, `pub fn is_known(file: &str) -> bool`
  - `pub struct CollectorStatus { pub last_attempt: Option<String>, pub last_ok: Option<String>, pub last_error: Option<String> }` (Serialize, Clone, Default) ; `pub struct StatusBoard` (clonable, partagé) : `new()`, `record_ok(&self, name: &str, now: DateTime<Utc>)`, `record_err(&self, name: &str, now: DateTime<Utc>, msg: &str)`, `snapshot(&self) -> BTreeMap<String, CollectorStatus>`

- [ ] **Step 1: Écrire les tests**

`render_js_is_compact_and_terminated` (`{"a":[1,2]}` → `window.DATA={"a":[1,2]};`) ; `render_js_keeps_accents` (pas de `é`) ; `write_atomic_replaces_file` ; `write_atomic_failure_keeps_old_file` (cible dans un dossier inexistant → `Err`, ancien fichier d'un autre test intact) ; `store_rejects_unknown_file_name` (`is_known("../.env")` faux, `is_known("rome.js")` vrai) ; `status_records_ok_then_error_keeps_last_ok` ; `status_snapshot_is_sorted_by_name` ; **cas dossier absent** : `save` vers un `Store` dont le dossier n'existe pas → `Err`, rien créé.

- [ ] **Step 2: Lancer, vérifier l'échec** — `cargo test -p veille_emploi store status` : FAIL (modules absents).

- [ ] **Step 3: Implémenter** `store.rs` et `status.rs` selon les signatures ci-dessus. Horodatages au format RFC 3339.

- [ ] **Step 4: Lancer** — `cargo test -p veille_emploi` : PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat(server): stockage atomique et état des collectes"`.

---

### Task 3: Extraire JavaScript et CSS d'`index.html`

**Files:**
- Create: `app.js` (corps du `<script>` des lignes 153 à 738), `style.css` (contenu du `<style>` des lignes 7 à 66), `tests/ui/matrix.js`
- Modify: `index.html` (remplacer le bloc `<style>` par `<link rel="stylesheet" href="style.css">`, le dernier `<script>` par `<script src="app.js"></script>`, **supprimer les trois attributs `onerror="void 0"`**, l'ordre des scripts reste identique)
- Test: parcours automatisé `tests/ui/matrix.js` (dans le navigateur)

**Interfaces:**
- Produces: l'interface lit toujours `window.DATA/ROME/TECH/AI/AIROME` ; **logique inchangée**. Le pied de page affiche en plus : « Dernières collectes : Indeed/Eurostat/DARES `DATA.updated`, France Travail `ROME.updated` » (absent si `ROME` n'existe pas) et la mention de source « France Travail, API Marché du travail, Licence Ouverte Etalab » dans l'écran « Sources, limites et licences ».

- [ ] **Step 1: Écrire `tests/ui/matrix.js`**

Une fonction asynchrone à coller dans la console du navigateur : pour chaque thème → vue → niveau de détail → indicateur tech → période (boutons existants), clique, attend 60 ms, puis vérifie : aucune erreur `window.onerror`, la liste `#list li` n'est pas vide, pas de débordement horizontal (`scrollWidth <= innerWidth`). Elle retourne `{combos, nbErreurs, nbListesVides, nbDebordements}`.

- [ ] **Step 2: Lancer le parcours sur le prototype actuel** — via `python -m http.server`, ouvrir `index.html`, exécuter `matrix.js`. Expected: `combos: 133, nbErreurs: 0, nbListesVides: 0, nbDebordements: 0` (référence avant extraction).

- [ ] **Step 3: Extraire** les deux blocs sans toucher à leur contenu, retirer les `onerror`, ajouter les dates de collecte dans le pied de page (un `try/catch` si `ROME` absent).

- [ ] **Step 4: Relancer `matrix.js`** — Expected: mêmes résultats qu'à l'étape 2, **et** aucune violation CSP dans la console quand la politique de la tâche 4 est active (vérifié à la tâche 11).

- [ ] **Step 5: Commit** — `git add index.html app.js style.css tests/ui && git commit -m "refactor(ui): extrait le JS et le CSS d'index.html (logique inchangée)"`.

---

### Task 4: Serveur HTTP

**Files:**
- Create: `server/src/http.rs`, `server/tests/http.rs`
- Modify: `server/src/lib.rs`
- Test: `server/tests/http.rs` (requêtes via `tower::ServiceExt::oneshot`, dossier de données temporaire)

**Interfaces:**
- Consumes: `Store`, `StatusBoard` (tâche 2).
- Produces: `pub struct AppState { pub store: Store, pub status: StatusBoard, pub version: &'static str }` (Clone) ; `pub fn router(state: AppState) -> axum::Router`. Ressources statiques embarquées avec `rust-embed` (`#[folder = ".."]`, `#[include]` sur `index.html`, `app.js`, `style.css`, `vendor/chart.umd.min.js`). `/health` renvoie `{"version":…,"collectors":{nom:{last_ok,last_error,last_attempt}},"files":{nom:{"exists":bool,"age_seconds":n|null}}}`.

- [ ] **Step 1: Écrire les tests**

`index_served_with_security_headers` (200, `text/html`, les 4 en-têtes exacts des contraintes globales) ; `static_assets_have_correct_types` (`app.js` → `javascript`, `style.css` → `text/css`) ; `data_file_404_when_missing` ; `data_file_served_after_save` (200, `application/javascript`, corps = `window.DATA=…;`) ; `unknown_data_name_is_404` pour `/data/..%2f.env`, `/data/%2e%2e/.env`, `/data/secret.js`, `/data/data.js.bak` ; `health_has_no_secret_and_lists_files` (le corps ne contient pas `FT_CLIENT`) ; `post_is_405` sur `/` et `/data/data.js` ; `unknown_path_is_404` ; `gzip_when_requested` (`Accept-Encoding: gzip` sur `/data/data.js` → `content-encoding: gzip`).

- [ ] **Step 2: Lancer, vérifier l'échec** — `cargo test -p veille_emploi --test http` : FAIL.

- [ ] **Step 3: Implémenter** `router` avec `tower-http` (`CompressionLayer`, couche d'en-têtes de sécurité). Les fichiers de données sont relus à chaque requête, `Cache-Control: public, max-age=300`. Toute route hors liste → 404.

- [ ] **Step 4: Lancer** — `cargo test -p veille_emploi` : PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat(server): routes HTTP, ressources embarquées, en-têtes de sécurité"`.

---

### Task 5: Fixtures, jeux de référence Python et analyseurs Indeed

**Files:**
- Create: `tools/make_fixtures.py`, `tools/make_golden.py`, `server/src/collect/mod.rs`, `server/src/collect/indeed.rs`, `server/tests/fixtures/indeed/{aggregate.csv,sectors.csv}`, `server/tests/golden/indeed_{aggregate,sectors}.json`
- Test: dans `indeed.rs`

**Interfaces:**
- Produces: `pub enum ParseError { Format(String), Mismatch(String) }` (thiserror, dans `collect/mod.rs`) ; `indeed::parse_aggregate(csv: &str) -> Result<serde_json::Value, ParseError>` → `{"total postings":{"dates":[..],"values":[..]},"new postings":{…}}` (colonne `indeed_job_postings_index_SA`) ; `indeed::parse_sectors(csv: &str) -> Result<serde_json::Value, ParseError>` → `{"dates":[..],"series":{catégorie:[..]}}` (ligne `total postings` seulement, colonne `indeed_job_postings_index`, **dates identiques pour toutes les catégories** sinon `Mismatch`, ordre d'apparition conservé).

- [ ] **Step 1: Outils de référence Python**

`tools/make_fixtures.py` télécharge les vraies réponses une fois et en garde un extrait réduit (en-tête + ~40 lignes datées, 3 catégories). `tools/make_golden.py` charge `fetch_data.py`, **remplace `fetch_data.get`** pour lire les fixtures, appelle `indeed_aggregate()` / `indeed_sectors()` et écrit les sorties dans `tests/golden/`. Lancer les deux ; les fichiers produits sont commités.

- [ ] **Step 2: Écrire les tests**

`aggregate_matches_python_golden` (JSON parsé égal au golden) ; `sectors_matches_python_golden` ; `sectors_keep_first_seen_order` ; `sectors_reject_misaligned_dates` (une catégorie avec une date de moins → `Mismatch`) ; `parse_rejects_html_error_page` (`"<html>502</html>"` → `Format`) ; `parse_rejects_empty_body` ; `parse_rejects_truncated_row` (ligne avec trop peu de colonnes → `Format`, pas de panique) ; `non_numeric_index_is_an_error_not_zero`.

- [ ] **Step 3: Lancer, vérifier l'échec** — FAIL.

- [ ] **Step 4: Implémenter** les deux analyseurs (crate `csv`).

- [ ] **Step 5: Lancer** — PASS. **Step 6: Commit** — `git add tools server && git commit -m "feat(collect): analyseurs Indeed avec jeux de référence Python"`.

---

### Task 6: Analyseurs Eurostat et DARES

**Files:**
- Create: `server/src/collect/eurostat.rs`, `server/src/collect/dares.rs`, fixtures et golden correspondants (`eurostat_*.json`, `dares_*.csv`) via les outils de la tâche 5
- Test: dans les deux fichiers

**Interfaces:**
- Produces: `eurostat::period_to_date(p: &str) -> String` (`2026-Q2` → `2026-05-15` ; `2026-09` → `2026-09-15`) ; `eurostat::parse_jsonstat(json: &str) -> Result<Vec<(String, f64)>, ParseError>` (valeurs absentes ignorées, tri par position) ; `dares::parse_offres(csv: &str) -> Result<Vec<(String,f64)>, ParseError>` (France métropolitaine, type d'emploi `Total`, qualification `Total`, offres collectées, dates ≥ `2019-01`) ; `dares::parse_dei(csv: &str) -> Result<Vec<(String,f64)>, ParseError>` (champ France métropolitaine, catégorie `A`, tous les autres critères `Total`) ; `dares::parse_vacants(csv: &str) -> Result<serde_json::Value, ParseError>` → `{code NAF:{"label":…,"points":[[date,taux]]}}` (nomenclature `A10`, champ « Plus de 10 salariés », ≥ `2019-T1`, **taux vide ignoré**). Les CSV DARES commencent par un BOM (parfois doublé) et utilisent `;`. Dates DARES : `AAAA-MM` → `AAAA-MM-15` ; `AAAA-Tn` → milieu de trimestre.

- [ ] **Step 1: Écrire les tests**

`period_to_date_quarters_and_months` (2020-Q1 → `2020-02-15`, 2020-Q4 → `2020-11-15`, `2021-03` → `2021-03-15`) ; `jsonstat_skips_missing_values` ; `jsonstat_matches_python_golden` ; `jsonstat_rejects_html` ; `dares_strips_double_bom` ; `dares_offres_matches_golden` ; `dares_dei_matches_golden` ; `vacants_skip_empty_rate` (taux vide → point absent, pas `0`) ; `vacants_match_golden` ; `dares_rejects_wrong_separator` ; `dares_quarter_label_to_mid_quarter_date`.

- [ ] **Step 2: Lancer, vérifier l'échec** — FAIL.
- [ ] **Step 3: Implémenter** les analyseurs.
- [ ] **Step 4: Lancer** — PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(collect): analyseurs Eurostat et DARES"`.

---

### Task 7: Collecteur `core` (Indeed + Eurostat + DARES → `data.js`)

**Files:**
- Create: `server/src/collect/core.rs`
- Test: dans `core.rs`, serveur simulé avec `wiremock`

**Interfaces:**
- Consumes: analyseurs des tâches 5 et 6, `Store`, `StatusBoard`.
- Produces: `pub struct Sources { pub indeed: String, pub eurostat: String, pub dares: String }` avec `Sources::production()` (URLs réelles du prototype) ; `pub async fn run(http: &reqwest::Client, src: &Sources, store: &Store, today: chrono::NaiveDate) -> Result<(), CollectError>` ; `pub enum CollectError { Fetch(String), Parse(ParseError), Write(String) }` (Display sans secret). `data.js` = `window.DATA={aggregate, sectors, macro:{chomage,pib,exports,imports,ft_offres,ft_dei}, vacants, updated:"AAAA-MM-JJ"}` ; chaque série macro porte `label`, `freq` (`mensuel`/`trimestriel`) et `points` ; paramètres Eurostat identiques à `fetch_data.py` (`une_rt_m`, `namq_10_gdp` avec `B1GQ`, `P6`, `P7`, `sinceTimePeriod=2019-01`). Libellés : « Taux de chômage », « PIB (volume) », « Exportations (volume) », « Importations (volume) », « Offres France Travail », « Demandeurs d'emploi (cat. A) ».

- [ ] **Step 1: Écrire les tests**

`run_writes_data_js_matching_python_golden` (serveur simulé servant les fixtures ; `data.js` parsé égal au golden `data.json` produit par `make_golden.py`) ; `one_source_failing_keeps_previous_file` (Eurostat répond 500 → `Err(Fetch)`, `data.js` précédent octet pour octet identique) ; `html_error_page_keeps_previous_file` ; `empty_body_keeps_previous_file` ; `write_failure_keeps_serving_old_file` (dossier de données en lecture seule ou supprimé → `Err(Write)`) ; `updated_uses_given_date`.

- [ ] **Step 2: Lancer, vérifier l'échec** — FAIL.
- [ ] **Step 3: Implémenter** `run` : trois récupérations, assemblage complet **avant** toute écriture, une seule écriture atomique à la fin.
- [ ] **Step 4: Lancer** — PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(collect): collecteur core avec échec sans perte de données"`.

---

### Task 8: Client France Travail (jeton, reprise, limitation)

**Files:**
- Create: `server/src/collect/ft.rs`
- Test: dans `ft.rs`, `wiremock`, horloge `tokio::time::pause`

**Interfaces:**
- Consumes: `FtCreds` (tâche 1).
- Produces: `pub struct FtEndpoints { pub token_url: String, pub base_url: String }` (+ `FtEndpoints::production()`) ; `pub struct RateLimiter` : `new(per_second: u32)`, `async fn acquire(&self)` ; `pub enum FtError { Auth, Http(u16), Network, Parse }` (Display **sans secret ni jeton**) ; `pub struct FtClient` : `new(http: reqwest::Client, creds: FtCreds, ep: FtEndpoints, limiter: RateLimiter) -> Self`, `pub async fn call(&self, path: &str, body: Option<&serde_json::Value>) -> Result<Option<serde_json::Value>, FtError>` (`Ok(None)` sur 204/404). Droits demandés : `api_stats-offres-demandes-emploiv1 offresetdemandesemploi`. Reprise sur 429, 5xx et erreur réseau : 5 essais, attente `1,5 × n` s (réglable à 0 en test).

- [ ] **Step 1: Écrire les tests**

`token_requested_once_and_cached` ; `token_refreshed_when_close_to_expiry` (`expires_in` court, avancer l'horloge) ; `token_expired_mid_run_is_renewed_after_401` ; `retries_on_429_then_succeeds` ; `gives_up_after_five_attempts` ; `no_content_and_not_found_give_none` ; `rate_limiter_allows_at_most_n_per_second` (horloge en pause) ; **`secret_never_appears_in_errors_or_logs`** (réponses 401 simulées ; `format!("{err}")`, `format!("{err:?}")` et les logs capturés ne contiennent ni secret, ni jeton, ni identifiant) ; `invalid_json_is_parse_error`.

- [ ] **Step 2: Lancer, vérifier l'échec** — FAIL.
- [ ] **Step 3: Implémenter** (jeton gardé derrière un `Mutex`, renouvelé 60 s avant l'expiration comme dans `fetch_rome.py`).
- [ ] **Step 4: Lancer** — PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(collect): client France Travail avec reprise et limitation"`.

---

### Task 9: Collecteurs `rome` et `tech`

**Files:**
- Create: `server/src/collect/rome.rs`, `server/src/collect/tech.rs`, fixtures et golden `rome_*.json`, `tech_*.json` (`make_golden.py` remplace `fetch_rome.call` par la lecture des fixtures)
- Test: dans les deux fichiers

**Interfaces:**
- Consumes: `FtClient::call` (tâche 8), `Store`.
- Produces: `rome::parse_offers(resp: &Value) -> BTreeMap<String, f64>` (garde `codeNomenclature == "TOFF"`, `codePeriode >= "2021T1"`, valeur non nulle) ; `rome::assemble(acts: &[(String,String)], results: &BTreeMap<String, BTreeMap<String,f64>>, today: NaiveDate) -> Value` → `{updated, periods:[…triées], items:{code:{l,o:[valeur|null…]}}}` (codes sans donnée omis) ; `pub async fn rome::run(client: &FtClient, store: &Store, today: NaiveDate, workers: usize) -> Result<(), CollectError>` (liste via `/v1/referentiel/activites/ROME`, puis POST `/v1/indicateur/stat-offres` par code, 4 appels simultanés par défaut). `tech::TECH_CODES: [&str; 18]` = `["M1805","M1855","M1861","M1853","M1827","M1811","M1419","M1405","M1856","M1884","M1802","M1801","M1810","M1806","M1867","M1803","I1404","I1401"]` ; `tech::run(client, store, today) -> Result<(), CollectError>` → `{updated, periods, items:{code:{l,d:[…],e:[…],t:{AAAA:niveau}}}}` (demandeurs : nomenclature `A` ; embauches : `TOUT-TOUTE` ; tension : `PERSPECTIVE`).

- [ ] **Step 1: Écrire les tests**

`parse_offers_keeps_only_toff_from_2021t1` ; `parse_offers_skips_masked_values` (valeur absente ou nulle → absente, pas `0`) ; `assemble_matches_python_golden` ; `assemble_omits_codes_without_data` ; `one_code_failing_does_not_block_others` (un code répond 500 → les autres figurent dans le fichier) ; `all_codes_failing_keeps_previous_file` (si **aucun** résultat, rien n'est écrit) ; `html_response_is_skipped_not_fatal_for_one_code_but_fatal_if_list_fails` ; `tech_matches_python_golden` ; `tech_periods_are_union_of_demandeurs_and_embauches_from_2021t1` ; `tech_tension_is_year_to_level_map` ; `rome_never_exceeds_worker_limit` (jamais plus de `workers` appels simultanés).

- [ ] **Step 2: Lancer, vérifier l'échec** — FAIL.
- [ ] **Step 3: Implémenter** (`futures::stream::buffer_unordered(workers)`).
- [ ] **Step 4: Lancer** — PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(collect): collecteurs ROME et tech"`.

---

### Task 10: Planificateur et ligne de commande

**Files:**
- Create: `server/src/schedule.rs`
- Modify: `server/src/main.rs`, `server/src/lib.rs`
- Test: dans `schedule.rs` (fonctions pures, sans horloge réelle) et `main.rs` (analyse des arguments)

**Interfaces:**
- Consumes: collecteurs et `StatusBoard`.
- Produces: `pub enum Every { DailyAt { hour: u32, min: u32 }, WeeklyAt { weekday: chrono::Weekday, hour: u32, min: u32 } }` ; `pub const STARTUP_DELAY_SECS: i64 = 30` ; `pub const RETRY_AFTER_FAILURE_SECS: i64 = 3600` ; `pub fn next_due(now: DateTime<Utc>, last_ok: Option<DateTime<Utc>>, every: &Every) -> DateTime<Utc>` (sans succès connu : `now + 30 s` ; sinon le prochain créneau après `last_ok`, **ou `now` si ce créneau est déjà passé**) ; `pub struct Ctx { pub http: reqwest::Client, pub store: Store, pub status: StatusBoard, pub cfg: Config }` ; `pub async fn run_forever(ctx: Arc<Ctx>)` (une boucle `core` quotidienne à 02:30 UTC, une boucle France Travail hebdomadaire le lundi à 03:00 UTC qui lance `rome` puis `tech` ; chaque boucle est séquentielle, donc jamais de lancement en double) ; `pub enum Command { Serve, Once(Job) }`, `pub enum Job { Core, Rome, Tech }` ; `pub fn parse_args(args: &[String]) -> Result<Command, String>` (`serve` par défaut ; `once core|rome|tech`). Sans identifiants France Travail, les boucles `rome`/`tech` sont désactivées avec un message dans les logs, `core` tourne quand même.

- [ ] **Step 1: Écrire les tests**

`no_success_means_start_in_30s` ; `daily_next_slot_after_last_ok` ; `daily_same_day_not_yet_reached` ; **`overdue_after_long_downtime_runs_now`** (dernier succès il y a 5 jours → `now`) ; `weekly_next_monday` ; `weekly_crossing_month_and_year_boundary` (31 décembre → lundi 4 janvier) ; `midnight_boundary_is_not_skipped` (`now` = 02:30:00 exactement) ; `failure_retries_in_one_hour` ; `parse_args_defaults_to_serve` ; `parse_args_once_rome` ; `parse_args_rejects_unknown_job`.

- [ ] **Step 2: Lancer, vérifier l'échec** — FAIL.
- [ ] **Step 3: Implémenter** `schedule.rs` et brancher `main.rs` (journalisation `tracing`, chargement de `.env` si présent, arrêt propre sur Ctrl-C).
- [ ] **Step 4: Lancer** — PASS.
- [ ] **Step 5: Commit** — `git commit -am "feat(server): planificateur, commandes serve et once"`.

---

### Task 11: Valeurs par défaut, intégration et non-régression de l'interface

**Files:**
- Create: `server/src/seed.rs`, `server/tests/integration.rs`
- Modify: `server/src/main.rs` (appeler le semis au démarrage)
- Test: `server/tests/integration.rs`, puis `tests/ui/matrix.js` contre le binaire

**Interfaces:**
- Produces: `pub fn seed_defaults(data_dir: &Path) -> std::io::Result<Vec<String>>` : crée `data_dir` s'il manque, copie `ai_exposure.js` et `ai_rome.js` embarqués **uniquement s'ils n'existent pas**, renvoie les noms créés. Ne touche jamais `data.js`, `rome.js`, `tech.js`.

- [ ] **Step 1: Écrire les tests**

`seed_creates_missing_ai_files` ; `seed_never_overwrites_existing_ai_file` (fichier modifié à la main : contenu intact) ; `seed_leaves_collected_files_alone` ; `integration_server_serves_everything` (dossier temporaire, `core` sur serveur simulé, puis requêtes sur `/`, `/data/data.js`, `/health` : `last_ok` renseigné) ; `integration_failure_visible_in_health_and_old_data_served`.

- [ ] **Step 2: Lancer, vérifier l'échec** — FAIL. **Step 3: Implémenter** `seed.rs`. **Step 4: Lancer** — `cargo test` : PASS (tous les tests).

- [ ] **Step 5: Lancer le service en local et la non-régression de l'interface**

Run: `cargo run -p veille_emploi --release -- once core` puis `cargo run -p veille_emploi --release` ; copier les `rome.js`/`tech.js` du prototype dans le dossier de données ; ouvrir `http://127.0.0.1:3210/` ; exécuter `tests/ui/matrix.js`. Expected: `combos: 133, nbErreurs: 0, nbListesVides: 0, nbDebordements: 0`, **aucune violation CSP** dans la console, `/health` indique `last_ok` pour `core`.

- [ ] **Step 6: Parité finale** — comparer `data.js` produit par `once core` à celui de `python fetch_data.py` lancé le même jour : séries identiques (tolérance : valeurs du jour). Consigner le résultat dans le message de commit.

- [ ] **Step 7: Commit** — `git commit -am "feat(server): semis des valeurs par défaut et tests d'intégration"`.

---

### Task 12: Guide de déploiement HTTPS, documentation et publication de la branche

**Files:**
- Create: `docs/deploy/DEPLOIEMENT.md`
- Modify: `README.md` (section « Version Rust »), `.gitignore`
- Test: relecture par commande à blanc (`bash -n` sur chaque bloc, `nginx -t` simulé dans le guide)

**Interfaces:** aucun code ; le guide est destiné à l'auteur, qui l'exécute lui-même sur le serveur.

- [ ] **Step 1: Rédiger `docs/deploy/DEPLOIEMENT.md`**, en français, une commande par bloc, avec pour chaque étape « ce que ça fait » et « résultat attendu ». Sections obligatoires :
  1. **Prérequis et contrôles en lecture seule** : espace disque (`df -h /` ≥ 3 Go libres, sinon s'arrêter), `rustc --version` (≥ 1.80, sinon `rustup update stable`), `which certbot`, `ls /etc/nginx/sites-enabled`, port libre (`ss -tln | grep 3210`).
  2. **Utilisateur et dossiers** : utilisateur système `veille` sans shell, `/opt/veille-emploi/` (binaire), `/var/lib/veille-emploi/data/` (données).
  3. **Compilation sur le serveur** : clone de la branche, `CARGO_BUILD_JOBS=2 nice cargo build --release`, copie du binaire, suppression de `target/` ensuite pour récupérer le disque.
  4. **Fichier d'environnement** `/etc/veille-emploi.env` en droits `0600` : `FT_CLIENT_ID`, `FT_CLIENT_SECRET`, `VE_BIND=127.0.0.1:3210`, `VE_DATA_DIR=/var/lib/veille-emploi/data` (le secret se saisit sur le serveur, jamais dans une commande qui l'écrit dans l'historique ni dans le dépôt).
  5. **Données initiales** : `scp` de `rome.js` et `tech.js` locaux vers le dossier de données, puis `chown veille`.
  6. **Service `systemd`** `veille-emploi.service` : `User=veille`, `EnvironmentFile=`, `Restart=on-failure`, `NoNewPrivileges=true`, `ProtectSystem=strict`, `ProtectHome=true`, `PrivateTmp=true`, `ReadWritePaths=/var/lib/veille-emploi`. Test local : `curl -s http://127.0.0.1:3210/health`.
  7. **nginx** : bloc `server` pour `ecostat21.applicationrush.fr` d'abord sur le port 80 (proxy vers `127.0.0.1:3210`, en-têtes `Host` et `X-Forwarded-For`), puis **`sudo nginx -t` avant tout `reload`** pour ne pas affecter les autres sites.
  8. **HTTPS (Let's Encrypt)** : `sudo certbot --nginx -d ecostat21.applicationrush.fr` (installation de `certbot` et `python3-certbot-nginx` si absents ; le DNS pointe déjà vers le serveur), redirection 80 → 443, `sudo certbot renew --dry-run` pour valider le renouvellement automatique, vérification `curl -I https://ecostat21.applicationrush.fr` et contrôle de la chaîne de certificat ; **HSTS** à n'ajouter qu'après validation, avec une durée courte d'abord.
  9. **Vérifications finales** : `/health`, les 133 combinaisons de l'interface, pied de page avec les dates de collecte.
  10. **Exploitation** : `journalctl -u veille-emploi -f`, mise à jour (reconstruire, remplacer le binaire, `systemctl restart`), **retour arrière** (garder l'ancien binaire sous `veille-emploi.prev`), forcer une collecte (`sudo -u veille … veille-emploi once core`).
  11. **Dépannage** : certificat refusé (DNS, port 80 fermé), 502 de nginx (service arrêté ou mauvais port), données absentes (`/health`).

- [ ] **Step 2: Mettre à jour `README.md`** (section « Version Rust », lien vers le guide, commandes `cargo test`, `cargo run`), vérifier que le `.gitignore` exclut `server/target/` et `.env`.

- [ ] **Step 3: Vérification finale** — `cargo test` PASS ; `git grep -n "FT_CLIENT_SECRET=" -- . ':!*.example' ':!docs'` ne renvoie aucune valeur réelle ; scan des secrets (clé et identifiant lus dans `.env`) sur tous les fichiers suivis : 0 occurrence.

- [ ] **Step 4: Commit et publication de la branche** — `git add -A && git commit -m "docs: guide de déploiement HTTPS et README Rust" && git push -u origin rust-port`. **Ne pas fusionner dans `main`** avant validation par l'auteur.
