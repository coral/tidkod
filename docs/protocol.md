# Tidkod v1 behavioral specification

The normative schema is [`tidkod.proto`](../protocol/proto/tidkod/v1/tidkod.proto). The [generated reference](messages.md) lists fields and tags. This document defines behavior and constraints beyond protobuf syntax. v1 has no migration or legacy-client negotiation.

## Discovery and connection

Advertise DNS-SD service `_tidkod._udp.local.`. SRV carries the endpoint port, A/AAAA records carry addresses, and TXT carries `id` (configured identity), `name` (display name), `v=1`, `fp` (64 hexadecimal characters, SHA-256 of the leaf certificate), and `transport=moq-lite-05`. Instance and host names contain a random session suffix, allowing equal display names. Consumers index by full service instance name, replace resolved records, and remove entries on DNS-SD expiry/goodbye. They must not identify a leader by display name alone.

The implementation polls interface changes every two seconds. Empty interface selection enables all interfaces. Explicit discovery addresses override automatic address publication; otherwise a specific bind address advertises that address, and wildcard binds use current interface addresses. IPv6 scopes are retained from DNS-SD interface metadata through `SocketAddrV6` into the QUIC dial; no URL conversion discards the scope. For explicit link-local server binds, restrict advertisement to the corresponding interface index. Discovery errors are reported independently and never prevent manual connections.

The follower selects one endpoint and opens one bidirectional MoQ connection. Native clients use raw QUIC with TLS ALPN exactly `moq-lite-05`. Browser clients use WebTransport over HTTP/3 (TLS ALPN `h3`) and negotiate WebTransport subprotocol exactly `moq-lite-05`. Other MoQ versions are not offered or accepted, including the published transport's WIP lite-06 default. Each decoded application message must have `version=1`; an unsupported version closes that attempt with an explicit error. TLS uses a generated self-signed certificate. Address-only trusted-LAN mode skips certificate identity validation; pinned mode verifies the exact SHA-256 leaf fingerprint. mDNS pin bootstrap authenticates possession of the advertised key, not the identity of the advertiser. Discovered pins do not override explicit user pins.

Native connections use directly driven Quinn protocol state and the upstream MoQ machines pinned in Cargo.toml. Socket-address dialing preserves scoped IPv6. TLS policy lives in `native/src/transport/tls.rs`. The native client uses raw QUIC. The same leader listener also accepts WebTransport CONNECT at `https://IP:PORT/`, without a relay or separate HTTP server. Browser clients offer only `moq-lite-05` in WebTransport `protocols` and check the negotiated MoQ version. All application messages, tracks, datagrams, and monotonic timestamp units below are identical across both transports; no browser-specific wire version is introduced.

Browsers validate TLS through a trusted certificate or `serverCertificateHashes`. The supplied browser follower requires the leader's displayed SHA-256 leaf fingerprint. The generated ECDSA P-256 certificate has a 14-day validity interval, compatible with WebTransport hash pinning; restart the leader and exchange a new pin before expiration. A secure page context is required (HTTPS or localhost); the WebTransport destination itself uses HTTPS. Browser APIs do not expose mDNS browsing, raw UDP/QUIC sockets, or an insecure skip-verification option. Application scheduling, timer precision, and tab suspension affect uncertainty and observable timing. The browser adapter uses `performance.now()` milliseconds, converted to monotonic nanoseconds by the WASM binding. It never sends wall-clock timestamps.

## Broadcast and tracks

Each direction announces `tidkod/v1`. The leader publishes tracks `state` and `clock/reply`; the follower publishes `clock/request`. A server creates private origin namespaces for each connection, so a follower's request and corresponding reply track cannot be subscribed to by another follower. The logical leader-state track is shared: every connection receives the same complete snapshot through a latest-value state channel. State is copied into that connection's MoQ track. No follower publication is forwarded to another connection.

Each state snapshot is the only frame in an independently decodable, finished MoQ group. Monotonic group sequence numbers are transport-local; `Snapshot.revision` is the application ordering authority. Track timestamps are MoQ metadata, not clock measurements. A newly connected follower receives the current snapshot. Complete snapshots repeat on heartbeat; no state delta depends on an older group. State caches retain two seconds and have a 256 KiB eviction target per origin (the MoQ pool is a target, not a strict total-memory ceiling).

Clock requests and replies use MoQ-lite-05 unreliable datagrams, not reliable group streams. Their loss and reordering must not stall state delivery. Clock probes have their own sequence numbers; a reply echoes the request's sequence and t1. Only one outstanding entry with matching sequence and t1 can be consumed, and duplicate or expired replies are ignored. A request has both t2 and t3 zero; a reply has both nonzero with t3 >= t2. Replies must arrive within 500 ms of t1, including leader residence time, and leader residence cannot exceed the complete exchange duration. Invalid echoes do not consume a still-valid outstanding request. The implementation retains 128 outstanding entries per connection. Reconnecting creates fresh private tracks and outstanding entries.

## Encoding and limits

Every application payload is a single protobuf message, without another length prefix. MoQ framing supplies its length. Maximum payload is 512 bytes, including unknown fields. Enforce the state frame length before assembling it in the application. Empty/malformed protobuf, missing required message objects, invalid enums, invalid rate/format values, invalid session identity, and unsupported versions are errors. Unknown protobuf fields may be skipped within the limit. Ordinary state snapshots target less than 128 bytes; typical probes are less than 64 bytes.

`Snapshot.session_id` (tag 10) is an independent, caller-controlled recording-part
UUID. New leaders always send 16 bytes in UUID/network byte order. An empty field
means unknown/unsupported (legacy sender); other lengths are invalid. Old readers
ignore this additive field, and the existing golden fixtures remain unchanged.
The field is present in each complete state snapshot, including the initial
snapshot on connection and heartbeats. Setting/rotating it publishes immediately
with the normal monotonically increasing revision. Followers accept it only as
part of an otherwise valid newer snapshot. It does not alter `session`, reset the
clock, increment discontinuity, or change/suppress scheduled transport controls.
It is leader-authoritative state, not a follower-selected or negotiated value.
See [recording-part APIs](timing-apis.md#recording-part-session-ids) for delivery
and holdover semantics.

Session identifiers are 16 random bytes, newly generated on every leader startup. Revision starts at 1 and increases for each publication. Discontinuity starts at 0 and increases on each explicit change; scheduled changes reserve higher IDs. All IDs are unsigned, and wrap is not supported. The protocol assumes practical sessions finish before counters exhaust.

All application clock fields are unsigned monotonic nanoseconds within `0..=i64::MAX` (roughly 292 years). Each engine has its own arbitrary epoch. Only differences within a clock domain are directly comparable. Timestamp external samples with the leader engine's clock. Local reader timestamps use the follower engine's clock. Wall-clock time and the system clock are never modified.

Frame position is a signed, unwrapped whole-frame count plus an unsigned Q32 fractional part. Value is `frames + subframe / 2^32`; minus half a frame is `frames=-1, subframe=2147483648`. Arithmetic is performed with i128 intermediates. Position saturates at the Q32 representation bounds; display wrapping never changes the underlying position.

Supported exact `(numerator, denominator)` frame rates are `(24000,1001)`, `(24,1)`, `(25,1)`, `(30000,1001)`, `(30,1)`, `(48000,1001)`, `(48,1)`, `(50,1)`, `(60000,1001)`, and `(60,1)`. Fractional rates are never approximated as decimal rationals. Drop-frame labels are permitted only at 30000/1001 and 60000/1001. They omit labels 00–01 or 00–03 at each minute except every tenth minute. No frames are removed from the timeline. Negative labels wrap using Euclidean division. The label day is 24 nominal timecode hours; at drop-frame rates this comprises 2,589,408 or 5,178,816 actual frames.

Playback rate is a signed rational multiplier of the frame rate. Denominator must be `1..=1_000_000`, absolute numerator at most 1,000,000, and absolute ratio at most 64. The public API reduces fractions. Zero is paused and negative is reverse. For anchor `(T, P, R)`, frame rate `F`, and leader-clock time `t`, position is `P + (t-T) * F * R / 1e9`. The fixed-point delta truncates toward zero at less than one Q32 unit of error.

## Timeline and transport changes

A snapshot contains session, revision, active discontinuity, source kind and health, frame format, active anchor, and up to four scheduled anchors. Schedule entries must have strictly increasing effective times beyond the active anchor and strictly increasing discontinuity IDs. Each is a complete replacement trajectory, not a relative operation.

Generated leaders expose play, pause, seek, speed, and atomic position/rate changes. Controls are immediate unless an effective engine-clock timestamp is supplied. Late timestamps are applied immediately at their stated time and extrapolated to now. Immediate commands cancel outstanding schedules and choose a discontinuity above every reserved ID. Future commands must be submitted in increasing effective-time order; full or unordered schedules are rejected synchronously.

A schedule is retained in every snapshot until its effective time, at which point it becomes the active anchor and is removed from the retained queue. Readers can apply a received schedule at its effective time without waiting for another packet, including during holdover. Applied schedules are latched, so a subsequent clock correction cannot move the follower back across an already-applied discontinuity. Explicit discontinuities clear correction immediately. A follower extrapolates late anchors to the current estimated leader time, never to the time of receipt.

A follower drops revisions not greater than its current revision for that session. A transport connection accepts only the session identity from its first valid snapshot. Messages with another identity on that connection are discarded. A new connection permits a different leader session; its first snapshot resets clock estimation if the session changed. Previous-session messages cannot cross the new private transport tracks. Reconnection to the same session retains the previous clock and timeline until refreshed.

## Clock estimation and correction

The follower records t1 immediately before probe publication and sends it as the request identifier. For estimation, it uses the local time at which publication returned, provided that keeps the path delay nonnegative. Natively, publication includes the socket send. The leader records t2 immediately on datagram receipt and t3 immediately before reply publication. The follower records t4 immediately on reply receipt, before queueing the exchange for estimation. Application and transport scheduling remain part of the measured path. Only the follower's own send time is local and observable; it is excluded.

The four-timestamp model is:

```
offset = ((t2 - t1) + (t3 - t4)) / 2
round_trip_delay = (t4 - t1) - (t3 - t2)
leader(local) = local + offset_at_reference + drift * (local - reference)
```

This follows the [NTP timing model, RFC 5905 §8](https://www.rfc-editor.org/rfc/rfc5905.html#section-8). Reject non-monotonic pairs, negative delays, complete exchanges greater than 500 ms, old observed-sample midpoints, and unmatched replies. History is bounded to 128 accepted samples and 32 seconds. Delay outliers more than 20 ms above the current minimum are quarantined once eight samples exist, as are large offset outliers. Invalid timestamps and expired exchanges never enter recovery.

Quarantine holds at most 16 observations. Eight consistent observations spanning at least 500 ms establish a changed path or clock regime and replace the stale fit history. The default switches to fast probes while quarantine is pending, retaining the 500 ms evidence-span requirement; at a fixed 4 Hz cadence this would take 1.75 seconds without loss. Relative to the first candidate, drift-compensated offsets must agree within max(2 ms, first delay / 4), delays within max(5 ms, first delay / 4), and the candidate span must not exceed five seconds. An inconsistent candidate restarts quarantine; an ordinarily accepted sample clears it. A confirmed replacement reacquires the fit and uses acquisition probe cadence. Isolated, alternating, reordered, or widely separated outliers cannot force reacquisition. Old history also expires normally during an outage.

Fit the lowest-delay half of the history (at least four when available), using centered least squares to avoid losing precision at large clock epochs. Equal-delay observations prefer the newest samples. Drift is fitted only from at least eight selected observations spanning two seconds, clamped to ±500 ppm; discarded observations cannot supply the time span. Before that, retain the prior drift estimate. Convergence requires at least 12 accepted exchanges spanning 500 ms. Probe intervals default to 50 ms during acquisition, holdover, correction, uncertain alignment, or quarantined clock/path recovery, and 250 ms while aligned. Native and WASM share this decision. Missing accepted exchanges for eight steady intervals (two seconds by default) marks synchronization as holdover even if QUIC remains connected.

For each selected sample, propagate its half-round-trip-delay asymmetry bound from that sample's midpoint to the latest receipt time using the holdover growth rate. Uncertainty is the tightest of those age-adjusted bounds, plus the largest selected regression residual and a 100 µs scheduling floor. Each bound retains its own age: an old low-delay observation cannot borrow a newer observation's timestamp, and receiving a noisy excluded observation cannot make old fit support fresh. It grows by 1000 ppm of sample age during holdover and between samples, covering the difference between a fitted drift within ±500 ppm and a changed relative oscillator rate within ±500 ppm. Arbitrary clock steps violate that oscillator bound and require reacquisition. This is a conservative engineering estimate under the stated oscillator/path assumptions, not a universal error bound. A one-way asymmetric path is not identifiable from two-clock round trips alone. QUIC RTT and packet-loss counters are reported separately; the normal transport spin-bit behavior is unchanged and no packet capture is required.

The first accepted clock measurement establishes the initial mapping immediately. Routine corrections preserve output continuity, targeting a 250 ms settling time from the remaining position error, limited to 10% of the absolute playback velocity. Repeated mapping/state refinements re-evaluate the remaining error; the target is not an unconditional recovery deadline. The previous fixed 0.03 frames/second policy remains available through `CorrectionPolicy::legacy()`. This prevents correction from reversing a slow shuttle. Paused output does not slew: any residual adjustment remains until playback resumes, an explicit discontinuity applies, or a confirmed large error causes hard resynchronization. Errors greater than one frame require three accepted clock measurements in the same correction direction before a hard resynchronization. Direction reversals, small errors, reconnects, and explicit discontinuities reset confirmation. Initial acquisition permits immediate alignment; later clock reacquisition does not bypass this confirmation policy. State heartbeats do not count as clock confirmations. Explicit discontinuities bypass this confirmation and slew policy. Correction events report new sessions, discontinuities, slews, and hard resynchronizations; events are diagnostic and may be dropped if not drained.

## Source tracking and health

Generated leaders default to paused. Tracked leaders consume timestamped source samples with optional rational rate hints and explicit discontinuity hints. Timestamps must increase and must not be in the future. The tracker keeps up to 64 samples from the last second since the latest discontinuity. A gap longer than 500 ms discards that rate evidence.

Speed without a hint:
- Changed-speed candidates come from the longest available baseline in that window, not from consecutive samples. Baselines under 1 ms do not supply an estimate.
- Three consistent candidates more than 2% from the current rate trigger a discontinuity.
- Once the window spans at least 100 ms, an unhinted rate is refined continuously from a least-squares fit, with no discontinuity. It is represented at about 1 ppm resolution, or proportionally coarser above 1x.

Jumps and hints:
- Three position errors exceeding one frame in the same direction trigger a discontinuity.
- Alternating position errors do not confirm a jump, and a discontinuity clears prior confirmation evidence.
- Rate and explicit discontinuity hints apply immediately. Exact rational rate hints are respected even for very slow shuttle rates.

Routine position:
- Each routine position comes from that fit evaluated at the sample timestamp.
- A single robust pass drops samples whose residual exceeds six median absolute residuals, with a 10 µs floor.
- Until the window spans 100 ms, routine jitter uses a bounded correction of 20% of error, capped to ±0.1 frame per sample.

Tracked publications coalesce to at most 10 Hz for routine samples; explicit changes and health transitions publish immediately. Generated heartbeats default to 1 Hz. If no external sample arrives within 500 ms, source health becomes degraded and the last signed trajectory continues indefinitely. Source kind remains `Tracked`, independent of health and connectivity. A subsequent sample restores health.

## Recovery, API, and resource bounds

Connection state (`Disconnected`, `Connecting`, `Connected`, `Shutdown`), synchronization state (`Uninitialized`, `Acquiring`, `Synchronized`, `Holdover`), source kind, and source health are distinct. Status also reports uncertainty, accepted sample age, RTT, current offset, drift ppm, and lost QUIC packets. Before a usable anchor/clock pair exists, readers return the configured paused fallback (default 00:00:00:00 at 30 fps), marked uninitialized.

When connection or measurements disappear, keep the last timeline and affine drift estimate indefinitely. Running and reverse timelines continue; paused timelines stay paused. Pending scheduled controls still take effect. Retry only the configured endpoint with exponential backoff from 100 ms to 5 s, and a default three-second connection timeout. Stable connections reset backoff. No automatic discovery reselection or certificate-pin replacement occurs.

One engine-owned thread polls nonblocking UDP sockets and directly steps QUIC, HTTP/3, and MoQ protocol state. It starts no Tokio runtime. Public APIs contain no Tokio, MoQ, or generated wire types. Commands and event notifications have capacity 64. Up to 64 live preallocated reader triple buffers and 64 followers per leader are allowed by default (the follower connection bound is configurable to 1–1024). Reader evaluation copies plain state and performs arithmetic without allocation, locking, or networking. Event delivery uses nonblocking sends. Shutdown cancels protocol state, closes listeners, withdraws mDNS, and joins the worker; retained readers own their final snapshots. See [native execution](native-bindings.md) for polling budgets and remaining upstream utility dependencies.

## Wire fixtures

[`paused.hex`](../protocol/fixtures/paused.hex) is a 40-byte paused 30 fps snapshot with a 16-byte session of `01`, revision 1, generated/healthy source, time 0, frame 0, and rate 0/1. [`probe.hex`](../protocol/fixtures/probe.hex) is a 12-byte reply with sequence 1, t1=1000, t2=1500, t3=1510. Fixture tests decode and re-encode byte-for-byte. Protobuf omission of scalar zero values is intentional; present empty position messages still establish required message presence.

### Acquisition and timeline adjustment

During initial clock acquisition, the reference follower applies each accepted mapping directly,
including the first converged mapping. These provisional estimates must not be retained by the
steady-state slew limiter: a delayed first probe could otherwise leave timecode ahead for seconds
after the clock itself converges. Once acquired, small corrections use the configured bounded slew.
Clock uncertainty describes the clock mapping; `Status::correction_frames` separately reports the
remaining signed timeline adjustment. Neither the synchronized flag nor clock uncertainty alone
asserts that an in-progress timeline adjustment has finished. These are local implementation/status
semantics; the wire messages are unchanged.

### Snapshot transition validation

Within one session, a newer revision must not roll back the effective discontinuity. Retained
schedules are first advanced through any discontinuity already applied by the follower, so a
legitimate heartbeat that still contains that schedule remains valid. A frame-format change
requires a new discontinuity. Inconsistent transitions are discarded without modifying the
current timeline. These checks complement per-message protobuf validation and revision/session
checks; no additional fields or protocol version are required.

### Local timing evidence and scheduling APIs

The shared client core additionally exposes predicted local frame/control deadlines and
intersections of drift-bounded offset intervals from accepted exchanges. These are local
APIs, not new wire messages. Their assumptions, exact reverse-motion semantics, bounded
capture/replay format, and timestamp placement are defined in [timing APIs](timing-apis.md).

### Alignment assessment and output timing

`Status::aligned` requires an initialized, connected, synchronized, healthy source,
consistent offset evidence, no pending estimator quarantine, and estimated position
error at most 1 ms in nominal frame-time units. `alignment_error_ns` combines clock
uncertainty multiplied by absolute playback speed with the magnitude of remaining
correction divided by frame rate. This is a local engineering assessment under the
clock model's existing assumptions; source timestamp error and physical output
latency are excluded. Acquisition state remains a separate field for compatibility.
A leader uses its own clock and does not accumulate remote-clock uncertainty.

`resync_generation` is a saturating, reader-visible local counter incremented on new
leader sessions and hard realignments, including provisional initial acquisition.
It persists when diagnostics are dropped, and is retained by frozen snapshots. It
is distinct from the leader's discontinuity ID and recording-part UUID. Output
adapters can observe it when deciding whether to rearm after lost alignment.

See [output timing and acceptance](synchronization-acceptance.md) for clock bridges,
per-sample evaluation, defaults, and independently referenced measurements.
