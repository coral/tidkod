//! Generate LTC through CPAL, standalone or from a Tidkod follower.
mod common;
mod ltc_audio;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    ltc_audio::run(false)
}
