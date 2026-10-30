//! Print the deterministic impairment matrix as a Markdown table.
//!
//! `cargo run --release -p tidkod-protocol --example impairment -- [--seconds N] [scenario...]`
#[path = "../tests/sim/mod.rs"]
mod sim;

fn main() {
    let mut seconds = None;
    let mut names = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--seconds" {
            seconds = Some(
                args.next()
                    .and_then(|s| s.parse().ok())
                    .expect("--seconds N"),
            );
        } else {
            names.push(a);
        }
    }
    println!("core {}", tidkod_protocol::CORE_BUILD_ID);
    println!("{}", sim::Report::header());
    for mut s in sim::matrix() {
        if !names.is_empty() && !names.iter().any(|n| n == s.name) {
            continue;
        }
        if let Some(n) = seconds {
            s.seconds = n;
        }
        println!(
            "{}",
            sim::pooled(&s).row(s.seconds as f64 * sim::SEEDS.len() as f64)
        );
    }
}
