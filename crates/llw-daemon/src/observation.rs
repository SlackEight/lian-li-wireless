//! Persistence filter turning raw GetDev readbacks into dropout observations
//! (M2a experiment: single-poll blips are healthy-channel background noise;
//! only consecutive-poll readback loss while commanded is link trouble).

/// Per-device streak tracker. Feed it every GetDev poll result.
#[derive(Debug, Default)]
pub struct DropoutFilter {
    streak: u32,
}

impl DropoutFilter {
    /// `commanded`: we have nonzero desired PWM for at least one active slot.
    /// `readback_zero`: every active fan slot read back 0.
    /// Returns true when THIS poll should be reported as a dropout
    /// observation (i.e. streak has reached `threshold`).
    pub fn observe(&mut self, commanded: bool, readback_zero: bool, threshold: u32) -> bool {
        if commanded && readback_zero {
            self.streak = self.streak.saturating_add(1);
            self.streak >= threshold.max(1)
        } else {
            self.streak = 0;
            false
        }
    }

    pub fn streak(&self) -> u32 {
        self.streak
    }
}


/// A judged fan surge: a zero-readback window (plus the fan-inertia tail
/// after recovery) whose peak RPM ran away from the healthy baseline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surge {
    pub peak_rpm: u16,
    pub baseline_rpm: u16,
}

/// How many post-recovery polls the tracker keeps peaking before judging.
/// The 2026-07-17 4Hz captures show the physical peak lands ~3s AFTER
/// readback recovers (fan inertia) and decays within ~5s; 8 one-second polls
/// cover it with margin.
pub const SURGE_TAIL_POLLS: u8 = 8;

/// Peak ≥ ⅓ + 100 rpm above baseline = surge. Wide enough to ignore wobble
/// (871 vs 730 observed), tight enough to catch runaway-to-full (~2100).
fn is_surge(baseline: u16, peak: u16) -> bool {
    peak as u32 > baseline as u32 + baseline as u32 / 3 + 100
}

#[derive(Debug, Default)]
enum SurgePhase {
    #[default]
    Idle,
    /// Inside a zero-readback window.
    InWindow { peak: u16 },
    /// Window closed; still watching for the inertia peak.
    Tail { peak: u16, polls_left: u8 },
}

/// Per-device surge watchdog. Feed it every GetDev poll; it reports a
/// [`Surge`] when a dropout window (plus tail) peaked well above the healthy
/// baseline. The physical surge outlives the readback window (fan inertia),
/// so judgment happens SURGE_TAIL_POLLS after recovery, and a re-opened
/// window during the tail merges into the same surge episode.
///
/// Caller contract: call `reset()` whenever the commanded PWM changes
/// materially — a legitimate curve move looks like a surge otherwise.
#[derive(Debug, Default)]
pub struct SurgeTracker {
    phase: SurgePhase,
    baseline: u16,
}

impl SurgeTracker {
    /// `commanded`/`readback_zero` as in [`DropoutFilter::observe`];
    /// `max_rpm` = the record's highest active-fan RPM this poll.
    pub fn observe(&mut self, commanded: bool, readback_zero: bool, max_rpm: u16) -> Option<Surge> {
        let in_window = commanded && readback_zero;
        match &mut self.phase {
            SurgePhase::Idle => {
                if in_window {
                    self.phase = SurgePhase::InWindow { peak: max_rpm };
                } else {
                    self.baseline = max_rpm;
                }
                None
            }
            SurgePhase::InWindow { peak } => {
                let peak = (*peak).max(max_rpm);
                self.phase = if in_window {
                    SurgePhase::InWindow { peak }
                } else {
                    SurgePhase::Tail { peak, polls_left: SURGE_TAIL_POLLS }
                };
                None
            }
            SurgePhase::Tail { peak, polls_left } => {
                let peak = (*peak).max(max_rpm);
                if in_window {
                    // interference struck again mid-tail — same episode
                    self.phase = SurgePhase::InWindow { peak };
                    return None;
                }
                let left = *polls_left - 1;
                if left == 0 {
                    let baseline = self.baseline;
                    self.phase = SurgePhase::Idle;
                    // decayed rpm becomes the fresh baseline next poll
                    return is_surge(baseline, peak).then_some(Surge {
                        peak_rpm: peak,
                        baseline_rpm: baseline,
                    });
                }
                self.phase = SurgePhase::Tail { peak, polls_left: left };
                None
            }
        }
    }

    /// Forget any in-flight episode (commanded PWM changed — curve move).
    pub fn reset(&mut self) {
        self.phase = SurgePhase::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_poll_blips_are_filtered() {
        // Replay of the experiment's healthy run 1: singles at 51.6s/65.2s
        // and one 3-poll burst — with threshold 2, only the burst's polls
        // 2 and 3 count (2 observations in the whole run).
        let mut f = DropoutFilter::default();
        let mut observations = 0;
        // single blip
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, false, 2) { observations += 1; }
        // 3-poll burst
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, false, 2) { observations += 1; }
        // another single
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, false, 2) { observations += 1; }
        assert_eq!(observations, 2);
    }

    #[test]
    fn sustained_loss_accumulates_fast() {
        // June-style sustained loss: every poll past the threshold reports.
        let mut f = DropoutFilter::default();
        let count = (0..10).filter(|_| f.observe(true, true, 2)).count();
        assert_eq!(count, 9); // polls 2..=10
        assert_eq!(f.streak(), 10);
    }

    #[test]
    fn uncommanded_never_observes() {
        let mut f = DropoutFilter::default();
        assert!(!f.observe(false, true, 2));
        assert!(!f.observe(false, true, 2));
        assert_eq!(f.streak(), 0);
    }

    #[test]
    fn recovery_resets_streak() {
        let mut f = DropoutFilter::default();
        f.observe(true, true, 2);
        f.observe(true, true, 2);
        assert_eq!(f.streak(), 2);
        f.observe(true, false, 2);
        assert_eq!(f.streak(), 0);
    }

    #[test]
    fn surge_peaks_after_window_closes_is_caught() {
        // The 2026-07-17 signature: rpm is still low while readback is
        // zeroed; the physical peak arrives AFTER recovery (fan inertia).
        let mut t = SurgeTracker::default();
        assert_eq!(t.observe(true, false, 730), None); // baseline
        assert_eq!(t.observe(true, true, 725), None); // window opens, rpm low
        assert_eq!(t.observe(true, true, 728), None);
        assert_eq!(t.observe(true, false, 1567), None); // recovered; tail begins
        assert_eq!(t.observe(true, false, 1900), None); // inertia peak
        for _ in 0..(SURGE_TAIL_POLLS - 2) {
            assert_eq!(t.observe(true, false, 900), None);
        }
        let surge = t.observe(true, false, 760).expect("tail end must judge");
        assert_eq!(surge, Surge { peak_rpm: 1900, baseline_rpm: 730 });
        // fresh baseline after the episode
        assert_eq!(t.observe(true, false, 735), None);
        assert_eq!(t.observe(true, true, 730), None);
    }

    #[test]
    fn quiet_window_is_not_a_surge() {
        let mut t = SurgeTracker::default();
        t.observe(true, false, 730);
        t.observe(true, true, 735);
        t.observe(true, true, 871); // wobble
        t.observe(true, false, 736);
        for _ in 0..(SURGE_TAIL_POLLS - 1) {
            assert_eq!(t.observe(true, false, 733), None);
        }
        assert_eq!(t.observe(true, false, 731), None, "871 vs 730 is wobble");
    }

    #[test]
    fn reopened_window_merges_into_one_episode() {
        let mut t = SurgeTracker::default();
        t.observe(true, false, 730);
        t.observe(true, true, 725);
        t.observe(true, false, 1500); // tail
        t.observe(true, true, 1800); // struck again mid-tail — merge
        t.observe(true, false, 2000); // tail restarts
        let mut result = None;
        for _ in 0..SURGE_TAIL_POLLS {
            result = t.observe(true, false, 800);
            if result.is_some() {
                break;
            }
        }
        let surge = result.expect("merged episode must judge once");
        assert_eq!(surge.peak_rpm, 2000);
        assert_eq!(surge.baseline_rpm, 730);
    }

    #[test]
    fn reset_aborts_the_episode() {
        let mut t = SurgeTracker::default();
        t.observe(true, false, 730);
        t.observe(true, true, 725);
        t.observe(true, false, 2000); // would be a surge...
        t.reset(); // ...but the commanded PWM changed (curve move)
        for _ in 0..=SURGE_TAIL_POLLS {
            assert_eq!(t.observe(true, false, 2000), None);
        }
    }
}
