"""Replay the unresolved review probes, without leaving failing tests installed.

Run from any directory: python <this-file> --label UNIQUE_LABEL
These probes intentionally fail on the reviewed candidate. Exit 101 is RED
evidence, not an acceptance PASS. Do not run concurrently with cargo elsewhere.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
from datetime import datetime, timezone
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--label", required=True)
    args = parser.parse_args()
    if not args.label.replace("-", "").replace("_", "").isalnum():
        parser.error("invalid evidence label")
    here = Path(__file__).resolve().parent
    root = here.parents[3]
    source = here / "probes/phase2_rereview_probe.rs"
    target = root / "src-tauri/tests/phase2_rereview_probe.rs"
    payload = source.read_bytes()
    output = here.parent / "checks" / args.label
    output.mkdir(parents=True, exist_ok=False)
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, capture_output=True, check=True).stdout.decode().strip()
    diff = subprocess.run(["git", "diff", "--", "src-tauri/src"], cwd=root, capture_output=True, check=True).stdout
    (output / "production.diff").write_bytes(diff)
    with target.open("xb") as handle:
        handle.write(payload)
    command = ["cargo", "test", "--locked", "--test", "phase2_rereview_probe", "--", "--test-threads=1", "--nocapture"]
    timed_out = False
    try:
        try:
            result = subprocess.run(command, cwd=root / "src-tauri", capture_output=True, timeout=600, check=False)
            code, stdout, stderr = result.returncode, result.stdout, result.stderr
        except subprocess.TimeoutExpired as error:
            code, stdout, stderr = 124, error.stdout or b"", error.stderr or b""
            timed_out = True
        (output / "stdout.log").write_bytes(stdout)
        (output / "stderr.log").write_bytes(stderr)
        (output / "probe.rs").write_bytes(payload)
    finally:
        removed = target.exists() and target.read_bytes() == payload
        if removed:
            target.unlink()
    receipt = {
        "head": head, "command": command, "utc": datetime.now(timezone.utc).isoformat(),
        "exit_code": code, "timed_out": timed_out, "temporary_test_removed": removed,
        "probe_sha256": hashlib.sha256(payload).hexdigest(),
        "production_diff_sha256": hashlib.sha256(diff).hexdigest(),
        "stdout_sha256": hashlib.sha256(stdout).hexdigest(),
        "stderr_sha256": hashlib.sha256(stderr).hexdigest(),
        "note": "Unresolved safe-expectation probes: failures are RED, not acceptance PASS."
    }
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(receipt, indent=2))
    print(stdout.decode("utf-8", errors="replace"))
    return code


if __name__ == "__main__":
    raise SystemExit(main())
