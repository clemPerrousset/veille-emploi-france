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
//! - un échec est consigné (`StatusBoard::record_err`) et retenté après
//!   `RETRY_AFTER_FAILURE_SECS`.

use crate::collect::{core as core_collect, rome, tech, CollectError};
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
/// Délai avant de retenter une collecte en échec.
pub const RETRY_AFTER_FAILURE_SECS: i64 = 3600;
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

/// Moment du nouvel essai après un échec survenu à `now`.
pub fn retry_at(now: DateTime<Utc>) -> DateTime<Utc> {
    now + Span::seconds(RETRY_AFTER_FAILURE_SECS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    Core,
    Rome,
    Tech,
}

impl Job {
    /// Nom dans le `StatusBoard` et dans la ligne de commande.
    pub fn name(self) -> &'static str {
        match self {
            Job::Core => "core",
            Job::Rome => "rome",
            Job::Tech => "tech",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Serve,
    Once(Job),
}

pub const USAGE: &str = "Usage : veille_emploi [serve | once core|rome|tech]";

/// `serve` par défaut ; `once core|rome|tech`.
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        [] | ["serve"] => Ok(Command::Serve),
        ["once", "core"] => Ok(Command::Once(Job::Core)),
        ["once", "rome"] => Ok(Command::Once(Job::Rome)),
        ["once", "tech"] => Ok(Command::Once(Job::Tech)),
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
            Job::Rome | Job::Tech => {
                let ft = self.ft.as_ref().ok_or_else(|| {
                    CollectError::Fetch("identifiants France Travail absents".into())
                })?;
                if job == Job::Rome {
                    rome::run(ft, &self.store, today, rome::DEFAULT_WORKERS).await
                } else {
                    tech::run(ft, &self.store, today).await
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
/// Travail). Ne rend jamais la main. Tous les jobs du groupe sont tentés même si l'un échoue ;
/// le groupe est en échec si l'un d'eux l'est.
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
    tracing::info!(jobs = %label, prochain = %due, "planification");
    loop {
        sleep_until(&*clock, due).await;
        let mut failed = false;
        for job in &jobs {
            failed |= run_job(&*runner, &*clock, &status, *job).await.is_err();
        }
        let now = clock.now();
        due = if failed {
            retry_at(now)
        } else {
            next_after_success(now, &every)
        };
        tracing::info!(jobs = %label, prochain = %due, "planification");
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
            "identifiants France Travail absents : collectes rome et tech désactivées (core reste actif)"
        );
    }
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
    futures::future::join_all(loops).await;
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
    use std::sync::atomic::{AtomicUsize, Ordering};
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
        async fn run(&self, job: Job) -> Result<(), CollectError> {
            self.calls.lock().unwrap().push((job, self.clock.now()));
            let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(n, Ordering::SeqCst);
            if !self.duration.is_zero() {
                tokio::time::sleep(self.duration).await;
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
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
            // Le groupe est en échec : nouvel essai (rome puis tech) une heure plus tard.
            pass(Duration::from_secs(3601)).await;
            assert_eq!(r.jobs(), vec![Job::Rome, Job::Tech, Job::Rome, Job::Tech]);
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
}
