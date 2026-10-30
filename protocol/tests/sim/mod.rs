//! Deterministic impairment simulation of the shared follower core against a ground-truth
//! leader clock. Results are simulated software evidence, not physical-network accuracy.
#![allow(dead_code)]
use tidkod_protocol::{
    Correction, CorrectionPolicy, Position, Rate, SyncState,
    clock::Exchange,
    timeline::{Anchor, FollowerCore, Timeline},
};

const START_NS: u64 = 1_000_000_000;
const LEADER_EPOCH_NS: f64 = 5_000_000_000.;
const RESIDENCE_NS: f64 = 20_000.;
const EVALUATE_NS: u64 = 10_000_000;

/// Seeded LCG matching the estimator unit tests; independent streams per scenario seed.
pub struct Random(u64);
impl Random {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn unit(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn exponential(&mut self, mean: f64) -> f64 {
        -mean * (1. - self.unit()).ln()
    }
    fn lomax(&mut self, scale: f64, alpha: f64) -> f64 {
        scale * ((1. - self.unit()).powf(-1. / alpha) - 1.)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Delay {
    Uniform {
        min_ns: f64,
        max_ns: f64,
    },
    /// Propagation floor, exponential queueing and an occasional heavy (Lomax) tail.
    Queue {
        base_ns: f64,
        mean_ns: f64,
        tail_probability: f64,
        tail_scale_ns: f64,
        tail_alpha: f64,
    },
    /// Bufferbloat: a queue filling linearly to `peak_ns` each `period_ns`, then draining,
    /// plus exponential jitter. Slowly varying one-way delay defeats minimum filtering.
    Bloat {
        base_ns: f64,
        peak_ns: f64,
        period_ns: f64,
        phase: f64,
        jitter_ns: f64,
    },
}
impl Delay {
    fn sample(self, r: &mut Random, now: f64) -> f64 {
        match self {
            Self::Bloat {
                base_ns,
                peak_ns,
                period_ns,
                phase,
                jitter_ns,
            } => base_ns + peak_ns * (now / period_ns + phase).fract() + r.exponential(jitter_ns),
            Self::Uniform { min_ns, max_ns } => min_ns + r.unit() * (max_ns - min_ns),
            Self::Queue {
                base_ns,
                mean_ns,
                tail_probability,
                tail_scale_ns,
                tail_alpha,
            } => {
                let tail = if r.unit() < tail_probability {
                    r.lomax(tail_scale_ns, tail_alpha).min(400_000_000.)
                } else {
                    0.
                };
                base_ns + r.exponential(mean_ns) + tail
            }
        }
    }
}

/// One direction. `stack_ns` is constant unobservable asymmetry; `stack_jitter_ns` is the mean
/// of exponential application scheduling delay between the wire and the timestamp.
#[derive(Clone, Copy, Debug)]
pub struct Path {
    pub delay: Delay,
    pub stack_ns: f64,
    pub stack_jitter_ns: f64,
}
impl Path {
    pub fn new(delay: Delay) -> Self {
        Self {
            delay,
            stack_ns: 0.,
            stack_jitter_ns: 0.,
        }
    }
    fn sample(self, r: &mut Random, now: f64) -> f64 {
        self.delay.sample(r, now)
            + self.stack_ns
            + if self.stack_jitter_ns > 0. {
                r.exponential(self.stack_jitter_ns)
            } else {
                0.
            }
    }
}

/// 802.11 power save at the leader: frames toward a dozing station wait for the next beacon.
#[derive(Clone, Copy, Debug)]
pub struct PowerSave {
    pub idle_ns: f64,
    pub beacon_ns: f64,
}

#[derive(Clone, Debug)]
pub struct Scenario {
    pub name: &'static str,
    /// Follower to leader.
    pub up: Path,
    /// Leader to follower.
    pub down: Path,
    pub drift_ppm: f64,
    pub drift_ramp_ppm_per_s: f64,
    /// Follower timestamp quantization, e.g. 100 µs for a non-isolated browser.
    pub timer_ns: u64,
    pub loss: f64,
    /// Gilbert burst loss: probability of entering, then of remaining in, the lossy state.
    pub burst_loss: Option<(f64, f64)>,
    /// (time, up, down): added to each direction from this simulated time onward.
    pub path_step: Option<(u64, f64, f64)>,
    /// (period, duration): every period, all packets are lost for this long (roaming).
    pub outage: Option<(u64, u64)>,
    /// (time, step): the leader clock jumps relative to the follower, as after the
    /// follower's monotonic clock pauses during system sleep.
    pub clock_step: Option<(u64, f64)>,
    pub power_save: Option<PowerSave>,
    pub seconds: u64,
    pub warmup_seconds: u64,
}
impl Scenario {
    pub fn new(name: &'static str, up: Path, down: Path) -> Self {
        Self {
            name,
            up,
            down,
            drift_ppm: 0.,
            drift_ramp_ppm_per_s: 0.,
            timer_ns: 1,
            loss: 0.,
            burst_loss: None,
            path_step: None,
            outage: None,
            clock_step: None,
            power_save: None,
            seconds: 300,
            warmup_seconds: 10,
        }
    }
    fn leader(&self, t: u64) -> f64 {
        let step = self
            .clock_step
            .filter(|(at, _)| t >= START_NS + at)
            .map_or(0., |(_, d)| d);
        let t = t as f64;
        LEADER_EPOCH_NS
            + step
            + t
            + 1e-6 * (self.drift_ppm * t + self.drift_ramp_ppm_per_s * t * t / 2e9)
    }
    fn out(&self, t: f64) -> bool {
        self.outage.is_some_and(|(period, duration)| {
            let t = t - START_NS as f64;
            t > 0. && t % period as f64 >= (period - duration) as f64
        })
    }
    fn step(&self, t: f64) -> (f64, f64) {
        self.path_step
            .filter(|(at, _, _)| t >= (START_NS + at) as f64)
            .map_or((0., 0.), |(_, up, down)| (up, down))
    }
}

fn wired() -> Path {
    Path::new(Delay::Queue {
        base_ns: 120_000.,
        mean_ns: 25_000.,
        tail_probability: 0.01,
        tail_scale_ns: 200_000.,
        tail_alpha: 1.5,
    })
}
fn wifi() -> Path {
    Path::new(Delay::Queue {
        base_ns: 1_200_000.,
        mean_ns: 1_500_000.,
        tail_probability: 0.05,
        tail_scale_ns: 5_000_000.,
        tail_alpha: 1.2,
    })
}

/// The standing matrix. Names are stable so reports remain comparable across changes.
pub fn matrix() -> Vec<Scenario> {
    let mut v = Vec::new();
    v.push(Scenario {
        drift_ppm: 30.,
        ..Scenario::new("wired", wired(), wired())
    });
    let loaded = Path::new(Delay::Queue {
        base_ns: 120_000.,
        mean_ns: 300_000.,
        tail_probability: 0.05,
        tail_scale_ns: 2_000_000.,
        tail_alpha: 1.3,
    });
    v.push(Scenario {
        drift_ppm: -40.,
        ..Scenario::new("wired-loaded", loaded, loaded)
    });
    v.push(Scenario {
        drift_ppm: 30.,
        ..Scenario::new(
            "wired-stack-asymmetry",
            Path {
                stack_ns: 150_000.,
                ..wired()
            },
            wired(),
        )
    });
    let uniform = Path::new(Delay::Uniform {
        min_ns: 1_000_000.,
        max_ns: 5_000_000.,
    });
    v.push(Scenario {
        drift_ppm: 200.,
        loss: 0.01,
        ..Scenario::new("lan-uniform", uniform, uniform)
    });
    v.push(Scenario {
        drift_ppm: 15.,
        burst_loss: Some((0.01, 0.6)),
        ..Scenario::new("wifi", wifi(), wifi())
    });
    let light = Path::new(Delay::Queue {
        base_ns: 1_200_000.,
        mean_ns: 100_000.,
        tail_probability: 0.,
        tail_scale_ns: 0.,
        tail_alpha: 1.,
    });
    v.push(Scenario {
        drift_ppm: 15.,
        ..Scenario::new("wifi-uplink-queue", wifi(), light)
    });
    v.push(Scenario {
        drift_ppm: 15.,
        power_save: Some(PowerSave {
            idle_ns: 50_000_000.,
            beacon_ns: 102_400_000.,
        }),
        ..Scenario::new("wifi-leader-power-save", wifi(), wifi())
    });
    v.push(Scenario {
        drift_ppm: 30.,
        timer_ns: 100_000,
        ..Scenario::new(
            "browser",
            Path {
                stack_jitter_ns: 200_000.,
                ..wired()
            },
            Path {
                stack_jitter_ns: 400_000.,
                ..wired()
            },
        )
    });
    // Cross-origin isolated pages get a ~5 µs timer instead of ~100 µs.
    let mut isolated = v.last().unwrap().clone();
    isolated.name = "browser-isolated";
    isolated.timer_ns = 5_000;
    v.push(isolated);
    v.push(Scenario {
        drift_ppm: -100.,
        drift_ramp_ppm_per_s: 1.,
        ..Scenario::new("wired-drift-ramp", wired(), wired())
    });
    v.push(Scenario {
        drift_ppm: 30.,
        path_step: Some((120_000_000_000, 5_000_000., 5_000_000.)),
        ..Scenario::new("wired-latency-step", wired(), wired())
    });
    v.push(Scenario {
        drift_ppm: 30.,
        burst_loss: Some((0.02, 0.8)),
        ..Scenario::new("wired-burst-loss", wired(), wired())
    });
    // Hostile conditions. Bounds for these are about graceful degradation, not precision.
    v.push(Scenario {
        drift_ppm: 30.,
        loss: 0.3,
        ..Scenario::new("lossy-30", wired(), wired())
    });
    let congested = Path::new(Delay::Queue {
        base_ns: 1_500_000.,
        mean_ns: 3_000_000.,
        tail_probability: 0.2,
        tail_scale_ns: 10_000_000.,
        tail_alpha: 1.1,
    });
    v.push(Scenario {
        drift_ppm: 15.,
        loss: 0.1,
        burst_loss: Some((0.02, 0.7)),
        ..Scenario::new("wifi-congested", congested, congested)
    });
    let bloat = |peak_ns, period_ns, phase| {
        Path::new(Delay::Bloat {
            base_ns: 1_000_000.,
            peak_ns,
            period_ns,
            phase,
            jitter_ns: 500_000.,
        })
    };
    v.push(Scenario {
        drift_ppm: 15.,
        ..Scenario::new("bufferbloat-uplink", bloat(150e6, 10e9, 0.), light)
    });
    v.push(Scenario {
        drift_ppm: 15.,
        ..Scenario::new(
            "bufferbloat-both",
            bloat(80e6, 7e9, 0.),
            bloat(120e6, 11e9, 0.5),
        )
    });
    v.push(Scenario {
        drift_ppm: 15.,
        outage: Some((60_000_000_000, 4_000_000_000)),
        ..Scenario::new("wifi-roaming-outage", wifi(), wifi())
    });
    v.push(Scenario {
        drift_ppm: 30.,
        path_step: Some((120_000_000_000, 2_000_000., 0.)),
        ..Scenario::new("route-asymmetry-step", wired(), wired())
    });
    v.push(Scenario {
        drift_ppm: 30.,
        clock_step: Some((120_000_000_000, 3e9)),
        ..Scenario::new("suspend-resume", wired(), wired())
    });
    v.push(Scenario {
        drift_ppm: 450.,
        ..Scenario::new("drift-450ppm", wifi(), wifi())
    });
    v.push(Scenario {
        drift_ppm: 30.,
        ..Scenario::new(
            "leader-stalls",
            Path::new(Delay::Queue {
                base_ns: 120_000.,
                mean_ns: 25_000.,
                tail_probability: 0.03,
                tail_scale_ns: 10_000_000.,
                tail_alpha: 1.5,
            }),
            wired(),
        )
    });
    v
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub name: &'static str,
    /// Signed timeline error at 1x in nanoseconds, sampled every 10 ms after warmup.
    pub errors: Vec<f64>,
    /// Evaluations whose reported alignment error did not cover the actual error.
    pub uncovered: usize,
    /// Reported alignment error at each evaluation.
    pub reported: Vec<f64>,
    pub acquisition_ns: Vec<u64>,
    pub hard_resyncs: u64,
    pub probes: u64,
}
impl Report {
    fn sorted_abs(&self) -> Vec<f64> {
        let mut a: Vec<f64> = self.errors.iter().map(|e| e.abs()).collect();
        a.sort_by(f64::total_cmp);
        a
    }
    pub fn median(&self) -> f64 {
        let mut s = self.errors.clone();
        s.sort_by(f64::total_cmp);
        s[s.len() / 2]
    }
    pub fn quantile(&self, q: f64) -> f64 {
        let a = self.sorted_abs();
        a[((a.len() as f64 * q) as usize).min(a.len() - 1)]
    }
    pub fn max(&self) -> f64 {
        *self.sorted_abs().last().unwrap()
    }
    pub fn reported_median(&self) -> f64 {
        let mut s = self.reported.clone();
        s.sort_by(f64::total_cmp);
        s[s.len() / 2]
    }
    pub fn over_fraction(&self, limit_ns: f64) -> f64 {
        self.errors.iter().filter(|e| e.abs() > limit_ns).count() as f64 / self.errors.len() as f64
    }
    pub fn uncovered_fraction(&self) -> f64 {
        self.uncovered as f64 / self.errors.len() as f64
    }
    pub fn worst_acquisition_ns(&self) -> u64 {
        self.acquisition_ns
            .iter()
            .copied()
            .max()
            .unwrap_or(u64::MAX)
    }
    pub fn merge(&mut self, other: Report) {
        self.name = other.name;
        self.errors.extend(other.errors);
        self.uncovered += other.uncovered;
        self.reported.extend(other.reported);
        self.acquisition_ns.extend(other.acquisition_ns);
        self.hard_resyncs += other.hard_resyncs;
        self.probes += other.probes;
    }
    pub fn header() -> &'static str {
        "| Scenario | Signed median | p95 | p99 | Max | Over 1 ms | Reported p50 | Uncovered | Acquisition | Resyncs | Probes/s |\n\
         | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    }
    pub fn row(&self, seconds: f64) -> String {
        format!(
            "| {} | {:+.3} ms | {:.3} ms | {:.3} ms | {:.3} ms | {:.2}% | {:.3} ms | {:.2}% | {:.2} s | {} | {:.2} |",
            self.name,
            self.median() / 1e6,
            self.quantile(0.95) / 1e6,
            self.quantile(0.99) / 1e6,
            self.max() / 1e6,
            self.over_fraction(1e6) * 100.,
            self.reported_median() / 1e6,
            self.uncovered_fraction() * 100.,
            self.worst_acquisition_ns() as f64 / 1e9,
            self.hard_resyncs,
            self.probes as f64 / seconds,
        )
    }
}

struct Pending {
    deliver: u64,
    exchange: Exchange,
}

/// Probe cadence used by the simulated transport.
pub fn probe_interval(core: &FollowerCore, now: u64) -> u64 {
    if core.needs_fast_probes(now) {
        50_000_000
    } else {
        250_000_000
    }
}

pub fn run(s: &Scenario, seed: u64) -> Report {
    let mut r = Random::new(seed);
    let mut core = FollowerCore::new(Timeline::default(), CorrectionPolicy::default());
    let timeline = Timeline {
        session: [1; 16],
        revision: 1,
        anchor: Anchor {
            time_ns: 0,
            position: Position::ZERO,
            rate: Rate::NORMAL,
        },
        ..Default::default()
    };
    core.connected();
    core.state(timeline, START_NS);
    let fps = timeline.format.fps();
    let quantize = |t: u64| t / s.timer_ns * s.timer_ns;
    let end = START_NS + s.seconds * 1_000_000_000;
    let warmup = START_NS + s.warmup_seconds * 1_000_000_000;
    let mut report = Report {
        name: s.name,
        ..Default::default()
    };
    let mut pending: Vec<Pending> = Vec::new();
    let mut next_probe = START_NS;
    let mut next_evaluation = START_NS;
    let mut acquired = None;
    let mut lossy = false;
    let mut leader_awake_until = f64::NEG_INFINITY;
    let beacon_phase = r.unit();
    loop {
        let next_delivery = pending.first().map_or(u64::MAX, |p| p.deliver);
        let now = next_probe.min(next_evaluation).min(next_delivery);
        if now >= end {
            break;
        }
        if now == next_delivery {
            let p = pending.remove(0);
            let e = Exchange {
                t4: quantize(p.exchange.t4),
                ..p.exchange
            };
            if let Some(Correction::HardResync { .. }) = core.measurement(e)
                && acquired.is_some()
            {
                report.hard_resyncs += 1;
            }
        } else if now == next_probe {
            report.probes += 1;
            let lost = match s.burst_loss {
                Some((enter, stay)) => {
                    lossy = r.unit() < if lossy { stay } else { enter };
                    lossy
                }
                None => false,
            } || r.unit() < s.loss
                || r.unit() < s.loss;
            let (up, down) = s.step(now as f64);
            let mut arrive = now as f64 + s.up.sample(&mut r, now as f64) + up;
            if let Some(ps) = s.power_save {
                if arrive > leader_awake_until {
                    let phase = beacon_phase * ps.beacon_ns;
                    arrive = ((arrive - phase) / ps.beacon_ns).ceil() * ps.beacon_ns + phase;
                }
                leader_awake_until = leader_awake_until.max(arrive + ps.idle_ns);
            }
            let depart = arrive + RESIDENCE_NS;
            let receive = depart + s.down.sample(&mut r, depart) + down;
            if !lost && !s.out(now as f64) && !s.out(depart) {
                let p = Pending {
                    deliver: receive as u64,
                    exchange: Exchange {
                        t1: quantize(now),
                        t2: s.leader(arrive as u64) as u64,
                        t3: s.leader(depart as u64) as u64,
                        t4: receive as u64,
                    },
                };
                let i = pending.partition_point(|q| q.deliver <= p.deliver);
                pending.insert(i, p);
            }
            next_probe = now + probe_interval(&core, now);
        } else {
            if let Some(Correction::HardResync { .. }) = core.tick(now, 2_000_000_000)
                && acquired.is_some()
            {
                report.hard_resyncs += 1;
            }
            let reading = core.view.evaluate(now);
            if acquired.is_none() && reading.status.synchronization == SyncState::Synchronized {
                acquired = Some(now);
                report.acquisition_ns.push(now - START_NS);
            }
            if now >= warmup {
                let truth = timeline.at(s.leader(now) as u64).0;
                let error =
                    (reading.position.fixed() - truth.fixed()) as f64 / 4294967296. / fps * 1e9;
                report.errors.push(error);
                report.reported.push(reading.status.alignment_error_ns);
                if error.abs() > reading.status.alignment_error_ns {
                    report.uncovered += 1;
                }
            }
            next_evaluation = now + EVALUATE_NS;
        }
    }
    report
}

pub const SEEDS: [u64; 3] = [7, 19, 997];

pub fn pooled(s: &Scenario) -> Report {
    let mut total = Report::default();
    for seed in SEEDS {
        total.merge(run(s, seed));
    }
    total
}
