//! Loopback one-way delay decomposition. Both engines share the host monotonic clock, so the
//! true leader/follower epoch offset is exact and each exchange's up and down legs, including
//! application timestamping lag, are observable. Physical networks cannot be decomposed this way.
//! Run optimized: cargo run --release -p tidkod --example asymmetry -- --seconds 30
use std::time::{Duration, Instant};
use tidkod::{Engine, Event, FollowerConfig, LeaderConfig, Rate};

fn argument(name: &str, default: u64) -> u64 {
    let args: Vec<_> = std::env::args().collect();
    args.windows(2)
        .find(|p| p[0] == name)
        .map_or(default, |p| p[1].parse().expect("integer argument"))
}
fn quantiles(name: &str, values: &mut [f64]) {
    values.sort_by(f64::total_cmp);
    let q = |p: f64| values[((values.len() as f64 * p) as usize).min(values.len() - 1)] / 1e3;
    println!(
        "{name:<22} p10 {:>8.1} µs  median {:>8.1} µs  p90 {:>8.1} µs",
        q(0.1),
        q(0.5),
        q(0.9)
    );
}
fn main() -> tidkod::Result<()> {
    let seconds = argument("--seconds", 30);
    let leader_engine = Engine::new()?;
    let leader = leader_engine.leader(LeaderConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        advertise: false,
        rate: Rate::NORMAL,
        ..Default::default()
    })?;
    let engine = Engine::new()?;
    let follower = engine.follower(FollowerConfig {
        clock_diagnostics: true,
        ..FollowerConfig::direct(leader.info().address)
    })?;
    let instant = Instant::now();
    let truth = leader_engine.clock().ns_at(instant).unwrap() as f64
        - engine.clock().ns_at(instant).unwrap() as f64;
    let (mut up, mut down, mut residence, mut asymmetry, mut fit, mut publication) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(seconds) {
        while let Some(event) = follower.try_event() {
            let Event::ClockObservation(o) = event else {
                continue;
            };
            if !o.accepted || began.elapsed() < Duration::from_secs(3) {
                continue;
            }
            let e = o.exchange;
            let u = e.t2 as f64 - truth - e.t1 as f64;
            let d = e.t4 as f64 - (e.t3 as f64 - truth);
            up.push(u);
            down.push(d);
            residence.push((e.t3 - e.t2) as f64);
            publication.extend(o.publication_ns.map(|p| p as f64));
            // Positive: the exchange alone would place the follower ahead of the leader.
            asymmetry.push((u - d) / 2.);
            fit.push(o.mapping.offset_at(e.t4) - truth);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    println!("core {} exchanges {}", tidkod::CORE_BUILD_ID, up.len());
    quantiles("up (t1 -> t2)", &mut up);
    quantiles("down (t3 -> t4)", &mut down);
    quantiles("leader residence", &mut residence);
    quantiles("follower t1 -> sent", &mut publication);
    quantiles("exchange offset error", &mut asymmetry);
    quantiles("fitted offset error", &mut fit);
    engine.shutdown()?;
    leader_engine.shutdown()?;
    Ok(())
}
