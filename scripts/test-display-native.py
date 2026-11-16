#!/usr/bin/env python3
"""Check Mac hardware encoding and native-pipe backpressure without capturing a desktop.

Writes synthetic codec fixtures under target/display-check. No screen is added,
no mouse/keyboard input is generated, and installed Kayiver processes are untouched.
"""
import argparse
import json
import pathlib
import struct
import subprocess
import threading
import time


def exact(stream, size):
    result = bytearray()
    while len(result) < size:
        block = stream.read(size - len(result))
        if not block:
            raise RuntimeError("truncated native video output")
        result.extend(block)
    return bytes(result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/debug/kayiver")
    parser.add_argument("--output", default="target/display-check")
    args = parser.parse_args()
    output = pathlib.Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    child = subprocess.Popen(
        [args.binary, "screen-native", "test-source", "1920", "1080", "60", "20000000"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    started = time.monotonic()
    deadline = threading.Timer(15, child.kill)
    deadline.start()
    try:
        ready = json.loads(child.stdout.readline())
        assert ready.get("hardware") and ready.get("synthetic"), ready
        with (output / "synthetic-1080p.frames").open("wb") as fixture, (output / "synthetic-1080p.h264").open("wb") as video:
            released = None
            for frame in range(1, 181):
                size, captured = struct.unpack(">IQ", exact(child.stdout, 12))
                assert 0 < size <= 8 * 1024 * 1024
                data = exact(child.stdout, size)
                if frame == 2:
                    # The first frame was deliberately left unacknowledged:
                    # frame 2 must be freshly generated after it is released.
                    assert captured >= released, "old frames accumulated in the native pipe"
                fixture.write(struct.pack(">IQQ", size, captured, frame))
                fixture.write(data)
                video.write(data)
                if frame == 1:
                    time.sleep(0.25)
                    released = time.time_ns() // 1000
                child.stdin.write(bytes([1]))
                child.stdin.flush()
        assert child.stdout.read(1) == b"", "unexpected extra frame"
        assert child.wait(timeout=5) == 0, child.stderr.read().decode()
        print(json.dumps({"frames": 180, "hardware_encoder": True,
                          "backpressure": "passed", "seconds": round(time.monotonic() - started, 3)}))
    finally:
        deadline.cancel()
        if child.poll() is None:
            child.stdin.write(bytes([0]))
            child.stdin.flush()
            child.stdout.close()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


if __name__ == "__main__":
    main()
