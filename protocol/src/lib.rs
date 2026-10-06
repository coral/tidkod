//! Portable Tidkod v1 wire schema, timecode arithmetic, and synchronization.
//! All timestamps are supplied by the caller; no networking or runtime is required.
//!
//! A transport adapter decodes complete messages and feeds the shared follower:
//!
//! ```
//! use tidkod_protocol::{decode_snapshot, CorrectionPolicy, timeline::{FollowerCore, Timeline}};
//! let mut follower = FollowerCore::new(Timeline::default(), CorrectionPolicy::default());
//! follower.connected();
//! # let wire_bytes = tidkod_protocol::encode(&Timeline {session: [1;16], revision: 1, ..Default::default()}.wire())?;
//! let snapshot = decode_snapshot(&wire_bytes)?;
//! follower.state(Timeline::from_wire(&snapshot)?, 1_000_000);
//! // Feed matched four-timestamp exchanges through follower.measurement(...).
//! // Until the first valid clock measurement, this returns the configured fallback.
//! let reading = follower.view.evaluate(2_000_000);
//! assert_eq!(reading.label().to_string(), "00:00:00:00");
//! # Ok::<(), tidkod_protocol::Error>(())
//! ```
pub mod boundary;
pub mod ltc;
pub use boundary::{Boundary, BoundaryKind};
pub mod clock;
pub mod output;
pub use output::{ClockBridge, OutputTime, sample_time};
pub mod probes;
pub mod timeline;
pub mod tracking;
pub use timeline::{
    ConnectionState, Correction, CorrectionPolicy, Reading, SourceHealth, SourceKind, Status,
    SyncState, TimecodeSnapshot,
};
#[derive(Clone, Copy, Debug)]
pub struct SourceSample {
    pub timestamp_ns: u64,
    pub position: Position,
    pub rate_hint: Option<Rate>,
    pub discontinuity: bool,
}
pub mod timecode;
pub use timecode::*;
/// Generated exclusively from the authoritative protobuf schema.
pub mod wire {
    include!(concat!(env!("OUT_DIR"), "/tidkod.v1.rs"));
}
pub const DESCRIPTOR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tidkod.bin"));
pub const VERSION: u32 = 1;
/// Package version and deterministic core-source identity; not a wire version.
pub const CORE_BUILD_ID: &str = concat!(env!("CARGO_PKG_VERSION"), "/", env!("TIDKOD_CORE_ID"));
pub const MAX_MESSAGE: usize = 512;
pub const MAX_SCHEDULED: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid timecode or wire value: {0}")]
    Invalid(&'static str),
    #[error("unsupported Tidkod protocol version {0}")]
    Version(u32),
    #[error("application message exceeds 512 bytes")]
    Size,
    #[error("malformed protobuf: {0}")]
    Decode(#[from] prost::DecodeError),
}
pub fn encode<T: prost::Message>(value: &T) -> Result<Vec<u8>, Error> {
    if value.encoded_len() > MAX_MESSAGE {
        return Err(Error::Size);
    }
    Ok(value.encode_to_vec())
}
pub fn decode_snapshot(bytes: &[u8]) -> Result<wire::Snapshot, Error> {
    use prost::Message;
    if bytes.len() > MAX_MESSAGE {
        return Err(Error::Size);
    }
    let s = wire::Snapshot::decode(bytes)?;
    validate_snapshot(&s)?;
    Ok(s)
}
pub fn validate_snapshot(s: &wire::Snapshot) -> Result<(), Error> {
    if s.version != VERSION {
        return Err(Error::Version(s.version));
    }
    if s.session.len() != 16 || s.revision == 0 {
        return Err(Error::Invalid("identity/revision"));
    }
    if !s.session_id.is_empty() && s.session_id.len() != 16 {
        return Err(Error::Invalid("recording session ID"));
    }
    if !matches!(s.source_kind, 1 | 2) || !matches!(s.source_health, 1 | 2) {
        return Err(Error::Invalid("source enum"));
    }
    let f = s.format.as_ref().ok_or(Error::Invalid("format missing"))?;
    FrameFormat::new(f.numerator, f.denominator, f.drop_frame)?;
    let a = s.anchor.as_ref().ok_or(Error::Invalid("anchor missing"))?;
    validate_anchor(a)?;
    if s.scheduled.len() > MAX_SCHEDULED {
        return Err(Error::Invalid("schedule capacity"));
    }
    let mut time = a.time_ns;
    let mut disc = s.discontinuity;
    for c in &s.scheduled {
        let a = c
            .anchor
            .as_ref()
            .ok_or(Error::Invalid("scheduled anchor missing"))?;
        validate_anchor(a)?;
        if a.time_ns <= time || c.discontinuity <= disc {
            return Err(Error::Invalid("schedule order"));
        }
        time = a.time_ns;
        disc = c.discontinuity;
    }
    Ok(())
}
fn validate_anchor(a: &wire::Anchor) -> Result<(), Error> {
    if a.time_ns > i64::MAX as u64 {
        return Err(Error::Invalid("timestamp"));
    }
    a.position
        .as_ref()
        .ok_or(Error::Invalid("position missing"))?;
    let r = a.rate.as_ref().ok_or(Error::Invalid("rate missing"))?;
    Rate::new(r.numerator, r.denominator)?;
    Ok(())
}
pub fn decode_probe(bytes: &[u8]) -> Result<wire::Probe, Error> {
    use prost::Message;
    if bytes.len() > MAX_MESSAGE {
        return Err(Error::Size);
    }
    let p = wire::Probe::decode(bytes)?;
    if p.version != VERSION {
        return Err(Error::Version(p.version));
    }
    if p.sequence == 0
        || [p.t1, p.t2, p.t3].iter().any(|t| *t > i64::MAX as u64)
        || p.t3 < p.t2
        || (p.t2 == 0) != (p.t3 == 0)
    {
        return Err(Error::Invalid("probe"));
    }
    Ok(p)
}
