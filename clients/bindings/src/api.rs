//! The sole foreign API definition. Bridge declarations and adapters are generated
//! from these public structs and functions by build.rs.
use tidkod_protocol::{self as protocol, timeline};
#[cfg(feature = "native")]
use tidkod_protocol::{FrameFormat, Position, Rate};
type Result<T> = std::result::Result<T, String>;
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Validated timecode arithmetic shared by native and runtime-free clients.
pub struct TimecodeFormat {
    inner: protocol::FrameFormat,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FramePosition {
    pub frames: i64,
    pub subframe: u32,
}
impl From<protocol::Position> for FramePosition {
    fn from(p: protocol::Position) -> Self {
        Self {
            frames: p.frames,
            subframe: p.subframe,
        }
    }
}
pub fn timecode_format_new(
    numerator: u32,
    denominator: u32,
    drop_frame: bool,
) -> Result<TimecodeFormat> {
    Ok(TimecodeFormat {
        inner: protocol::FrameFormat::new(numerator, denominator, drop_frame).map_err(error)?,
    })
}
pub fn timecode_format_position(
    format: &TimecodeFormat,
    hours: u8,
    minutes: u8,
    seconds: u8,
    frames: u8,
) -> Result<FramePosition> {
    Ok(format
        .inner
        .position(hours, minutes, seconds, frames)
        .map_err(error)?
        .into())
}
/// Elapsed real time, not nominal timecode label seconds. Negative durations retain Q32 precision.
pub fn timecode_format_elapsed(format: &TimecodeFormat, nanoseconds: i64) -> FramePosition {
    protocol::Position::ZERO
        .advance(nanoseconds, format.inner, protocol::Rate::NORMAL)
        .into()
}

/// Fixed-width, allocation-free snapshot. Positions are signed whole frames plus
/// unsigned Q32 fractional frames; timestamps and uncertainty use nanoseconds.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Reading {
    pub has_session_id: bool,
    /// First/last eight UUID bytes, interpreted as big-endian unsigned integers.
    pub session_id_high: u64,
    pub session_id_low: u64,
    pub frames: i64,
    pub subframe: u32,
    pub fps_numerator: u32,
    pub fps_denominator: u32,
    pub drop_frame: bool,
    pub rate_numerator: i32,
    pub rate_denominator: u32,
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frame: u8,
    pub discontinuity: u64,
    pub connection: u8,
    pub synchronization: u8,
    pub source_kind: u8,
    pub source_health: u8,
    pub uncertainty_ns: f64,
    pub sample_age_ns: u64,
    pub rtt_ns: u64,
    pub offset_ns: f64,
    pub drift_ppm: f64,
    pub lost_packets: u64,
    pub correction_frames: f64,
    pub has_offset_evidence: bool,
    pub offset_lower_ns: f64,
    pub offset_upper_ns: f64,
    pub offset_evidence_reference_ns: u64,
    pub offset_evidence_samples: u32,
    pub accepted_observations: u64,
    pub alignment_error_ns: f64,
    pub aligned: bool,
    pub resync_generation: u64,
}
impl From<timeline::Reading> for Reading {
    fn from(r: timeline::Reading) -> Self {
        let l = r.label();
        let s = r.status;
        Self {
            has_session_id: r.session_id.is_some(),
            session_id_high: u64::from_be_bytes(
                r.session_id.unwrap_or_default()[..8].try_into().unwrap(),
            ),
            session_id_low: u64::from_be_bytes(
                r.session_id.unwrap_or_default()[8..].try_into().unwrap(),
            ),
            frames: r.position.frames,
            subframe: r.position.subframe,
            fps_numerator: r.format.numerator(),
            fps_denominator: r.format.denominator(),
            drop_frame: r.format.drop_frame(),
            rate_numerator: r.rate.numerator(),
            rate_denominator: r.rate.denominator(),
            hours: l.hours,
            minutes: l.minutes,
            seconds: l.seconds,
            frame: l.frames,
            discontinuity: r.discontinuity,
            connection: s.connection as u8,
            synchronization: s.synchronization as u8,
            source_kind: s.source_kind as u8,
            source_health: s.source_health as u8,
            uncertainty_ns: s.uncertainty_ns,
            sample_age_ns: s.sample_age_ns,
            rtt_ns: s.rtt_ns,
            offset_ns: s.offset_ns,
            drift_ppm: s.drift_ppm,
            lost_packets: s.lost_packets,
            correction_frames: s.correction_frames,
            has_offset_evidence: s.offset_evidence.is_some(),
            offset_lower_ns: s.offset_evidence.map_or(0., |e| e.lower_ns),
            offset_upper_ns: s.offset_evidence.map_or(0., |e| e.upper_ns),
            offset_evidence_reference_ns: s.offset_evidence.map_or(0, |e| e.reference_ns),
            offset_evidence_samples: s.offset_evidence.map_or(0, |e| e.samples),
            accepted_observations: s.accepted_observations,
            alignment_error_ns: s.alignment_error_ns,
            aligned: s.aligned,
            resync_generation: s.resync_generation,
        }
    }
}
/// Network-free follower; the embedding application supplies bytes and monotonic timestamps.
pub struct Core {
    core: timeline::FollowerCore,
    probes: protocol::probes::Probes,
}
pub fn core_new() -> Core {
    Core {
        core: timeline::FollowerCore::new(timeline::Timeline::default(), Default::default()),
        probes: Default::default(),
    }
}
pub fn core_connected(core: &mut Core) {
    core.probes = Default::default();
    core.core.connected();
}
pub fn core_disconnected(core: &mut Core) {
    core.core.disconnected();
}
pub fn core_state(core: &mut Core, bytes: &[u8], now_ns: u64) -> Result<()> {
    let state = timeline::Timeline::from_wire(&protocol::decode_snapshot(bytes).map_err(error)?)
        .map_err(error)?;
    core.core.state(state, now_ns);
    Ok(())
}
pub fn core_probe(core: &mut Core, now_ns: u64) -> Result<Vec<u8>> {
    core.probes.request(now_ns).map_err(error)
}
pub fn core_publication_finished(core: &mut Core, now_ns: u64) {
    core.probes.publication_finished(now_ns);
}
pub fn core_reply(core: &mut Core, bytes: &[u8], receipt_ns: u64) -> Result<()> {
    if let Some(exchange) = core.probes.reply(bytes, receipt_ns).map_err(error)? {
        core.core
            .measurement_timed(exchange, core.probes.last_publication_ns(), receipt_ns);
    }
    Ok(())
}
pub fn core_tick(core: &mut Core, now_ns: u64, stale_after_ns: u64) {
    core.core.tick(now_ns, stale_after_ns);
}
/// Acquisition/correction/recovery cadence shared with native and browser clients.
pub fn core_probe_interval_ns(core: &Core, now_ns: u64) -> u64 {
    if core.core.needs_fast_probes(now_ns) {
        50_000_000
    } else {
        250_000_000
    }
}
pub fn core_read(core: &Core, now_ns: u64) -> Reading {
    core.core.view.evaluate(now_ns).into()
}
pub fn core_read_for_presentation(core: &Core, now_ns: u64, delay_ns: u64) -> Result<Reading> {
    Ok(core
        .core
        .view
        .evaluate_for_presentation(now_ns, delay_ns)
        .map_err(error)?
        .into())
}

/// Owned timing state. Capture/copy returns a new foreign allocation; capture_into
/// reuses an existing handle. Evaluation never refreshes the captured lifecycle state.
pub struct TimecodeSnapshot {
    inner: protocol::TimecodeSnapshot,
}
pub fn timecode_snapshot_new() -> TimecodeSnapshot {
    TimecodeSnapshot {
        inner: Default::default(),
    }
}
pub fn core_build_id() -> String {
    protocol::CORE_BUILD_ID.into()
}
pub fn timecode_snapshot_copy(snapshot: &TimecodeSnapshot) -> TimecodeSnapshot {
    TimecodeSnapshot {
        inner: snapshot.inner,
    }
}
pub fn timecode_snapshot_read(snapshot: &TimecodeSnapshot, now_ns: u64) -> Reading {
    snapshot.inner.evaluate(now_ns).into()
}
pub fn timecode_snapshot_read_sample(
    snapshot: &TimecodeSnapshot,
    origin_ns: u64,
    sample_index: u64,
    sample_rate: u32,
) -> Result<Reading> {
    Ok(snapshot
        .inner
        .evaluate_sample(origin_ns, sample_index, sample_rate)
        .map_err(error)?
        .into())
}

/// Epoch bridge for same-rate host timestamps, not an independent device clock.
pub struct ClockBridge {
    inner: protocol::ClockBridge,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct OutputTime {
    pub local_ns: u64,
    pub uncertainty_ns: f64,
}
pub fn clock_bridge_new(
    local_before_ns: u64,
    external_ns: u64,
    local_after_ns: u64,
) -> Result<ClockBridge> {
    Ok(ClockBridge {
        inner: protocol::ClockBridge::new(local_before_ns, external_ns, local_after_ns)
            .map_err(error)?,
    })
}
pub fn clock_bridge_convert(bridge: &ClockBridge, external_ns: u64) -> Result<OutputTime> {
    let time = bridge.inner.convert(external_ns).map_err(error)?;
    Ok(OutputTime {
        local_ns: time.local_ns,
        uncertainty_ns: time.uncertainty_ns,
    })
}
pub fn timecode_snapshot_read_for_presentation(
    snapshot: &TimecodeSnapshot,
    now_ns: u64,
    delay_ns: u64,
) -> Result<Reading> {
    Ok(snapshot
        .inner
        .evaluate_for_presentation(now_ns, delay_ns)
        .map_err(error)?
        .into())
}
pub fn timecode_snapshot_next_boundary(snapshot: &TimecodeSnapshot, now_ns: u64) -> Boundary {
    boundary(snapshot.inner.next_boundary(now_ns))
}
pub fn core_snapshot(core: &Core) -> TimecodeSnapshot {
    TimecodeSnapshot {
        inner: core.core.view.into(),
    }
}
pub fn core_snapshot_into(core: &Core, snapshot: &mut TimecodeSnapshot) {
    snapshot.inner = core.core.view.into();
}

#[cfg(feature = "native")]
pub struct Engine {
    inner: std::sync::Arc<tidkod::Engine>,
}
#[cfg(feature = "native")]
pub struct LeaderOptions {
    inner: tidkod::LeaderConfig,
}
#[cfg(feature = "native")]
pub struct FollowerOptions {
    inner: tidkod::FollowerConfig,
}
#[cfg(feature = "native")]
pub struct Leader {
    inner: tidkod::Leader,
    _engine: std::sync::Arc<tidkod::Engine>,
    advertisement: tidkod::LeaderConfig,
}
#[cfg(feature = "native")]
pub struct Follower {
    inner: tidkod::Follower,
    _engine: std::sync::Arc<tidkod::Engine>,
}
#[cfg(feature = "native")]
pub struct Reader {
    inner: tidkod::TimecodeReader,
}
#[cfg(feature = "native")]
pub struct Discovery {
    inner: tidkod::Discovery,
    _engine: std::sync::Arc<tidkod::Engine>,
    leaders: Vec<tidkod::DiscoveredLeader>,
}
#[cfg(feature = "native")]
pub fn engine_new() -> Result<Engine> {
    Ok(Engine {
        inner: std::sync::Arc::new(tidkod::Engine::new().map_err(error)?),
    })
}
#[cfg(feature = "native")]
pub fn engine_now(engine: &Engine) -> u64 {
    engine.inner.clock().now_ns()
}
#[cfg(feature = "native")]
pub fn engine_shutdown(engine: &Engine) -> Result<()> {
    engine.inner.shutdown().map_err(error)
}
#[cfg(feature = "native")]
pub fn leader_options_new() -> LeaderOptions {
    LeaderOptions {
        inner: Default::default(),
    }
}
#[cfg(feature = "native")]
pub fn leader_options_bind(options: &mut LeaderOptions, address: &str) -> Result<()> {
    options.inner.bind = address.parse().map_err(error)?;
    Ok(())
}
#[cfg(feature = "native")]
pub fn leader_options_name(options: &mut LeaderOptions, name: &str) {
    options.inner.name = name.into();
}
#[cfg(feature = "native")]
pub fn leader_options_advertise(options: &mut LeaderOptions, enabled: bool) {
    options.inner.advertise = enabled;
}
#[cfg(feature = "native")]
pub fn leader_options_capacity(options: &mut LeaderOptions, capacity: u32) {
    options.inner.max_followers = capacity as usize;
}
#[cfg(feature = "native")]
pub fn leader_options_format(
    options: &mut LeaderOptions,
    numerator: u32,
    denominator: u32,
    drop_frame: bool,
) -> Result<()> {
    options.inner.format = FrameFormat::new(numerator, denominator, drop_frame).map_err(error)?;
    Ok(())
}
#[cfg(feature = "native")]
pub fn leader_options_tracked(options: &mut LeaderOptions, tracked: bool) {
    options.inner.source_kind = if tracked {
        tidkod::SourceKind::Tracked
    } else {
        tidkod::SourceKind::Generated
    };
}
#[cfg(feature = "native")]
pub fn engine_leader(engine: &Engine, options: &LeaderOptions) -> Result<Leader> {
    Ok(Leader {
        inner: engine.inner.leader(options.inner.clone()).map_err(error)?,
        _engine: engine.inner.clone(),
        advertisement: options.inner.clone(),
    })
}
#[cfg(feature = "native")]
pub fn leader_address(leader: &Leader) -> String {
    leader.inner.info().address.to_string()
}
#[cfg(feature = "native")]
pub fn leader_fingerprint(leader: &Leader) -> String {
    leader.inner.info().fingerprint.clone()
}
#[cfg(feature = "native")]
pub fn leader_play(leader: &Leader) -> Result<()> {
    leader.inner.play().map_err(error)
}
#[cfg(feature = "native")]
pub fn leader_pause(leader: &Leader) -> Result<()> {
    leader.inner.pause().map_err(error)
}
#[cfg(feature = "native")]
pub fn leader_seek(leader: &Leader, frames: i64, subframe: u32) -> Result<()> {
    leader
        .inner
        .seek(Position { frames, subframe })
        .map_err(error)
}
#[cfg(feature = "native")]
pub fn leader_speed(leader: &Leader, numerator: i32, denominator: u32) -> Result<()> {
    leader
        .inner
        .speed(Rate::new(numerator, denominator).map_err(error)?)
        .map_err(error)
}
#[cfg(feature = "native")]
pub fn leader_set_transport(
    leader: &Leader,
    frames: i64,
    subframe: u32,
    rate_numerator: i32,
    rate_denominator: u32,
    scheduled: bool,
    effective_ns: u64,
) -> Result<()> {
    leader
        .inner
        .set_transport(
            Position { frames, subframe },
            Rate::new(rate_numerator, rate_denominator).map_err(error)?,
            scheduled.then_some(effective_ns),
        )
        .map_err(error)
}
#[cfg(feature = "native")]
pub fn leader_track(
    leader: &Leader,
    frames: i64,
    subframe: u32,
    timestamp_ns: u64,
    has_rate: bool,
    rate_numerator: i32,
    rate_denominator: u32,
    discontinuity: bool,
) -> Result<()> {
    leader
        .inner
        .track(tidkod::SourceSample {
            position: Position { frames, subframe },
            timestamp_ns,
            rate_hint: if has_rate {
                Some(Rate::new(rate_numerator, rate_denominator).map_err(error)?)
            } else {
                None
            },
            discontinuity,
        })
        .map_err(error)
}
#[cfg(feature = "native")]
pub fn leader_reader(leader: &Leader) -> Result<Reader> {
    Ok(Reader {
        inner: leader.inner.reader().map_err(error)?,
    })
}
#[cfg(feature = "native")]
pub fn leader_shutdown(leader: &Leader) -> Result<()> {
    leader.inner.shutdown().map_err(error)
}
#[cfg(feature = "native")]
pub fn follower_options_new(address: &str) -> Result<FollowerOptions> {
    Ok(FollowerOptions {
        inner: tidkod::FollowerConfig::direct(address.parse().map_err(error)?),
    })
}
#[cfg(feature = "native")]
pub fn follower_options_bind(options: &mut FollowerOptions, address: &str) -> Result<()> {
    options.inner.bind = address.parse().map_err(error)?;
    Ok(())
}
#[cfg(feature = "native")]
pub fn follower_options_pin(options: &mut FollowerOptions, fingerprint: &str) {
    options.inner.trust = if fingerprint.is_empty() {
        tidkod::Trust::TrustedLan
    } else {
        tidkod::Trust::Pinned(fingerprint.into())
    };
}
#[cfg(feature = "native")]
pub fn engine_follower(engine: &Engine, options: &FollowerOptions) -> Result<Follower> {
    Ok(Follower {
        inner: engine
            .inner
            .follower(options.inner.clone())
            .map_err(error)?,
        _engine: engine.inner.clone(),
    })
}
#[cfg(feature = "native")]
pub fn follower_reader(follower: &Follower) -> Result<Reader> {
    Ok(Reader {
        inner: follower.inner.reader().map_err(error)?,
    })
}
#[cfg(feature = "native")]
pub fn follower_reconnect(follower: &Follower) -> Result<()> {
    follower.inner.reconnect().map_err(error)
}
#[cfg(feature = "native")]
pub fn follower_shutdown(follower: &Follower) -> Result<()> {
    follower.inner.shutdown().map_err(error)
}
#[cfg(feature = "native")]
pub fn reader_read(reader: &mut Reader) -> Reading {
    reader.inner.read().into()
}
#[cfg(feature = "native")]
pub fn reader_read_at(reader: &mut Reader, now_ns: u64) -> Reading {
    reader.inner.read_at(now_ns).into()
}
#[cfg(feature = "native")]
pub fn reader_snapshot(reader: &mut Reader) -> TimecodeSnapshot {
    TimecodeSnapshot {
        inner: reader.inner.snapshot(),
    }
}
#[cfg(feature = "native")]
pub fn reader_snapshot_into(reader: &mut Reader, snapshot: &mut TimecodeSnapshot) {
    snapshot.inner = reader.inner.snapshot();
}
#[cfg(feature = "native")]
pub fn reader_read_for_presentation(
    reader: &mut Reader,
    now_ns: u64,
    delay_ns: u64,
) -> Result<Reading> {
    Ok(reader
        .inner
        .read_for_presentation_at(now_ns, std::time::Duration::from_nanos(delay_ns))
        .map_err(error)?
        .into())
}
#[cfg(feature = "native")]
pub fn engine_discovery(engine: &Engine) -> Result<Discovery> {
    Ok(Discovery {
        inner: engine.inner.discovery(Default::default()).map_err(error)?,
        _engine: engine.inner.clone(),
        leaders: Vec::new(),
    })
}
#[cfg(feature = "native")]
pub fn discovery_poll(discovery: &mut Discovery) -> u32 {
    discovery.leaders = discovery.inner.poll();
    discovery.leaders.len() as u32
}
#[cfg(feature = "native")]
pub fn discovery_name(discovery: &Discovery, index: u32) -> Result<String> {
    Ok(discovery
        .leaders
        .get(index as usize)
        .ok_or("discovery index")?
        .name
        .clone())
}
#[cfg(feature = "native")]
pub fn discovery_address(discovery: &Discovery, index: u32, address_index: u32) -> Result<String> {
    Ok(discovery
        .leaders
        .get(index as usize)
        .ok_or("discovery index")?
        .addresses
        .get(address_index as usize)
        .ok_or("address index")?
        .to_string())
}
#[cfg(feature = "native")]
pub fn discovery_fingerprint(discovery: &Discovery, index: u32) -> Result<String> {
    Ok(discovery
        .leaders
        .get(index as usize)
        .ok_or("discovery index")?
        .fingerprint
        .clone())
}
/// Predicted local monotonic boundary; valid=false means no crossing is known.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Boundary {
    pub valid: bool,
    pub local_ns: u64,
    pub uncertainty_ns: f64,
    pub frames: i64,
    pub subframe: u32,
    pub discontinuity: u64,
    pub kind: u8,
}
pub fn core_next_boundary(core: &Core, now_ns: u64) -> Boundary {
    boundary(core.core.view.next_boundary(now_ns))
}
fn boundary(value: Option<protocol::Boundary>) -> Boundary {
    match value {
        None => Boundary::default(),
        Some(b) => Boundary {
            valid: true,
            local_ns: b.local_deadline_ns,
            uncertainty_ns: b.uncertainty_ns,
            frames: b.position.frames,
            subframe: b.position.subframe,
            discontinuity: b.discontinuity,
            kind: b.kind as u8,
        },
    }
}
#[cfg(feature = "native")]
pub fn reader_next_boundary(reader: &mut Reader, now_ns: u64) -> Boundary {
    boundary(reader.inner.next_boundary_at(now_ns))
}
#[cfg(feature = "native")]
pub fn leader_options_identity(options: &mut LeaderOptions, identity: &str) {
    options.inner.identity = identity.into();
}
#[cfg(feature = "native")]
pub fn leader_options_interface(options: &mut LeaderOptions, name: &str) {
    options.inner.discovery.interfaces.push(name.into());
}
#[cfg(feature = "native")]
pub fn leader_options_address(options: &mut LeaderOptions, address: &str) -> Result<()> {
    options
        .inner
        .discovery
        .addresses
        .push(address.parse().map_err(error)?);
    Ok(())
}
#[cfg(feature = "native")]
pub fn leader_options_start(
    options: &mut LeaderOptions,
    frames: i64,
    subframe: u32,
    numerator: i32,
    denominator: u32,
) -> Result<()> {
    let rate = Rate::new(numerator, denominator).map_err(error)?;
    options.inner.position = Position { frames, subframe };
    options.inner.rate = rate;
    Ok(())
}
#[cfg(feature = "native")]
fn timing(
    acquisition_ns: u64,
    steady_ns: u64,
    heartbeat_ns: u64,
    tracked_ns: u64,
    source_timeout_ns: u64,
    connect_timeout_ns: u64,
    retry_min_ns: u64,
    retry_max_ns: u64,
) -> tidkod::Timing {
    use std::time::Duration as D;
    tidkod::Timing {
        acquisition_probe: D::from_nanos(acquisition_ns),
        steady_probe: D::from_nanos(steady_ns),
        heartbeat: D::from_nanos(heartbeat_ns),
        tracked_publish: D::from_nanos(tracked_ns),
        source_timeout: D::from_nanos(source_timeout_ns),
        connect_timeout: D::from_nanos(connect_timeout_ns),
        retry_min: D::from_nanos(retry_min_ns),
        retry_max: D::from_nanos(retry_max_ns),
    }
}
#[cfg(feature = "native")]
pub fn leader_options_timing(
    options: &mut LeaderOptions,
    acquisition_ns: u64,
    steady_ns: u64,
    heartbeat_ns: u64,
    tracked_ns: u64,
    source_timeout_ns: u64,
    connect_timeout_ns: u64,
    retry_min_ns: u64,
    retry_max_ns: u64,
) {
    options.inner.timing = timing(
        acquisition_ns,
        steady_ns,
        heartbeat_ns,
        tracked_ns,
        source_timeout_ns,
        connect_timeout_ns,
        retry_min_ns,
        retry_max_ns,
    );
}
#[cfg(feature = "native")]
pub fn follower_options_timing(
    options: &mut FollowerOptions,
    acquisition_ns: u64,
    steady_ns: u64,
    heartbeat_ns: u64,
    tracked_ns: u64,
    source_timeout_ns: u64,
    connect_timeout_ns: u64,
    retry_min_ns: u64,
    retry_max_ns: u64,
) {
    options.inner.timing = timing(
        acquisition_ns,
        steady_ns,
        heartbeat_ns,
        tracked_ns,
        source_timeout_ns,
        connect_timeout_ns,
        retry_min_ns,
        retry_max_ns,
    );
}
#[cfg(feature = "native")]
pub fn follower_options_correction(
    options: &mut FollowerOptions,
    slew_frames_per_second: f64,
    hard_threshold_frames: f64,
    confirmations: u32,
) -> Result<()> {
    options.inner.correction = timeline::CorrectionPolicy {
        settle_time_ns: 0,
        slew_frames_per_second,
        hard_threshold_frames,
        confirmations: confirmations.try_into().map_err(error)?,
    };
    Ok(())
}
#[cfg(feature = "native")]
pub fn follower_options_fallback(
    options: &mut FollowerOptions,
    frames: i64,
    subframe: u32,
    fps_numerator: u32,
    fps_denominator: u32,
    drop_frame: bool,
) -> Result<()> {
    let format = FrameFormat::new(fps_numerator, fps_denominator, drop_frame).map_err(error)?;
    options.inner.fallback_position = Position { frames, subframe };
    options.inner.fallback_format = format;
    Ok(())
}
#[cfg(feature = "native")]
pub fn discovery_address_count(discovery: &Discovery, index: u32) -> Result<u32> {
    Ok(discovery
        .leaders
        .get(index as usize)
        .ok_or("discovery index")?
        .addresses
        .len() as u32)
}
#[cfg(feature = "native")]
pub fn discovery_identity(discovery: &Discovery, index: u32) -> Result<String> {
    Ok(discovery
        .leaders
        .get(index as usize)
        .ok_or("discovery index")?
        .identity
        .clone())
}
#[cfg(feature = "native")]
pub fn discovery_version(discovery: &Discovery, index: u32) -> Result<u32> {
    Ok(discovery
        .leaders
        .get(index as usize)
        .ok_or("discovery index")?
        .protocol_version)
}
#[cfg(feature = "native")]
pub fn discovery_shutdown(discovery: &mut Discovery) -> Result<()> {
    discovery.inner.shutdown().map_err(error)
}
/// An owned event. kind: 0 none, 1 connection, 2 correction, 3 observation,
/// 4 source health, 5 error. Event strings are separate from the reading path.
#[cfg(feature = "native")]
pub struct Event {
    inner: Option<tidkod::Event>,
}
#[cfg(feature = "native")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct EventData {
    pub probe_lateness_ns: u64,
    pub kind: u8,
    pub state: u8,
    pub correction_kind: u8,
    pub discontinuity: u64,
    pub frames: f64,
    pub t1: u64,
    pub t2: u64,
    pub t3: u64,
    pub t4: u64,
    pub accepted: bool,
    pub publication_ns: u64,
    pub has_publication: bool,
    pub processing_delay_ns: u64,
    pub offset_ns: f64,
    pub drift_ppm: f64,
    pub uncertainty_ns: f64,
    pub accepted_observations: u64,
}
#[cfg(feature = "native")]
pub fn leader_event(leader: &Leader) -> Event {
    Event {
        inner: leader.inner.try_event(),
    }
}
#[cfg(feature = "native")]
pub fn follower_event(follower: &Follower) -> Event {
    Event {
        inner: follower.inner.try_event(),
    }
}
#[cfg(feature = "native")]
pub fn event_data(event: &Event) -> EventData {
    let mut data = EventData::default();
    match &event.inner {
        None => {}
        Some(tidkod::Event::Connection(state)) => {
            data.kind = 1;
            data.state = *state as u8;
        }
        Some(tidkod::Event::Correction(c)) => {
            data.kind = 2;
            match c {
                timeline::Correction::Discontinuity(d) => {
                    data.correction_kind = 1;
                    data.discontinuity = *d;
                }
                timeline::Correction::Slew { frames } => {
                    data.correction_kind = 2;
                    data.frames = *frames;
                }
                timeline::Correction::HardResync { frames } => {
                    data.correction_kind = 3;
                    data.frames = *frames;
                }
                timeline::Correction::NewSession => data.correction_kind = 4,
            }
        }
        Some(tidkod::Event::ClockObservation(o)) => {
            data.kind = 3;
            data.t1 = o.exchange.t1;
            data.t2 = o.exchange.t2;
            data.t3 = o.exchange.t3;
            data.t4 = o.exchange.t4;
            data.accepted = o.accepted;
            data.publication_ns = o.publication_ns.unwrap_or(0);
            data.has_publication = o.publication_ns.is_some();
            data.processing_delay_ns = o.processing_delay_ns;
            data.offset_ns = o.mapping.offset_ns;
            data.drift_ppm = o.mapping.drift * 1e6;
            data.uncertainty_ns = o.mapping.uncertainty_ns;
            data.accepted_observations = o.mapping.accepted_observations;
        }
        Some(tidkod::Event::SourceHealth(health)) => {
            data.kind = 4;
            data.state = *health as u8;
        }
        Some(tidkod::Event::Error(_)) => data.kind = 5,
        Some(tidkod::Event::ProbeTiming { lateness_ns }) => {
            data.kind = 6;
            data.probe_lateness_ns = *lateness_ns;
        }
    }
    data
}
#[cfg(feature = "native")]
pub fn event_message(event: &Event) -> String {
    match &event.inner {
        Some(tidkod::Event::Error(e)) => e.clone(),
        _ => String::new(),
    }
}
#[cfg(feature = "native")]
pub fn follower_options_diagnostics(options: &mut FollowerOptions, enabled: bool) {
    options.inner.clock_diagnostics = enabled;
}
#[cfg(feature = "native")]
pub struct DiscoveryOptions {
    inner: tidkod::DiscoveryConfig,
}
#[cfg(feature = "native")]
pub fn discovery_options_new() -> DiscoveryOptions {
    DiscoveryOptions {
        inner: Default::default(),
    }
}
#[cfg(feature = "native")]
pub fn discovery_options_interface(options: &mut DiscoveryOptions, name: &str) {
    options.inner.interfaces.push(name.into());
}
#[cfg(feature = "native")]
pub fn discovery_options_address(options: &mut DiscoveryOptions, address: &str) -> Result<()> {
    options
        .inner
        .addresses
        .push(address.parse().map_err(error)?);
    Ok(())
}
#[cfg(feature = "native")]
pub fn engine_discovery_configured(
    engine: &Engine,
    options: &DiscoveryOptions,
) -> Result<Discovery> {
    Ok(Discovery {
        inner: engine
            .inner
            .discovery(options.inner.clone())
            .map_err(error)?,
        _engine: engine.inner.clone(),
        leaders: Vec::new(),
    })
}
#[cfg(feature = "native")]
pub fn follower_options_discovered(
    discovery: &Discovery,
    index: u32,
    address_index: u32,
) -> Result<FollowerOptions> {
    let leader = discovery
        .leaders
        .get(index as usize)
        .ok_or("discovery index")?;
    let address = *leader
        .addresses
        .get(address_index as usize)
        .ok_or("address index")?;
    Ok(FollowerOptions {
        inner: tidkod::FollowerConfig::discovered(leader, address).map_err(error)?,
    })
}
/// Configure a runtime-free follower before processing packets. Rejects invalid
/// rates and correction policies with the same constraints as the native client.
pub fn core_configured(
    frames: i64,
    subframe: u32,
    fps_numerator: u32,
    fps_denominator: u32,
    drop_frame: bool,
    slew_frames_per_second: f64,
    hard_threshold_frames: f64,
    confirmations: u8,
) -> Result<Core> {
    let format =
        protocol::FrameFormat::new(fps_numerator, fps_denominator, drop_frame).map_err(error)?;
    if !slew_frames_per_second.is_finite()
        || slew_frames_per_second <= 0.
        || !hard_threshold_frames.is_finite()
        || hard_threshold_frames <= 0.
        || confirmations == 0
    {
        return Err("invalid correction policy".into());
    }
    let fallback = timeline::Timeline {
        format,
        anchor: timeline::Anchor {
            position: protocol::Position { frames, subframe },
            ..Default::default()
        },
        ..Default::default()
    };
    Ok(Core {
        core: timeline::FollowerCore::new(
            fallback,
            timeline::CorrectionPolicy {
                settle_time_ns: 0,
                slew_frames_per_second,
                hard_threshold_frames,
                confirmations,
            },
        ),
        probes: Default::default(),
    })
}

/// Lifetime scheduling high-water marks, including startup and handshakes.
#[cfg(feature = "native")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WorkerTiming {
    pub passes: u64,
    pub max_pass_ns: u64,
    pub max_deadline_lateness_ns: u64,
}
#[cfg(feature = "native")]
pub fn engine_worker_timing(engine: &Engine) -> WorkerTiming {
    let timing = engine.inner.worker_timing();
    WorkerTiming {
        passes: timing.passes,
        max_pass_ns: timing.max_pass_ns,
        max_deadline_lateness_ns: timing.max_deadline_lateness_ns,
    }
}

/// Immutable numeric IP endpoint. Scope IDs are numeric OS interface indices.
#[cfg(feature = "native")]
pub struct Endpoint {
    inner: std::net::SocketAddr,
}
#[cfg(feature = "native")]
pub fn endpoint_parse(address: &str) -> Result<Endpoint> {
    Ok(Endpoint {
        inner: address.parse().map_err(error)?,
    })
}
#[cfg(feature = "native")]
pub fn endpoint_ipv4(bytes: &[u8], port: u32) -> Result<Endpoint> {
    let bytes: [u8; 4] = bytes.try_into().map_err(error)?;
    let port = u16::try_from(port).map_err(error)?;
    Ok(Endpoint {
        inner: (std::net::Ipv4Addr::from(bytes), port).into(),
    })
}
#[cfg(feature = "native")]
pub fn endpoint_ipv6(bytes: &[u8], port: u32, scope_id: u32) -> Result<Endpoint> {
    let bytes: [u8; 16] = bytes.try_into().map_err(error)?;
    let port = u16::try_from(port).map_err(error)?;
    Ok(Endpoint {
        inner: std::net::SocketAddrV6::new(bytes.into(), port, 0, scope_id).into(),
    })
}
#[cfg(feature = "native")]
pub fn endpoint_loopback(port: u32) -> Result<Endpoint> {
    endpoint_ipv4(&[127, 0, 0, 1], port)
}
#[cfg(feature = "native")]
pub fn endpoint_any_ipv4(port: u32) -> Result<Endpoint> {
    endpoint_ipv4(&[0, 0, 0, 0], port)
}
#[cfg(feature = "native")]
pub fn endpoint_address(endpoint: &Endpoint) -> String {
    endpoint.inner.to_string()
}
#[cfg(feature = "native")]
pub fn endpoint_port(endpoint: &Endpoint) -> u32 {
    endpoint.inner.port().into()
}
#[cfg(feature = "native")]
pub fn leader_options_bind_endpoint(options: &mut LeaderOptions, endpoint: &Endpoint) {
    options.inner.bind = endpoint.inner;
}
#[cfg(feature = "native")]
pub fn follower_options_bind_endpoint(options: &mut FollowerOptions, endpoint: &Endpoint) {
    options.inner.bind = endpoint.inner;
}
#[cfg(feature = "native")]
pub fn follower_options_endpoint(endpoint: &Endpoint) -> Result<FollowerOptions> {
    if endpoint.inner.port() == 0 || endpoint.inner.ip().is_unspecified() {
        return Err("a follower needs a concrete IP address and nonzero port".into());
    }
    Ok(FollowerOptions {
        inner: tidkod::FollowerConfig::direct(endpoint.inner),
    })
}
#[cfg(feature = "native")]
pub fn leader_endpoint(leader: &Leader) -> Endpoint {
    Endpoint {
        inner: leader.inner.info().address,
    }
}

/// Stable owned result of one local interface enumeration.
#[cfg(feature = "native")]
pub struct EndpointList {
    inner: Vec<std::net::SocketAddr>,
}
#[cfg(feature = "native")]
pub fn leader_local_endpoints(leader: &Leader) -> Result<EndpointList> {
    Ok(EndpointList {
        inner: leader.inner.info().local_endpoints().map_err(error)?,
    })
}
#[cfg(feature = "native")]
pub fn endpoint_list_count(list: &EndpointList) -> u32 {
    list.inner.len() as u32
}
#[cfg(feature = "native")]
pub fn endpoint_list_get(list: &EndpointList, index: u32) -> Result<Endpoint> {
    Ok(Endpoint {
        inner: *list
            .inner
            .get(index as usize)
            .ok_or("endpoint index out of range")?,
    })
}

/// UUID represented by two big-endian 64-bit halves, preserving all 128 bits.
#[cfg(feature = "native")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SessionId {
    pub high: u64,
    pub low: u64,
}
#[cfg(feature = "native")]
fn session_id_bytes(high: u64, low: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&high.to_be_bytes());
    bytes[8..].copy_from_slice(&low.to_be_bytes());
    bytes
}
#[cfg(feature = "native")]
pub fn leader_options_session_id(options: &mut LeaderOptions, high: u64, low: u64) {
    options.inner.session_id = Some(session_id_bytes(high, low));
}
#[cfg(feature = "native")]
pub fn leader_set_session_id(leader: &Leader, high: u64, low: u64) -> Result<()> {
    leader
        .inner
        .set_session_id(session_id_bytes(high, low))
        .map_err(error)
}
#[cfg(feature = "native")]
pub fn leader_rotate_session_id(leader: &Leader) -> Result<SessionId> {
    let id = leader.inner.rotate_session_id().map_err(error)?;
    Ok(SessionId {
        high: u64::from_be_bytes(id[..8].try_into().unwrap()),
        low: u64::from_be_bytes(id[8..].try_into().unwrap()),
    })
}

/// Platform-adapter factory. Retains the requested advertisement configuration,
/// but delegates registration to the caller rather than starting Rust mDNS.
#[cfg(feature = "native")]
pub fn engine_leader_external_discovery(
    engine: &Engine,
    options: &LeaderOptions,
) -> Result<Leader> {
    let mut config = options.inner.clone();
    config.advertise = false;
    Ok(Leader {
        inner: engine.inner.leader(config).map_err(error)?,
        _engine: engine.inner.clone(),
        advertisement: options.inner.clone(),
    })
}
/// Immutable registration metadata; reading it never starts a discovery daemon.
#[cfg(feature = "native")]
pub struct LeaderAdvertisement {
    config: tidkod::LeaderConfig,
    info: tidkod::LeaderInfo,
}
#[cfg(feature = "native")]
pub fn leader_advertisement(leader: &Leader) -> LeaderAdvertisement {
    LeaderAdvertisement {
        config: leader.advertisement.clone(),
        info: leader.inner.info().clone(),
    }
}
#[cfg(feature = "native")]
pub fn leader_advertisement_enabled(info: &LeaderAdvertisement) -> bool {
    info.config.advertise
}
#[cfg(feature = "native")]
pub fn leader_advertisement_name(info: &LeaderAdvertisement) -> String {
    info.config.name.clone()
}
#[cfg(feature = "native")]
pub fn leader_advertisement_identity(info: &LeaderAdvertisement) -> String {
    info.info.identity.clone()
}
#[cfg(feature = "native")]
pub fn leader_advertisement_fingerprint(info: &LeaderAdvertisement) -> String {
    info.info.fingerprint.clone()
}
#[cfg(feature = "native")]
pub fn leader_advertisement_bind_address(info: &LeaderAdvertisement) -> String {
    info.info.address.to_string()
}
#[cfg(feature = "native")]
pub fn leader_advertisement_hostname(info: &LeaderAdvertisement) -> String {
    let id: String = info
        .info
        .session
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("tidkod-{id}.local.")
}
#[cfg(feature = "native")]
pub fn leader_advertisement_instance(info: &LeaderAdvertisement) -> String {
    let mut name = info.config.name.clone();
    while name.len() > 54 {
        name.pop();
    }
    let suffix: String = info.info.session[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{name}-{suffix}")
}
#[cfg(feature = "native")]
pub fn leader_advertisement_interface_count(info: &LeaderAdvertisement) -> u32 {
    info.config.discovery.interfaces.len() as u32
}
#[cfg(feature = "native")]
pub fn leader_advertisement_interface(info: &LeaderAdvertisement, index: u32) -> Result<String> {
    info.config
        .discovery
        .interfaces
        .get(index as usize)
        .cloned()
        .ok_or_else(|| "interface index".into())
}
#[cfg(feature = "native")]
pub fn leader_advertisement_address_count(info: &LeaderAdvertisement) -> u32 {
    info.config.discovery.addresses.len() as u32
}
#[cfg(feature = "native")]
pub fn leader_advertisement_address(info: &LeaderAdvertisement, index: u32) -> Result<String> {
    info.config
        .discovery
        .addresses
        .get(index as usize)
        .map(ToString::to_string)
        .ok_or_else(|| "address index".into())
}
#[cfg(feature = "native")]
pub fn discovery_options_interface_count(options: &DiscoveryOptions) -> u32 {
    options.inner.interfaces.len() as u32
}
#[cfg(feature = "native")]
pub fn discovery_options_interface_name(options: &DiscoveryOptions, index: u32) -> Result<String> {
    options
        .inner
        .interfaces
        .get(index as usize)
        .cloned()
        .ok_or_else(|| "interface index".into())
}
/// Use the same discovered-endpoint validation for platform-owned discovery.
#[cfg(feature = "native")]
pub fn follower_options_resolved(
    address: &str,
    fingerprint: &str,
    version: u32,
) -> Result<FollowerOptions> {
    let address = address.parse().map_err(error)?;
    let discovered = tidkod::DiscoveredLeader {
        service_name: String::new(),
        identity: String::new(),
        name: String::new(),
        protocol_version: version,
        addresses: vec![address],
        fingerprint: fingerprint.into(),
    };
    Ok(FollowerOptions {
        inner: tidkod::FollowerConfig::discovered(&discovered, address).map_err(error)?,
    })
}

/// LTC processing result. Processing never allocates; construction may allocate.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LtcResult {
    pub consumed: u64,
    pub state: u8,
    pub has_frame: bool,
    pub frames: i64,
    pub user_bits: u32,
    pub binary_group_flags: u8,
    pub color_frame: bool,
    pub polarity_valid: bool,
    pub start_sample: u64,
    pub end_sample: u64,
    pub start_ns: u64,
    pub end_ns: u64,
    pub has_observation: bool,
    pub source_frames: i64,
    pub discontinuity: bool,
}
impl From<protocol::ltc::DecodeResult> for LtcResult {
    fn from(r: protocol::ltc::DecodeResult) -> Self {
        let mut out = Self {
            consumed: r.consumed as u64,
            state: r.state as u8,
            ..Default::default()
        };
        if let Some(f) = r.frame {
            out.has_frame = true;
            out.frames = f.position.frames;
            out.user_bits = f.metadata.user_bits;
            out.binary_group_flags = f.metadata.binary_group_flags;
            out.color_frame = f.metadata.color_frame;
            out.polarity_valid = f.polarity_valid;
            out.start_sample = f.start_sample;
            out.end_sample = f.end_sample;
            out.start_ns = f.start_ns;
            out.end_ns = f.end_ns;
        }
        if let Some(s) = r.observation {
            out.has_observation = true;
            out.source_frames = s.position.frames;
            out.discontinuity = s.discontinuity;
        }
        out
    }
}
fn ltc_config(
    numerator: u32,
    denominator: u32,
    drop_frame: bool,
    sample_rate: u32,
) -> Result<protocol::ltc::LtcConfig> {
    protocol::ltc::LtcConfig::new(
        protocol::FrameFormat::new(numerator, denominator, drop_frame).map_err(error)?,
        sample_rate,
    )
    .map_err(error)
}
pub struct LtcEncoder {
    inner: protocol::ltc::LtcEncoder,
}
pub fn ltc_encoder_new(
    numerator: u32,
    denominator: u32,
    drop_frame: bool,
    sample_rate: u32,
    frames: i64,
) -> Result<LtcEncoder> {
    Ok(LtcEncoder {
        inner: protocol::ltc::LtcEncoder::new(
            ltc_config(numerator, denominator, drop_frame, sample_rate)?,
            protocol::Position::from_frames(frames),
        ),
    })
}
pub fn ltc_encoder_reset(value: &mut LtcEncoder, frames: i64) {
    value.inner.reset(protocol::Position::from_frames(frames));
}
pub fn ltc_encoder_set_amplitude(value: &mut LtcEncoder, amplitude: f32) -> bool {
    value.inner.set_amplitude(amplitude)
}
pub fn ltc_encoder_set_metadata(
    value: &mut LtcEncoder,
    user_bits: u32,
    binary_group_flags: u8,
    color_frame: bool,
) {
    value.inner.set_metadata(protocol::ltc::LtcMetadata {
        user_bits,
        binary_group_flags,
        color_frame,
    });
}
pub struct LtcDecoder {
    inner: protocol::ltc::LtcDecoder,
}
pub fn ltc_decoder_new(
    numerator: u32,
    denominator: u32,
    drop_frame: bool,
    sample_rate: u32,
) -> Result<LtcDecoder> {
    Ok(LtcDecoder {
        inner: protocol::ltc::LtcDecoder::new(ltc_config(
            numerator,
            denominator,
            drop_frame,
            sample_rate,
        )?),
    })
}
pub fn ltc_decoder_reset(value: &mut LtcDecoder) {
    value.inner.reset();
}
pub fn ltc_decoder_process(
    value: &mut LtcDecoder,
    samples: &[f32],
    first_sample: u64,
    first_sample_ns: u64,
) -> LtcResult {
    value
        .inner
        .process(
            samples,
            protocol::ltc::SampleBlock {
                first_sample,
                first_sample_ns,
            },
        )
        .into()
}
pub struct LtcInput {
    inner: protocol::ltc::LtcInput,
}
pub fn ltc_input_new(
    numerator: u32,
    denominator: u32,
    drop_frame: bool,
    sample_rate: u32,
) -> Result<LtcInput> {
    Ok(LtcInput {
        inner: protocol::ltc::LtcInput::new(ltc_config(
            numerator,
            denominator,
            drop_frame,
            sample_rate,
        )?),
    })
}
pub fn ltc_input_reset(value: &mut LtcInput, frames: i64) {
    value.inner.reset(protocol::Position::from_frames(frames));
}
pub fn ltc_input_process(
    value: &mut LtcInput,
    samples: &[f32],
    first_sample: u64,
    first_sample_ns: u64,
) -> LtcResult {
    value
        .inner
        .process(
            samples,
            protocol::ltc::SampleBlock {
                first_sample,
                first_sample_ns,
            },
        )
        .into()
}
pub struct LtcOutput {
    inner: protocol::ltc::LtcOutput,
}
pub fn ltc_output_new(
    numerator: u32,
    denominator: u32,
    drop_frame: bool,
    sample_rate: u32,
) -> Result<LtcOutput> {
    Ok(LtcOutput {
        inner: protocol::ltc::LtcOutput::new(ltc_config(
            numerator,
            denominator,
            drop_frame,
            sample_rate,
        )?),
    })
}
pub fn ltc_output_reset(value: &mut LtcOutput) {
    value.inner.reset();
}
pub fn ltc_output_set_amplitude(value: &mut LtcOutput, amplitude: f32) -> bool {
    value.inner.set_amplitude(amplitude)
}
pub fn ltc_output_set_metadata(
    value: &mut LtcOutput,
    user_bits: u32,
    binary_group_flags: u8,
    color_frame: bool,
) {
    value.inner.set_metadata(protocol::ltc::LtcMetadata {
        user_bits,
        binary_group_flags,
        color_frame,
    });
}

pub fn ltc_encoder_render(value: &mut LtcEncoder, samples: &mut [f32]) -> u8 {
    value.inner.render(samples) as u8
}
pub fn ltc_output_render(
    value: &mut LtcOutput,
    snapshot: &TimecodeSnapshot,
    samples: &mut [f32],
    first_sample: u64,
    first_sample_ns: u64,
) -> u8 {
    value.inner.render(
        &snapshot.inner,
        protocol::ltc::SampleBlock {
            first_sample,
            first_sample_ns,
        },
        samples,
    ) as u8
}
pub fn ltc_output_mute(value: &mut LtcOutput, muted: bool) {
    value.inner.mute(muted);
}
pub fn ltc_input_set_timeout_ns(value: &mut LtcInput, timeout_ns: u64) {
    value.inner.set_timeout_ns(timeout_ns);
}
pub fn ltc_input_read(value: &LtcInput, now_ns: u64) -> Reading {
    value.inner.read(now_ns).into()
}
pub fn ltc_input_state(value: &LtcInput, now_ns: u64) -> u8 {
    value.inner.state(now_ns) as u8
}
pub fn ltc_input_capture_into(value: &LtcInput, snapshot: &mut TimecodeSnapshot, now_ns: u64) {
    snapshot.inner = value.inner.snapshot(now_ns);
}
