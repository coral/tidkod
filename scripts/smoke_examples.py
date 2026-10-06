#!/usr/bin/env python3
"""Build and exercise every example, including actual state delivery across processes."""
import pathlib
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
subprocess.run(["cargo", "build", "-p", "tidkod", "--examples", "--locked"], cwd=ROOT, check=True)
BIN = ROOT / "target" / "debug" / "examples"
# Device-independent smoke: CI has no capture/playback device.
for name in ["ltc_in", "ltc_out"]:
    subprocess.run([str(BIN / name), "--help"], check=True, timeout=10)
with tempfile.TemporaryFile(mode="w+") as log:
    leader = subprocess.Popen([str(BIN / "leader"), "--bind", "127.0.0.1:0", "--no-mdns", "--seconds", "4"], stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT, text=True)
    try:
        deadline = time.monotonic() + 5
        endpoint = None
        while time.monotonic() < deadline:
            log.seek(0)
            for line in log.read().splitlines():
                if line.startswith("Listening "):
                    endpoint = line.split()[1]
                    break
            if endpoint:
                break
            time.sleep(0.02)
        assert endpoint, "leader did not report its listening address"
        leader.stdin.write("play\nshuttle -1 2\n")
        leader.stdin.flush()
        follower = subprocess.run([str(BIN / "follower"), "--address", endpoint, "--seconds", "2"], text=True, capture_output=True, check=True, timeout=10)
        assert "Synchronized" in follower.stdout, follower.stdout + follower.stderr
        assert "-0.500x" in follower.stdout, follower.stdout
        leader.stdin.write("quit\n")
        leader.stdin.flush()
        assert leader.wait(timeout=10) == 0
    finally:
        if leader.poll() is None:
            leader.terminate()
            leader.wait(timeout=5)
tracked = subprocess.run([str(BIN / "tracked"), "--bind", "127.0.0.1:0", "--no-mdns", "--seconds", "8"], text=True, capture_output=True, check=True, timeout=15)
assert "Tracked/Degraded" in tracked.stdout, tracked.stdout
assert "-1.000x" in tracked.stdout, tracked.stdout
assert "+0.000x" in tracked.stdout, tracked.stdout
print("All examples passed: cross-process reverse sync, tracked jitter/loss/pause/reverse.")
