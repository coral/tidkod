# Boundary deadlines, interval evidence, and timing traces

These additions live in `tidkod-protocol` and are shared by native and WASM clients.
They do not change the protobuf schema, the four-timestamp exchange, or the current point
estimator. They add local scheduling and evidence needed to evaluate further algorithm changes.
For the precision correction default, alignment readiness, output-clock bridges and
sample evaluation, see [synchronization acceptance](synchronization-acceptance.md).

## Application timestamps and immutable snapshots

Native `MonotonicClock::ns_at(instant)` converts a `std::time::Instant` into the engine's
nanosecond domain using its exact stored origin. It returns `None` for marks captured before
engine creation or beyond the supported `i64::MAX` nanosecond range, and `Some(0)` at the
origin. It does not sample two clocks or introduce a clock-pairing error. The supplied Instant
must already represent the event being timed; conversion does not remove error from an audio
device's own clock bridge. Foreign native callers continue supplying engine-relative
nanoseconds; core-only and browser callers retain their application-provided monotonic epoch.

```rust,ignore
let clock = engine.clock();
let snapshot = reader.snapshot();
for mark in marks {
    if let Some(ns) = clock.ns_at(mark) {
        let reading = snapshot.evaluate(ns);
        // Resolve each mark against the same captured clock and timeline.
    }
}
```

`TimecodeReader::snapshot()` returns a `Copy` `TimecodeSnapshot`, re-exported by `tidkod`.
Protocol-only callers can convert a `View` with `TimecodeSnapshot::from(view)`. Its private state
is evaluated through immutable `evaluate(local_ns)`,
`evaluate_for_presentation(local_ns, delay_ns)`, and `next_boundary(local_ns)` methods. Capture
and evaluation are bounded and allocate nothing, acquire no locks, and perform no networking.
Snapshots remain valid after their reader, follower, or engine is destroyed.

A snapshot freezes the published mapping, trajectory, correction policy/state, and lifecycle
status. Positions, sample age, uncertainty, remaining slew, and known scheduled controls are
evaluated at the requested time. Connection and synchronization flags remain as captured;
evaluating a future timestamp does not refresh them or latch controls in the owner. Capture a
new snapshot to see updates. This is not a history of previous timeline revisions: a timestamp
conversion alone cannot recover an earlier control that the current snapshot no longer contains.

WASM `follower.capture_snapshot()` returns an independently owned snapshot with `read`,
`read_for_presentation`, and `next_boundary`, using the existing millisecond domain and result
shapes. Capture does not tick the follower; call the live follower's `read(nowMs)` first when
current staleness/lifecycle state is required. Free the snapshot when finished. Browser result
serialization and owned snapshot creation may allocate. The inbound wire-state method remains
`follower.snapshot(bytes, nowMs)`. Browser trace capture continues recording the live follower's
operations; standalone snapshot evaluations do not change the follower or enter that trace.

## Accepted clock observations

`ClockMapping::accepted_observations` and `Reading::status.accepted_observations` count accepted
observations in the current acquisition. This saturating `u64` counter can exceed the 128-entry
history and trace bounds. Ordinary rejection, duplicate/unmatched responses, and diagnostic
event loss do not increase it. Rejected observations in quarantine contribute only when a
confirmed recovery promotes them: the new count is then the size of the promoted batch.
Earlier trace entries retain their original rejected result.

New leader sessions reset the count. When the entire accepted history expires, the next
accepted observation starts at one. Partial eviction, holdover, and reconnecting to the same
session preserve the count; reading alone does not expire estimator history. Leaders report
zero because they do not estimate their own clock from follower exchanges. Clock-observation
diagnostics include the resulting acquisition count; WASM readings and traces expose it as the
exact decimal string `acceptedObservations`.

The count describes accepted exchanges, not statistical independence, recency, or measured
physical accuracy. Applications may require a minimum count alongside synchronization,
uncertainty, and source-health criteria. This counter itself does not change convergence or correction.
`offset_evidence.samples` still counts currently retained interval-support observations, and
the bounded diagnostic trace still includes rejections; neither is an acquisition counter.

## Predicting a boundary

Native code uses `reader.next_boundary_at(engine.clock().now_ns())`, or the convenience
`reader.next_boundary()`. Protocol-only users call `View::next_boundary(local_ns)`.
WASM exposes `follower.next_boundary(performance.now())`.

A result includes a local deadline, unwrapped position, discontinuity identifier, clock
uncertainty in local-time units, and a kind:

- `Frame`: the next strictly future integer-position crossing in the playback direction.
  At exactly frame 12, forward playback targets 13 and reverse playback targets 11. At 12.5,
  reverse playback targets 12. This is a phase crossing, not the instant a floor-formatted
  reverse timecode label changes.
- `ScheduledChange`: a retained control takes effect before (or simultaneously with) that
  frame crossing. This wakeup can seek, change speed, pause, or start paused playback.
  Its position need not be an integer. Recompute after the control.

Paused playback without a future control, uninitialized synchronization, or no representable
future deadline returns `None` (undefined in the WASM binding). Holdover still predicts
boundaries, with growing uncertainty. Drop-frame and midnight affect labels only.

The calculation uses bounded monotonic searches over integer local nanoseconds, evaluating
exactly the same Q32 trajectory and bounded slew as `read_at`. It returns the first nanosecond
that reaches the target, avoiding a rounded-early inverse. Searches end before a scheduled
control, where the trajectory could cease to be monotonic. There are at most two searches,
with at most 63 bisections each. Native/protocol evaluation allocates nothing and does not
lock, sleep, or perform networking; WASM result serialization allocates.

A deadline is a prediction from one snapshot, not a scheduled callback. New anchors, clock
mappings, or correction updates can invalidate it. Refresh predictions when state changes,
and read the current state on wakeup. OS timers and browser rendering cannot be assumed to
honor nanosecond deadlines. The returned uncertainty covers clock mapping (including holdover
and a conservative allowance for reduced velocity during slew), not source timestamp error,
execution jitter, or screen presentation.

WASM returns `localDeadlineMs`, exact decimal-string `frames`, `subframe`, decimal-string
`discontinuity`, `kind`, and `uncertaintyMs`. Millisecond deadlines use JS numbers; wire/frame
integers remain exact inside Rust.

## Offset interval evidence

`ClockMapping::evidence` and `Reading::status.offset_evidence` expose `OffsetEvidence`:
reference timestamp, lower/upper offset endpoints, number of supporting observations, and
`consistent()`. WASM `read().offsetEvidence` provides millisecond endpoints and a boolean.

With constant offset and nonnegative path delays, an exchange constrains offset to:

```
t3 - t4 <= offset <= t2 - t1
```

For each accepted exchange, the implementation starts with its midpoint offset ± half the
path delay. It widens that interval by 500 ppm of half the complete exchange duration,
500 ppm of distance from the midpoint to the common reference time, and a 100 µs timestamp
allowance. This permits a relative oscillator rate anywhere within ±500 ppm without using
the fitted drift as independent evidence. Intersect all accepted observations retained in
the bounded 128-sample/32-second history. The result is independent of low-delay regression.
At another read time, a consistent interval expands by 500 ppm of elapsed time.

If lower exceeds upper, the constraints are inconsistent. Preserve that fact until conflicting
samples expire or reacquisition replaces the history; holdover must not magically make an
empty intersection trustworthy. Do not clamp the estimate into the interval or pretend it
is an accurate symmetric confidence distribution. This is a diagnostic feasibility test
under explicit assumptions, not proof of a correct clock. Quarantined/rejected observations
do not constrain this accepted-history interval; their rejection is available in the trace.
The existing clock uncertainty and estimator behavior remain separate.

## Timestamp placement and diagnostics

Native and browser transports prepare the MoQ presentation timestamp before sampling t1/t3,
then encode and publish. The request carries that t1 as its identifier. After publication
returns, `Probes::publication_finished` records the local time, and the matched exchange
uses it as the follower's departure time. Native publication drives the MoQ runtime and
the QUIC endpoint through `sendmsg` in the same worker pass, so this time follows the
socket send. The browser's publication is only an in-memory MoQ append, so its departure
time stays within microseconds of t1. A send call can block until the datagram departs:
on a macOS host with a network content filter, sends took 0.1–1.4 ms. Before-send t1
therefore counted local send time as outbound delay, and loopback showed a +30–130 µs
offset bias. That bias is now within about ±10 µs. A late departure time is never
allowed to make the path delay negative. The leader also flushes replies in the pass
that creates them, but t3 is still taken before its send, since the reply carries it.

Receive timestamps are taken when the application dequeues the datagram from MoQ, before
parsing. Natively, this follows UDP receive, QUIC decryption and MoQ routing in the same
worker pass; it is not a socket arrival time.
No MoQ presentation timestamp is used as a receive timestamp: the published `moq-net 0.2.22`
datagram API provides presentation time and payload, not a kernel/hardware arrival timestamp.
`cargo run --release -p tidkod --example asymmetry` decomposes loopback exchanges into true
up/down legs. The engines share a host clock, so this works only on one machine.

`ClockObservation` records the four timestamps, accepted/rejected result, resulting mapping,
interval evidence, optional t1-to-publication-return duration, and receipt-to-processing-entry
delay. These durations are diagnostic; they are not subtracted from RTT or treated as known
one-way network delays. Leader residence is already observable as t3−t2. Hidden transport,
kernel, radio, and browser callback queues remain included in the measured path.

The core keeps the last 128 estimator observations, including rejections, via
`ClockEstimator::trace()`. Replaying their exchanges through `observe()` is deterministic
when starting with equivalent estimator state; an evicted prefix requires a warmup or saved
state. Native `FollowerConfig::clock_diagnostics = true` additionally emits
`Event::ClockObservation` through the existing bounded nonblocking channel. A slow event
consumer can lose diagnostics without blocking synchronization; this event stream alone is
not guaranteed to be a complete replay log. Session changes reset estimator history.

WASM exposes `probe_published(nowMs)`, `reply_timed(bytes, receivedMs, processedMs)`, and
`clock_trace()`. Publication timing is correlated with each outstanding request even if
responses reorder. All supplied times must use the same monotonic epoch. Trace nanosecond
integers are decimal strings to avoid JS integer precision loss. Ordinary `reply(bytes, nowMs)`
still works when additional timing observations are unavailable.

## Browser capture and replay

1. Open the web app **before connecting**. Bounded capture is enabled by default; `?trace=0` opts out.
2. Connect and exercise playback, latency changes, reconnects, and rendering load.
3. Under Connection details, click **Export timing trace**.
4. From the repository root run:

   ```sh
   pnpm --dir web replay /path/to/tidkod-timing.json
   ```

The log records lifecycle, raw snapshot/probe/reply bytes, monotonic call times, publication
completion, reads used for rendering, and returned values/errors. Capture happens after each
operation. It has overhead, including between probe creation and publication; the publication
diagnostic includes that overhead. It does not observe compositor presentation time.

Capture retains the first 16,384 operations and then counts omissions instead of evicting the
startup state. Oversized (>512-byte) message arguments also stop the retained prefix. Reload
for a fresh capture. Export identifies truncation and also includes the latest bounded clock
observations for inspection. Replay uses only the complete retained prefix with a fresh
compiled-WASM follower and checks operation results exactly, including probe bytes, deadlines,
and rendered readings. Use the same code build for exact comparison. Synthetic compiled-WASM
capture/replay and truncation tests pass; real browser scheduling remains a separate measurement.

## Client-owned presentation prediction

Use `View::evaluate_for_presentation(now_ns, compensation_delay_ns)` in the protocol core,
`TimecodeReader::read_for_presentation_at(now_ns, Duration)` (or `read_for_presentation(Duration)`)
in the native library, and `follower.read_for_presentation(performance.now(), delayMs)` in WASM.
This is an output-adapter API, not an end-user calibration control.

Positive delay evaluates at `now + delay`: the position needed when this output becomes visible
or audible. Evaluate local time through clock offset/drift and then the complete timeline,
including signed playback, slew, and known scheduled changes. Paused output remains paused unless
a known control starts it before presentation. Zero delay matches an ordinary read. Prediction
never latches future controls or advances the follower's actual staleness clock. Diagnostics such
as mapping uncertainty and sample age refer to the prediction horizon; connection state refers
to the present. Invalid/negative JS delays and timestamp overflow are rejected.

The adapter owns the delay estimate from the timestamp it supplies to actual presentation.
Include known application buffering, device latency, and elapsed preparation time once each.
Do not add RTT, leader residence, clock offset, or snapshot age: synchronization and anchor
extrapolation already account for those. Positive compensation advances forward playback; it
cannot correct a slower reference display by delaying an already-faster display. Unannounced
future controls and unknown device/compositor latency cannot be inferred by the library.
No automatic fixed frame offset or user-entered browser compensation is applied.

## Independent same-computer TOD check

The browser now pairs `Date.now()` with bracketed `performance.now()` and evaluates its reading
at the bracket midpoint. Under Connection details, **Same-computer TOD check** reports the signed
difference between that sampled timeline and local wall time. It is valid after issuing `tod`
on a leader on the same computer using the same timezone, while running at +1×, without a later
wall-clock adjustment. This reference does not use the Tidkod clock estimator. Millisecond wall
timestamps and the bracket limit precision; it does not measure either screen's presentation.

With capture enabled, the latest 256 paired readings are included as `wallReferences`, independently
of the replayable operation prefix. `pnpm --dir web analyze /path/to/trace.json` summarizes RTT,
remaining slew, read cadence, and the independent TOD differences when present. Earlier trace
files lack wall references: exact replay of those files establishes reproducibility, not accuracy.
`python3 scripts/measure_display.py --tod` independently measures leader TOD sample age at PTY
receipt, excluding terminal/compositor latency.

Both terminal examples and the browser also display an independent **System HH:MM:SS.mmm**
reference beside their timecode. The terminal pairs its wall-clock sample with the midpoint
of two engine monotonic reads and reports its own TOD difference. These system-clock labels
bypass the Tidkod mapping and timeline. Comparing them on screen is a control experiment:
a similar gap in both system-clock labels exists independently of the synchronization math;
agreement of system clocks but disagreement in timecode calls for further timeline/clock auditing.
The test must use the same computer/timezone. It does not assume Ghostty is slow or insert an offset.

## Recording-part session IDs

`Reading::session_id: Option<[u8; 16]>` identifies a caller-defined recording part.
Record it alongside the timecode to distinguish repeated playback of the same
frames. It is separate from the internal leader-lifetime `session` used to reject
stale connections and reset clock acquisition.

Each leader startup generates a fresh UUID v4 unless `LeaderConfig::session_id`
is supplied. Cloning a default config still generates a different ID for each
leader. A caller-supplied ID is exactly 16 bytes in UUID/network byte order; all
bit patterns, including the nil UUID, are accepted.

```rust
let leader = engine.leader(tidkod::LeaderConfig {
    session_id: Some([0x12; 16]),
    ..Default::default()
})?;
let next_part = leader.rotate_session_id()?;
leader.set_session_id(next_part)?;
let mut reader = leader.reader()?;
let reading = reader.read(); // reading.session_id travels with this timecode
```

Set/rotate calls synchronously update local readers before returning and request
an immediate complete state publication. Connected followers receive the new ID
through the reliable state track; new connections receive the current ID in the
initial snapshot. No additional handshake or reconnection is required. Setting
the same ID is idempotent. Playback, scheduled controls, discontinuity counters,
clock observations, and slew state are unchanged. Seeking does not automatically
rotate the ID: the application decides what constitutes a new part.

Readers expose the latest received ID, including during acquisition and holdover.
Before receiving an ID, or with an older sender, it is `None`. Frozen snapshots
retain their captured ID. These updates are latest-state metadata, not a durable
part-change log or a scheduled frame boundary: rapid rotations can coalesce,
network delay affects when a follower learns an ID, and readers cannot infer
unreceived rotations during holdover. Future presentation evaluations use the
captured ID. They cannot predict a future caller rotation.

WASM readings expose `sessionId` as a canonical UUID string (absent/undefined
when unknown); browser timing traces retain it in their captured reading output.
Foreign `Reading` records contain `has_session_id`, `session_id_high`, and
`session_id_low` (camel/Pascal case in Swift/C#). The two halves are the first and
last eight bytes interpreted as big-endian unsigned 64-bit integers. This keeps
reader evaluation allocation-free and retains all 128 bits. Generated leader
options accept `session_id(high, low)`; leaders expose `set_session_id(high, low)`
and `rotate_session_id()`, returning a `SessionId` record with `high` and `low`.
Use the generated language's usual method casing. Core-only readers expose the
same ID fields without requiring a native engine.

The foreign Reading layout has grown; regenerate bindings and rebuild consumers
with the matching library rather than mixing this ABI with older binaries.
