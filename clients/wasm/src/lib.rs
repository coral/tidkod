//! Thin browser binding; parsing and synchronization live in tidkod-protocol.
use tidkod_protocol::{
    CorrectionPolicy, decode_snapshot,
    probes::Probes,
    timeline::{FollowerCore, Timeline, View},
};
use wasm_bindgen::prelude::*;
#[wasm_bindgen]
pub fn core_build_id() -> String {
    tidkod_protocol::CORE_BUILD_ID.into()
}

fn timestamp(ms: f64) -> Result<u64, JsValue> {
    if !ms.is_finite() || !(0.0..=9_223_372_036_854.0).contains(&ms) {
        return Err(JsValue::from_str(
            "timestamp must be finite nonnegative monotonic milliseconds within the signed nanosecond range",
        ));
    }
    Ok((ms * 1e6).round() as u64)
}
fn error(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

#[wasm_bindgen]
pub struct Follower {
    core: FollowerCore,
    probes: Probes,
    last_event: String,
}
#[wasm_bindgen]
impl Follower {
    pub fn build_id(&self) -> String {
        core_build_id()
    }
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            core: FollowerCore::new(Timeline::default(), CorrectionPolicy::default()),
            probes: Probes::default(),
            last_event: String::new(),
        }
    }
    /// Explicit compatibility with the original slow fixed-slew policy.
    pub fn legacy() -> Self {
        let mut follower = Self::new();
        follower.core.policy = CorrectionPolicy::legacy();
        follower
    }
    pub fn connecting(&mut self) {
        self.core.view.connection = tidkod_protocol::ConnectionState::Connecting;
    }
    pub fn connected(&mut self) {
        self.probes = Probes::default();
        self.core.connected();
    }
    pub fn disconnected(&mut self) {
        self.core.disconnected();
    }
    pub fn snapshot(&mut self, bytes: &[u8], now_ms: f64) -> Result<(), JsValue> {
        let now = timestamp(now_ms)?;
        let state = Timeline::from_wire(&decode_snapshot(bytes).map_err(error)?).map_err(error)?;
        if let Some(e) = self.core.state(state, now) {
            self.last_event = format!("{e:?}");
        }
        Ok(())
    }
    pub fn probe(&mut self, now_ms: f64) -> Result<Vec<u8>, JsValue> {
        self.probes.request(timestamp(now_ms)?).map_err(error)
    }
    pub fn probe_published(&mut self, now_ms: f64) -> Result<(), JsValue> {
        self.probes.publication_finished(timestamp(now_ms)?);
        Ok(())
    }
    pub fn reply(&mut self, bytes: &[u8], now_ms: f64) -> Result<(), JsValue> {
        self.reply_timed(bytes, now_ms, now_ms)
    }
    pub fn reply_timed(
        &mut self,
        bytes: &[u8],
        received_ms: f64,
        processed_ms: f64,
    ) -> Result<(), JsValue> {
        let received = timestamp(received_ms)?;
        let processed = timestamp(processed_ms)?;
        if processed < received {
            return Err(error("processing timestamp precedes receipt"));
        }
        if let Some(exchange) = self.probes.reply(bytes, received).map_err(error)?
            && let Some(e) =
                self.core
                    .measurement_timed(exchange, self.probes.last_publication_ns(), processed)
        {
            self.last_event = format!("{e:?}");
        }
        Ok(())
    }
    /// Last 128 matched exchanges. Decimal strings preserve all nanosecond timestamp bits.
    pub fn clock_trace(&self) -> Result<JsValue, JsValue> {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Observation {
            t1: String,
            t2: String,
            t3: String,
            t4: String,
            accepted: bool,
            accepted_observations: String,
            publication_ns: Option<String>,
            processing_delay_ns: String,
            offset_ns: f64,
            drift_ppm: f64,
            evidence: Option<Evidence>,
        }
        let records: Vec<_> = self
            .core
            .estimator
            .trace()
            .map(|o| Observation {
                t1: o.exchange.t1.to_string(),
                t2: o.exchange.t2.to_string(),
                t3: o.exchange.t3.to_string(),
                t4: o.exchange.t4.to_string(),
                accepted: o.accepted,
                accepted_observations: o.mapping.accepted_observations.to_string(),
                publication_ns: o.publication_ns.map(|n| n.to_string()),
                processing_delay_ns: o.processing_delay_ns.to_string(),
                offset_ns: o.mapping.offset_ns,
                drift_ppm: o.mapping.drift * 1e6,
                evidence: o.mapping.evidence.map(Evidence::from),
            })
            .collect();
        serde_wasm_bindgen::to_value(&records).map_err(error)
    }
    /// Null/undefined when uninitialized or paused without a future control.
    pub fn next_boundary(&self, now_ms: f64) -> Result<JsValue, JsValue> {
        serialize_boundary(self.core.view, now_ms)
    }

    pub fn probe_interval_ms(&self) -> u32 {
        if self.core.needs_fast_probes(
            self.core
                .view
                .mapping
                .last_sample_ns
                .max(self.core.view.correction_at),
        ) {
            50
        } else {
            250
        }
    }
    pub fn probe_interval_at(&self, now_ms: f64) -> Result<u32, JsValue> {
        Ok(if self.core.needs_fast_probes(timestamp(now_ms)?) {
            50
        } else {
            250
        })
    }
    pub fn read(&mut self, now_ms: f64) -> Result<JsValue, JsValue> {
        self.read_for_presentation(now_ms, 0.)
    }
    /// Positive delay predicts ahead to presentation. Tick only at real now, never
    /// at the predicted time: future controls must not be latched prematurely.
    pub fn read_for_presentation(
        &mut self,
        now_ms: f64,
        compensation_delay_ms: f64,
    ) -> Result<JsValue, JsValue> {
        let now = timestamp(now_ms)?;
        let delay = timestamp(compensation_delay_ms)?;
        tidkod_protocol::timeline::presentation_time(now, delay).map_err(error)?;
        if let Some(e) = self.core.tick(now, 2_000_000_000) {
            self.last_event = format!("{e:?}");
        }
        serialize_reading(self.core.view, now, delay, &self.last_event)
    }
    /// Copy current state without ticking or refreshing it. Call read(nowMs) first
    /// when current staleness/lifecycle state is required. The snapshot owns its state.
    pub fn capture_snapshot(&self) -> TimecodeSnapshot {
        TimecodeSnapshot {
            view: self.core.view,
            event: self.last_event.clone(),
        }
    }
}
impl Default for Follower {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Evidence {
    lower_ms: f64,
    upper_ms: f64,
    samples: u32,
    consistent: bool,
}
impl From<tidkod_protocol::clock::OffsetEvidence> for Evidence {
    fn from(e: tidkod_protocol::clock::OffsetEvidence) -> Self {
        Self {
            lower_ms: e.lower_ns / 1e6,
            upper_ms: e.upper_ns / 1e6,
            samples: e.samples,
            consistent: e.consistent(),
        }
    }
}

/// Frozen timing state. Reads do not tick the follower or latch future controls.
#[wasm_bindgen]
pub struct TimecodeSnapshot {
    view: View,
    event: String,
}
#[wasm_bindgen]
impl TimecodeSnapshot {
    pub fn read(&self, now_ms: f64) -> Result<JsValue, JsValue> {
        self.read_for_presentation(now_ms, 0.)
    }
    pub fn read_for_presentation(&self, now_ms: f64, delay_ms: f64) -> Result<JsValue, JsValue> {
        serialize_reading(
            self.view,
            timestamp(now_ms)?,
            timestamp(delay_ms)?,
            &self.event,
        )
    }
    pub fn next_boundary(&self, now_ms: f64) -> Result<JsValue, JsValue> {
        serialize_boundary(self.view, now_ms)
    }
    pub fn read_sample(
        &self,
        origin_ms: f64,
        sample_index: u64,
        sample_rate: u32,
    ) -> Result<JsValue, JsValue> {
        let at = tidkod_protocol::sample_time(timestamp(origin_ms)?, sample_index, sample_rate)
            .map_err(error)?;
        serialize_reading(self.view, at, 0, &self.event)
    }
}

/// Browser-owned refresh estimate, NOT a compositor presentation timestamp.
/// Feed rAF's timestamp and performance.now() from the same callback. A gap
/// resets the estimate; late callbacks never predict into the past.
#[wasm_bindgen]
#[derive(Default)]
pub struct PresentationClock {
    previous: Option<f64>,
    intervals: [f64; 32],
    count: usize,
}
#[wasm_bindgen]
impl PresentationClock {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn target(&mut self, frame_ms: f64, now_ms: f64) -> Result<JsValue, JsValue> {
        timestamp(frame_ms)?;
        timestamp(now_ms)?;
        if let Some(previous) = self.previous {
            let interval = frame_ms - previous;
            if !(1. ..=100.).contains(&interval) {
                self.reset();
            } else {
                self.intervals[self.count % 32] = interval;
                self.count = self.count.saturating_add(1);
            }
        }
        self.previous = Some(frame_ms);
        let n = self.count.min(32);
        let mut intervals = self.intervals;
        intervals[..n].sort_by(f64::total_cmp);
        let estimated = n >= 8;
        let period = if estimated { intervals[n / 2] } else { 0. };
        let target = frame_ms + period;
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Target {
            local_ms: f64,
            estimated: bool,
            missed: bool,
            refresh_ms: f64,
        }
        serde_wasm_bindgen::to_value(&Target {
            local_ms: if estimated {
                target.max(now_ms)
            } else {
                now_ms
            },
            estimated,
            missed: estimated && target < now_ms,
            refresh_ms: period,
        })
        .map_err(error)
    }
}

fn serialize_boundary(view: View, now_ms: f64) -> Result<JsValue, JsValue> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Boundary {
        local_deadline_ms: f64,
        frames: String,
        subframe: u32,
        discontinuity: String,
        kind: String,
        uncertainty_ms: f64,
    }
    let b = view.next_boundary(timestamp(now_ms)?).map(|b| Boundary {
        local_deadline_ms: b.local_deadline_ns as f64 / 1e6,
        frames: b.position.frames.to_string(),
        subframe: b.position.subframe,
        discontinuity: b.discontinuity.to_string(),
        kind: format!("{:?}", b.kind),
        uncertainty_ms: b.uncertainty_ns / 1e6,
    });
    serde_wasm_bindgen::to_value(&b).map_err(error)
}

fn serialize_reading(view: View, now: u64, delay: u64, event: &str) -> Result<JsValue, JsValue> {
    let presentation = tidkod_protocol::timeline::presentation_time(now, delay).map_err(error)?;
    let r = view.evaluate_for_presentation(now, delay).map_err(error)?;
    let s = r.status;
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Reading<'a> {
        session_id: Option<String>,
        label: String,
        frames: f64,
        whole_frames: String,
        subframe: u32,
        speed: f64,
        fps: f64,
        connection: String,
        synchronization: String,
        source: String,
        health: String,
        uncertainty_ms: f64,
        sample_age_ms: f64,
        offset_ms: f64,
        drift_ppm: f64,
        mapped_leader_ms: f64,
        correction_frames: f64,
        alignment_error_ms: f64,
        aligned: bool,
        resync_generation: String,
        offset_evidence: Option<Evidence>,
        accepted_observations: String,
        discontinuity: String,
        event: &'a str,
    }
    serde_wasm_bindgen::to_value(&Reading {
        session_id: r.session_id.map(|id| {
            let hex = format!("{:032x}", u128::from_be_bytes(id));
            format!(
                "{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            )
        }),
        label: r.label().to_string(),
        frames: r.position.fixed() as f64 / 4294967296.,
        whole_frames: r.position.frames.to_string(),
        subframe: r.position.subframe,
        speed: r.rate.as_f64(),
        fps: r.format.fps(),
        connection: format!("{:?}", s.connection),
        synchronization: format!("{:?}", s.synchronization),
        source: format!("{:?}", s.source_kind),
        health: format!("{:?}", s.source_health),
        uncertainty_ms: s.uncertainty_ns / 1e6,
        sample_age_ms: s.sample_age_ns as f64 / 1e6,
        offset_ms: s.offset_ns / 1e6,
        drift_ppm: s.drift_ppm,
        mapped_leader_ms: view.mapping.leader_time(presentation) as f64 / 1e6,
        correction_frames: s.correction_frames,
        alignment_error_ms: s.alignment_error_ns / 1e6,
        aligned: s.aligned,
        resync_generation: s.resync_generation.to_string(),
        offset_evidence: s.offset_evidence.map(Evidence::from),
        accepted_observations: s.accepted_observations.to_string(),
        discontinuity: r.discontinuity.to_string(),
        event,
    })
    .map_err(error)
}

mod ltc;
pub use ltc::*;
