//! Planificateur : quand lancer chaque collecte, et boucles qui les exécutent.
//!
//! Tout ce qui touche au temps est injectable : `next_due` est une fonction pure, les boucles
//! reçoivent un `Runner` (qui exécute une collecte) et une `WallClock` (heure murale). Les tests
//! n'utilisent ni réseau ni horloge réelle.
//!
//! Garanties des boucles :
//! - un retard (arrêt prolongé du programme) est rattrapé immédiatement au démarrage ;
//! - une boucle est séquentielle : une collecte ne se chevauche jamais elle-même, et après un
//!   succès la suivante est toujours au créneau STRICTEMENT suivant (jamais deux fois le même) ;
//! - un échec est consigné (`StatusBoard::record_err`) et seul le job en échec est retenté :
//!   `core` après `RETRY_AFTER_FAILURE_SECS` (1 h, collecte peu coûteuse) ; France Travail
//!   (~1 900 appels) avec un délai croissant `FT_RETRY_LADDER_SECS` (1 h, 6 h, 24 h), jamais
//!   au-delà du créneau hebdomadaire suivant, remis à zéro après un succès ;
//! - `carte` (~19 200 appels, une heure) a sa propre boucle (`carte_loop`) : lundi 04:00 UTC, seulement
//!   si `carte.js` est absent ou vieux de plus de 28 jours (la donnée est trimestrielle), et jamais en
//!   même temps que `rome` ou `tech` (`FtGate`) pour ménager le quota de l'API.

use crate::collect::{carte, core as core_collect, rome, tech, CollectError};
use crate::config::Config;
use crate::status::StatusBoard;
use crate::store::Store;
use chrono::{DateTime, Datelike, Duration as Span, NaiveDate, TimeZone, Utc, Weekday};
use futures::FutureExt;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// Délai avant la première collecte quand aucun succès n'est connu.
pub const STARTUP_DELAY_SECS: i64 = 30;
/// Délai avant de retenter une collecte `core` en échec.
pub const RETRY_AFTER_FAILURE_SECS: i64 = 3600;
/// Délais avant de retenter France Travail après 1, 2, 3 échecs consécutifs (le dernier vaut
/// pour les suivants) : l'API n'est interrogée qu'au plus une fois par semaine en régime normal.
pub const FT_RETRY_LADDER_SECS: [i64; 3] = [3600, 6 * 3600, 24 * 3600];
/// Âge à partir duquel `carte.js` est renouvelé (strictement plus de 28 jours) : la donnée de France
/// Travail est trimestrielle et la collecte est lourde.
pub const CARTE_MAX_AGE_DAYS: i64 = 28;
/// Quand `carte` est dû mais que `rome.js` manque encore : nouvelle vérification (peu coûteuse) après ce délai.
pub const CARTE_RECHECK_SECS: i64 = 3600;
/// Une attente longue est découpée en tranches de cette durée, puis l'heure murale est relue :
/// une mise en veille de la machine ne retarde donc pas la collecte de plus de ce délai.
pub const MAX_SLEEP_SECS: u64 = 900;

/// Boucle `core` : tous les jours à 02:30 UTC (Indeed, Eurostat, DARES).
pub const CORE_EVERY: Every = Every::DailyAt { hour: 2, min: 30 };
/// Boucle France Travail : le lundi à 03:00 UTC (`rome` puis `tech`).
pub const FT_EVERY: Every = Every::WeeklyAt {
    weekday: Weekday::Mon,
    hour: 3,
    min: 0,
};

/// Boucle `carte` : le lundi à 04:00 UTC, une heure après le job France Travail (`rome` puis `tech`).
pub const CARTE_EVERY: Every = Every::WeeklyAt {
    weekday: Weekday::Mon,
    hour: 4,
    min: 0,
};

/// Périodicité d'une collecte (heures en UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Every {
    DailyAt {
        hour: u32,
        min: u32,
    },
    WeeklyAt {
        weekday: Weekday,
        hour: u32,
        min: u32,
    },
}

fn slot_on(day: NaiveDate, hour: u32, min: u32) -> DateTime<Utc> {
    let t = day
        .and_hms_opt(hour, min, 0)
        .expect("heure de créneau valide (constantes du programme)");
    Utc.from_utc_datetime(&t)
}

impl Every {
    /// Durée nominale entre deux créneaux.
    pub fn period(&self) -> Span {
        match self {
            Every::DailyAt { .. } => Span::days(1),
            Every::WeeklyAt { .. } => Span::days(7),
        }
    }
}

/// Dates de dernière collecte supposées au démarrage, UNIQUEMENT pour la planification (date de
/// modification des fichiers de données). Elles ne passent jamais par le `StatusBoard`, qui ne
/// montre que des collectes réellement exécutées.
pub type SeedTimes = BTreeMap<String, DateTime<Utc>>;

/// Prochain lancement après un SUCCÈS à `t` : premier créneau strictement après `t` + demi-période.
/// Écart minimal entre deux succès = la moitié de la période : un succès tardif ou obtenu après
/// une reprise (02:20 au lieu de 02:30) ne déclenche pas une seconde collecte le même jour.
pub fn next_after_success(t: DateTime<Utc>, every: &Every) -> DateTime<Utc> {
    next_slot_after(t + every.period() / 2, every)
}

/// Premier créneau STRICTEMENT après `t`.
pub fn next_slot_after(t: DateTime<Utc>, every: &Every) -> DateTime<Utc> {
    let mut day = t.date_naive();
    // 8 jours suffisent pour trouver un lundi ; la borne évite toute boucle infinie.
    for _ in 0..8 {
        let candidate = match *every {
            Every::DailyAt { hour, min } => Some(slot_on(day, hour, min)),
            Every::WeeklyAt { weekday, hour, min } if day.weekday() == weekday => {
                Some(slot_on(day, hour, min))
            }
            Every::WeeklyAt { .. } => None,
        };
        if let Some(c) = candidate.filter(|c| *c > t) {
            return c;
        }
        day = day.succ_opt().unwrap_or(day);
    }
    t + Span::days(8)
}

/// Prochain lancement. Sans succès connu : `now` + 30 s. Sinon le créneau qui suit `last_ok`
/// (avec l'écart minimal d'une demi-période, voir `next_after_success`), ou `now` si ce créneau
/// est déjà passé (rattrapage après un arrêt prolongé).
pub fn next_due(
    now: DateTime<Utc>,
    last_ok: Option<DateTime<Utc>>,
    every: &Every,
) -> DateTime<Utc> {
    match last_ok {
        None => now + Span::seconds(STARTUP_DELAY_SECS),
        Some(ok) => next_after_success(ok, every).max(now),
    }
}

/// Délai avant le nouvel essai après `failures` (>= 1) échecs consécutifs : échelle croissante
/// pour la boucle hebdomadaire (France Travail), 1 h fixe pour la boucle quotidienne (`core`).
pub fn retry_delay_secs(every: &Every, failures: u32) -> i64 {
    match every {
        Every::DailyAt { .. } => RETRY_AFTER_FAILURE_SECS,
        Every::WeeklyAt { .. } => {
            let i = (failures.max(1) as usize - 1).min(FT_RETRY_LADDER_SECS.len() - 1);
            FT_RETRY_LADDER_SECS[i]
        }
    }
}

/// Moment du nouvel essai après un échec survenu à `now`.
pub fn retry_at(now: DateTime<Utc>) -> DateTime<Utc> {
    now + Span::seconds(RETRY_AFTER_FAILURE_SECS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    Core,
    Rome,
    Tech,
    Carte,
}

impl Job {
    /// Nom dans le `StatusBoard` et dans la ligne de commande.
    pub fn name(self) -> &'static str {
        match self {
            Job::Core => "core",
            Job::Rome => "rome",
            Job::Tech => "tech",
            Job::Carte => "carte",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Serve,
    Once(Job),
}

pub const USAGE: &str = "Usage : veille_emploi [serve | once core|rome|tech|carte]";

/// `serve` par défaut ; `once core|rome|tech|carte`.
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        [] | ["serve"] => Ok(Command::Serve),
        ["once", "core"] => Ok(Command::Once(Job::Core)),
        ["once", "rome"] => Ok(Command::Once(Job::Rome)),
        ["once", "tech"] => Ok(Command::Once(Job::Tech)),
        ["once", "carte"] => Ok(Command::Once(Job::Carte)),
        ["once", other] => Err(format!("collecte inconnue « {other} ». {USAGE}")),
        ["once"] => Err(format!("collecte manquante. {USAGE}")),
        _ => Err(format!("arguments non reconnus. {USAGE}")),
    }
}

/// Exécute une collecte.
///
/// Le futur n'est volontairement pas `Send` : `rome::run` (collecteur, non modifié ici) n'est pas
/// prouvé `Send` par `rustc` (limite connue des flux `buffer_unordered`). Les boucles tournent
/// donc sur la tâche qui les attend, sans `tokio::spawn`.
pub trait Runner: 'static {
    fn run(&self, job: Job) -> impl Future<Output = Result<(), CollectError>>;

    /// Le job a-t-il ce qu'il lui faut pour tourner ? (`carte` a besoin de `rome.js`.) Vérifié par le
    /// planificateur avant de lancer une collecte DUE : sinon rien n'est lancé, sans erreur consignée.
    fn can_run(&self, _job: Job) -> bool {
        true
    }
}

/// Heure murale (injectable pour les tests).
pub trait WallClock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// État partagé du programme.
pub struct Ctx {
    /// Client des sources publiques (Indeed, Eurostat, DARES).
    pub http: reqwest::Client,
    pub store: Store,
    pub status: StatusBoard,
    pub cfg: Config,
    /// Client France Travail, construit une fois au démarrage si les identifiants sont présents.
    pub ft: Option<crate::collect::ft::FtClient>,
    /// Dates supposées de dernière collecte (planification seulement, voir `SeedTimes`).
    pub seed: SeedTimes,
}

impl Runner for Ctx {
    fn can_run(&self, job: Job) -> bool {
        match job {
            Job::Carte => self.store.modified("rome.js").is_some(),
            _ => true,
        }
    }

    async fn run(&self, job: Job) -> Result<(), CollectError> {
        let today = Utc::now().date_naive();
        match job {
            Job::Core => {
                core_collect::run(
                    &self.http,
                    &core_collect::Sources::production(),
                    &self.store,
                    today,
                )
                .await
            }
            Job::Rome | Job::Tech | Job::Carte => {
                let ft = self.ft.as_ref().ok_or_else(|| {
                    CollectError::Fetch("identifiants France Travail absents".into())
                })?;
                match job {
                    Job::Rome => rome::run(ft, &self.store, today, rome::DEFAULT_WORKERS).await,
                    Job::Tech => tech::run(ft, &self.store, today).await,
                    _ => carte::run(ft, &self.store, today).await,
                }
            }
        }
    }
}

/// Lance une collecte et consigne le résultat dans le `StatusBoard` sous le nom du job.
/// Les messages d'erreur de `CollectError` ne contiennent jamais de secret.
pub async fn run_job<R: Runner, C: WallClock>(
    runner: &R,
    clock: &C,
    status: &StatusBoard,
    job: Job,
) -> Result<(), CollectError> {
    tracing::info!(job = job.name(), "collecte : début");
    // Une panique dans un collecteur ne doit pas tuer le serveur ni l'autre boucle : elle devient
    // un échec ordinaire (consigné, nouvel essai dans 1 h). Le message de la panique n'est jamais
    // repris (il pourrait contenir des données).
    let res = match std::panic::AssertUnwindSafe(runner.run(job))
        .catch_unwind()
        .await
    {
        Ok(r) => r,
        Err(_) => Err(CollectError::Fetch(format!(
            "panique interne {}",
            job.name()
        ))),
    };
    let now = clock.now();
    match &res {
        Ok(()) => {
            status.record_ok(job.name(), now);
            tracing::info!(job = job.name(), "collecte : réussie");
        }
        Err(e) => {
            let msg = e.to_string();
            status.record_err(job.name(), now, &msg);
            tracing::warn!(job = job.name(), error = %msg, "collecte : échec");
        }
    }
    res
}

/// Dernier succès réel (tableau d'état), à défaut la date supposée au démarrage.
fn last_ok_of(status: &StatusBoard, seed: &SeedTimes, name: &str) -> Option<DateTime<Utc>> {
    let real = status
        .snapshot()
        .get(name)
        .and_then(|s| s.last_ok.clone())
        .and_then(|raw| DateTime::parse_from_rfc3339(&raw).ok())
        .map(|d| d.with_timezone(&Utc));
    real.or_else(|| seed.get(name).copied())
}

/// Plus ancien dernier succès du groupe ; `None` dès qu'un job n'en a aucun.
fn group_last_ok(status: &StatusBoard, seed: &SeedTimes, jobs: &[Job]) -> Option<DateTime<Utc>> {
    let mut oldest: Option<DateTime<Utc>> = None;
    for j in jobs {
        let ok = last_ok_of(status, seed, j.name())?;
        oldest = Some(oldest.map_or(ok, |o| o.min(ok)));
    }
    oldest
}

async fn sleep_until<C: WallClock>(clock: &C, due: DateTime<Utc>) {
    loop {
        let remaining = due - clock.now();
        let Ok(remaining) = remaining.to_std() else {
            return;
        };
        if remaining.is_zero() {
            return;
        }
        tokio::time::sleep(remaining.min(Duration::from_secs(MAX_SLEEP_SECS))).await;
    }
}

/// Boucle d'un groupe de collectes lancées l'une après l'autre (`rome` puis `tech` pour France
/// Travail). Ne rend jamais la main. Tous les jobs du tour sont tentés même si l'un échoue ; seuls
/// les jobs en échec sont retentés (`rome` réussi n'est pas relancé parce que `tech` a échoué),
/// après `retry_delay_secs`, sans jamais dépasser le créneau suivant, où tout le groupe repart.
pub async fn job_loop<R: Runner, C: WallClock>(
    runner: Arc<R>,
    clock: Arc<C>,
    status: StatusBoard,
    seed: SeedTimes,
    jobs: Vec<Job>,
    every: Every,
) {
    let label = jobs.iter().map(|j| j.name()).collect::<Vec<_>>().join("+");
    let mut due = next_due(clock.now(), group_last_ok(&status, &seed, &jobs), &every);
    let mut pending = jobs.clone();
    let mut failures: u32 = 0;
    tracing::info!(jobs = %label, prochain = %due, "planification");
    loop {
        sleep_until(&*clock, due).await;
        let mut failed = Vec::new();
        for job in &pending {
            if run_job(&*runner, &*clock, &status, *job).await.is_err() {
                failed.push(*job);
            }
        }
        let now = clock.now();
        if failed.is_empty() {
            failures = 0;
            pending = jobs.clone();
            due = next_after_success(now, &every);
        } else {
            failures += 1;
            let retry = now + Span::seconds(retry_delay_secs(&every, failures));
            let slot = next_slot_after(now, &every);
            if retry >= slot {
                // Le créneau suivant arrive avant le nouvel essai : nouveau cycle complet.
                failures = 0;
                pending = jobs.clone();
                due = slot;
            } else {
                pending = failed;
                due = retry;
            }
        }
        tracing::info!(jobs = %label, prochain = %due, "planification");
    }
}

/// Âge de `carte.js` : dû s'il est absent (aucun succès connu) ou vieux de PLUS de 28 jours.
pub fn carte_is_stale(now: DateTime<Utc>, last_ok: Option<DateTime<Utc>>) -> bool {
    last_ok.is_none_or(|ok| now - ok > Span::days(CARTE_MAX_AGE_DAYS))
}

/// Premier lancement de `carte` au démarrage du programme : 30 s plus tard si `carte.js` est absent ou
/// vieux de plus de 28 jours, sinon au prochain lundi 04:00 (où l'âge est revérifié).
pub fn carte_first_due(now: DateTime<Utc>, last_ok: Option<DateTime<Utc>>) -> DateTime<Utc> {
    if carte_is_stale(now, last_ok) {
        now + Span::seconds(STARTUP_DELAY_SECS)
    } else {
        next_slot_after(now, &CARTE_EVERY)
    }
}

/// Boucle du job `carte`. Ne rend jamais la main. À chaque échéance (30 s après le démarrage si dû, puis
/// chaque lundi 04:00 UTC) :
/// - `carte.js` récent (28 jours au plus) : rien, prochaine vérification au lundi suivant ;
/// - `rome.js` absent (`Runner::can_run`) : rien à faire, journal explicite, ni erreur ni échec
///   consigné ; nouvelle vérification dans `CARTE_RECHECK_SECS` (au plus tard au lundi suivant) ;
/// - sinon la collecte tourne ; après un échec, reprise selon `retry_delay_secs` (1 h, 6 h, 24 h), sans
///   jamais dépasser le lundi suivant, où l'âge est revérifié.
pub async fn carte_loop<R: Runner, C: WallClock>(
    runner: Arc<R>,
    clock: Arc<C>,
    status: StatusBoard,
    seed: SeedTimes,
) {
    let name = Job::Carte.name();
    let mut due = carte_first_due(clock.now(), last_ok_of(&status, &seed, name));
    let mut failures: u32 = 0;
    tracing::info!(job = name, prochain = %due, "planification");
    loop {
        sleep_until(&*clock, due).await;
        let now = clock.now();
        let slot = next_slot_after(now, &CARTE_EVERY);
        if !carte_is_stale(now, last_ok_of(&status, &seed, name)) {
            tracing::info!(
                job = name,
                "carte.js a moins de {CARTE_MAX_AGE_DAYS} jours : rien à collecter"
            );
            failures = 0;
            due = slot;
        } else if !runner.can_run(Job::Carte) {
            tracing::info!(
                job = name,
                "rome.js absent : la collecte carte attend (rien à faire pour l'instant)"
            );
            due = (now + Span::seconds(CARTE_RECHECK_SECS)).min(slot);
        } else if run_job(&*runner, &*clock, &status, Job::Carte)
            .await
            .is_ok()
        {
            failures = 0;
            due = next_slot_after(clock.now(), &CARTE_EVERY);
        } else {
            let now = clock.now();
            failures += 1;
            let retry = now + Span::seconds(retry_delay_secs(&CARTE_EVERY, failures));
            let slot = next_slot_after(now, &CARTE_EVERY);
            if retry >= slot {
                failures = 0;
                due = slot;
            } else {
                due = retry;
            }
        }
        tracing::info!(job = name, prochain = %due, "planification");
    }
}

/// Un seul job France Travail à la fois : `rome`, `tech` et `carte` se partagent un verrou (le quota de
/// l'API est partagé ; `carte` dure une heure). `core` (sources publiques) n'est jamais retardé. Le
/// verrou est équitable : un job qui attend passe dans l'ordre d'arrivée.
pub struct FtGate<R> {
    inner: Arc<R>,
    lock: tokio::sync::Mutex<()>,
}

impl<R: Runner> FtGate<R> {
    pub fn new(inner: Arc<R>) -> Self {
        FtGate {
            inner,
            lock: tokio::sync::Mutex::new(()),
        }
    }
}

impl<R: Runner> Runner for FtGate<R> {
    fn can_run(&self, job: Job) -> bool {
        self.inner.can_run(job)
    }

    async fn run(&self, job: Job) -> Result<(), CollectError> {
        let _turn = match job {
            Job::Core => None,
            Job::Rome | Job::Tech | Job::Carte => Some(self.lock.lock().await),
        };
        self.inner.run(job).await
    }
}

/// Boucles à démarrer : `core` toujours, France Travail seulement avec identifiants.
pub fn loops_to_start(ft_enabled: bool) -> Vec<(Vec<Job>, Every)> {
    let mut v = vec![(vec![Job::Core], CORE_EVERY)];
    if ft_enabled {
        v.push((vec![Job::Rome, Job::Tech], FT_EVERY));
    }
    v
}

/// Lance les boucles avec un exécuteur et une horloge donnés. Ne rend jamais la main.
pub async fn run_forever_with<R: Runner, C: WallClock>(
    runner: Arc<R>,
    clock: Arc<C>,
    status: StatusBoard,
    seed: SeedTimes,
    ft_enabled: bool,
) {
    if !ft_enabled {
        tracing::warn!(
            "identifiants France Travail absents : collectes rome, tech et carte désactivées (core reste actif)"
        );
    }
    let runner = Arc::new(FtGate::new(runner));
    let loops = loops_to_start(ft_enabled).into_iter().map(|(jobs, every)| {
        job_loop(
            runner.clone(),
            clock.clone(),
            status.clone(),
            seed.clone(),
            jobs,
            every,
        )
    });
    let carte = async {
        if ft_enabled {
            carte_loop(runner.clone(), clock.clone(), status.clone(), seed.clone()).await;
        }
    };
    futures::future::join(futures::future::join_all(loops), carte).await;
}

/// Planificateur de production : `core` quotidien, France Travail hebdomadaire.
pub async fn run_forever(ctx: Arc<Ctx>) {
    let status = ctx.status.clone();
    let seed = ctx.seed.clone();
    let ft_enabled = ctx.ft.is_some();
    run_forever_with(ctx, Arc::new(SystemWallClock), status, seed, ft_enabled).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    const DAILY: Every = CORE_EVERY;
    const WEEKLY: Every = FT_EVERY;

    #[test]
    fn no_success_means_start_in_30s() {
        let now = utc(2026, 11, 9, 10, 0, 0);
        assert_eq!(next_due(now, None, &DAILY), now + Span::seconds(30));
        assert_eq!(STARTUP_DELAY_SECS, 30);
    }

    #[test]
    fn daily_next_slot_after_last_ok() {
        let ok = utc(2026, 11, 9, 2, 31, 0);
        let now = utc(2026, 11, 9, 12, 0, 0);
        assert_eq!(next_due(now, Some(ok), &DAILY), utc(2026, 11, 10, 2, 30, 0));
    }

    #[test]
    fn daily_same_day_not_yet_reached() {
        // (modifié : succès la veille à 03:00, la demi-période passée, le créneau du jour compte)
        let ok = utc(2026, 11, 8, 3, 0, 0);
        let now = utc(2026, 11, 9, 1, 30, 0);
        assert_eq!(next_due(now, Some(ok), &DAILY), utc(2026, 11, 9, 2, 30, 0));
        // Succès hier 02:31 -> aujourd'hui 02:30.
        let ok = utc(2026, 11, 8, 2, 31, 0);
        assert_eq!(next_due(now, Some(ok), &DAILY), utc(2026, 11, 9, 2, 30, 0));
    }

    #[test]
    fn success_close_to_slot_does_not_run_twice_the_same_day() {
        let now = utc(2026, 11, 9, 2, 20, 0);
        // Succès à 02:20 (reprise après échec) : le lendemain, pas 02:30 le même jour.
        assert_eq!(
            next_due(now, Some(now), &DAILY),
            utc(2026, 11, 10, 2, 30, 0)
        );
        // Premier lancement à 02:00:30.
        let t = utc(2026, 11, 9, 2, 0, 30);
        assert_eq!(next_due(t, Some(t), &DAILY), utc(2026, 11, 10, 2, 30, 0));
        // Hebdomadaire : succès lundi 02:50 -> lundi suivant, pas 03:00 le même jour.
        let t = utc(2026, 11, 9, 2, 50, 0);
        assert_eq!(next_due(t, Some(t), &WEEKLY), utc(2026, 11, 16, 3, 0, 0));
        assert_eq!(next_after_success(t, &WEEKLY), utc(2026, 11, 16, 3, 0, 0));
    }

    #[test]
    fn overdue_after_long_downtime_runs_now() {
        let now = utc(2026, 11, 9, 10, 0, 0);
        let ok = now - Span::days(5);
        assert_eq!(next_due(now, Some(ok), &DAILY), now);
        assert_eq!(next_due(now, Some(ok), &WEEKLY), now);
    }

    #[test]
    fn weekly_next_monday() {
        // Mercredi 4 novembre 2026 -> lundi 9 novembre 03:00.
        let ok = utc(2026, 11, 4, 10, 0, 0);
        let now = utc(2026, 11, 5, 10, 0, 0);
        assert_eq!(next_due(now, Some(ok), &WEEKLY), utc(2026, 11, 9, 3, 0, 0));
        // (modifié) Un lundi avant 03:00 avec succès récent : le lundi suivant (demi-période).
        let ok = utc(2026, 11, 9, 1, 0, 0);
        assert_eq!(
            next_due(utc(2026, 11, 9, 1, 5, 0), Some(ok), &WEEKLY),
            utc(2026, 11, 16, 3, 0, 0)
        );
        // Un lundi après 03:00 : le lundi suivant.
        let ok = utc(2026, 11, 9, 3, 0, 1);
        assert_eq!(
            next_due(utc(2026, 11, 9, 4, 0, 0), Some(ok), &WEEKLY),
            utc(2026, 11, 16, 3, 0, 0)
        );
    }

    #[test]
    fn weekly_crossing_month_and_year_boundary() {
        // Jeudi 31 décembre 2026 -> lundi 4 janvier 2027.
        let ok = utc(2026, 12, 31, 10, 0, 0);
        let now = utc(2026, 12, 31, 12, 0, 0);
        assert_eq!(next_due(now, Some(ok), &WEEKLY), utc(2027, 1, 4, 3, 0, 0));
        // Fin de mois pour le quotidien.
        let ok = utc(2026, 12, 31, 2, 31, 0);
        assert_eq!(next_due(now, Some(ok), &DAILY), utc(2027, 1, 1, 2, 30, 0));
    }

    #[test]
    fn midnight_boundary_is_not_skipped() {
        // Dernier succès exactement sur le créneau d'hier ; il est 02:30:00 pile : c'est dû.
        let ok = utc(2026, 11, 8, 2, 30, 0);
        let now = utc(2026, 11, 9, 2, 30, 0);
        assert_eq!(next_due(now, Some(ok), &DAILY), now);
        // Et un succès exactement sur un créneau ne redonne pas ce même créneau (jamais deux fois).
        assert_eq!(next_due(ok, Some(ok), &DAILY), utc(2026, 11, 9, 2, 30, 0));
    }

    #[test]
    fn failure_retries_in_one_hour() {
        let now = utc(2026, 11, 9, 10, 0, 0);
        assert_eq!(retry_at(now), now + Span::seconds(3600));
        assert_eq!(RETRY_AFTER_FAILURE_SECS, 3600);
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_args_defaults_to_serve() {
        assert_eq!(parse_args(&[]), Ok(Command::Serve));
        assert_eq!(parse_args(&args(&["serve"])), Ok(Command::Serve));
    }

    #[test]
    fn parse_args_once_rome() {
        assert_eq!(
            parse_args(&args(&["once", "rome"])),
            Ok(Command::Once(Job::Rome))
        );
        assert_eq!(
            parse_args(&args(&["once", "core"])),
            Ok(Command::Once(Job::Core))
        );
        assert_eq!(
            parse_args(&args(&["once", "tech"])),
            Ok(Command::Once(Job::Tech))
        );
        assert_eq!(
            parse_args(&args(&["once", "carte"])),
            Ok(Command::Once(Job::Carte))
        );
        assert_eq!(Job::Carte.name(), "carte");
        assert!(USAGE.contains("carte"));
    }

    #[test]
    fn parse_args_rejects_unknown_job() {
        let e = parse_args(&args(&["once", "nope"])).unwrap_err();
        assert!(e.contains("nope") && e.contains("Usage"), "{e}");
        assert!(parse_args(&args(&["once"])).is_err());
        assert!(parse_args(&args(&["frobnicate"])).is_err());
        assert!(parse_args(&args(&["serve", "extra"])).is_err());
    }

    // ----- boucles, sans réseau, avec le temps de tokio en pause -----

    /// Heure murale qui suit l'horloge (en pause) de tokio.
    struct TokioClock {
        start_utc: DateTime<Utc>,
        start: tokio::time::Instant,
    }

    impl TokioClock {
        fn new(start_utc: DateTime<Utc>) -> Arc<Self> {
            Arc::new(TokioClock {
                start_utc,
                start: tokio::time::Instant::now(),
            })
        }
    }

    impl WallClock for TokioClock {
        fn now(&self) -> DateTime<Utc> {
            self.start_utc + Span::from_std(self.start.elapsed()).unwrap()
        }
    }

    struct FakeRunner {
        clock: Arc<TokioClock>,
        calls: Mutex<Vec<(Job, DateTime<Utc>)>>,
        /// Résultats successifs (vide : succès).
        script: Mutex<Vec<Result<(), String>>>,
        duration: Duration,
        active: AtomicUsize,
        max_active: AtomicUsize,
        /// Jobs France Travail (rome, tech, carte) en cours : jamais plus d'un à la fois.
        ft_active: AtomicUsize,
        ft_max_active: AtomicUsize,
        /// `rome.js` est-il présent ? (réponse de `can_run` pour `carte`)
        rome_ready: AtomicBool,
    }

    impl FakeRunner {
        fn new(clock: &Arc<TokioClock>) -> Arc<Self> {
            Arc::new(FakeRunner {
                clock: clock.clone(),
                calls: Mutex::new(vec![]),
                script: Mutex::new(vec![]),
                duration: Duration::ZERO,
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                ft_active: AtomicUsize::new(0),
                ft_max_active: AtomicUsize::new(0),
                rome_ready: AtomicBool::new(true),
            })
        }
        fn with(
            clock: &Arc<TokioClock>,
            script: Vec<Result<(), String>>,
            duration: Duration,
        ) -> Arc<Self> {
            let mut r = Arc::try_unwrap(Self::new(clock)).ok().unwrap();
            r.script = Mutex::new(script);
            r.duration = duration;
            Arc::new(r)
        }
        fn calls(&self) -> Vec<(Job, DateTime<Utc>)> {
            self.calls.lock().unwrap().clone()
        }
        fn jobs(&self) -> Vec<Job> {
            self.calls().into_iter().map(|c| c.0).collect()
        }
    }

    impl Runner for FakeRunner {
        fn can_run(&self, job: Job) -> bool {
            job != Job::Carte || self.rome_ready.load(Ordering::SeqCst)
        }

        async fn run(&self, job: Job) -> Result<(), CollectError> {
            self.calls.lock().unwrap().push((job, self.clock.now()));
            let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(n, Ordering::SeqCst);
            if job != Job::Core {
                let n = self.ft_active.fetch_add(1, Ordering::SeqCst) + 1;
                self.ft_max_active.fetch_max(n, Ordering::SeqCst);
            }
            if !self.duration.is_zero() {
                tokio::time::sleep(self.duration).await;
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            if job != Job::Core {
                self.ft_active.fetch_sub(1, Ordering::SeqCst);
            }
            let next = {
                let mut s = self.script.lock().unwrap();
                if s.is_empty() {
                    Ok(())
                } else {
                    s.remove(0)
                }
            };
            if matches!(&next, Err(m) if m == "PANIC") {
                panic!("secret-xyz dans le message de panique");
            }
            next.map_err(CollectError::Fetch)
        }
    }

    fn start() -> DateTime<Utc> {
        utc(2026, 11, 9, 10, 0, 0) // un lundi, après 03:00
    }

    async fn local<F: Future<Output = ()>>(f: F) {
        tokio::task::LocalSet::new().run_until(f).await;
    }

    async fn pass(d: Duration) {
        tokio::time::sleep(d).await;
    }

    fn spawn_loop(
        r: &Arc<FakeRunner>,
        clock: &Arc<TokioClock>,
        status: &StatusBoard,
        jobs: Vec<Job>,
        every: Every,
    ) -> tokio::task::JoinHandle<()> {
        tokio::task::spawn_local(job_loop(
            r.clone(),
            clock.clone(),
            status.clone(),
            SeedTimes::new(),
            jobs,
            every,
        ))
    }

    #[tokio::test(start_paused = true)]
    async fn overdue_job_runs_within_first_scheduling_step() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            status.record_ok("core", start() - Span::days(5));
            let r = FakeRunner::new(&clock);
            let h = spawn_loop(&r, &clock, &status, vec![Job::Core], DAILY);
            pass(Duration::from_millis(1)).await;
            assert_eq!(
                r.jobs(),
                vec![Job::Core],
                "rattrapage immédiat, sans attendre 30 s"
            );
            assert_eq!(r.calls()[0].1, start());
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn failure_is_recorded_and_retried_after_exactly_one_hour() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::with(
                &clock,
                vec![Err("panne simulée".into()), Ok(())],
                Duration::ZERO,
            );
            let h = spawn_loop(&r, &clock, &status, vec![Job::Core], DAILY);
            pass(Duration::from_secs(31)).await;
            assert_eq!(r.calls().len(), 1);
            let st = &status.snapshot()["core"];
            assert!(st.last_error.as_deref().unwrap().contains("panne simulée"));
            assert_eq!(st.last_ok, None);
            pass(Duration::from_secs(3590)).await; // t = 3621 s ; retry attendu à 30 + 3600 = 3630 s
            assert_eq!(r.calls().len(), 1, "pas de nouvel essai avant 1 h");
            pass(Duration::from_secs(10)).await;
            let calls = r.calls();
            assert_eq!(calls.len(), 2);
            let gap = (calls[1].1 - calls[0].1).num_milliseconds();
            assert!((3_600_000..=3_600_005).contains(&gap), "écart {gap} ms");
            assert!(status.snapshot()["core"].last_ok.is_some());
            assert_eq!(status.snapshot()["core"].last_error, None);
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn success_is_followed_by_next_slot_exactly_once() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::new(&clock);
            let h = spawn_loop(&r, &clock, &status, vec![Job::Core], DAILY);
            pass(Duration::from_secs(31)).await;
            assert_eq!(r.calls().len(), 1);
            // 10 novembre 02:29 : pas encore.
            pass(Duration::from_secs(16 * 3600 + 29 * 60)).await;
            assert_eq!(r.calls().len(), 1);
            // 02:31 : exactement un lancement de plus, au créneau.
            pass(Duration::from_secs(2 * 60)).await;
            let calls = r.calls();
            assert_eq!(calls.len(), 2);
            let slot = utc(2026, 11, 10, 2, 30, 0);
            assert!(
                calls[1].1 >= slot && calls[1].1 < slot + Span::seconds(1),
                "{:?}",
                calls[1].1
            );
            // Le reste de la journée : aucun lancement de plus.
            pass(Duration::from_secs(20 * 3600)).await;
            assert_eq!(r.calls().len(), 2);
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn same_job_never_runs_concurrently() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            // Une collecte dure 30 h : plus d'un créneau quotidien s'écoule pendant qu'elle tourne.
            let r = FakeRunner::with(&clock, vec![], Duration::from_secs(30 * 3600));
            let h = spawn_loop(&r, &clock, &status, vec![Job::Core], DAILY);
            pass(Duration::from_secs(100 * 3600)).await;
            assert!(r.calls().len() >= 2, "{:?}", r.calls());
            assert_eq!(r.max_active.load(Ordering::SeqCst), 1);
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn france_travail_runs_rome_then_tech_with_separate_statuses() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::with(
                &clock,
                vec![Err("rome en panne".into()), Ok(())],
                Duration::ZERO,
            );
            let h = spawn_loop(&r, &clock, &status, vec![Job::Rome, Job::Tech], WEEKLY);
            pass(Duration::from_secs(31)).await;
            assert_eq!(
                r.jobs(),
                vec![Job::Rome, Job::Tech],
                "tech est tenté même si rome échoue"
            );
            let snap = status.snapshot();
            assert!(snap["rome"].last_error.is_some() && snap["rome"].last_ok.is_none());
            assert!(snap["tech"].last_ok.is_some() && snap["tech"].last_error.is_none());
            assert!(!snap.contains_key("core"));
            // Seul rome (en échec) est retenté une heure plus tard ; tech, réussi, ne l'est pas.
            pass(Duration::from_secs(3601)).await;
            assert_eq!(r.jobs(), vec![Job::Rome, Job::Tech, Job::Rome]);
            assert!(status.snapshot()["rome"].last_ok.is_some());
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn without_ft_credentials_only_core_loop_runs() {
        local(async move {
            assert_eq!(loops_to_start(false).len(), 1);
            assert_eq!(loops_to_start(true).len(), 2);
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::new(&clock);
            let h = tokio::task::spawn_local(run_forever_with(
                r.clone(),
                clock.clone(),
                status.clone(),
                SeedTimes::new(),
                false,
            ));
            pass(Duration::from_secs(10 * 24 * 3600)).await;
            let jobs = r.jobs();
            assert!(!jobs.is_empty());
            assert!(jobs.iter().all(|j| *j == Job::Core), "{jobs:?}");
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn with_ft_credentials_both_loops_run() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::new(&clock);
            let h = tokio::task::spawn_local(run_forever_with(
                r.clone(),
                clock.clone(),
                status.clone(),
                SeedTimes::new(),
                true,
            ));
            pass(Duration::from_secs(60)).await;
            let jobs = r.jobs();
            assert!(
                jobs.contains(&Job::Core) && jobs.contains(&Job::Rome) && jobs.contains(&Job::Tech)
            );
            let rome = jobs.iter().position(|j| *j == Job::Rome).unwrap();
            let tech = jobs.iter().position(|j| *j == Job::Tech).unwrap();
            assert!(rome < tech);
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn panic_in_collector_is_isolated_recorded_and_retried_in_one_hour() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            // 1er appel (core, à +30 s) : panique ; ensuite succès.
            let r = FakeRunner::with(&clock, vec![Err("PANIC".into())], Duration::ZERO);
            let h = tokio::task::spawn_local(run_forever_with(
                r.clone(),
                clock.clone(),
                status.clone(),
                SeedTimes::new(),
                true,
            ));
            pass(Duration::from_secs(31)).await;
            let snap = status.snapshot();
            let err = snap["core"].last_error.clone().unwrap();
            assert_eq!(err, "récupération impossible : panique interne core");
            assert!(!err.contains("secret-xyz"));
            // L'autre boucle (France Travail) a bien tourné.
            assert!(snap["rome"].last_ok.is_some() && snap["tech"].last_ok.is_some());
            pass(Duration::from_secs(3590)).await;
            let cores = |r: &FakeRunner| r.calls().into_iter().filter(|c| c.0 == Job::Core).count();
            assert_eq!(cores(&r), 1);
            pass(Duration::from_secs(10)).await;
            let core_calls: Vec<_> = r.calls().into_iter().filter(|c| c.0 == Job::Core).collect();
            assert_eq!(core_calls.len(), 2);
            let gap = (core_calls[1].1 - core_calls[0].1).num_milliseconds();
            assert!((3_600_000..=3_600_005).contains(&gap), "écart {gap} ms");
            assert!(status.snapshot()["core"].last_ok.is_some());
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn seeded_time_is_used_for_scheduling_when_board_has_no_success() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::new(&clock);
            // Fichier vieux de 5 jours : dû tout de suite ; la planche reste vide.
            let mut seed = SeedTimes::new();
            seed.insert("core".into(), start() - Span::days(5));
            let h = tokio::task::spawn_local(job_loop(
                r.clone(),
                clock.clone(),
                status.clone(),
                seed,
                vec![Job::Core],
                DAILY,
            ));
            pass(Duration::from_millis(1)).await;
            assert_eq!(r.calls().len(), 1);
            assert_eq!(r.calls()[0].1, start());
            h.abort();
            // Fichier récent (il y a 1 h) : rien avant le créneau suivant (demain 02:30).
            let clock = TokioClock::new(start());
            let r = FakeRunner::new(&clock);
            let mut seed = SeedTimes::new();
            seed.insert("core".into(), start() - Span::hours(1));
            let h = tokio::task::spawn_local(job_loop(
                r.clone(),
                clock.clone(),
                StatusBoard::new(),
                seed,
                vec![Job::Core],
                DAILY,
            ));
            pass(Duration::from_secs(10 * 3600)).await;
            assert!(r.calls().is_empty());
            h.abort();
        })
        .await;
    }

    fn gaps_ms(calls: &[(Job, DateTime<Utc>)], job: Job) -> Vec<i64> {
        let t: Vec<_> = calls.iter().filter(|c| c.0 == job).map(|c| c.1).collect();
        t.windows(2)
            .map(|w| (w[1] - w[0]).num_milliseconds())
            .collect()
    }

    fn close(gap: i64, secs: i64) -> bool {
        (secs * 1000..=secs * 1000 + 5).contains(&gap)
    }

    fn fails(n: usize) -> Vec<Result<(), String>> {
        (0..n).map(|_| Err("panne".to_string())).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_failed_ft_job_is_retried() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            // rome réussit, tech échoue une fois.
            let r = FakeRunner::with(&clock, vec![Ok(()), Err("tech".into())], Duration::ZERO);
            let h = spawn_loop(&r, &clock, &status, vec![Job::Rome, Job::Tech], WEEKLY);
            pass(Duration::from_secs(3 * 3600)).await;
            assert_eq!(r.jobs(), vec![Job::Rome, Job::Tech, Job::Tech]);
            let calls = r.calls();
            assert!(close((calls[2].1 - calls[1].1).num_milliseconds(), 3600));
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn ft_failures_back_off_1h_6h_24h() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::with(&clock, fails(4), Duration::ZERO);
            let h = spawn_loop(&r, &clock, &status, vec![Job::Rome], WEEKLY);
            pass(Duration::from_secs(40 * 3600)).await;
            let g = gaps_ms(&r.calls(), Job::Rome);
            assert_eq!(g.len(), 3, "{g:?}");
            assert!(
                close(g[0], 3600) && close(g[1], 6 * 3600) && close(g[2], 24 * 3600),
                "{g:?}"
            );
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn ft_ladder_never_goes_past_the_next_weekly_slot() {
        local(async move {
            // Dimanche 15 novembre 20:00 : échec, échec (retard 1 h), le 6 h dépasserait lundi 03:00.
            let clock = TokioClock::new(utc(2026, 11, 15, 20, 0, 0));
            let status = StatusBoard::new();
            let r = FakeRunner::with(&clock, fails(4), Duration::ZERO);
            let h = spawn_loop(&r, &clock, &status, vec![Job::Rome, Job::Tech], WEEKLY);
            pass(Duration::from_secs(8 * 3600)).await;
            let rome: Vec<_> = r.calls().into_iter().filter(|c| c.0 == Job::Rome).collect();
            assert_eq!(rome.len(), 3);
            let slot = utc(2026, 11, 16, 3, 0, 0);
            assert!(
                rome[2].1 >= slot && rome[2].1 < slot + Span::milliseconds(5),
                "{:?}",
                rome[2].1
            );
            // Nouveau cycle complet au créneau : tech y est relancé aussi.
            assert!(r.jobs().iter().filter(|j| **j == Job::Tech).count() >= 2);
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn ft_success_resets_the_ladder() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let script = vec![Err("a".into()), Err("b".into()), Ok(()), Err("c".into())];
            let r = FakeRunner::with(&clock, script, Duration::ZERO);
            let h = spawn_loop(&r, &clock, &status, vec![Job::Rome], WEEKLY);
            pass(Duration::from_secs(8 * 24 * 3600)).await;
            let g = gaps_ms(&r.calls(), Job::Rome);
            // 1 h, 6 h, (succès puis lundi suivant), puis échec -> 1 h de nouveau.
            assert!(close(g[0], 3600) && close(g[1], 6 * 3600), "{g:?}");
            assert!(
                close(g[3], 3600),
                "après succès, l'échelle repart à 1 h : {g:?}"
            );
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn core_keeps_a_flat_one_hour_retry() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::with(&clock, fails(3), Duration::ZERO);
            let h = spawn_loop(&r, &clock, &status, vec![Job::Core], DAILY);
            pass(Duration::from_secs(4 * 3600)).await;
            let g = gaps_ms(&r.calls(), Job::Core);
            assert!(
                g.len() >= 3 && g.iter().take(3).all(|x| close(*x, 3600)),
                "{g:?}"
            );
            h.abort();
        })
        .await;
    }

    #[test]
    fn retry_delay_ladder_values() {
        assert_eq!(retry_delay_secs(&WEEKLY, 1), 3600);
        assert_eq!(retry_delay_secs(&WEEKLY, 2), 21_600);
        assert_eq!(retry_delay_secs(&WEEKLY, 3), 86_400);
        assert_eq!(retry_delay_secs(&WEEKLY, 9), 86_400);
        assert_eq!(retry_delay_secs(&DAILY, 5), 3600);
    }

    // ----- job carte : lundi 04:00 UTC, seulement si carte.js est absent ou vieux de plus de 28 jours -----

    const CARTE: Every = CARTE_EVERY;

    #[test]
    fn carte_slot_is_monday_0400_utc_after_the_ft_job() {
        assert_eq!(
            CARTE,
            Every::WeeklyAt {
                weekday: Weekday::Mon,
                hour: 4,
                min: 0
            }
        );
        // le job FT de 03:00 passe avant : même lundi, une heure d'écart
        let ft = utc(2026, 11, 9, 3, 0, 0);
        assert_eq!(next_slot_after(ft, &WEEKLY), utc(2026, 11, 16, 3, 0, 0));
        assert_eq!(next_slot_after(ft, &CARTE), utc(2026, 11, 9, 4, 0, 0));
        // dimanche soir -> lundi 04:00 ; lundi 04:00 pile -> lundi suivant (strictement après)
        assert_eq!(
            next_slot_after(utc(2026, 11, 8, 23, 59, 0), &CARTE),
            utc(2026, 11, 9, 4, 0, 0)
        );
        assert_eq!(
            next_slot_after(utc(2026, 11, 9, 4, 0, 0), &CARTE),
            utc(2026, 11, 16, 4, 0, 0)
        );
        assert_eq!(
            next_slot_after(utc(2026, 12, 31, 12, 0, 0), &CARTE),
            utc(2027, 1, 4, 4, 0, 0)
        );
        assert_eq!(retry_delay_secs(&CARTE, 1), 3600);
        assert_eq!(retry_delay_secs(&CARTE, 2), 6 * 3600);
        assert_eq!(retry_delay_secs(&CARTE, 3), 24 * 3600);
        assert_eq!(retry_delay_secs(&CARTE, 8), 24 * 3600);
        assert_eq!((CARTE_MAX_AGE_DAYS, CARTE_RECHECK_SECS), (28, 3600));
    }

    #[test]
    fn carte_is_stale_when_absent_or_strictly_older_than_28_days() {
        let now = utc(2026, 11, 9, 4, 0, 0);
        assert!(carte_is_stale(now, None));
        assert!(!carte_is_stale(now, Some(now)));
        assert!(!carte_is_stale(now, Some(now - Span::days(27))));
        // exactement 28 jours : pas encore ; une seconde de plus : oui
        assert!(!carte_is_stale(now, Some(now - Span::days(28))));
        assert!(carte_is_stale(
            now,
            Some(now - Span::days(28) - Span::seconds(1))
        ));
        assert!(carte_is_stale(now, Some(now - Span::days(120))));
        // une date dans le futur n'est pas vieille
        assert!(!carte_is_stale(now, Some(now + Span::hours(1))));
    }

    #[test]
    fn carte_first_due_is_30s_when_stale_else_next_monday_0400() {
        let now = utc(2026, 11, 11, 15, 0, 0); // mercredi
        assert_eq!(carte_first_due(now, None), now + Span::seconds(30));
        assert_eq!(
            carte_first_due(now, Some(now - Span::days(29))),
            now + Span::seconds(30)
        );
        assert_eq!(
            carte_first_due(now, Some(now - Span::days(28))),
            utc(2026, 11, 16, 4, 0, 0)
        );
        assert_eq!(
            carte_first_due(now, Some(now - Span::days(3))),
            utc(2026, 11, 16, 4, 0, 0)
        );
        // un lundi après 04:00 : le lundi suivant
        let monday = utc(2026, 11, 9, 10, 0, 0);
        assert_eq!(
            carte_first_due(monday, Some(monday)),
            utc(2026, 11, 16, 4, 0, 0)
        );
    }

    fn spawn_carte(
        r: &Arc<FakeRunner>,
        clock: &Arc<TokioClock>,
        status: &StatusBoard,
        seed: SeedTimes,
    ) -> tokio::task::JoinHandle<()> {
        tokio::task::spawn_local(carte_loop(r.clone(), clock.clone(), status.clone(), seed))
    }

    fn carte_seed(at: DateTime<Utc>) -> SeedTimes {
        let mut seed = SeedTimes::new();
        seed.insert("carte".into(), at);
        seed
    }

    fn carte_calls(r: &FakeRunner) -> Vec<DateTime<Utc>> {
        r.calls()
            .into_iter()
            .filter(|c| c.0 == Job::Carte)
            .map(|c| c.1)
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn carte_absent_at_startup_runs_30s_later() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::new(&clock);
            let h = spawn_carte(&r, &clock, &status, SeedTimes::new());
            pass(Duration::from_secs(29)).await;
            assert!(carte_calls(&r).is_empty());
            pass(Duration::from_secs(2)).await;
            let calls = carte_calls(&r);
            assert_eq!(calls.len(), 1);
            let gap = (calls[0] - start()).num_milliseconds();
            assert!((30_000..=30_005).contains(&gap), "écart {gap} ms");
            assert!(status.snapshot()["carte"].last_ok.is_some());
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn carte_older_than_28_days_at_startup_runs_30s_later() {
        local(async move {
            let clock = TokioClock::new(start());
            let r = FakeRunner::new(&clock);
            let seed = carte_seed(start() - Span::days(29));
            let h = spawn_carte(&r, &clock, &StatusBoard::new(), seed);
            pass(Duration::from_secs(31)).await;
            assert_eq!(carte_calls(&r).len(), 1);
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn recent_carte_waits_for_a_monday_where_it_is_older_than_28_days() {
        local(async move {
            // départ lundi 9 novembre 10:00 ; fichier du 20 octobre 10:00 (20 jours)
            let clock = TokioClock::new(start());
            let r = FakeRunner::new(&clock);
            let h = spawn_carte(
                &r,
                &clock,
                &StatusBoard::new(),
                carte_seed(start() - Span::days(20)),
            );
            // lundi 16 (26 j 18 h) : trop récent ; rien dans les 10 premiers jours
            pass(Duration::from_secs(10 * 24 * 3600)).await;
            assert!(carte_calls(&r).is_empty(), "{:?}", carte_calls(&r));
            // lundi 23 novembre 04:00 (33 jours) : lancé, une seule fois
            pass(Duration::from_secs(5 * 24 * 3600)).await;
            let calls = carte_calls(&r);
            assert_eq!(calls.len(), 1);
            let slot = utc(2026, 11, 23, 4, 0, 0);
            assert!(
                calls[0] >= slot && calls[0] < slot + Span::milliseconds(5),
                "{:?}",
                calls[0]
            );
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn carte_fresh_at_startup_never_runs_on_a_monday_before_28_days() {
        local(async move {
            let clock = TokioClock::new(start());
            let r = FakeRunner::new(&clock);
            // fichier de la veille (8 novembre 10:00) : lundis 16, 23 et 30 novembre trop récents ;
            // lundi 7 décembre 04:00 : 28 j 18 h, plus vieux de 28 jours : lancé
            let h = spawn_carte(
                &r,
                &clock,
                &StatusBoard::new(),
                carte_seed(start() - Span::days(1)),
            );
            pass(Duration::from_secs(27 * 24 * 3600)).await; // jusqu'au 6 décembre 10:00
            assert!(carte_calls(&r).is_empty());
            pass(Duration::from_secs(24 * 3600)).await;
            let calls = carte_calls(&r);
            assert_eq!(calls.len(), 1, "{calls:?}");
            let slot = utc(2026, 12, 7, 4, 0, 0);
            assert!(
                calls[0] >= slot && calls[0] < slot + Span::milliseconds(5),
                "{:?}",
                calls[0]
            );
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn carte_without_rome_js_does_nothing_quietly_then_runs_when_it_appears() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::new(&clock);
            r.rome_ready.store(false, Ordering::SeqCst);
            let h = spawn_carte(&r, &clock, &status, SeedTimes::new());
            // 5 heures de vérifications : aucune collecte, aucune erreur, aucun état consigné
            pass(Duration::from_secs(5 * 3600)).await;
            assert!(r.calls().is_empty());
            assert!(status.snapshot().is_empty(), "{:?}", status.snapshot());
            // rome.js apparaît : la collecte part à la vérification suivante (au plus 1 h plus tard)
            r.rome_ready.store(true, Ordering::SeqCst);
            pass(Duration::from_secs(3640)).await;
            assert_eq!(carte_calls(&r).len(), 1);
            let snap = status.snapshot();
            assert!(snap["carte"].last_ok.is_some() && snap["carte"].last_error.is_none());
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn carte_old_file_without_rome_js_is_not_collected_either() {
        local(async move {
            let clock = TokioClock::new(start());
            let r = FakeRunner::new(&clock);
            r.rome_ready.store(false, Ordering::SeqCst);
            let h = spawn_carte(
                &r,
                &clock,
                &StatusBoard::new(),
                carte_seed(start() - Span::days(60)),
            );
            pass(Duration::from_secs(3 * 24 * 3600)).await;
            assert!(r.calls().is_empty());
            h.abort();
        })
        .await;
    }

    fn carte_gaps(r: &FakeRunner) -> Vec<i64> {
        carte_calls(r)
            .windows(2)
            .map(|w| (w[1] - w[0]).num_milliseconds())
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn carte_failures_back_off_1h_6h_24h_and_are_recorded() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::with(&clock, fails(4), Duration::ZERO);
            let h = spawn_carte(&r, &clock, &status, SeedTimes::new());
            pass(Duration::from_secs(40 * 3600)).await;
            let g = carte_gaps(&r);
            assert_eq!(g.len(), 3, "{g:?}");
            assert!(
                close(g[0], 3600) && close(g[1], 6 * 3600) && close(g[2], 24 * 3600),
                "{g:?}"
            );
            let st = &status.snapshot()["carte"];
            assert!(st.last_error.as_deref().unwrap().contains("panne") && st.last_ok.is_none());
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn carte_ladder_resets_after_success_and_waits_28_days_before_the_next_run() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            // échec, échec, succès, puis (au premier lundi où le fichier a plus de 28 jours) un échec :
            // l'échelle repart de 1 h
            let script = vec![Err("a".into()), Err("b".into()), Ok(()), Err("c".into())];
            let r = FakeRunner::with(&clock, script, Duration::ZERO);
            let h = spawn_carte(&r, &clock, &status, SeedTimes::new());
            pass(Duration::from_secs(40 * 24 * 3600)).await;
            let t = carte_calls(&r);
            let g = carte_gaps(&r);
            assert!(close(g[0], 3600) && close(g[1], 6 * 3600), "{g:?}");
            // succès le lundi 9 novembre à 17:00:30 + 6 h ; aucun lancement aux lundis 16, 23, 30 novembre
            // ni 7 décembre 04:00 (27 j 11 h) ; relancé lundi 14 décembre 04:00
            assert!(t.len() >= 4, "{t:?}");
            let slot = utc(2026, 12, 14, 4, 0, 0);
            assert!(
                t[3] >= slot && t[3] < slot + Span::milliseconds(5),
                "{:?}",
                t[3]
            );
            assert!(t.len() >= 5, "{t:?}");
            assert!(
                close(g[3], 3600),
                "après succès, l'échelle repart à 1 h : {g:?}"
            );
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn carte_retry_never_goes_past_the_next_monday_slot() {
        local(async move {
            // dimanche 15 novembre 20:00 : échec à 20:00:30, 21:00:30, 03:00:30 ; le prochain essai (+24 h)
            // dépasserait lundi 04:00 : nouveau contrôle à ce créneau, où la collecte est relancée
            let clock = TokioClock::new(utc(2026, 11, 15, 20, 0, 0));
            let status = StatusBoard::new();
            let r = FakeRunner::with(&clock, fails(3), Duration::ZERO);
            let h = spawn_carte(&r, &clock, &status, SeedTimes::new());
            pass(Duration::from_secs(9 * 3600)).await;
            let t = carte_calls(&r);
            assert_eq!(t.len(), 4, "{t:?}");
            let slot = utc(2026, 11, 16, 4, 0, 0);
            assert!(
                t[3] >= slot && t[3] < slot + Span::milliseconds(5),
                "{:?}",
                t[3]
            );
            assert!(status.snapshot()["carte"].last_ok.is_some());
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn carte_panic_is_isolated_and_retried_in_one_hour() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            let r = FakeRunner::with(&clock, vec![Err("PANIC".into())], Duration::ZERO);
            let h = spawn_carte(&r, &clock, &status, SeedTimes::new());
            pass(Duration::from_secs(31)).await;
            let err = status.snapshot()["carte"].last_error.clone().unwrap();
            assert_eq!(err, "récupération impossible : panique interne carte");
            assert!(!err.contains("secret-xyz"));
            pass(Duration::from_secs(3601)).await;
            assert_eq!(carte_calls(&r).len(), 2);
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn carte_never_overlaps_rome_or_tech_but_core_is_not_delayed() {
        local(async move {
            let clock = TokioClock::new(start());
            let status = StatusBoard::new();
            // chaque collecte dure 2 h ; rome, tech et carte sont tous dus 30 s après le démarrage
            let r = FakeRunner::with(&clock, vec![], Duration::from_secs(2 * 3600));
            let h = tokio::task::spawn_local(run_forever_with(
                r.clone(),
                clock.clone(),
                status.clone(),
                SeedTimes::new(),
                true,
            ));
            pass(Duration::from_secs(60)).await;
            // core part tout de suite, en même temps qu'un job France Travail
            assert!(r.jobs().contains(&Job::Core));
            assert!(r.max_active.load(Ordering::SeqCst) >= 2);
            pass(Duration::from_secs(10 * 3600)).await;
            let jobs = r.jobs();
            for j in [Job::Rome, Job::Tech, Job::Carte] {
                assert!(jobs.contains(&j), "{j:?} jamais lancé : {jobs:?}");
            }
            assert_eq!(
                r.ft_max_active.load(Ordering::SeqCst),
                1,
                "rome, tech et carte ne se chevauchent jamais"
            );
            // chaque job FT a commencé après la fin du précédent (2 h d'écart au moins)
            let mut ft: Vec<_> = r.calls().into_iter().filter(|c| c.0 != Job::Core).collect();
            ft.sort_by_key(|c| c.1);
            for w in ft.windows(2) {
                assert!(w[1].1 - w[0].1 >= Span::hours(2), "{ft:?}");
            }
            h.abort();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn ft_gate_serializes_ft_jobs_only_and_forwards_can_run() {
        local(async move {
            let clock = TokioClock::new(start());
            let r = FakeRunner::with(&clock, vec![], Duration::from_secs(100));
            let gate = Arc::new(FtGate::new(r.clone()));
            let started = clock.now();
            let (a, b, c, d) = tokio::join!(
                gate.run(Job::Rome),
                gate.run(Job::Carte),
                gate.run(Job::Tech),
                gate.run(Job::Core)
            );
            assert!(a.is_ok() && b.is_ok() && c.is_ok() && d.is_ok());
            assert_eq!(r.ft_max_active.load(Ordering::SeqCst), 1);
            assert_eq!(
                r.max_active.load(Ordering::SeqCst),
                2,
                "core tourne en parallèle"
            );
            // 3 jobs de 100 s l'un après l'autre
            assert!(clock.now() - started >= Span::seconds(300));
            // ordre d'arrivée respecté
            let ft: Vec<Job> = r.jobs().into_iter().filter(|j| *j != Job::Core).collect();
            assert_eq!(ft, vec![Job::Rome, Job::Carte, Job::Tech]);
            r.rome_ready.store(false, Ordering::SeqCst);
            assert!(!gate.can_run(Job::Carte));
            assert!(gate.can_run(Job::Rome) && gate.can_run(Job::Core));
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn carte_loop_runs_with_ft_credentials_only() {
        local(async move {
            let clock = TokioClock::new(start());
            let r = FakeRunner::new(&clock);
            let h = tokio::task::spawn_local(run_forever_with(
                r.clone(),
                clock.clone(),
                StatusBoard::new(),
                SeedTimes::new(),
                true,
            ));
            pass(Duration::from_secs(60)).await;
            assert!(r.jobs().contains(&Job::Carte));
            h.abort();
            let clock = TokioClock::new(start());
            let r = FakeRunner::new(&clock);
            let h = tokio::task::spawn_local(run_forever_with(
                r.clone(),
                clock.clone(),
                StatusBoard::new(),
                SeedTimes::new(),
                false,
            ));
            pass(Duration::from_secs(3 * 24 * 3600)).await;
            assert!(!r.jobs().contains(&Job::Carte));
            h.abort();
        })
        .await;
    }

    #[test]
    fn ctx_needs_rome_js_for_carte_and_ft_credentials_to_run_it() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx {
            http: reqwest::Client::new(),
            store: Store::new(dir.path()),
            status: StatusBoard::new(),
            cfg: Config::from_map(&Default::default()).unwrap(),
            ft: None,
            seed: SeedTimes::new(),
        };
        assert!(!ctx.can_run(Job::Carte));
        assert!(ctx.can_run(Job::Rome) && ctx.can_run(Job::Tech) && ctx.can_run(Job::Core));
        std::fs::write(dir.path().join("rome.js"), "window.ROME={};").unwrap();
        assert!(ctx.can_run(Job::Carte));
        // sans identifiants, le job lui-même échoue avec un message explicite
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let err = rt.block_on(ctx.run(Job::Carte)).unwrap_err().to_string();
        assert!(err.contains("identifiants France Travail absents"), "{err}");
    }
}
