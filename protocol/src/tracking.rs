//! Timestamped external-source tracking, independent of any input device or runtime.
use crate::{
    timeline::{Anchor, Timeline},
    *,
};
/// Recent samples since the last discontinuity used for rate and position fitting.
const WINDOW: usize = 64;
const WINDOW_NS: u64 = 1_000_000_000;
/// Rate evidence older than a source gap describes a different trajectory.
const GAP_NS: u64 = 500_000_000;
/// Minimum span before a fit may refine an inferred rate or smooth position.
const FIT_SPAN_NS: u64 = 100_000_000;
/// Residuals below this never count as timestamp outliers.
const OUTLIER_FLOOR_NS: f64 = 10_000.;
pub struct Tracker {
    last: Option<SourceSample>,
    /// Ring of (timestamp, Q32 position), oldest first from `start`.
    history: [(u64, i128); WINDOW],
    start: usize,
    len: usize,
    candidate: Option<(f64, u8)>,
    jumps: u8,
    jumps_sign: i8,
}
impl Default for Tracker {
    fn default() -> Self {
        Self {
            last: None,
            history: [(0, 0); WINDOW],
            start: 0,
            len: 0,
            candidate: None,
            jumps: 0,
            jumps_sign: 0,
        }
    }
}
impl Tracker {
    fn sample_at(&self, i: usize) -> (u64, i128) {
        self.history[(self.start + i) % WINDOW]
    }
    fn push(&mut self, timestamp: u64, position: i128) {
        while self.len > 0 && timestamp - self.sample_at(0).0 > WINDOW_NS {
            self.start = (self.start + 1) % WINDOW;
            self.len -= 1;
        }
        if self.len == WINDOW {
            self.start = (self.start + 1) % WINDOW;
            self.len -= 1;
        }
        self.history[(self.start + self.len) % WINDOW] = (timestamp, position);
        self.len += 1;
    }
    /// Least-squares line through the window, relative to the newest sample: (frames at the
    /// newest timestamp, frames per second). Requires the minimum fit span.
    fn fit(&self, fps: f64) -> Option<(f64, f64)> {
        let (t0, p0) = self.sample_at(self.len.checked_sub(1)?);
        if self.len < 4 || t0 - self.sample_at(0).0 < FIT_SPAN_NS {
            return None;
        }
        let point = |i| {
            let (t, p) = self.sample_at(i);
            ((t as f64 - t0 as f64) / 1e9, (p - p0) as f64 / 4294967296.)
        };
        let line = |keep: &dyn Fn(f64, f64) -> bool| {
            let (mut n, mut x, mut y) = (0., 0., 0.);
            for i in 0..self.len {
                let (a, b) = point(i);
                if keep(a, b) {
                    n += 1.;
                    x += a;
                    y += b;
                }
            }
            let (x, y) = (x / n, y / n);
            let (mut xx, mut xy) = (0., 0.);
            for i in 0..self.len {
                let (a, b) = point(i);
                if keep(a, b) {
                    xx += (a - x) * (a - x);
                    xy += (a - x) * (b - y);
                }
            }
            (n >= 3. && xx > 0.).then(|| {
                let slope = xy / xx;
                (y - slope * x, slope)
            })
        };
        let (c, m) = line(&|_, _| true)?;
        // One robust pass: a single late timestamp (a preempted sampler) would otherwise
        // shift every estimate while it remains in the window.
        let mut residuals = [0f64; WINDOW];
        for (i, r) in residuals[..self.len].iter_mut().enumerate() {
            let (a, b) = point(i);
            *r = (b - (c + m * a)).abs();
        }
        residuals[..self.len].sort_by(f64::total_cmp);
        // Six median absolute residuals is about four standard deviations for Gaussian noise.
        let limit = (6. * residuals[self.len / 2]).max(OUTLIER_FLOOR_NS / 1e9 * fps);
        line(&|a, b| (b - (c + m * a)).abs() <= limit).or(Some((c, m)))
    }
    /// Returns whether this input introduces an immediate discontinuity.
    pub fn sample(&mut self, t: &mut Timeline, s: SourceSample, now: u64) -> Result<bool, Error> {
        if t.source_kind != SourceKind::Tracked {
            return Err(Error::Invalid("source is not tracked"));
        }
        if s.timestamp_ns > now || self.last.is_some_and(|p| s.timestamp_ns <= p.timestamp_ns) {
            return Err(Error::Invalid(
                "source timestamps must increase and not be in the future",
            ));
        }
        if self
            .last
            .is_some_and(|p| s.timestamp_ns - p.timestamp_ns > GAP_NS)
        {
            self.len = 0;
            self.candidate = None;
        }
        // Infer speed over the longest available baseline: consecutive samples turn small
        // timestamp jitter into large apparent speed changes at high sample rates.
        let inferred = (self.len > 0)
            .then(|| self.sample_at(0))
            .and_then(|(time, position)| {
                let elapsed = (s.timestamp_ns - time) as f64 / 1e9;
                (elapsed >= 0.001).then(|| {
                    ((s.position.fixed() - position) as f64 / 4294967296.)
                        / (elapsed * t.format.fps())
                })
            });
        let speed = s
            .rate_hint
            .map(Rate::as_f64)
            .or(inferred)
            .unwrap_or(t.anchor.rate.as_f64())
            .clamp(-64., 64.);
        let error = (s.position.fixed() - t.anchor.at(s.timestamp_ns, t.format).fixed()) as f64
            / 4294967296.;
        let mut changed = s.discontinuity || self.last.is_none();
        if error.abs() > 1. {
            let sign = if error > 0. { 1 } else { -1 };
            if sign != self.jumps_sign {
                self.jumps = 0;
            }
            self.jumps_sign = sign;
            self.jumps += 1;
        } else {
            self.jumps = 0;
        }
        if self.jumps >= 3 {
            changed = true;
            self.jumps = 0;
        }
        let mut rate = t.anchor.rate;
        if let Some(hint) = s.rate_hint
            && hint != rate
        {
            rate = hint;
            changed = true;
        } else if s.rate_hint.is_none() && (speed - rate.as_f64()).abs() > 0.02 {
            let count = self
                .candidate
                .filter(|(v, _)| (v - speed).abs() < 0.05)
                .map_or(1, |(_, n)| n + 1);
            self.candidate = Some((speed, count));
            if count >= 3 {
                rate = quantize(speed)?;
                changed = true;
                self.candidate = None;
            }
        } else {
            self.candidate = None;
        }
        if changed {
            self.len = 0;
        }
        self.push(s.timestamp_ns, s.position.fixed());
        let fit = if changed {
            None
        } else {
            self.fit(t.format.fps())
        };
        // Small inferred speed refinements stay continuous; only confirmed large changes,
        // explicit hints, and jumps are discontinuities.
        if let Some((_, slope)) = fit
            && s.rate_hint.is_none()
            && self.candidate.is_none()
        {
            rate = quantize(slope / t.format.fps())?;
        }
        let position = match fit {
            _ if changed => s.position,
            Some((intercept, _)) => {
                Position::from_fixed(s.position.fixed() + (intercept * 4294967296.) as i128)
            }
            None => Position::from_fixed(
                t.anchor.at(s.timestamp_ns, t.format).fixed()
                    + (error.clamp(-0.1, 0.1) * 0.2 * 4294967296.) as i128,
            ),
        };
        t.anchor = Anchor {
            time_ns: s.timestamp_ns,
            position,
            rate,
        };
        t.source_health = SourceHealth::Healthy;
        if changed {
            self.jumps = 0;
            self.jumps_sign = 0;
            self.candidate = None;
            t.discontinuity += 1;
            t.scheduled_len = 0;
        }
        self.last = Some(s);
        Ok(changed)
    }
    pub fn health(&self, now: u64, timeout: u64) -> SourceHealth {
        if self
            .last
            .is_none_or(|s| now.saturating_sub(s.timestamp_ns) > timeout)
        {
            SourceHealth::Degraded
        } else {
            SourceHealth::Healthy
        }
    }
}

/// Finest representable rational rate: about 1 ppm at unity, coarser only above 1x.
fn quantize(speed: f64) -> Result<Rate, Error> {
    let speed = speed.clamp(-64., 64.);
    let denominator = (1_000_000. / speed.abs().ceil().max(1.)) as u32;
    Rate::new((speed * denominator as f64).round() as i32, denominator)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jitter_rate_estimation_loss_and_changes() {
        let mut t = Timeline {
            source_kind: SourceKind::Tracked,
            ..Default::default()
        };
        let mut tracker = Tracker::default();
        for i in 1..=100 {
            let now = i * 100_000_000;
            let jitter = if i % 2 == 0 { 0.003 } else { -0.003 };
            let position = Position::from_fixed(((i as f64 * 3. + jitter) * 4294967296.) as i128);
            tracker
                .sample(
                    &mut t,
                    SourceSample {
                        timestamp_ns: now,
                        position,
                        rate_hint: None,
                        discontinuity: false,
                    },
                    now,
                )
                .unwrap();
        }
        assert!((t.anchor.rate.as_f64() - 1.).abs() < 0.01);
        assert_eq!(
            tracker.health(11_000_000_000, 500_000_000),
            SourceHealth::Degraded
        );
        assert_eq!(t.source_kind, SourceKind::Tracked);
        assert!(t.anchor.at(11_000_000_000, t.format) > t.anchor.position);
        assert!(
            tracker
                .sample(
                    &mut t,
                    SourceSample {
                        timestamp_ns: 10_000_000_000,
                        position: Position::ZERO,
                        rate_hint: None,
                        discontinuity: false
                    },
                    11_000_000_000
                )
                .is_err()
        );
        let changed = tracker
            .sample(
                &mut t,
                SourceSample {
                    timestamp_ns: 11_000_000_000,
                    position: Position::from_frames(900),
                    rate_hint: Some(Rate::new(-1, 1).unwrap()),
                    discontinuity: true,
                },
                11_000_000_000,
            )
            .unwrap();
        assert!(changed);
        assert_eq!(t.anchor.rate, Rate::new(-1, 1).unwrap());
    }
}

#[cfg(test)]
mod stability_tests {
    use super::*;
    #[test]
    fn tiny_explicit_rate_hints_are_not_discarded() {
        let mut tracker = Tracker::default();
        let mut timeline = Timeline {
            source_kind: SourceKind::Tracked,
            ..Default::default()
        };
        let hint = Rate::new(1, 100000).unwrap();
        tracker
            .sample(
                &mut timeline,
                SourceSample {
                    timestamp_ns: 1,
                    position: Position::ZERO,
                    rate_hint: Some(hint),
                    discontinuity: false,
                },
                1,
            )
            .unwrap();
        assert_eq!(timeline.anchor.rate, hint);
    }
    #[test]
    fn alternating_source_errors_do_not_confirm_a_jump() {
        let mut tracker = Tracker::default();
        let mut timeline = Timeline {
            source_kind: SourceKind::Tracked,
            ..Default::default()
        };
        tracker
            .sample(
                &mut timeline,
                SourceSample {
                    timestamp_ns: 0,
                    position: Position::ZERO,
                    rate_hint: Some(Rate::PAUSED),
                    discontinuity: false,
                },
                0,
            )
            .unwrap();
        for i in 1..10 {
            let now = i * 1_000_000_000;
            assert!(
                !tracker
                    .sample(
                        &mut timeline,
                        SourceSample {
                            timestamp_ns: now,
                            position: Position::from_frames(if i % 2 == 0 { 2 } else { -2 }),
                            rate_hint: Some(Rate::PAUSED),
                            discontinuity: false
                        },
                        now
                    )
                    .unwrap()
            );
        }
        assert_eq!(timeline.discontinuity, 1);
        for i in 10..13 {
            let now = i * 1_000_000_000;
            let changed = tracker
                .sample(
                    &mut timeline,
                    SourceSample {
                        timestamp_ns: now,
                        position: Position::from_frames(2),
                        rate_hint: Some(Rate::PAUSED),
                        discontinuity: false,
                    },
                    now,
                )
                .unwrap();
            assert_eq!(changed, i == 12);
        }
    }
}
