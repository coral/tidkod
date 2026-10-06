//! Streaming LTC samples, without devices, clocks, threads, or allocation.
//!
//! A block's timestamp is the capture/presentation time of its first sample.
//! Keep sample indices cumulative; timestamps may be refreshed from a device
//! clock each block. Frame timestamps describe bit zero, not decode completion.
//! Waveforms use normalized mono PCM; electrical interface compliance belongs
//! to the application. See docs/ltc.md for the complete timing contract.
use crate::{
    ConnectionState, Error, FrameFormat, Position, Rate, Reading, SourceHealth, SourceKind,
    SourceSample, SyncState, TimecodeSnapshot,
    clock::ClockMapping,
    sample_time,
    timeline::{Timeline, View},
    tracking::Tracker,
};
use broadcast_common::{Parse, Serialize};

/// Supported exact format and sample clock. Immutable after validation.
#[derive(Clone, Copy, Debug)]
pub struct LtcConfig {
    format: FrameFormat,
    sample_rate: u32,
}
impl LtcConfig {
    pub fn new(format: FrameFormat, sample_rate: u32) -> Result<Self, Error> {
        if format.nominal() > 30 {
            return Err(Error::Invalid("LTC supports 23.976 through 30 fps"));
        }
        if !(16000..=384000).contains(&sample_rate) {
            return Err(Error::Invalid("LTC sample rate must be 16000..=384000"));
        }
        Ok(Self {
            format,
            sample_rate,
        })
    }
    pub fn format(self) -> FrameFormat {
        self.format
    }
    pub fn sample_rate(self) -> u32 {
        self.sample_rate
    }
}
/// Opaque user nibbles, low nibble first, and SMPTE BGF bits (bits 0..2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LtcMetadata {
    pub user_bits: u32,
    pub binary_group_flags: u8,
    pub color_frame: bool,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct SampleBlock {
    pub first_sample: u64,
    pub first_sample_ns: u64,
}
impl SampleBlock {
    fn at(self, offset: usize, rate: u32) -> Option<Edge> {
        Some(Edge {
            index: self.first_sample.checked_add(offset as u64)?,
            ns: sample_time(self.first_sample_ns, offset as u64, rate).ok()?,
        })
    }
    /// Advance within a block while preserving its timestamp convention.
    pub fn advance(self, samples: usize, sample_rate: u32) -> Option<Self> {
        let e = self.at(samples, sample_rate)?;
        Some(Self {
            first_sample: e.index,
            first_sample_ns: e.ns,
        })
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum LtcState {
    #[default]
    Searching,
    Acquiring,
    Locked,
    Holdover,
    Muted,
    InvalidTiming,
    Unsupported,
}
#[derive(Clone, Copy, Debug)]
pub struct DecodedLtcFrame {
    pub position: Position,
    pub metadata: LtcMetadata,
    pub start_sample: u64,
    pub end_sample: u64,
    pub start_ns: u64,
    pub end_ns: u64,
    /// Polarity correction is reported, not treated as an error-correcting CRC.
    pub polarity_valid: bool,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct DecodeResult {
    pub consumed: usize,
    pub frame: Option<DecodedLtcFrame>,
    pub observation: Option<SourceSample>,
    pub state: LtcState,
}
fn word(config: LtcConfig, p: Position, m: LtcMetadata) -> [u8; 10] {
    let l = config.format.label(p);
    let mut f = st12_1::LtcFrame {
        hours: l.hours,
        minutes: l.minutes,
        seconds: l.seconds,
        frames: l.frames,
        drop_frame_flag: l.drop_frame,
        color_frame_flag: m.color_frame,
        flag_bit_27: false,
        flag_bit_43: false,
        flag_bit_58: m.binary_group_flags & 2 != 0,
        flag_bit_59: false,
        user_bits: std::array::from_fn(|i| (m.user_bits >> (4 * i)) as u8 & 15),
    };
    if config.format.nominal() == 25 {
        f.flag_bit_27 = m.binary_group_flags & 1 != 0;
        f.flag_bit_43 = m.binary_group_flags & 4 != 0;
    } else {
        f.flag_bit_43 = m.binary_group_flags & 1 != 0;
        f.flag_bit_59 = m.binary_group_flags & 4 != 0;
    }
    let mut bytes = [0; 10];
    // Fields are constructed from validated Tidkod labels and masked nibbles.
    let _ = f.serialize_into(&mut bytes);
    let parity_bit = if config.format.nominal() == 25 {
        59
    } else {
        27
    };
    // Excluding the correction bit, an odd number of zeros requires a one.
    let zeros = bytes[..8].iter().map(|b| b.count_zeros()).sum::<u32>() - 1;
    if zeros % 2 == 1 {
        bytes[parity_bit / 8] |= 1 << (parity_bit % 8);
    }
    bytes
}
fn unpack(c: LtcConfig, bytes: &[u8; 10], start: Edge, end: Edge) -> Option<DecodedLtcFrame> {
    let f = st12_1::LtcFrame::parse(bytes).ok()?;
    if f.drop_frame_flag != c.format.drop_frame() {
        return None;
    }
    let position = c
        .format
        .position(f.hours, f.minutes, f.seconds, f.frames)
        .ok()?;
    let rate = match c.format.nominal() {
        24 => st12_1::FrameRate::Fps24,
        25 => st12_1::FrameRate::Fps25,
        _ => st12_1::FrameRate::Fps30,
    };
    let flags = f.binary_group_flags(rate);
    Some(DecodedLtcFrame {
        position,
        metadata: LtcMetadata {
            user_bits: f
                .user_bits
                .iter()
                .enumerate()
                .fold(0, |v, (i, b)| v | ((*b as u32) << (4 * i))),
            binary_group_flags: u8::from(flags.bgf0)
                | (u8::from(flags.bgf1) << 1)
                | (u8::from(flags.bgf2) << 2),
            color_frame: f.color_frame_flag,
        },
        start_sample: start.index,
        end_sample: end.index,
        start_ns: start.ns,
        end_ns: end.ns,
        polarity_valid: bytes[..8].iter().map(|b| b.count_zeros()).sum::<u32>() % 2 == 1,
    })
}
#[derive(Default)]
struct Wave {
    cell: Option<i128>,
    bytes: [u8; 10],
    level: bool,
}
impl Wave {
    fn sample(&mut self, c: LtcConfig, p: Position, m: LtcMetadata, amplitude: f32) -> f32 {
        let cell = (p.fixed() * 160).div_euclid(1i128 << 32);
        if self.cell.is_some_and(|old| cell < old || cell > old + 1) {
            self.cell = None;
        }
        if self.cell != Some(cell) {
            let half = cell.rem_euclid(160) as usize;
            if half == 0 || self.cell.is_none() {
                self.bytes = word(c, p, m);
            }
            if half.is_multiple_of(2) || self.bytes[half / 16] & (1 << ((half / 2) % 8)) != 0 {
                self.level = !self.level;
            }
            self.cell = Some(cell);
        }
        if self.level { amplitude } else { -amplitude }
    }
}
/// Free-running, forward LTC. Reset starts a new codeword at the integer label.
pub struct LtcEncoder {
    config: LtcConfig,
    start: Position,
    sample: u64,
    wave: Wave,
    metadata: LtcMetadata,
    amplitude: f32,
}
impl LtcEncoder {
    pub fn new(config: LtcConfig, position: Position) -> Self {
        Self {
            config,
            start: Position::from_frames(position.frames),
            sample: 0,
            wave: Wave::default(),
            metadata: LtcMetadata::default(),
            amplitude: 0.5,
        }
    }
    pub fn reset(&mut self, position: Position) {
        self.start = Position::from_frames(position.frames);
        self.sample = 0;
        self.wave = Wave::default();
    }
    pub fn set_metadata(&mut self, metadata: LtcMetadata) {
        self.metadata = metadata;
    }
    pub fn set_amplitude(&mut self, amplitude: f32) -> bool {
        if amplitude.is_finite() && (0.0..=1.0).contains(&amplitude) {
            self.amplitude = amplitude;
            true
        } else {
            false
        }
    }
    pub fn render(&mut self, out: &mut [f32]) -> LtcState {
        if self.sample.checked_add(out.len() as u64).is_none() {
            out.fill(0.);
            return LtcState::InvalidTiming;
        }
        let den =
            i128::from(self.config.sample_rate) * i128::from(self.config.format.denominator());
        for value in out {
            let delta = i128::from(self.sample)
                * i128::from(self.config.format.numerator())
                * (1i128 << 32)
                / den;
            *value = self.wave.sample(
                self.config,
                Position::from_fixed(self.start.fixed() + delta),
                self.metadata,
                self.amplitude,
            );
            self.sample += 1;
        }
        LtcState::Locked
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct Edge {
    index: u64,
    ns: u64,
}
/// Biphase state machine adapted from Danny Wensley's BSD-3-Clause ltc 0.2.0.
/// See ../licenses/ltc-BSD-3-Clause.txt. Uses exact edge differences, fixed
/// history and strict codewords instead of the upstream growable frame queue.
pub struct LtcDecoder {
    config: LtcConfig,
    expected: Option<u64>,
    last_ns: Option<u64>,
    level: bool,
    edge: Option<Edge>,
    half: Option<Edge>,
    period: f64,
    bits: u128,
    count: usize,
    starts: [Edge; 80],
    dc: f32,
    peak: f32,
}
impl LtcDecoder {
    pub fn new(config: LtcConfig) -> Self {
        Self {
            config,
            expected: None,
            last_ns: None,
            level: false,
            edge: None,
            half: None,
            period: f64::from(config.sample_rate) / config.format.fps() / 80.,
            bits: 0,
            count: 0,
            starts: [Edge::default(); 80],
            dc: 0.,
            peak: 0.,
        }
    }
    pub fn reset(&mut self) {
        *self = Self::new(self.config);
    }
    fn gap(&self, block: SampleBlock) -> bool {
        self.expected.is_some_and(|i| i != block.first_sample)
            || self.last_ns.is_some_and(|ns| {
                block.first_sample_ns <= ns || block.first_sample_ns - ns > 2_000_000
            })
    }
    fn lose(&mut self) {
        self.edge = None;
        self.half = None;
        self.bits = 0;
        self.count = 0;
    }
    pub fn process(&mut self, samples: &[f32], block: SampleBlock) -> DecodeResult {
        let mut result = DecodeResult::default();
        if samples.is_empty() {
            return result;
        }
        if block.at(samples.len(), self.config.sample_rate).is_none() {
            result.state = LtcState::InvalidTiming;
            return result;
        }
        if self.gap(block) {
            self.reset();
        }
        for (i, &sample) in samples.iter().enumerate() {
            let now = block.at(i, self.config.sample_rate).unwrap();
            self.expected = now.index.checked_add(1);
            self.last_ns = Some(now.ns);
            result.consumed = i + 1;
            if !sample.is_finite() {
                self.lose();
                self.dc = 0.;
                self.peak = 0.;
                continue;
            }
            let sample = sample.clamp(-1., 1.);
            self.dc += (sample - self.dc) * (10. / self.config.sample_rate as f32);
            let x = sample - self.dc;
            self.peak = (self.peak * 0.9999).max(x.abs());
            let threshold = (self.peak * 0.1).max(0.001);
            let level = if x > threshold {
                true
            } else if x < -threshold {
                false
            } else {
                self.level
            };
            if level == self.level {
                if self
                    .edge
                    .is_some_and(|e| now.index.saturating_sub(e.index) as f64 > self.period * 3.)
                {
                    self.lose();
                }
                continue;
            }
            self.level = level;
            let previous = self.edge.replace(now);
            let Some(previous) = previous else {
                continue;
            };
            let distance = (now.index - previous.index) as f64;
            if distance < self.period * 0.25 || distance > self.period * 1.5 {
                self.half = None;
                self.count = 0;
                self.bits = 0;
                continue;
            }
            let short = distance < self.period * 0.75;
            let nominal = f64::from(self.config.sample_rate) / self.config.format.fps() / 80.;
            self.period = (self.period * 0.98 + distance * if short { 0.04 } else { 0.02 })
                .clamp(nominal * 0.9, nominal * 1.1);
            let (bit, start) = if short {
                match self.half.take() {
                    Some(start) => (true, start),
                    None => {
                        self.half = Some(previous);
                        continue;
                    }
                }
            } else {
                if self.half.take().is_some() {
                    self.count = 0;
                    self.bits = 0;
                    continue;
                }
                (false, previous)
            };
            self.bits = (self.bits << 1) | u128::from(bit);
            self.starts[self.count % 80] = start;
            self.count += 1;
            if self.count >= 160 {
                self.count = 80;
            }
            if self.count < 80 || self.bits as u16 != 0x3ffd {
                continue;
            }
            let bytes = std::array::from_fn(|b| {
                (0..8).fold(0, |v, j| {
                    v | ((((self.bits >> (79 - (b * 8 + j))) & 1) as u8) << j)
                })
            });
            let start = self.starts[self.count % 80];
            if let Some(frame) = unpack(self.config, &bytes, start, now) {
                let duration = (now.index - start.index) as f64;
                let nominal = f64::from(self.config.sample_rate) / self.config.format.fps();
                if (duration / nominal - 1.).abs() <= 0.02 {
                    result.frame = Some(frame);
                    result.state = LtcState::Locked;
                    return result;
                }
            }
        }
        result.state = if self.count > 0 {
            LtcState::Acquiring
        } else {
            LtcState::Searching
        };
        result
    }
}
/// Normal-speed snapshot output. Reuse the handle across snapshot refreshes.
pub struct LtcOutput {
    config: LtcConfig,
    wave: Wave,
    previous: Option<Position>,
    phase: Position,
    phase_remainder: i128,
    identity: Option<(u64, u64)>,
    expected: Option<u64>,
    armed: bool,
    started: bool,
    muted: bool,
    metadata: LtcMetadata,
    amplitude: f32,
}
impl LtcOutput {
    pub fn new(config: LtcConfig) -> Self {
        Self {
            config,
            wave: Wave::default(),
            previous: None,
            phase: Position::ZERO,
            phase_remainder: 0,
            identity: None,
            expected: None,
            armed: false,
            started: false,
            muted: false,
            metadata: LtcMetadata::default(),
            amplitude: 0.5,
        }
    }
    pub fn reset(&mut self) {
        self.wave = Wave::default();
        self.previous = None;
        self.phase = Position::ZERO;
        self.phase_remainder = 0;
        self.identity = None;
        self.expected = None;
        self.armed = false;
        self.started = false;
    }
    pub fn mute(&mut self, muted: bool) {
        if muted != self.muted {
            self.reset();
            self.muted = muted;
        }
    }
    pub fn set_metadata(&mut self, metadata: LtcMetadata) {
        self.metadata = metadata;
    }
    pub fn set_amplitude(&mut self, amplitude: f32) -> bool {
        if amplitude.is_finite() && (0.0..=1.0).contains(&amplitude) {
            self.amplitude = amplitude;
            true
        } else {
            false
        }
    }
    pub fn render(
        &mut self,
        snapshot: &TimecodeSnapshot,
        block: SampleBlock,
        out: &mut [f32],
    ) -> LtcState {
        if block.at(out.len(), self.config.sample_rate).is_none() {
            out.fill(0.);
            return LtcState::InvalidTiming;
        }
        if self.expected.is_some_and(|x| x != block.first_sample) {
            self.reset();
        }
        self.expected = block.first_sample.checked_add(out.len() as u64);
        let mut state = LtcState::Muted;
        for (i, value) in out.iter_mut().enumerate() {
            let r = snapshot.evaluate(block.at(i, self.config.sample_rate).unwrap().ns);
            let identity = (r.discontinuity, r.status.resync_generation);
            if self.identity != Some(identity) {
                self.armed = false;
                self.previous = None;
                self.wave = Wave::default();
                self.identity = Some(identity);
            }
            let valid = r.format == self.config.format && r.rate == Rate::NORMAL;
            let ready = r.status.synchronization == SyncState::Synchronized
                || (self.started && r.status.synchronization != SyncState::Uninitialized);
            let boundary = self.previous.is_some_and(|p| {
                p.frames.checked_add(1) == Some(r.position.frames)
                    && r.position.fixed() - p.fixed() <= (1i128 << 31)
            }) || (self.previous.is_none() && r.position.subframe == 0);
            if self
                .previous
                .is_some_and(|p| (r.position.fixed() - p.fixed()).abs() > (1i128 << 31))
            {
                self.armed = false;
                self.wave = Wave::default();
            }
            if !valid || !ready || self.muted {
                self.armed = false;
            } else if !self.armed && boundary {
                self.armed = true;
                self.started = true;
                self.phase = r.position;
                self.phase_remainder = 0;
                self.wave = Wave::default();
            }
            self.previous = Some(r.position);
            if self.armed {
                *value = self
                    .wave
                    .sample(self.config, self.phase, self.metadata, self.amplitude);
                // Keep the emitted phase continuous across snapshot revisions.
                // Limit correction to 1% of nominal speed, safely inside the
                // decoder's 2% frame-duration tolerance, including quantization.
                let denominator = i128::from(self.config.sample_rate)
                    * i128::from(self.config.format.denominator());
                let numerator =
                    (i128::from(self.config.format.numerator()) << 32) + self.phase_remainder;
                let step = numerator / denominator;
                self.phase_remainder = numerator % denominator;
                let error = r.position.fixed() - self.phase.fixed();
                let correction = error.clamp(-step / 100, step / 100);
                self.phase = Position::from_fixed(self.phase.fixed() + step + correction);
                state = if r.status.synchronization == SyncState::Holdover
                    || r.status.source_health == SourceHealth::Degraded
                    || r.status.connection != ConnectionState::Connected
                {
                    LtcState::Holdover
                } else {
                    LtcState::Locked
                };
            } else {
                *value = 0.;
                state = if !valid {
                    LtcState::Unsupported
                } else {
                    LtcState::Muted
                };
            }
        }
        state
    }
}
/// Local sample source with three-frame acquisition and extrapolated holdover.
pub struct LtcInput {
    decoder: LtcDecoder,
    tracker: Tracker,
    timeline: Timeline,
    previous: Option<DecodedLtcFrame>,
    consecutive: u8,
    last: Option<SourceSample>,
    seed: Position,
    timeout_ns: u64,
    pending_discontinuity: bool,
}
impl LtcInput {
    pub fn new(config: LtcConfig) -> Self {
        Self {
            decoder: LtcDecoder::new(config),
            tracker: Tracker::default(),
            timeline: Timeline {
                format: config.format,
                source_kind: SourceKind::Tracked,
                ..Default::default()
            },
            previous: None,
            consecutive: 0,
            last: None,
            seed: Position::ZERO,
            timeout_ns: 500_000_000,
            pending_discontinuity: true,
        }
    }
    pub fn reset(&mut self, seed: Position) {
        let timeout = self.timeout_ns;
        *self = Self::new(self.decoder.config);
        self.seed = seed;
        self.timeout_ns = timeout;
    }
    pub fn set_timeout_ns(&mut self, timeout: u64) {
        self.timeout_ns = timeout;
    }
    pub fn process(&mut self, samples: &[f32], block: SampleBlock) -> DecodeResult {
        if samples.is_empty() {
            return DecodeResult {
                state: self.state(block.first_sample_ns),
                ..Default::default()
            };
        }
        let gap = self.decoder.gap(block);
        let mut result = self.decoder.process(samples, block);
        if result.state == LtcState::InvalidTiming {
            return result;
        }
        if gap {
            self.previous = None;
            self.consecutive = 0;
            self.pending_discontinuity = true;
        }
        if let Some(frame) = result.frame {
            let day = self.timeline.format.frames_per_day();
            let continuous = self.previous.is_some_and(|p| {
                frame.position.frames == (p.position.frames + 1) % day
                    && frame.start_sample == p.end_sample
                    && frame.start_ns > p.start_ns
            });
            if !continuous {
                self.pending_discontinuity = true;
            }
            self.consecutive = if continuous {
                self.consecutive.saturating_add(1).min(3)
            } else {
                1
            };
            self.previous = Some(frame);
            if self.consecutive >= 3 {
                let reference = self
                    .last
                    .map_or(self.seed, |_| self.timeline.at(frame.start_ns).0);
                let label = frame.position.frames;
                let days = (i128::from(reference.frames) - i128::from(label) + i128::from(day) / 2)
                    .div_euclid(i128::from(day));
                let position =
                    Position::from_fixed((i128::from(label) + days * i128::from(day)) << 32);
                let discontinuity = self.pending_discontinuity
                    || self.last.is_none()
                    || !self
                        .last
                        .is_some_and(|s| s.position.frames.checked_add(1) == Some(position.frames));
                let sample = SourceSample {
                    timestamp_ns: frame.start_ns,
                    position,
                    rate_hint: Some(Rate::NORMAL),
                    discontinuity,
                };
                if self
                    .tracker
                    .sample(&mut self.timeline, sample, frame.end_ns)
                    .is_ok()
                {
                    self.last = Some(sample);
                    result.observation = Some(sample);
                    self.pending_discontinuity = false;
                } else {
                    self.consecutive = 0;
                    self.pending_discontinuity = true;
                }
            }
        }
        result.state = self.state(
            block
                .at(result.consumed, self.decoder.config.sample_rate)
                .map_or(block.first_sample_ns, |e| e.ns),
        );
        result
    }
    pub fn state(&self, now_ns: u64) -> LtcState {
        if self.last.is_some() {
            if self.consecutive < 3
                || self.tracker.health(now_ns, self.timeout_ns) == SourceHealth::Degraded
            {
                LtcState::Holdover
            } else {
                LtcState::Locked
            }
        } else if self.consecutive > 0 {
            LtcState::Acquiring
        } else {
            LtcState::Searching
        }
    }
    pub fn view(&self, now_ns: u64) -> View {
        let state = self.state(now_ns);
        let healthy = state == LtcState::Locked;
        let mut timeline = self.timeline;
        timeline.source_health = if healthy {
            SourceHealth::Healthy
        } else {
            SourceHealth::Degraded
        };
        View {
            timeline,
            local_clock: true,
            connection: ConnectionState::Connected,
            sync: if self.last.is_none() {
                SyncState::Uninitialized
            } else if healthy {
                SyncState::Synchronized
            } else {
                SyncState::Holdover
            },
            mapping: ClockMapping {
                last_sample_ns: self.last.map_or(0, |s| s.timestamp_ns.max(1)),
                uncertainty_ns: 0.,
                converged: healthy,
                ..Default::default()
            },
            fallback_format: self.timeline.format,
            fallback_position: self.seed,
            ..Default::default()
        }
    }
    pub fn snapshot(&self, now_ns: u64) -> TimecodeSnapshot {
        self.view(now_ns).into()
    }
    pub fn read(&self, now_ns: u64) -> Reading {
        self.snapshot(now_ns).evaluate(now_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fractional_phase_remains_exact_after_a_day() {
        for (n, d, drop) in [
            (24000, 1001, false),
            (30000, 1001, false),
            (30000, 1001, true),
        ] {
            let c = LtcConfig::new(FrameFormat::new(n, d, drop).unwrap(), 44100).unwrap();
            let mut encoder = LtcEncoder::new(c, Position::ZERO);
            let first = 86400 * 44100;
            encoder.sample = first; // Skip the preceding day without rendering billions of samples.
            let mut samples = [0.; 8192];
            encoder.render(&mut samples);
            let mut decoder = LtcDecoder::new(c);
            let mut offset = 0;
            let mut count = 0;
            while offset < samples.len() {
                let index = first + offset as u64;
                let r = decoder.process(
                    &samples[offset..],
                    SampleBlock {
                        first_sample: index,
                        first_sample_ns: sample_time(1, index, 44100).unwrap(),
                    },
                );
                assert!(r.consumed > 0);
                offset += r.consumed;
                if let Some(frame) = r.frame {
                    let frames =
                        u128::from(frame.start_sample) * u128::from(n) / (44100 * u128::from(d));
                    assert_eq!(
                        frame.position.frames,
                        (frames as i64).rem_euclid(c.format.frames_per_day())
                    );
                    let boundary = frames * 44100 * u128::from(d) / u128::from(n);
                    assert!(frame.start_sample.abs_diff(boundary as u64) <= 1);
                    count += 1;
                }
            }
            assert!(count >= 3);
        }
    }
}
