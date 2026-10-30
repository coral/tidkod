//! Hint-less tracked sources across sample cadences, timestamp jitter, irregular intervals,
//! dropped samples and gaps. Simulated inputs; not a device accuracy claim.
use tidkod_protocol::{Position, SourceKind, SourceSample, timeline::Timeline, tracking::Tracker};

fn random(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (*seed >> 11) as f64 / (1u64 << 53) as f64
}

struct Outcome {
    /// Discontinuities after initial rate acquisition (the first 500 ms).
    late_discontinuities: u64,
    p99_ms: f64,
    rate: f64,
}

/// `irregular` scales each interval uniformly within ±that fraction; `gap` removes samples
/// during (start, end) seconds while the source keeps moving.
fn run(hz: f64, jitter_ns: f64, rate: f64, irregular: f64, gap: Option<(f64, f64)>) -> Outcome {
    let mut seed = 7;
    let mut t = Timeline {
        source_kind: SourceKind::Tracked,
        ..Default::default()
    };
    let mut tracker = Tracker::default();
    let fps = t.format.fps();
    let mut errors = Vec::new();
    let mut late_discontinuities = 0;
    let mut truth = 1_000_000_000u64;
    let mut last = 0;
    while truth < 61_000_000_000 {
        truth += (1e9 / hz * (1. + irregular * (2. * random(&mut seed) - 1.))) as u64;
        let elapsed = (truth - 1_000_000_000) as f64 / 1e9;
        if random(&mut seed) < 0.02 || gap.is_some_and(|(a, b)| elapsed > a && elapsed < b) {
            continue;
        }
        // The reported instant differs from when the position was true.
        let stamp = (truth as f64 + (random(&mut seed) - 0.5) * jitter_ns) as u64;
        if stamp <= last {
            continue;
        }
        last = stamp;
        let ideal = elapsed * fps * rate;
        let changed = tracker
            .sample(
                &mut t,
                SourceSample {
                    timestamp_ns: stamp,
                    position: Position::from_fixed((ideal * 4294967296.) as i128),
                    rate_hint: None,
                    discontinuity: false,
                },
                stamp,
            )
            .unwrap();
        if changed && elapsed > 0.5 {
            late_discontinuities += 1;
        }
        if elapsed > 5. {
            let got = t.anchor.at(truth, t.format).fixed() as f64 / 4294967296.;
            errors.push(((got - ideal) / fps * 1e3).abs());
        }
    }
    errors.sort_by(f64::total_cmp);
    Outcome {
        late_discontinuities,
        p99_ms: errors[errors.len() * 99 / 100],
        rate: t.anchor.rate.as_f64(),
    }
}

#[test]
fn jittered_sources_do_not_create_discontinuities_and_filter_jitter() {
    for hz in [10., 30., 60., 240.] {
        for jitter in [0., 200_000., 1_000_000.] {
            for rate in [1., 1.0004, 0.97, -1.] {
                for irregular in [0., 0.3] {
                    let o = run(hz, jitter, rate, irregular, None);
                    let context = format!("{hz} Hz, {jitter} ns jitter, {rate}x, {irregular}");
                    assert_eq!(o.late_discontinuities, 0, "{context}");
                    // Uniform jitter has max |e| = jitter/2; the fit must filter, not follow it.
                    assert!(
                        o.p99_ms <= 0.45 * jitter / 1e6 + 0.001,
                        "{context}: p99 {:.3} ms",
                        o.p99_ms
                    );
                    assert!(
                        (o.rate - rate).abs() <= 0.001 * (jitter / 1e6).max(0.001),
                        "{context}: rate {}",
                        o.rate
                    );
                }
            }
        }
    }
}

#[test]
fn continuous_motion_resumes_after_a_gap_without_a_discontinuity() {
    for hz in [10., 60.] {
        let o = run(hz, 200_000., 1.0004, 0., Some((20., 21.5)));
        assert_eq!(o.late_discontinuities, 0, "{hz} Hz");
        assert!(o.p99_ms < 0.1, "{hz} Hz: p99 {:.3} ms", o.p99_ms);
    }
}

#[test]
fn exact_motion_uses_micro_rate_resolution() {
    // The previous 1e-4 quantization could not represent this rate.
    let o = run(30., 0., 1.000137, 0., None);
    assert!((o.rate - 1.000137).abs() < 3e-6, "rate {}", o.rate);
}

#[test]
fn hinted_sources_reject_isolated_timestamp_outliers() {
    // A 20 Hz hinted source whose sampler is occasionally preempted for up to 500 µs.
    let mut seed = 3;
    let mut t = Timeline {
        source_kind: SourceKind::Tracked,
        ..Default::default()
    };
    let mut tracker = Tracker::default();
    let mut errors = Vec::new();
    for i in 1..1200u64 {
        let truth = 1_000_000_000 + i * 50_000_000;
        let jitter = if random(&mut seed) < 0.01 {
            (random(&mut seed) - 0.5) * 1_000_000.
        } else {
            (random(&mut seed) - 0.5) * 1_000.
        };
        let stamp = (truth as f64 + jitter) as u64;
        let ideal = (truth - 1_000_000_000) as f64 / 1e9 * 30.;
        tracker
            .sample(
                &mut t,
                SourceSample {
                    timestamp_ns: stamp,
                    position: Position::from_fixed((ideal * 4294967296.) as i128),
                    rate_hint: Some(tidkod_protocol::Rate::NORMAL),
                    discontinuity: false,
                },
                stamp.max(truth),
            )
            .unwrap();
        if i > 40 {
            let got = t.anchor.at(truth, t.format).fixed() as f64 / 4294967296.;
            errors.push(((got - ideal) / 30. * 1e6).abs());
        }
    }
    errors.sort_by(f64::total_cmp);
    let p99 = errors[errors.len() * 99 / 100];
    assert!(p99 <= 20., "p99 {p99:.1} µs");
}
