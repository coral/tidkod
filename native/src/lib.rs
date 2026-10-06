//! Direct LAN timecode synchronization with synchronous handles.
//!
//! ```no_run
//! use tidkod::{Engine, LeaderConfig};
//! let engine = Engine::new()?;
//! let leader = engine.leader(LeaderConfig::default())?;
//! leader.play()?;
//! let mut reader = leader.reader()?;
//! println!("{}", reader.read().label());
//! engine.shutdown()?;
//! # Ok::<(), tidkod::Error>(())
//! ```
pub use tidkod_protocol::CORE_BUILD_ID;
pub use tidkod_protocol::ltc;
pub use tidkod_protocol::{Boundary, BoundaryKind, FrameFormat, Label, Position, Rate};
pub use tidkod_protocol::{ClockBridge, OutputTime, sample_time};
mod api;
pub use tidkod_protocol::clock;
mod discovery;
mod network;
pub use api::*;
pub use discovery::*;
use tidkod_protocol::timeline;
use tidkod_protocol::tracking;
pub use timeline::{
    ConnectionState, Correction, CorrectionPolicy, Reading, SourceHealth, SourceKind, Status,
    SyncState, TimecodeSnapshot,
};
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid configuration: {0}")]
    Invalid(&'static str),
    #[error("worker has shut down")]
    Shutdown,
    #[error("bounded command queue is full")]
    QueueFull,
    #[error("reader capacity reached")]
    ReaderLimit,
    #[error("protocol: {0}")]
    Protocol(#[from] tidkod_protocol::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("transport: {0}")]
    Transport(String),
    #[error("discovery: {0}")]
    Discovery(String),
}
pub type Result<T> = std::result::Result<T, Error>;

mod worker;

// Keep the complete upstream transport adapter, including currently unused hooks.
#[allow(dead_code)]
mod transport;
