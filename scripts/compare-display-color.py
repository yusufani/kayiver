#!/usr/bin/env python3
"""Compare synthetic GPU output with a software-decoded reference of the same H.264.

Use the 64px calibration blocks from test-display-native.py. Sample their
interiors: GPU and software decoders legitimately interpolate chroma differently
at block edges. Inputs are tightly packed BGRA, not captured user desktops.
"""
import argparse
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("reference", type=Path)
parser.add_argument("candidate", type=Path)
parser.add_argument("--width", type=int, default=1920)
parser.add_argument("--height", type=int, default=1080)
parser.add_argument("--tolerance", type=int, default=3)
args = parser.parse_args()
reference, candidate = args.reference.read_bytes(), args.candidate.read_bytes()
expected = args.width * args.height * 4
if len(reference) != expected or len(candidate) != expected:
    raise SystemExit("Expected two equally sized, tightly packed BGRA frames")
errors = []
for y in range(32, args.height, 64):
    for x in range(160, args.width, 64):
        i = (y * args.width + x) * 4
        errors.extend(abs(reference[i+c] - candidate[i+c]) for c in range(3))
if not errors:
    raise SystemExit("Calibration frame is too small")
report = {"patches": len(errors)//3, "max_channel_error": max(errors),
          "mean_channel_error": round(sum(errors)/len(errors), 4),
          "tolerance": args.tolerance, "passed": max(errors) <= args.tolerance}
print(json.dumps(report))
raise SystemExit(0 if report["passed"] else 1)
