//! Loose regression bounds over the simulated impairment matrix. The report example prints
//! the full table; these limits only catch regressions and are not physical accuracy claims.
mod sim;

/// Pooled p99 absolute timeline error bound in milliseconds, and the hard resyncs allowed
/// across all seeds after acquisition.
fn limits(name: &str) -> (f64, u64) {
    match name {
        "wired" | "wired-burst-loss" | "lossy-30" | "leader-stalls" => (0.05, 0),
        "wired-latency-step" | "wired-loaded" | "browser" | "browser-isolated" => (0.15, 0),
        "wired-stack-asymmetry" | "wired-drift-ramp" => (0.2, 0),
        "wifi" | "wifi-uplink-queue" | "wifi-leader-power-save" | "wifi-roaming-outage" => (0.3, 0),
        "drift-450ppm" => (0.3, 0),
        "lan-uniform" | "wifi-congested" => (0.6, 0),
        // Unobservable: the path became 2 ms longer in one direction only.
        "route-asymmetry-step" => (1.5, 0),
        // One legitimate realignment per seed after the clock step.
        "suspend-resume" => (0.05, sim::SEEDS.len() as u64),
        // Acquisition while queues are full is unobservable until a queue drains; these
        // bound degradation, and before stability fixes they produced 70–90 resyncs.
        "bufferbloat-uplink" => (30., 0),
        "bufferbloat-both" => (20., sim::SEEDS.len() as u64),
        other => panic!("no bound for scenario {other}"),
    }
}

#[test]
fn impairment_matrix_stays_within_regression_bounds() {
    for s in sim::matrix() {
        let r = sim::pooled(&s);
        eprintln!("{}", r.row(s.seconds as f64 * sim::SEEDS.len() as f64));
        let (p99_limit, resyncs) = limits(s.name);
        let p99 = r.quantile(0.99) / 1e6;
        assert!(p99 <= p99_limit, "{}: p99 {p99:.3} ms", s.name);
        // A clock step is invisible to every estimator until the next exchange.
        let coverage = if s.clock_step.is_some() { 0.01 } else { 0.005 };
        assert!(
            r.uncovered_fraction() <= coverage,
            "{}: reported alignment error missed {:.2}% of evaluations",
            s.name,
            r.uncovered_fraction() * 100.
        );
        assert!(r.hard_resyncs <= resyncs, "{}: {} hard resyncs", s.name, r.hard_resyncs);
        assert!(r.worst_acquisition_ns() < 2_000_000_000, "{}", s.name);
    }
}
