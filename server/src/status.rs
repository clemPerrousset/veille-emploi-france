//! État partagé des dernières collectes.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct CollectorStatus {
    pub last_attempt: Option<String>,
    pub last_ok: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct StatusBoard {
    inner: Arc<Mutex<BTreeMap<String, CollectorStatus>>>,
}

fn ts(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl StatusBoard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_ok(&self, name: &str, now: DateTime<Utc>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let s = g.entry(name.to_string()).or_default();
        s.last_attempt = Some(ts(now));
        s.last_ok = Some(ts(now));
        s.last_error = None;
    }

    pub fn record_err(&self, name: &str, now: DateTime<Utc>, msg: &str) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let s = g.entry(name.to_string()).or_default();
        s.last_attempt = Some(ts(now));
        s.last_error = Some(msg.to_string());
    }

    pub fn snapshot(&self) -> BTreeMap<String, CollectorStatus> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn status_records_ok_then_error_keeps_last_ok() {
        let b = StatusBoard::new();
        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 1, 2, 10, 0, 0).unwrap();
        b.record_ok("ft", t1);
        b.record_err("ft", t2, "boom");
        let s = &b.snapshot()["ft"];
        assert_eq!(s.last_ok.as_deref(), Some("2026-01-01T10:00:00Z"));
        assert_eq!(s.last_attempt.as_deref(), Some("2026-01-02T10:00:00Z"));
        assert_eq!(s.last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn status_snapshot_is_sorted_by_name() {
        let b = StatusBoard::new();
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        b.record_ok("zeta", t);
        b.record_ok("alpha", t);
        let keys: Vec<_> = b.snapshot().keys().cloned().collect();
        assert_eq!(keys, ["alpha", "zeta"]);
    }
}
