//! Capture LTC through CPAL and publish a Tidkod tracked leader.
mod common;
mod ltc_audio;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    ltc_audio::run(true)
}
