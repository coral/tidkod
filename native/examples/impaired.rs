//! Real leader, follower and QUIC/MoQ stack through an in-process UDP impairment proxy.
//! Both engines share the host clock, so timeline and clock errors are exact. Profiles add
//! seeded delay, loss, queueing and outages per direction; the proxy's own scheduling adds a
//! little more jitter. This exercises transport behaviour, not a calibrated physical network.
//! Run optimized: cargo run --release -p tidkod --example impaired -- --profile all
use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tidkod::{ConnectionState, Engine, Event, FollowerConfig, LeaderConfig, Rate};

struct Random(u64);
impl Random {
    fn unit(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn exponential(&mut self, mean: f64) -> f64 {
        -mean * (1. - self.unit()).ln()
    }
}

/// One direction, in milliseconds. `bloat` is (peak, period seconds, phase): a queue that
/// fills linearly and drains once per period.
#[derive(Clone, Copy, Default)]
struct Direction {
    base: f64,
    jitter: f64,
    tail_probability: f64,
    tail: f64,
    loss: f64,
    bloat: Option<(f64, f64, f64)>,
}
impl Direction {
    fn delay(&self, r: &mut Random, elapsed: f64) -> f64 {
        let tail = if r.unit() < self.tail_probability {
            r.exponential(self.tail)
        } else {
            0.
        };
        let queue = self.bloat.map_or(0., |(peak, period, phase)| {
            peak * (elapsed / period + phase).fract()
        });
        self.base + r.exponential(self.jitter) + tail + queue
    }
}

#[derive(Clone, Copy)]
struct Profile {
    name: &'static str,
    /// Follower to leader.
    up: Direction,
    /// Leader to follower.
    down: Direction,
    /// (period, duration) in seconds: total loss, as while roaming between access points.
    outage: Option<(f64, f64)>,
}
fn profiles() -> Vec<Profile> {
    let wifi = Direction {
        base: 1.2,
        jitter: 1.5,
        tail_probability: 0.05,
        tail: 8.,
        loss: 0.01,
        bloat: None,
    };
    let congested = Direction {
        base: 1.5,
        jitter: 3.,
        tail_probability: 0.2,
        tail: 15.,
        loss: 0.08,
        bloat: None,
    };
    vec![
        Profile {
            name: "clean",
            up: Direction::default(),
            down: Direction::default(),
            outage: None,
        },
        Profile {
            name: "wifi",
            up: wifi,
            down: wifi,
            outage: None,
        },
        Profile {
            name: "congested",
            up: congested,
            down: congested,
            outage: None,
        },
        Profile {
            name: "lossy-30",
            up: Direction {
                base: 0.2,
                jitter: 0.05,
                loss: 0.3,
                ..Default::default()
            },
            down: Direction {
                base: 0.2,
                jitter: 0.05,
                loss: 0.3,
                ..Default::default()
            },
            outage: None,
        },
        Profile {
            name: "bufferbloat-uplink",
            up: Direction {
                bloat: Some((150., 10., 0.)),
                ..wifi
            },
            down: Direction {
                base: 1.2,
                jitter: 0.1,
                ..Default::default()
            },
            outage: None,
        },
        Profile {
            name: "roaming",
            up: wifi,
            down: wifi,
            outage: Some((30., 4.)),
        },
    ]
}

/// Forward datagrams between the follower and the leader with per-direction impairment.
/// Each direction is a FIFO queue: a delayed packet holds back the ones behind it.
fn proxy(profile: Profile, leader: SocketAddr, stop: Arc<AtomicBool>) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let address = socket.local_addr().unwrap();
    thread::spawn(move || {
        let mut r = Random(7);
        let began = Instant::now();
        let mut client: Option<SocketAddr> = None;
        let mut queue = BinaryHeap::new();
        let mut serial = 0u64;
        let mut tail = [began; 2];
        let mut buffer = [0u8; 2048];
        while !stop.load(Ordering::Acquire) {
            while let Ok((n, from)) = socket.recv_from(&mut buffer) {
                let up = from != leader;
                if up {
                    client = Some(from);
                }
                let now = Instant::now();
                let elapsed = (now - began).as_secs_f64();
                let direction = if up { profile.up } else { profile.down };
                let out = profile
                    .outage
                    .is_some_and(|(period, duration)| elapsed % period >= period - duration);
                if out || r.unit() < direction.loss {
                    continue;
                }
                let delay = Duration::from_secs_f64(direction.delay(&mut r, elapsed) / 1e3);
                let at = (now + delay).max(tail[up as usize]);
                tail[up as usize] = at;
                serial += 1;
                queue.push(Reverse((at, serial, up, buffer[..n].to_vec())));
            }
            let now = Instant::now();
            while queue.peek().is_some_and(|Reverse((at, ..))| *at <= now) {
                let Reverse((_, _, up, bytes)) = queue.pop().unwrap();
                let to = if up { Some(leader) } else { client };
                if let Some(to) = to {
                    let _ = socket.send_to(&bytes, to);
                }
            }
            thread::sleep(Duration::from_micros(100));
        }
    });
    address
}

fn argument(name: &str) -> Option<String> {
    let args: Vec<_> = std::env::args().collect();
    args.windows(2).find(|p| p[0] == name).map(|p| p[1].clone())
}
fn quantile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() as f64 * q) as usize).min(sorted.len() - 1)]
}

fn run(profile: Profile, warmup: Duration, duration: Duration) -> tidkod::Result<()> {
    let leader_engine = Engine::new()?;
    let leader = leader_engine.leader(LeaderConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        advertise: false,
        rate: Rate::NORMAL,
        ..Default::default()
    })?;
    let stop = Arc::new(AtomicBool::new(false));
    let relay = proxy(profile, leader.info().address, stop.clone());
    let engine = Engine::new()?;
    // --trace prints each clock exchange: elapsed s, path delay, exchange and fitted offset
    // errors (ms), accepted.
    let trace = std::env::args().any(|a| a == "--trace");
    let follower = engine.follower(FollowerConfig {
        clock_diagnostics: trace,
        ..FollowerConfig::direct(relay)
    })?;
    let truth_offset = leader_engine.clock().ns_at(Instant::now()).unwrap() as f64
        - engine.clock().ns_at(Instant::now()).unwrap() as f64;
    let mut reference = leader.reader()?;
    let mut reader = follower.reader()?;
    let (mut errors, mut signed) = (Vec::new(), Vec::new());
    let (mut uncovered, mut aligned, mut disconnected, mut reconnects, mut resyncs) =
        (0, 0, 0, 0, 0);
    let mut previous: Option<(ConnectionState, u64)> = None;
    let began = Instant::now();
    while began.elapsed() < warmup + duration {
        thread::sleep(Duration::from_millis(5));
        while let Some(event) = follower.try_event() {
            if let Event::ClockObservation(o) = event {
                let e = o.exchange;
                let delay = (e.t4 - e.t1) as f64 - (e.t3 - e.t2) as f64;
                let offset = ((e.t2 as f64 - e.t1 as f64) + (e.t3 as f64 - e.t4 as f64)) / 2.;
                eprintln!(
                    "trace {:.2} {:.3} {:+.3} {:+.3} {}",
                    began.elapsed().as_secs_f64(),
                    delay / 1e6,
                    (offset - truth_offset) / 1e6,
                    (o.mapping.offset_at(e.t4) - truth_offset) / 1e6,
                    o.accepted
                );
            }
        }
        let instant = Instant::now();
        let truth = reference.read_at(leader_engine.clock().ns_at(instant).unwrap());
        let r = reader.read_at(engine.clock().ns_at(instant).unwrap());
        if began.elapsed() < warmup {
            previous = Some((r.status.connection, r.status.resync_generation));
            continue;
        }
        let ms = (r.position.fixed() - truth.position.fixed()) as f64
            / 4294967296.
            / truth.format.fps()
            * 1000.;
        signed.push(ms);
        errors.push(ms.abs());
        uncovered += usize::from(ms.abs() * 1e6 > r.status.alignment_error_ns);
        aligned += usize::from(r.status.aligned);
        disconnected += usize::from(r.status.connection != ConnectionState::Connected);
        if let Some((connection, generation)) = previous {
            reconnects += usize::from(
                connection != ConnectionState::Connected
                    && r.status.connection == ConnectionState::Connected,
            );
            resyncs += r.status.resync_generation.saturating_sub(generation);
        }
        previous = Some((r.status.connection, r.status.resync_generation));
    }
    stop.store(true, Ordering::Release);
    errors.sort_by(f64::total_cmp);
    signed.sort_by(f64::total_cmp);
    let n = errors.len() as f64;
    let percent = |count: usize| count as f64 / n * 100.;
    println!(
        "| {} | {:+.3} ms | {:.3} ms | {:.3} ms | {:.3} ms | {:.2}% | {:.2}% | {:.1}% | {:.1}% | {} | {} |",
        profile.name,
        signed[signed.len() / 2],
        quantile(&errors, 0.95),
        quantile(&errors, 0.99),
        errors.last().unwrap(),
        percent(errors.iter().filter(|e| **e > 1.).count()),
        percent(uncovered),
        percent(aligned),
        percent(disconnected),
        reconnects,
        resyncs,
    );
    engine.shutdown()?;
    leader_engine.shutdown()
}

fn main() -> tidkod::Result<()> {
    let name = argument("--profile").unwrap_or_else(|| "all".into());
    let seconds = |flag, default| Duration::from_secs(argument(flag).map_or(default, |s| s.parse().unwrap()));
    let (warmup, duration) = (seconds("--warmup-seconds", 20), seconds("--seconds", 120));
    println!("core {}", tidkod::CORE_BUILD_ID);
    println!(
        "| Profile | Signed median | p95 | p99 | Max | Over 1 ms | Uncovered | Aligned | Disconnected | Reconnects | Resyncs |\n\
         | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    );
    for profile in profiles() {
        if name == "all" || name == profile.name {
            run(profile, warmup, duration)?;
        }
    }
    Ok(())
}
