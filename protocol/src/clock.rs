//! Network-independent affine monotonic clock estimator.
use std::collections::VecDeque;
/// Maximum complete exchange duration, including leader application residence time.
pub const MAX_EXCHANGE_NS: u64 = 500_000_000;
const HISTORY_NS: f64 = 32_000_000_000.;
// Covers relative oscillator changes within ±500 ppm and a fitted drift within that range.
const HOLDOVER_GROWTH: f64 = 0.001;
/// Largest delay-floor change between halves of a recovery run that still counts as settled.
const SETTLED_FLOOR_NS: f64 = 2_000_000.;
/// Timestamp floor for delay weighting.
const WEIGHT_FLOOR_NS: f64 = 50_000.;
/// The delay spread of this many lowest-delay samples sets the weighting scale, so that
/// roughly this many observations always carry the fit.
const WEIGHT_SUPPORT: usize = 4;
/// Four monotonic nanosecond timestamps in the NTP convention.
#[derive(Clone, Copy, Debug)]
pub struct Exchange {
    pub t1: u64,
    pub t2: u64,
    pub t3: u64,
    pub t4: u64,
}
#[derive(Clone, Copy, Debug)]
struct Sample {
    local: f64,
    offset: f64,
    delay: f64,
    duration: f64,
}
/// Feasible offset interval under nonnegative path delay, ±500ppm relative clock
/// motion and a 100µs timestamp allowance. An empty intersection remains inconsistent
/// until its conflicting samples expire or the estimator reacquires.
#[derive(Clone, Copy, Debug)]
pub struct OffsetEvidence {
    pub reference_ns: u64,
    pub lower_ns: f64,
    pub upper_ns: f64,
    pub samples: u32,
}
impl OffsetEvidence {
    pub fn consistent(self) -> bool {
        self.lower_ns <= self.upper_ns
    }
    pub fn at(self, local: u64) -> Self {
        if !self.consistent() {
            return self;
        }
        let growth = local.abs_diff(self.reference_ns) as f64 * 0.0005;
        Self {
            reference_ns: local,
            lower_ns: self.lower_ns - growth,
            upper_ns: self.upper_ns + growth,
            ..self
        }
    }
}
/// Bounded diagnostic record. Timestamps are application observations, not radio/kernel times.
#[derive(Clone, Copy, Debug)]
pub struct ClockObservation {
    pub exchange: Exchange,
    pub accepted: bool,
    /// Time from the request's wire t1 until publication returned. Native publication
    /// includes the socket send; when consistent, `exchange.t1` is this departure instant.
    pub publication_ns: Option<u64>,
    /// Local receipt-to-processing-entry delay; t4 remains the application receive timestamp.
    pub processing_delay_ns: u64,
    pub mapping: ClockMapping,
}
/// Leader time = local time + offset + (local - reference) * drift.
#[derive(Clone, Copy, Debug)]
pub struct ClockMapping {
    pub reference_ns: u64,
    pub offset_ns: f64,
    pub drift: f64,
    pub uncertainty_ns: f64,
    pub last_sample_ns: u64,
    pub converged: bool,
    /// Accepted observations in this acquisition, including promoted recovery samples.
    /// Resets on replacement or complete expiry of the fit history, not partial eviction.
    /// Saturates at u64::MAX; distinct from the bounded evidence/trace sample counts.
    pub accepted_observations: u64,
    /// Intersection of accepted exchange constraints, independent of the point fit.
    pub evidence: Option<OffsetEvidence>,
}
impl Default for ClockMapping {
    fn default() -> Self {
        Self {
            reference_ns: 0,
            offset_ns: 0.,
            drift: 0.,
            uncertainty_ns: f64::INFINITY,
            last_sample_ns: 0,
            converged: false,
            accepted_observations: 0,
            evidence: None,
        }
    }
}
impl ClockMapping {
    pub fn leader_time(self, local: u64) -> u64 {
        (local as f64 + self.offset_at(local)).clamp(0., i64::MAX as f64) as u64
    }
    pub fn offset_at(self, local: u64) -> f64 {
        self.offset_ns + (local as f64 - self.reference_ns as f64) * self.drift
    }
    /// Conservative unknown asymmetry plus scheduling floor and 1000 ppm holdover growth.
    pub fn uncertainty_at(self, local: u64) -> f64 {
        self.uncertainty_ns + local.saturating_sub(self.last_sample_ns) as f64 * HOLDOVER_GROWTH
    }
}
/// Bounded low-delay regression with quarantined recovery from persistent path/clock changes.
#[derive(Debug, Default)]
pub struct ClockEstimator {
    samples: VecDeque<Sample>,
    mapping: ClockMapping,
    // Quarantined observations cannot move the mapping until a consistent run is confirmed.
    recovery: VecDeque<Sample>,
    last_observed: Option<f64>,
    trace: VecDeque<ClockObservation>,
}
impl ClockEstimator {
    pub fn recovering(&self) -> bool {
        !self.recovery.is_empty()
    }
    pub fn mapping(&self) -> ClockMapping {
        self.mapping
    }
    /// Last 128 observations, including rejected exchanges, in arrival order.
    /// Replay their exchanges into a fresh estimator; reconnect alone does not reset it.
    pub fn trace(&self) -> impl Iterator<Item = &ClockObservation> {
        self.trace.iter()
    }
    pub fn observe(&mut self, e: Exchange) -> bool {
        self.observe_timed(e, None, e.t4)
    }
    pub fn observe_timed(
        &mut self,
        e: Exchange,
        publication_ns: Option<u64>,
        processed_ns: u64,
    ) -> bool {
        let accepted = self.observe_inner(e);
        if self.trace.len() == 128 {
            self.trace.pop_front();
        }
        self.trace.push_back(ClockObservation {
            exchange: e,
            accepted,
            publication_ns,
            processing_delay_ns: processed_ns.saturating_sub(e.t4),
            mapping: self.mapping,
        });
        accepted
    }
    fn observe_inner(&mut self, e: Exchange) -> bool {
        if e.t4 < e.t1
            || e.t3 < e.t2
            || [e.t1, e.t2, e.t3, e.t4]
                .iter()
                .any(|x| *x > i64::MAX as u64)
        {
            return false;
        }
        if e.t4 - e.t1 > MAX_EXCHANGE_NS {
            return false;
        }
        let delay = (e.t4 - e.t1) as i128 - (e.t3 - e.t2) as i128;
        if !(0..=500_000_000).contains(&delay) {
            return false;
        }
        let local = e.t1 as f64 + (e.t4 - e.t1) as f64 / 2.;
        if self.last_observed.is_some_and(|last| local <= last) {
            return false;
        }
        self.last_observed = Some(local);
        let offset = ((e.t2 as i128 - e.t1 as i128) + (e.t3 as i128 - e.t4 as i128)) as f64 / 2.;
        // Keep a 32-second history so acquisition data cannot dominate long-running drift.
        while self
            .samples
            .front()
            .is_some_and(|s| local - s.local > HISTORY_NS)
        {
            self.samples.pop_front();
        }
        let min = self
            .samples
            .iter()
            .map(|s| s.delay)
            .fold(f64::INFINITY, f64::min);
        let sample = Sample {
            local,
            offset,
            delay: delay as f64,
            duration: (e.t4 - e.t1) as f64,
        };
        let rejected_delay = self.samples.len() >= 8 && delay as f64 > min + 20_000_000.;
        let rejected_offset = !self.samples.is_empty()
            && self.mapping.converged
            && (offset - self.mapping.offset_at(local as u64)).abs()
                > self.mapping.uncertainty_at(e.t4) + delay as f64 / 2. + 5_000_000.;
        let accepted_observations;
        if rejected_delay || rejected_offset {
            // A single spike never changes the map. Eight similar observations spanning
            // at least 500ms establish a changed path/clock regime, not an isolated outlier.
            let consistent = self.recovery.front().is_none_or(|first| {
                let phase = offset - first.offset - (local - first.local) * self.mapping.drift;
                phase.abs() <= (first.delay / 4.).max(2_000_000.)
                    && (sample.delay - first.delay).abs() <= (first.delay / 4.).max(5_000_000.)
                    && local - first.local <= 5_000_000_000.
            });
            if !consistent {
                self.recovery.clear();
            }
            self.recovery.push_back(sample);
            while self.recovery.len() > 64 {
                self.recovery.pop_front();
            }
            let first = self.recovery.front().unwrap().local;
            let half = self.recovery.len() / 2;
            let floor = |run: &mut dyn Iterator<Item = &Sample>| {
                run.map(|s| s.delay).fold(f64::INFINITY, f64::min)
            };
            let early = floor(&mut self.recovery.iter().take(half));
            let late = floor(&mut self.recovery.iter().skip(half));
            // A clock step at the usual delay is adopted quickly. An elevated delay is either
            // a changed path, which settles at a new floor, or a filling queue (bufferbloat),
            // which keeps raising it. Adopting a still-rising run would discard the low-delay
            // history the queue returns to and follow its growing asymmetry, so require a
            // longer run whose floor has stopped moving; jitter hides a ramp over short spans.
            let elevated = early.min(late) > min + 20_000_000.;
            let required = if elevated { 2_000_000_000. } else { 500_000_000. };
            if self.recovery.len() < 8
                || local - first < required
                || (elevated && (late - early).abs() > SETTLED_FLOOR_NS)
            {
                return false;
            }
            self.samples.clear();
            self.samples.append(&mut self.recovery);
            accepted_observations = self.samples.len() as u64;
        } else {
            self.recovery.clear();
            accepted_observations = if self.samples.is_empty() {
                1
            } else {
                self.mapping.accepted_observations.saturating_add(1)
            };
            self.samples.push_back(sample);
        }
        // At the fast cadence 128 samples span only 6.4 s. Beyond the newest 64, evict the
        // highest-delay sample instead of the oldest, so rare low-delay observations (a queue
        // that drains between bursts) survive for the whole history window.
        while self.samples.len() > 128 {
            let worst = (0..self.samples.len() - 64)
                .max_by(|a, b| self.samples[*a].delay.total_cmp(&self.samples[*b].delay))
                .unwrap();
            self.samples.remove(worst);
        }
        let mut selected: Vec<_> = self.samples.iter().copied().collect();
        selected.sort_by(|a, b| {
            a.delay
                .total_cmp(&b.delay)
                .then_with(|| b.local.total_cmp(&a.local))
        });
        selected.truncate((selected.len() / 2).max(4).min(selected.len()));
        // Extra delay above the path minimum bounds a sample's possible queueing asymmetry
        // by half that excess. Weight accordingly, floored by timestamp resolution, so one
        // queued direction cannot bias the fit the way an unweighted lower half does.
        let floor = selected[0].delay;
        let scale = ((selected[(WEIGHT_SUPPORT - 1).min(selected.len() - 1)].delay - floor) / 2.)
            .max(WEIGHT_FLOOR_NS);
        let weight = |s: &Sample| {
            let excess = (s.delay - floor) / 2.;
            1. / (scale * scale + excess * excess)
        };
        let total = selected.iter().map(weight).sum::<f64>();
        let x = selected
            .iter()
            .map(|s| weight(s) * (s.local - local))
            .sum::<f64>()
            / total;
        let y = selected.iter().map(|s| weight(s) * s.offset).sum::<f64>() / total;
        let xx = selected
            .iter()
            .map(|s| weight(s) * (s.local - local - x).powi(2))
            .sum::<f64>();
        let xy = selected
            .iter()
            .map(|s| weight(s) * (s.local - local - x) * (s.offset - y))
            .sum::<f64>();
        let span = self.samples.back().unwrap().local - self.samples.front().unwrap().local;
        let support_start = selected
            .iter()
            .map(|s| s.local)
            .fold(f64::INFINITY, f64::min);
        let support_end = selected
            .iter()
            .map(|s| s.local)
            .fold(f64::NEG_INFINITY, f64::max);
        // Use the time span of observations actually used by the fit, not discarded samples.
        // A slope beyond the ±500 ppm oscillator bound is not drift: delay that ramps with
        // time (a filling queue) moves offsets the same way. Keep the previous drift instead
        // of extrapolating a clamped artifact.
        let drift = if selected.len() >= 8
            && support_end - support_start >= 2_000_000_000.
            && xx > 0.
            && (xy / xx).abs() <= 0.0005
        {
            xy / xx
        } else {
            self.mapping.drift
        };
        let offset = y - drift * x;
        let residual = selected
            .iter()
            .map(|s| (s.offset - (offset + drift * (s.local - local))).abs())
            .fold(0., f64::max);
        // Offset at each midpoint lies in its send/receive interval, widened for
        // possible ±500ppm clock motion during the exchange. Propagate every constraint
        // to receipt time. Empty intersections are evidence of violated assumptions,
        // not a license to invent a precise offset or silently discard a constraint.
        let mut lower = f64::NEG_INFINITY;
        let mut upper = f64::INFINITY;
        for s in &self.samples {
            let radius = s.delay / 2.
                + 0.0005 * ((e.t4 as f64 - s.local).abs() + s.duration / 2.)
                + 100_000.;
            lower = lower.max(s.offset - radius);
            upper = upper.min(s.offset + radius);
        }
        self.mapping = ClockMapping {
            reference_ns: local as u64,
            offset_ns: offset,
            drift,
            // Each sample's delay bound belongs to that sample's epoch. Propagate
            // every bound to receipt time before choosing the tightest one; otherwise
            // old low-delay data can understate error as path asymmetry changes.
            uncertainty_ns: selected
                .iter()
                .map(|s| s.delay / 2. + (e.t4 as f64 - s.local).max(0.) * HOLDOVER_GROWTH)
                .fold(f64::INFINITY, f64::min)
                + residual
                + 100_000.,
            last_sample_ns: e.t4,
            converged: self.samples.len() >= 12 && span >= 500_000_000.,
            accepted_observations,
            evidence: Some(OffsetEvidence {
                reference_ns: e.t4,
                lower_ns: lower,
                upper_ns: upper,
                samples: self.samples.len() as u32,
            }),
        };
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn random(seed: &mut u64) -> f64 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (*seed >> 11) as f64 / (1u64 << 53) as f64
    }
    #[test]
    fn lan_simulation() {
        for initial_seed in [7, 19, 997] {
            for ppm in [-200., 0., 200.] {
                let mut seed = initial_seed;
                let mut c = ClockEstimator::default();
                let mut errors = Vec::new();
                let mut acquired = None;
                let mut next_probe = 1_000_000_000;
                let mut pending: VecDeque<Exchange> = VecDeque::new();
                let leader = |x: u64| (x as f64 * (1. + ppm / 1e6) + 5_000_000_000.) as u64;
                // Evaluate every 10 ms, including the intervals between probes, for 80 seconds.
                for i in 0..8000 {
                    let t = 1_000_000_000 + i * 10_000_000;
                    while pending.front().is_some_and(|e| e.t4 <= t) {
                        c.observe(pending.pop_front().unwrap());
                        if c.mapping.converged {
                            acquired.get_or_insert(t);
                        }
                    }
                    if t >= next_probe {
                        next_probe = t + if c.mapping.converged {
                            250_000_000
                        } else {
                            50_000_000
                        };
                        if random(&mut seed) >= 0.01 {
                            let a = (1_000_000. + random(&mut seed) * 4_000_000.) as u64;
                            let b = (1_000_000. + random(&mut seed) * 4_000_000.) as u64;
                            pending.push_back(Exchange {
                                t1: t,
                                t2: leader(t + a),
                                t3: leader(t + a + 20_000),
                                t4: t + a + b + 20_000,
                            });
                        }
                    }
                    if i >= 200 {
                        errors.push((c.mapping.leader_time(t) as f64 - leader(t) as f64).abs());
                    }
                }
                errors.sort_by(f64::total_cmp);
                let acquisition = acquired.unwrap() - 1_000_000_000;
                let p95 = errors[errors.len() * 95 / 100];
                eprintln!(
                    "seed={initial_seed} drift={ppm:+}ppm acquisition={:.2}s p95={:.3}ms",
                    acquisition as f64 / 1e9,
                    p95 / 1e6
                );
                assert!(acquisition < 2_000_000_000);
                assert!(p95 <= 2_000_000., "p95={p95}");
            }
        }
    }
    #[test]
    fn asymmetric_holdover_reordering_outliers() {
        let mut c = ClockEstimator::default();
        for i in 0..100 {
            let t = 1_000_000_000 + i * 50_000_000;
            assert!(c.observe(Exchange {
                t1: t,
                t2: t + 109_000_000,
                t3: t + 109_010_000,
                t4: t + 10_010_000
            }));
        }
        let m = c.mapping();
        assert!((m.offset_ns - 100_000_000.).abs() < m.uncertainty_ns);
        assert!(
            m.uncertainty_at(m.last_sample_ns + 60_000_000_000) > m.uncertainty_ns + 10_000_000.
        );
        assert!(!c.observe(Exchange {
            t1: 1,
            t2: 2,
            t3: 3,
            t4: 4
        }));
        assert!(!c.observe(Exchange {
            t1: 9,
            t2: 0,
            t3: 100,
            t4: 10
        }));
        assert!(!c.observe(Exchange {
            t1: 8_000_000_000,
            t2: 9_000_000_000,
            t3: 9_000_000_000,
            t4: 8_400_000_000
        }));
    }
}

#[cfg(test)]
mod observation_count_tests {
    use super::*;
    fn exchange(i: u64, offset: u64) -> Exchange {
        let t = 1_000_000_000 + i * 100_000_000;
        Exchange {
            t1: t,
            t2: t + offset + 1_000_000,
            t3: t + offset + 1_000_000,
            t4: t + 2_000_000,
        }
    }
    #[test]
    fn counts_acquisitions_not_trace_entries_or_retained_samples() {
        let mut c = ClockEstimator::default();
        for i in 0..200 {
            assert!(c.observe(exchange(i, 5_000_000_000)));
            assert_eq!(c.mapping().accepted_observations, i + 1);
        }
        assert_eq!(c.mapping().evidence.unwrap().samples, 128);
        assert_eq!(c.trace().count(), 128);
        assert!(!c.observe(exchange(199, 5_000_000_000)));
        assert_eq!(c.mapping().accepted_observations, 200);
        // Confirm a clock step. Earlier quarantined samples count only once promoted.
        for i in 200..207 {
            assert!(!c.observe(exchange(i, 5_100_000_000)));
            assert_eq!(c.mapping().accepted_observations, 200);
        }
        assert!(c.observe(exchange(207, 5_100_000_000)));
        assert_eq!(c.mapping().accepted_observations, 8);
        assert_eq!(c.trace().last().unwrap().mapping.accepted_observations, 8);
        assert!(c.observe(exchange(208, 5_100_000_000)));
        assert_eq!(c.mapping().accepted_observations, 9);
        // Partial time expiry retains the acquisition count; full expiry starts anew.
        assert!(c.observe(exchange(525, 5_100_000_000)));
        assert_eq!(c.mapping().accepted_observations, 10);
        assert!(c.observe(exchange(1000, 5_100_000_000)));
        assert_eq!(c.mapping().accepted_observations, 1);
        c.mapping.accepted_observations = u64::MAX;
        assert!(c.observe(exchange(1001, 5_100_000_000)));
        assert_eq!(c.mapping().accepted_observations, u64::MAX);
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn latency_step_outage_loss_reordering_and_recovery() {
        let mut c = ClockEstimator::default();
        let leader = |t: u64| (t as f64 * 1.0002 + 9_000_000_000.) as u64;
        for i in 0..2000 {
            let t = 1_000_000_000 + i * 50_000_000;
            if (200..400).contains(&i) || i % 97 == 0 {
                continue;
            }
            let d = if i < 600 { 2_000_000 } else { 30_000_000 };
            let accepted = c.observe(Exchange {
                t1: t,
                t2: leader(t + d),
                t3: leader(t + d + 10_000),
                t4: t + 2 * d + 10_000,
            });
            if i > 1400 {
                assert!(accepted);
                assert!((c.mapping().leader_time(t) as f64 - leader(t) as f64).abs() < 2_000_000.);
            }
            if accepted && i % 19 == 0 {
                assert!(!c.observe(Exchange {
                    t1: t - 100_000_000,
                    t2: leader(t - 98_000_000),
                    t3: leader(t - 98_000_000),
                    t4: t - 96_000_000
                }));
            }
        }
        assert!(c.mapping().converged);
        assert!(c.mapping().uncertainty_ns >= 30_000_000.);
    }
}

#[cfg(test)]
mod stability_tests {
    use super::*;
    fn exchange(t: u64, offset: u64, delay: u64) -> Exchange {
        Exchange {
            t1: t,
            t2: t + offset + delay,
            t3: t + offset + delay,
            t4: t + 2 * delay,
        }
    }
    fn locked() -> ClockEstimator {
        let mut c = ClockEstimator::default();
        for i in 0..80 {
            assert!(c.observe(exchange(
                1_000_000_000 + i * 250_000_000,
                100_000_000,
                1_000_000
            )));
        }
        c
    }
    #[test]
    fn sustained_latency_increase_recovers_within_three_seconds() {
        let mut c = locked();
        let mut first = None;
        for i in 0..12 {
            if c.observe(exchange(
                21_000_000_000 + i * 250_000_000,
                100_000_000,
                30_000_000,
            )) {
                first.get_or_insert(i);
            }
        }
        eprintln!("latency-step first accepted probe: {first:?}");
        assert!(first.is_some(), "new path rejected for over three seconds");
    }
    #[test]
    fn sustained_clock_step_recovers_but_one_outlier_does_not() {
        let mut c = locked();
        let before = c.mapping();
        assert!(!c.observe(exchange(21_000_000_000, 200_000_000, 1_000_000)));
        assert_eq!(before.offset_ns, c.mapping().offset_ns);
        assert!(c.observe(exchange(21_250_000_000, 100_000_000, 1_000_000)));
        let mut accepted = 0;
        for i in 0..24 {
            accepted += usize::from(c.observe(exchange(
                22_000_000_000 + i * 250_000_000,
                200_000_000,
                1_000_000,
            )));
        }
        eprintln!("clock-step accepted probes in six seconds: {accepted}");
        assert!(accepted >= 12);
        assert!((c.mapping().offset_ns - 200_000_000.).abs() < 1_000_000.);
    }
    #[test]
    fn server_stall_is_not_a_low_delay_observation() {
        let mut c = locked();
        assert!(!c.observe(Exchange {
            t1: 21_000_000_000,
            t2: 21_101_000_000,
            t3: 51_101_000_000,
            t4: 51_002_000_000
        }));
    }
}

#[cfg(test)]
mod confidence_tests {
    use super::*;
    #[test]
    fn short_baselines_do_not_turn_delay_jitter_into_drift() {
        let mut c = ClockEstimator::default();
        for i in 0..40 {
            let t = 1_000_000_000 + i * 50_000_000;
            assert!(c.observe(Exchange {
                t1: t,
                t2: t + 101_000_000 + i * 10_000,
                t3: t + 101_000_000 + i * 10_000,
                t4: t + 2_000_000
            }));
        }
        assert_eq!(
            c.mapping().drift,
            0.,
            "drift fitted from less than two seconds of selected observations"
        );
    }
    #[test]
    fn stale_fit_support_and_changed_oscillator_increase_uncertainty() {
        let mut c = ClockEstimator::default();
        for i in 0..100 {
            let t = 1_000_000_000 + i * 250_000_000;
            let d = if i < 80 { 1_000_000 } else { 10_000_000 };
            assert!(c.observe(Exchange {
                t1: t,
                t2: t + 100_000_000 + d,
                t3: t + 100_000_000 + d,
                t4: t + 2 * d
            }));
        }
        // Recent observations are accepted but excluded by the low-delay selection.
        assert!(c.mapping().uncertainty_ns > 3_000_000.);
        let m = ClockMapping {
            reference_ns: 1_000_000_000,
            last_sample_ns: 1_000_000_000,
            offset_ns: 0.,
            drift: 0.0005,
            uncertainty_ns: 0.,
            converged: true,
            accepted_observations: 0,
            evidence: None,
        };
        let t = 61_000_000_000;
        let true_time = 60_970_000_000.; // Relative oscillator changes from +500 to -500 ppm.
        assert!((m.leader_time(t) as f64 - true_time).abs() <= m.uncertainty_at(t));
    }
    #[test]
    fn alternating_outliers_and_reordered_candidates_never_reacquire() {
        let mut c = ClockEstimator::default();
        let e = |t, offset| Exchange {
            t1: t,
            t2: t + offset + 1_000_000,
            t3: t + offset + 1_000_000,
            t4: t + 2_000_000,
        };
        for i in 0..40 {
            c.observe(e(1_000_000_000 + i * 50_000_000, 100_000_000));
        }
        let before = c.mapping();
        for i in 0..40 {
            let t = 4_000_000_000 + i * 50_000_000;
            assert!(!c.observe(e(t, if i % 2 == 0 { 200_000_000 } else { 300_000_000 })));
            assert!(!c.observe(e(t - 25_000_000, 200_000_000)));
        }
        assert_eq!(c.mapping().last_sample_ns, before.last_sample_ns);
        assert_eq!(c.mapping().offset_ns, before.offset_ns);
    }
}

#[cfg(test)]
mod asymmetry_confidence_tests {
    use super::*;
    #[test]
    fn changing_asymmetry_cannot_borrow_an_old_samples_low_delay_bound() {
        let mut c = ClockEstimator::default();
        let mut now = 0;
        for i in 0..128 {
            let t = 1_000_000_000 + i * 250_000_000;
            let (up, down) = if i < 64 {
                (10_000_000, 10_000_000)
            } else {
                (2_000_000 + (i - 64) * 100_000, 100_000)
            };
            now = t + up + down;
            assert!(c.observe(Exchange {
                t1: t,
                t2: t + 100_000_000 + up,
                t3: t + 100_000_000 + up,
                t4: now
            }));
        }
        let m = c.mapping();
        let error = (m.leader_time(now) as f64 - (now + 100_000_000) as f64).abs();
        eprintln!(
            "changing asymmetry: error={:.3}ms uncertainty={:.3}ms",
            error / 1e6,
            m.uncertainty_at(now) / 1e6
        );
        assert!(error <= m.uncertainty_at(now));
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::*;
    #[test]
    fn intervals_cover_asymmetric_paths_and_drift_without_forcing_the_fit() {
        for ppm in [-500., 0., 500.] {
            let mut c = ClockEstimator::default();
            for i in 0..100u64 {
                let t = 1_000_000_000 + i * 250_000_000;
                let leader = |t: u64| (t as f64 * (1. + ppm / 1e6) + 5_000_000_000.) as u64;
                let e = Exchange {
                    t1: t,
                    t2: leader(t + 1_000_000),
                    t3: leader(t + 1_020_000),
                    t4: t + 9_020_000,
                };
                assert!(c.observe(e));
                let b = c.mapping().evidence.unwrap();
                let truth = leader(e.t4) as f64 - e.t4 as f64;
                assert!(b.consistent() && b.lower_ns <= truth && truth <= b.upper_ns);
                let future = e.t4 + 30_000_000_000;
                let b = b.at(future);
                let truth = leader(future) as f64 - future as f64;
                assert!(b.lower_ns <= truth && truth <= b.upper_ns);
            }
        }
    }
    #[test]
    fn contradictions_are_visible_and_trace_replays_rejections() {
        let mut c = ClockEstimator::default();
        for i in 0..20 {
            let t = 1_000_000_000 + i * 50_000_000;
            let offset = if i < 10 { 5_000_000 } else { 8_000_000 };
            c.observe_timed(
                Exchange {
                    t1: t,
                    t2: t + offset + 100_000,
                    t3: t + offset + 100_000,
                    t4: t + 200_000,
                },
                Some(40_000),
                t + 250_000,
            );
        }
        assert!(!c.mapping().evidence.unwrap().consistent());
        let mut replay = ClockEstimator::default();
        for o in c.trace() {
            assert_eq!(o.accepted, replay.observe(o.exchange));
            assert_eq!(o.mapping.offset_ns, replay.mapping().offset_ns);
            assert_eq!(o.publication_ns, Some(40_000));
            assert_eq!(o.processing_delay_ns, 50_000);
        }
        let invalid = Exchange {
            t1: 30,
            t2: 20,
            t3: 10,
            t4: 40,
        };
        for _ in 0..200 {
            assert!(!c.observe(invalid));
        }
        assert_eq!(c.trace().count(), 128);
        assert!(!c.trace().last().unwrap().accepted);
    }
}
