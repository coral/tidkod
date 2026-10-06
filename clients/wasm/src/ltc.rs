//! Reusable WASM sample storage; process calls do not serialize JS objects.
use super::{TimecodeSnapshot, error, serialize_reading, timestamp};
use tidkod_protocol::{FrameFormat, Position, ltc as core};
use wasm_bindgen::prelude::*;
#[wasm_bindgen]
pub fn ltc_memory() -> JsValue {
    wasm_bindgen::memory()
}
/// Create typed views again if WASM memory grows. Never use a view after free.
#[wasm_bindgen]
pub struct LtcSampleBuffer {
    samples: Vec<f32>,
}
#[wasm_bindgen]
impl LtcSampleBuffer {
    #[wasm_bindgen(constructor)]
    pub fn new(capacity: u32) -> Result<Self, JsValue> {
        if capacity == 0 || capacity > 1_048_576 {
            return Err(JsValue::from_str("capacity must be 1..=1048576"));
        }
        Ok(Self {
            samples: vec![0.; capacity as usize],
        })
    }
    pub fn pointer(&mut self) -> *mut f32 {
        self.samples.as_mut_ptr()
    }
    pub fn capacity(&self) -> u32 {
        self.samples.len() as u32
    }
}
fn config(n: u32, d: u32, drop: bool, sr: u32) -> Result<core::LtcConfig, JsValue> {
    core::LtcConfig::new(FrameFormat::new(n, d, drop).map_err(error)?, sr).map_err(error)
}
#[wasm_bindgen]
#[derive(Clone, Copy, Default)]
pub struct LtcResult {
    pub consumed: u32,
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
impl From<core::DecodeResult> for LtcResult {
    fn from(r: core::DecodeResult) -> Self {
        let mut out = Self {
            consumed: r.consumed as u32,
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
#[wasm_bindgen]
pub struct LtcEncoder {
    inner: core::LtcEncoder,
}
#[wasm_bindgen]
impl LtcEncoder {
    #[wasm_bindgen(constructor)]
    pub fn new(
        numerator: u32,
        denominator: u32,
        drop_frame: bool,
        sample_rate: u32,
        frames: i64,
    ) -> Result<Self, JsValue> {
        Ok(Self {
            inner: core::LtcEncoder::new(
                config(numerator, denominator, drop_frame, sample_rate)?,
                Position::from_frames(frames),
            ),
        })
    }
    pub fn reset(&mut self, frames: i64) {
        self.inner.reset(Position::from_frames(frames));
    }
    pub fn set_metadata(&mut self, user_bits: u32, binary_group_flags: u8, color_frame: bool) {
        self.inner.set_metadata(core::LtcMetadata {
            user_bits,
            binary_group_flags,
            color_frame,
        });
    }
    pub fn set_amplitude(&mut self, amplitude: f32) -> bool {
        self.inner.set_amplitude(amplitude)
    }
    pub fn render(&mut self, buffer: &mut LtcSampleBuffer, len: u32) -> u8 {
        let Some(samples) = buffer.samples.get_mut(..len as usize) else {
            return core::LtcState::InvalidTiming as u8;
        };
        self.inner.render(samples) as u8
    }
}
#[wasm_bindgen]
pub struct LtcDecoder {
    inner: core::LtcDecoder,
    last: LtcResult,
}
#[wasm_bindgen]
impl LtcDecoder {
    #[wasm_bindgen(constructor)]
    pub fn new(
        numerator: u32,
        denominator: u32,
        drop_frame: bool,
        sample_rate: u32,
    ) -> Result<Self, JsValue> {
        Ok(Self {
            inner: core::LtcDecoder::new(config(numerator, denominator, drop_frame, sample_rate)?),
            last: LtcResult::default(),
        })
    }
    pub fn reset(&mut self) {
        self.inner.reset();
        self.last = LtcResult::default();
    }
    pub fn process(
        &mut self,
        buffer: &LtcSampleBuffer,
        offset: u32,
        len: u32,
        first_sample: u64,
        first_sample_ns: u64,
    ) -> u32 {
        let end = (offset as usize).checked_add(len as usize);
        let Some(samples) = end.and_then(|end| buffer.samples.get(offset as usize..end)) else {
            self.last = LtcResult {
                state: core::LtcState::InvalidTiming as u8,
                ..Default::default()
            };
            return 0;
        };
        self.last = self
            .inner
            .process(
                samples,
                core::SampleBlock {
                    first_sample,
                    first_sample_ns,
                },
            )
            .into();
        self.last.consumed
    }
    /// Allocates a JS wrapper; call outside a hard realtime processing loop.
    pub fn result(&self) -> LtcResult {
        self.last
    }
    pub fn state(&self) -> u8 {
        self.last.state
    }
    pub fn has_frame(&self) -> bool {
        self.last.has_frame
    }
}
#[wasm_bindgen]
pub struct LtcInput {
    inner: core::LtcInput,
    last: LtcResult,
}
#[wasm_bindgen]
impl LtcInput {
    #[wasm_bindgen(constructor)]
    pub fn new(
        numerator: u32,
        denominator: u32,
        drop_frame: bool,
        sample_rate: u32,
    ) -> Result<Self, JsValue> {
        Ok(Self {
            inner: core::LtcInput::new(config(numerator, denominator, drop_frame, sample_rate)?),
            last: LtcResult::default(),
        })
    }
    pub fn reset(&mut self, frames: i64) {
        self.inner.reset(Position::from_frames(frames));
        self.last = LtcResult::default();
    }
    pub fn process(
        &mut self,
        buffer: &LtcSampleBuffer,
        offset: u32,
        len: u32,
        first_sample: u64,
        first_sample_ns: u64,
    ) -> u32 {
        let end = (offset as usize).checked_add(len as usize);
        let Some(samples) = end.and_then(|end| buffer.samples.get(offset as usize..end)) else {
            self.last = LtcResult {
                state: core::LtcState::InvalidTiming as u8,
                ..Default::default()
            };
            return 0;
        };
        self.last = self
            .inner
            .process(
                samples,
                core::SampleBlock {
                    first_sample,
                    first_sample_ns,
                },
            )
            .into();
        self.last.consumed
    }
    /// Allocates a JS wrapper; call outside a hard realtime processing loop.
    pub fn result(&self) -> LtcResult {
        self.last
    }
    pub fn state(&self) -> u8 {
        self.last.state
    }
    pub fn has_frame(&self) -> bool {
        self.last.has_frame
    }
    pub fn set_timeout_ns(&mut self, timeout_ns: u64) {
        self.inner.set_timeout_ns(timeout_ns);
    }
    pub fn read(&self, now_ms: f64) -> Result<JsValue, JsValue> {
        let now = timestamp(now_ms)?;
        serialize_reading(self.inner.view(now), now, 0, "")
    }
    pub fn capture_snapshot(&self, now_ms: f64) -> Result<TimecodeSnapshot, JsValue> {
        Ok(TimecodeSnapshot {
            view: self.inner.view(timestamp(now_ms)?),
            event: String::new(),
        })
    }
}
#[wasm_bindgen]
pub struct LtcOutput {
    inner: core::LtcOutput,
}
#[wasm_bindgen]
impl LtcOutput {
    #[wasm_bindgen(constructor)]
    pub fn new(
        numerator: u32,
        denominator: u32,
        drop_frame: bool,
        sample_rate: u32,
    ) -> Result<Self, JsValue> {
        Ok(Self {
            inner: core::LtcOutput::new(config(numerator, denominator, drop_frame, sample_rate)?),
        })
    }
    pub fn reset(&mut self) {
        self.inner.reset();
    }
    pub fn set_metadata(&mut self, user_bits: u32, binary_group_flags: u8, color_frame: bool) {
        self.inner.set_metadata(core::LtcMetadata {
            user_bits,
            binary_group_flags,
            color_frame,
        });
    }
    pub fn set_amplitude(&mut self, amplitude: f32) -> bool {
        self.inner.set_amplitude(amplitude)
    }
    pub fn mute(&mut self, muted: bool) {
        self.inner.mute(muted);
    }
    pub fn render(
        &mut self,
        snapshot: &TimecodeSnapshot,
        buffer: &mut LtcSampleBuffer,
        len: u32,
        first_sample: u64,
        first_sample_ns: u64,
    ) -> u8 {
        let Some(samples) = buffer.samples.get_mut(..len as usize) else {
            return core::LtcState::InvalidTiming as u8;
        };
        self.inner.render(
            &snapshot.view.into(),
            core::SampleBlock {
                first_sample,
                first_sample_ns,
            },
            samples,
        ) as u8
    }
}
