using System;
using System.Net;
using System.Threading;
using System.Threading.Tasks;
using System.Runtime.CompilerServices;
using Tidkod;

static void Check(bool value, string message) { if (!value) throw new Exception(message); }
using (var format = new TimecodeFormat(30000, 1001, true)) {
    Check(format.Position(0,1,0,2).Frames == 1800, "drop-frame ABI");
    try { format.Position(0,1,0,0); throw new Exception("invalid drop label accepted"); }
    catch (TidkodException) { }
}
using (var core = new Core())
{
    var initial = core.Read(123);
    Check(initial.Frames == 0 && initial.FpsNumerator == 30 && initial.Synchronization == Synchronization.Uninitialized, "fallback");
    using var captured = core.Snapshot();
    using var snapshot = new TimecodeSnapshot();
    core.SnapshotInto(snapshot);
    using var copied = snapshot.Copy();
    Check(copied.Read(123).AcceptedObservations == 0, "snapshot count");
    Check(!copied.NextBoundary(123).Valid, "fallback snapshot boundary");
    Check(copied.ReadForPresentation(123, TimeSpan.FromMilliseconds(1)).Frames == initial.Frames, "snapshot presentation");
    Check(copied.ReadSample(123, 48000, 48000).Frames == initial.Frames, "sample phase");
    Check(!copied.Read(123).Aligned, "uninitialized alignment");
    using var bridge = new ClockBridge(100, 10000, 110);
    var outputTime = bridge.Convert(10100);
    Check(outputTime.LocalNs == 205 && outputTime.UncertaintyNs >= 5, "host clock bridge");
    Check(Core.BuildId().Length > 0, "core build identity");
    try { core.State(new byte[] {255}, 123); throw new Exception("bad protobuf accepted"); }
    catch (TidkodException e) { Check(e.Status != 0 && e.Message.Length > 0, "managed error"); }
    core.Connected();
    Check(core.Probe(1_000_000).Length > 0, "owned byte output");
    core.Dispose();
    Check(copied.Read(123).Frames == initial.Frames, "snapshot owns state");
    try { core.Read(0); throw new Exception("disposed handle accepted"); }
    catch (ObjectDisposedException) { }
}
using (var core = Core.Configured(long.MaxValue - 100, uint.MaxValue, 30, 1, false, 0.1, 1, 3))
{
    Check(core.Read(0).Frames == long.MaxValue - 100 && core.Read(0).Subframe == uint.MaxValue, "exact Q32");
    Check(core.ReadForPresentation(0, TimeSpan.FromMilliseconds(20)).Frames == long.MaxValue - 100, "presentation duration");
    try { core.ReadForPresentation(0, TimeSpan.FromTicks(-1)); throw new Exception("negative delay accepted"); }
    catch (ArgumentOutOfRangeException) { }
    for (int i=0;i<10_000;i++) core.Read(0);
    using var reusable = new TimecodeSnapshot();
    for (int i=0;i<10_000;i++) { core.SnapshotInto(reusable); reusable.Read(0); }
    long before=GC.GetAllocatedBytesForCurrentThread();
    for (int i=0;i<100_000;i++) { core.Read((ulong)i); core.SnapshotInto(reusable); reusable.Read((ulong)i); }
    Check(GC.GetAllocatedBytesForCurrentThread()==before, "reads allocate managed memory");
}
#if TIDKOD_NATIVE
using var engine = new Engine();
using var options = new LeaderOptions();
options.SessionId(0x1234, 0x5678);
options.Advertise(false);
options.Bind(new IPEndPoint(IPAddress.Loopback, 0));
using var leader = engine.Leader(options);
using (var sessionReader = leader.Reader()) {
    Check(sessionReader.Read().HasSessionId && sessionReader.Read().SessionIdHigh == 0x1234, "configured recording ID");
    var part = leader.RotateSessionId();
    Check(sessionReader.Read().SessionIdLow == part.Low, "rotated recording ID");
    leader.SetSessionId(0x8765, 0x4321);
    Check(sessionReader.Read().SessionIdLow == 0x4321, "manual recording ID");
}

leader.Seek(-7, 0x80000000);
using var reader = leader.Reader();
Check(reader.Read().Frames == -7 && reader.Read().Subframe == 0x80000000, "native Q32");
using var saved = reader.Snapshot();
using var reusableNative = new TimecodeSnapshot();
reader.SnapshotInto(reusableNative);
using var endpoints = leader.LocalEndpoints();
Check(endpoints.Count() == 1, "specific listener endpoint");
using var firstEndpoint = endpoints.Get(0);
Check(firstEndpoint.Port() > 0, "listener port");
try { using var invalidEndpoint = endpoints.Get(endpoints.Count()); throw new Exception("endpoint bounds accepted"); }
catch (TidkodException) { }
using var endpoint = leader.Endpoint();
using var followOptions = new FollowerOptions(endpoint.ToIPEndPoint());
followOptions.Pin(leader.Fingerprint());
using var follower = engine.Follower(followOptions);
using var remote = follower.Reader();
for (int i=0;i<10_000;i++) reader.Read();
long readBefore=GC.GetAllocatedBytesForCurrentThread();
for (int i=0;i<100_000;i++) reader.Read();
Check(GC.GetAllocatedBytesForCurrentThread()==readBefore, "native reads allocate managed memory");
var until=DateTime.UtcNow.AddSeconds(5);
while(remote.Read().Synchronization != Synchronization.Synchronized)
{
    Check(DateTime.UtcNow<until,"follower acquisition");
    Thread.Sleep(10);
}
Check(remote.Read().Frames == -7 && remote.Read().Subframe == 0x80000000, "follower reading");
Check(remote.Read().AcceptedObservations >= 12, "accepted observation count");
Console.WriteLine(remote.Read().Timecode);
using var ipv6=Endpoint.From(new IPEndPoint(IPAddress.Parse("fe80::1%7"),4443));
Check(ipv6.ToIPEndPoint().Address.ScopeId == 7,"IPv6 scope");
try { using var invalid = new FollowerOptions(new IPEndPoint(IPAddress.Loopback,0)); throw new Exception("port zero accepted"); }
catch (TidkodException) { }
// Reads and disposal may race without freeing native memory under a call.
var raced=leader.Reader();
var worker=Task.Run(() => { for(int i=0;i<10_000;i++) { try { raced.Read(); } catch(ObjectDisposedException) { break; } } });
raced.Dispose(); worker.GetAwaiter().GetResult(); raced.Dispose();
// Dropping wrappers without Dispose still returns native reader slots.
for (int i=0;i<100;i++) {
    AbandonReader(leader);
    if (i % 10 == 0) { GC.Collect(); GC.WaitForPendingFinalizers(); }
}
follower.Reconnect();
follower.Shutdown();
engine.Shutdown();
Check(saved.Read(123).Frames == -7, "snapshot survives shutdown");
#endif
Console.WriteLine("C# wrappers passed");

#if TIDKOD_NATIVE
[MethodImpl(MethodImplOptions.NoInlining)]
static void AbandonReader(Leader leader) { var reader = leader.Reader(); reader.Read(); }
#endif

using (var ltcEncoder = new LtcEncoder(25, 1, false, 48000, 90))
using (var ltcDecoder = new LtcDecoder(25, 1, false, 48000)) {
    var pcm = new float[8000];
    Check(ltcEncoder.Render(pcm) == 2, "LTC render");
    int offset = 0, frames = 0;
    while (offset < pcm.Length) {
        var result = ltcDecoder.Process(pcm.AsSpan(offset), (ulong)offset, 1000000000UL + (ulong)offset * 1000000000UL / 48000);
        Check(result.Consumed > 0, "LTC progress"); offset += (int)result.Consumed;
        if (result.HasFrame) { frames++; Check(result.Frames >= 90, "LTC label"); }
    }
    Check(frames >= 3, "LTC frames");
    Check(ltcEncoder.Render(Span<float>.Empty) == 2, "LTC empty output");
}
