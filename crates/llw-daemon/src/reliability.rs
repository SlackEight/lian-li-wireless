//! Tiered link-recovery state machine (spec §4.2). Pure: all decisions are
//! functions of injected `Instant`s — no clocks, no I/O, no sleeps.
//!
//! Tier 1 (re-acquire): sustained PWM dropout → re-run channel acquisition.
//! Tier 2 (reconnect): repeated Tier-1 failure → full dongle reconnect.
//! The daemon supervisor (M2b) executes the returned `Action`s.

use crate::config::ReliabilityConfig;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Tier 1: reset + re-run scored acquisition + re-apply state.
    Reacquire,
    /// Tier 2: drop and reopen the dongle transports, full rediscovery.
    Reconnect,
}

pub struct Reliability {
    cfg: Cfg,
    dropouts: VecDeque<Instant>,
    acquired_at: Option<Instant>,
    last_tier1: Option<Instant>,
    last_tier2: Option<Instant>,
    failed_tier1_streak: u32,
    /// Telemetry counters (M2b exposes these over IPC).
    pub total_dropouts: u64,
    pub total_tier1: u64,
    pub total_tier2: u64,
}

/// Durations precomputed from the serializable config.
struct Cfg {
    grace: Duration,
    dropout_threshold: u32,
    window: Duration,
    tier1_cooldown: Duration,
    tier2_cooldown: Duration,
    tier2_after_failed_tier1: u32,
}

impl Reliability {
    pub fn new(cfg: &ReliabilityConfig) -> Self {
        Self {
            cfg: Cfg {
                grace: Duration::from_secs(cfg.grace_s),
                dropout_threshold: cfg.dropout_threshold.max(1),
                window: Duration::from_secs(cfg.window_s),
                tier1_cooldown: Duration::from_secs(cfg.tier1_cooldown_s),
                tier2_cooldown: Duration::from_secs(cfg.tier2_cooldown_s),
                tier2_after_failed_tier1: cfg.tier2_after_failed_tier1.max(1),
            },
            dropouts: VecDeque::new(),
            acquired_at: None,
            last_tier1: None,
            last_tier2: None,
            failed_tier1_streak: 0,
            total_dropouts: 0,
            total_tier1: 0,
            total_tier2: 0,
        }
    }

    /// Call after every successful acquisition (startup or recovery).
    /// Starts the grace period and clears transient state.
    pub fn on_acquired(&mut self, now: Instant) {
        self.acquired_at = Some(now);
        self.dropouts.clear();
    }

    /// Record one dropout observation (commanded PWM present, readback all-zero).
    pub fn on_dropout(&mut self, now: Instant) {
        self.total_dropouts += 1;
        self.dropouts.push_back(now);
        self.prune(now);
    }

    /// Decide what (if anything) to do right now.
    pub fn poll(&mut self, now: Instant) -> Action {
        self.prune(now);

        // Escalation: enough failed Tier-1 attempts → Tier 2, respecting cooldown.
        if self.failed_tier1_streak >= self.cfg.tier2_after_failed_tier1 {
            let cooled = self
                .last_tier2
                .is_none_or(|t| now.duration_since(t) >= self.cfg.tier2_cooldown);
            if cooled {
                self.last_tier2 = Some(now);
                self.total_tier2 += 1;
                self.failed_tier1_streak = 0;
                self.dropouts.clear();
                return Action::Reconnect;
            }
            return Action::None; // wait out the cooldown
        }

        // Tier 1: threshold within window, after grace, respecting cooldown.
        let in_grace = self
            .acquired_at
            .is_none_or(|t| now.duration_since(t) < self.cfg.grace);
        if in_grace {
            return Action::None;
        }
        if (self.dropouts.len() as u32) < self.cfg.dropout_threshold {
            return Action::None;
        }
        let cooled = self
            .last_tier1
            .is_none_or(|t| now.duration_since(t) >= self.cfg.tier1_cooldown);
        if !cooled {
            return Action::None;
        }

        self.last_tier1 = Some(now);
        self.total_tier1 += 1;
        self.dropouts.clear();
        Action::Reacquire
    }

    /// Report how the executed Tier-1 attempt went. Success resets the streak
    /// AND restarts the grace period (via on_acquired, called by the executor).
    pub fn on_tier1_result(&mut self, ok: bool) {
        if ok {
            self.failed_tier1_streak = 0;
        } else {
            self.failed_tier1_streak += 1;
        }
    }

    /// Dropouts currently inside the window (telemetry).
    pub fn recent_dropouts(&mut self, now: Instant) -> u32 {
        self.prune(now);
        self.dropouts.len() as u32
    }

    fn prune(&mut self, now: Instant) {
        while let Some(&front) = self.dropouts.front() {
            if now.duration_since(front) > self.cfg.window {
                self.dropouts.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ReliabilityConfig;

    fn machine() -> Reliability {
        Reliability::new(&ReliabilityConfig::default())
    }

    /// t0 + seconds helper.
    fn ts(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    fn storm(r: &mut Reliability, t0: Instant, start_s: u64, n: u32) {
        for i in 0..n {
            r.on_dropout(ts(t0, start_s + i as u64));
        }
    }

    #[test]
    fn grace_period_suppresses_tier1() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 10, 10); // heavy storm right after acquisition
        assert_eq!(r.poll(ts(t0, 30)), Action::None); // still in 120s grace
    }

    #[test]
    fn dropout_storm_after_grace_fires_tier1_once() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 5); // ≥5 within 60s window, after grace
        assert_eq!(r.poll(ts(t0, 135)), Action::Reacquire);
        // immediately after: events cleared + cooldown → no refire
        assert_eq!(r.poll(ts(t0, 136)), Action::None);
        assert_eq!(r.total_tier1, 1);
    }

    #[test]
    fn below_threshold_never_fires() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 4); // one short of threshold
        assert_eq!(r.poll(ts(t0, 135)), Action::None);
    }

    #[test]
    fn window_pruning_forgets_old_dropouts() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 4);
        // 5th dropout arrives 70s later — the first 4 are outside the window
        r.on_dropout(ts(t0, 200));
        assert_eq!(r.poll(ts(t0, 201)), Action::None);
        assert_eq!(r.recent_dropouts(ts(t0, 201)), 1);
    }

    #[test]
    fn tier1_cooldown_gates_refire() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 5);
        assert_eq!(r.poll(ts(t0, 135)), Action::Reacquire);
        r.on_tier1_result(true);
        r.on_acquired(ts(t0, 136)); // recovery restarts grace
        // new storm right away: suppressed by fresh grace
        storm(&mut r, t0, 140, 5);
        assert_eq!(r.poll(ts(t0, 145)), Action::None);
        // after grace expires (136+120=256) AND cooldown passed → fires again
        storm(&mut r, t0, 260, 5);
        assert_eq!(r.poll(ts(t0, 265)), Action::Reacquire);
    }

    #[test]
    fn two_failed_tier1_escalate_to_tier2() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 5);
        assert_eq!(r.poll(ts(t0, 135)), Action::Reacquire);
        r.on_tier1_result(false); // acquisition failed — no on_acquired
        storm(&mut r, t0, 200, 5); // cooldown (60s) passed by t=195
        assert_eq!(r.poll(ts(t0, 205)), Action::Reacquire);
        r.on_tier1_result(false);
        // streak = 2 → escalate regardless of dropout state
        assert_eq!(r.poll(ts(t0, 206)), Action::Reconnect);
        assert_eq!(r.total_tier2, 1);
        // tier2 cooldown (300s) suppresses immediate repeat even if tier1 keeps failing
        r.on_tier1_result(false);
        r.on_tier1_result(false);
        assert_eq!(r.poll(ts(t0, 210)), Action::None);
        assert_eq!(r.poll(ts(t0, 520)), Action::Reconnect);
    }

    #[test]
    fn successful_tier1_resets_escalation_streak() {
        let t0 = Instant::now();
        let mut r = machine();
        r.on_acquired(t0);
        storm(&mut r, t0, 130, 5);
        assert_eq!(r.poll(ts(t0, 135)), Action::Reacquire);
        r.on_tier1_result(false);
        r.on_tier1_result(true); // second attempt succeeded
        assert_eq!(r.poll(ts(t0, 300)), Action::None); // no escalation
    }
}
