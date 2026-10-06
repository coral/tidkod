# LTC samples and source tracking

`tidkod_protocol::ltc` (also `tidkod::ltc`) generates and decodes mono normalized
`f32` samples. It opens no devices, creates no threads, and requires no clock or
network runtime. The C, C++, Swift, C#, and WASM bindings expose the same codecs.
CPAL and the SPSC queue used by the examples are development dependencies only.

## Formats and processing

Configure an exact `FrameFormat`: 24000/1001, 24/1, 25/1, 30000/1001 NDF or DF,
or 30/1. The receiver must know the format: LTC does not distinguish 24 from
23.976 or 30 from 29.97 NDF. The drop-frame bit must match the configured format.
Higher-rate Tidkod formats remain available for timelines but are rejected by LTC.
Sample rates from 16,000 through 384,000 Hz are accepted; interoperability tests
cover 44,100, 48,000, and 96,000 Hz. Playback is forward at nominal speed, with
small oscillator differences tolerated (decoded frame length within 2%). Reverse
and shuttle playback are outside this release.

```rust
use tidkod_protocol::{FrameFormat, Position, ltc::*};
let config = LtcConfig::new(FrameFormat::new(30000, 1001, true)?, 48000)?;
let mut encoder = LtcEncoder::new(config, Position::ZERO);
let mut decoder = LtcDecoder::new(config);
let mut samples = [0.0; 512];
encoder.render(&mut samples);
let block = SampleBlock { first_sample: 0, first_sample_ns: 1_000_000_000 };
let mut offset = 0;
while offset < samples.len() {
    let timing = block.advance(offset, config.sample_rate()).unwrap();
    let result = decoder.process(&samples[offset..], timing);
    if result.consumed == 0 { break; } // InvalidTiming: inspect result.state.
    offset += result.consumed;
    if let Some(frame) = result.frame {
        // The frame may have started in an earlier callback.
        // frame.start_ns is its codeword start, not decode completion.
        let _label = config.format().label(frame.position);
    }
}
# Ok::<(), tidkod_protocol::Error>(())
```

Keep each codec on one processing thread or otherwise serialize access outside
realtime processing. Calls retain state across buffers. Each decoder call consumes
up to one complete frame, reports the number of consumed samples, and retains no
reference to caller memory. Continue from that offset, deriving each timestamp
from the original block. Empty input is a no-op. No internal frame queue can grow
or overflow. Constructors and foreign handles may allocate; sample processing,
reset, metadata changes, snapshot evaluation, and expected signal failures do not.
Work is proportional to the supplied sample count, with bounded work per sample.

States have identical numeric values across bindings:

| Value | State | Meaning |
| --- | --- | --- |
| 0 | Searching | No usable frame history |
| 1 | Acquiring | Partial signal or fewer than three continuous source frames |
| 2 | Locked | Decoded frame / established tracked source / active output |
| 3 | Holdover | Extrapolating an established source or degraded output timeline |
| 4 | Muted | Output explicitly muted or awaiting an initial/full frame boundary |
| 5 | InvalidTiming | Invalid sample-index/timestamp range; no input consumed |
| 6 | Unsupported | Snapshot format or playback rate cannot be rendered |

A raw decoder's `Locked` result describes a returned frame; `LtcInput` supplies
persistent acquisition and health. Malformed codewords, wrong drop-frame flags,
invalid labels, noise, silence, and nonfinite samples do not produce observations.
Input uses adaptive DC removal and hysteresis. Polarity correction is reported as
`polarity_valid`; it is not a CRC and is not required to accept a codeword.

## Time, phase, and metadata

`SampleBlock.first_sample` is a cumulative sample index, not an interleaved scalar
index. `first_sample_ns` is the capture or presentation time of that sample in the
application/engine monotonic domain. Device timestamps can refresh this mapping
at every callback. For simulated or offline streams use `sample_time` from a
stable origin and cumulative index; do not accumulate rounded buffer durations.
Backward timestamps, noncontiguous indices, or input timestamp gaps over 2 ms
reset decoder acquisition. A reset/reseed is appropriate after replacing a device.

Decoded `[start_sample, end_sample)` brackets an entire 80-bit codeword. The label
belongs to the start of bit zero, although it becomes available approximately one
frame later. Timestamps are measured threshold crossings, quantized to samples;
waveform conditioning and external hardware can shift them. They do not identify
a TV vertical-sync edge. Apply any video-standard alignment in the application.

`LtcEncoder` uses a persistent rational phase, emits square PCM at amplitude 0.5
by default, and starts at the supplied integer frame. Reset discards subframes and
starts a complete codeword. `set_amplitude` accepts finite 0..=1 values. The caller
owns reconstruction filtering, line levels, and physical output characteristics;
the samples alone do not establish electrical-interface compliance.

`LtcOutput` evaluates one immutable snapshot over the block, retaining phase across
snapshot refreshes. It waits for initial synchronization and a complete frame
boundary; pause, reverse, format mismatch, and explicit mute produce silence.
Discontinuities, sample-index gaps, and hard resynchronization rearm output. A
scheduled change truncates an old partial codeword rather than continuing a label
that no longer applies. Once armed, output advances a continuous sample-clock
phase and follows snapshot phase with speed correction limited to ±1% of nominal.
This preserves complete codewords across small forward/backward snapshot updates
and stays inside the decoder's 2% frame-duration tolerance. LTC can consequently
settle later than the timeline after a correction (a quarter-frame phase error
at 30 fps needs approximately 0.83 seconds at the maximum correction speed).
`Locked` describes active waveform generation, not zero phase error against the
latest snapshot. Explicit discontinuities and jumps over half a frame rearm output.
An established output continues in holdover and reports degraded state. Refresh
snapshots to observe connection and health changes; frozen snapshots retain their
captured lifecycle state.

Metadata consists of eight user nibbles packed low-nibble-first in `u32`, three
binary-group flags, and the color-frame bit. Bits above the low three BGF bits are
ignored. Metadata is latched at codeword boundaries. Tidkod generates the
rate-dependent polarity correction. Metadata defaults to zero and is local only:
user bits and flags are not transported over the current LAN protocol.

## Input tracking and the native worker

`LtcInput` combines decoding, three-consecutive-frame acquisition, day unwrapping,
and the existing `tracking::Tracker`. `process` returns raw frames immediately,
and adds `observation: Some(SourceSample)` once tracking is established. `read`,
`view`, and `snapshot` extrapolate the tracked timeline. The default source timeout
is 500 ms; configure it with `set_timeout_ns`. Loss preserves extrapolation with
`Holdover`/degraded health. A gap or seek requires acquisition again and the next
accepted source observation is discontinuous.

The first label is unwrapped to the day nearest the seed (zero by default).
Use `reset(seed)` to establish another epoch or resolve jumps of twelve hours or
more. Continuous midnight crossings advance the unwrapped frame count.

Create a native leader with `SourceKind::Tracked` and the same `FrameFormat`.
Deliver observations to `Leader::track` on a control thread: that method allocates
an acknowledgement channel and waits for the worker. The example uses a bounded
64-observation SPSC queue, counts drops, and marks the next successful delivery
discontinuous. Preserve the original observation timestamp across the queue.

Network clock uncertainty, sample timestamp quality, and correction debt are
separate. LTC readings reuse local-clock status; zero network uncertainty does
not measure ADC timing or physical alignment.

## Language buffers

The foreign facade is defined in `clients/bindings/src/api.rs`; generated files
belong in `target/`. `LtcResult` carries consumed count, state, optional frame,
and optional tracked-source observation as fixed-width fields. C/C++/Swift/C#
expose constructors, reset, metadata/amplitude controls, input reads, and snapshot
capture into an existing handle. Both native and core-only libraries include LTC.

- C spans are pointer plus element count. Empty spans may have a null pointer.
  Nonempty pointers must be aligned and refer to live storage. Mutable output
  must not alias handles, other spans, or return/error storage. Handles require
  exclusive access during processing. Invalid ABI arguments may allocate an error.
- C convenience wrappers use `TKFloatInput`/`TKFloatOutput`; C++ uses
  `FloatInput`/`FloatOutput` (pointer and count).
- Swift accepts `UnsafeBufferPointer<Float>` and `UnsafeMutableBufferPointer<Float>`;
  use `withUnsafeBufferPointer`/`withUnsafeMutableBufferPointer` around existing storage.
- C# accepts `ReadOnlySpan<float>`/`Span<float>`, pinned for the call.
- WASM accepts `LtcSampleBuffer`, allocated once. `pointer()` and `ltc_memory()`
  construct a `Float32Array` view. Recreate the view after memory growth, and never
  retain it after freeing the buffer. Processing accepts offsets and lengths;
  `result()` creates a JS wrapper and should be read outside a hard realtime loop.
  Integer timestamps/counters use `BigInt`. Input `read`/`capture_snapshot` use
  the existing browser millisecond domain. Rust processing is allocation-free;
  JavaScript scheduling and garbage collection have no hard realtime guarantee.

```js
const buffer = new LtcSampleBuffer(512);
const encoder = new LtcEncoder(30000, 1001, true, 48000, 0n);
encoder.render(buffer, 512);
const samples = new Float32Array(ltc_memory().buffer, buffer.pointer(), 512);
// Copy samples to the application-owned output channel.
encoder.free();
buffer.free();
```

## CPAL examples

```sh
cargo run -p tidkod --example ltc_in --locked -- --list-devices
cargo run -p tidkod --example ltc_in --locked -- --device 0 --channel 0 --fps 29.97 --drop-frame
cargo run -p tidkod --example ltc_out --locked -- --fps 25 --start-frame 900 --amplitude 0.5
cargo run -p tidkod --example ltc_out --locked -- --address 192.168.1.2:4443 --fps 29.97 --drop-frame
```

Both examples accept `--sample-rate` (default 48000), `--seconds` (default 10),
`--device` (index from listing), and zero-based `--channel`. Output supports
`--pin`; input supports `--bind` and `--no-mdns`. Without `--address`, output runs
standalone. Match the configured format to the source; unsupported snapshots mute.
F32, F64, I16, I32, and U16 device formats use fixed reusable conversion buffers.

Each CPAL stream has its own clock bridge. The control thread brackets
`StreamTrait::now()` with engine timestamps and refreshes the bridge. Callbacks
use capture/playback timestamps, including device latency once, and perform no
logging or native control calls. Device errors and source queue drops are counted
atomically and printed by the control thread. Recovering/replacing failed devices
is an application responsibility; examples exit after the requested duration.
Linux example builds need ALSA development headers (`libasound2-dev`).

## Dependencies and verification

Production code uses MIT/Apache-licensed `st12-1` and `broadcast-common`, plus the
attributed BSD decoder adaptation in `protocol/licenses/ltc-BSD-3-Clause.txt`.
`python3 scripts/check_ltc_dependencies.py` checks both SDK runtime graphs for
unwanted audio-device and GPL/LGPL dependencies. SDK notices include the BSD source.

Tests cover independently specified codewords, exact rates, arbitrary chunks,
metadata, damaged input, drop-frame rollover, day unwrapping, snapshot controls,
holdover, allocation counts, and language consumers. The optional independent
harness `scripts/ltc_reference.c` verifies both directions against libltc at all six
formats and three sample rates, including exact label sequences, user bits, and
sample boundaries with small allowances for reference-decoder acquisition and
edge shaping. libltc is an external test tool only; it is never
linked into Tidkod. For an installed libltc and a debug native bindings build:

```sh
cargo build -p tidkod-bindings --locked
cc -O2 -DNDEBUG -I target/debug/tidkod-generated/native scripts/ltc_reference.c \
  -L target/debug -Wl,-rpath,"$PWD/target/debug" -ltidkod_bindings \
  $(pkg-config --cflags --libs ltc) -o target/ltc-reference
target/ltc-reference
```

Linux CI runs this harness separately. Local validation used libltc 1.3.2 in both
directions, including its independently edge-shaped output. Synthetic and software
interoperability checks do not replace physical loopback and multi-device
measurements described in `docs/validation.md`.
