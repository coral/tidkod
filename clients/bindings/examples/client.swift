import Foundation
#if TIDKOD_NATIVE
import Network
#endif

let core = Core()
precondition(core.read(nowNs: 123).fpsNumerator == 30)
let snapshot = TimecodeSnapshot()
core.snapshotInto(snapshot: snapshot)
let frozen = snapshot.copy()
precondition(frozen.read(nowNs: 123).acceptedObservations == 0)
precondition(!frozen.nextBoundary(nowNs: 123).valid)
let predicted = try frozen.readForPresentation(nowNs: 123, delayNs: 1)
precondition(predicted.frames == 0)
let sample = try frozen.readSample(originNs: 123, sampleIndex: 48000, sampleRate: 48000)
precondition(sample.frames == predicted.frames && !sample.aligned)
let bridge = try ClockBridge(localBeforeNs: 100, externalNs: 10000, localAfterNs: 110)
let outputTime = try bridge.convert(externalNs: 10100)
precondition(outputTime.localNs == 205 && outputTime.uncertaintyNs >= 5)
precondition(!Core.buildId().isEmpty)
do {
    try core.state(bytes: [255], nowNs: 123)
    fatalError("malformed input accepted")
} catch let error as TidkodError {
    precondition(!error.description.isEmpty)
}
#if TIDKOD_NATIVE
let v6 = try Endpoint.ipv6(IPv6Address("fe80::1")!, port: 4443, scopeID: 7)
precondition(v6.address() == "[fe80::1%7]:4443")
let v4 = try Endpoint.ipv4(.loopback, port: 4443)
precondition(v4.address() == "127.0.0.1:4443")
do {
    _ = try FollowerOptions(endpoint: .loopback(port: .any))
    fatalError("zero destination port accepted")
} catch is TidkodError { }
let engine = try Engine()
let options = LeaderOptions()
options.advertise(enabled: false)
options.sessionId(high: 0x1234, low: 0x5678)
options.bind(to: try .loopback(port: .any))
let leader = try engine.leader(options: options)
precondition(leader.advertisementStatus == .disabled)
let reader = try leader.reader()
try leader.seek(frames: -7, subframe: 0x80000000)
precondition(reader.read().frames == -7)
precondition(reader.read().hasSessionId && reader.read().sessionIdHigh == 0x1234)
let part = try leader.rotateSessionId()
precondition(reader.read().sessionIdLow == part.low)
try leader.setSessionId(high: 0x8765, low: 0x4321)
precondition(reader.read().sessionIdLow == 0x4321)
reader.snapshotInto(snapshot: snapshot)
let endpoints = try leader.localEndpoints()
precondition(endpoints.count() == 1)
let firstEndpoint = try endpoints.get(index: 0)
precondition(firstEndpoint.address() == leader.endpoint().address())
do { _ = try endpoints.get(index: 1); fatalError("out-of-range endpoint accepted") }
catch is TidkodError { }
let follow = try FollowerOptions(endpoint: leader.endpoint())
follow.pin(fingerprint: leader.fingerprint())
let follower = try engine.follower(options: follow)
let remote = try follower.reader()
let deadline = Date().addingTimeInterval(5)
while remote.read().synchronization != .synchronized {
    precondition(Date() < deadline)
    Thread.sleep(forTimeInterval: 0.01)
}
precondition(remote.read().subframe == 0x80000000)
precondition(remote.read().acceptedObservations >= 12)
print(remote.read().timecode)
try follower.shutdown()
try engine.shutdown()
precondition(snapshot.read(nowNs: 123).frames == -7)
#endif
print("Swift client passed")

let ltcEncoder = try LtcEncoder(numerator: 25, denominator: 1, dropFrame: false, sampleRate: 48000, frames: 90)
let ltcDecoder = try LtcDecoder(numerator: 25, denominator: 1, dropFrame: false, sampleRate: 48000)
var pcm = [Float](repeating: 0, count: 8000)
let ltcState = pcm.withUnsafeMutableBufferPointer { ltcEncoder.render(samples: $0) }
precondition(ltcState == 2)
var ltcOffset = 0
var ltcFrames = 0
pcm.withUnsafeBufferPointer { buffer in
    while ltcOffset < buffer.count {
        let samples = UnsafeBufferPointer(rebasing: buffer[ltcOffset...])
        let result = ltcDecoder.process(samples: samples, firstSample: UInt64(ltcOffset), firstSampleNs: 1_000_000_000 + UInt64(ltcOffset) * 1_000_000_000 / 48000)
        precondition(result.consumed > 0)
        ltcOffset += Int(result.consumed)
        if result.hasFrame { ltcFrames += 1; precondition(result.frames >= 90) }
    }
}
precondition(ltcFrames >= 3)
let emptyLtcState = ltcEncoder.render(samples: UnsafeMutableBufferPointer<Float>(start: nil, count: 0))
precondition(emptyLtcState == 2)
