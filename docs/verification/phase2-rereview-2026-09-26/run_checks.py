"""Bounded review evidence capture. Writes only beside this script."""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
BASE = "3adfb99f9f7fbc0ad029d7c835daa28da416441a"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=["impact", "review", "rust", "frontend", "minor", "probe", "diff", "scope", "detect"])
    parser.add_argument("--label", required=True)
    args = parser.parse_args()
    if not args.label.replace("-", "").replace("_", "").isalnum():
        parser.error("label must contain only letters, digits, underscore or hyphen")
    out = HERE / "checks" / args.label
    out.mkdir(parents=True, exist_ok=False)
    cwd = ROOT
    input_bytes = None
    if args.mode in ("impact", "review"):
        tool = "code_insight" if args.mode == "impact" else "code_review"
        command = ["cmd", "/c", str(ROOT / ".mcp-probe-kit/bin/probe.cmd"), "exec", tool, "--stdin"]
        parameters = {"project_root": str(ROOT)}
        if args.mode == "impact":
            parameters.update(mode="impact", target="call_tool_prepared", direction="upstream", include_tests=True, max_depth=3)
        else:
            parameters.update(diff_mode="range", base_ref="afa476bff3abfd0dafa65b748271ecb9d78d7ced", head_ref=BASE, focus="all", max_diff_chars=120000)
        input_bytes = json.dumps(parameters).encode("utf-8")
        (out / "input.json").write_bytes(input_bytes)
    elif args.mode == "rust":
        cwd = ROOT / "src-tauri"
        command = ["cargo", "test", "--locked"]
    elif args.mode == "minor":
        cwd = ROOT / "src-tauri"
        command = ["cargo", "test", "--locked", "--test", "phase2_rereview_minor", "--", "--nocapture"]
    elif args.mode == "probe":
        cwd = ROOT / "src-tauri"
        executable = cwd / "target/debug/deps/phase2_rereview_probe-98ee4af2e4484f84.exe"
        source = cwd / "tests/phase2_rereview_probe.rs"
        if not source.exists():
            source = HERE / "repro/probes/phase2_rereview_probe.rs"
        (out / "probe.rs").write_bytes(source.read_bytes())
        (out / "binary-sha256.txt").write_text(hashlib.sha256(executable.read_bytes()).hexdigest() + "\n", encoding="ascii")
        command = [str(executable), "--test-threads=1", "--nocapture"]
    elif args.mode == "detect":
        cli = Path.home() / "AppData/Local/mcp-probe-kit/runtimes/gitnexus/1.6.9/win32-x64-node24/node_modules/gitnexus/dist/cli/index.js"
        command = ["node", str(cli), "detect-changes", "--scope", "all", "--repo", "coding-tools-mcp"]
    elif args.mode == "frontend":
        command = ["cmd", "/c", "npm", "run", "check"]
    elif args.mode == "scope":
        command = ["git", "diff", "--name-status", BASE]
    else:
        command = ["git", "diff", "--check"]
    start = time.monotonic()
    timed_out = False
    try:
        result = subprocess.run(command, cwd=cwd, input=input_bytes, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=600, check=False)
        code, stdout, stderr = result.returncode, result.stdout, result.stderr
    except subprocess.TimeoutExpired as error:
        timed_out = True
        code, stdout, stderr = 124, error.stdout or b"", error.stderr or b""
    (out / "stdout.log").write_bytes(stdout)
    (out / "stderr.log").write_bytes(stderr)
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, check=True).stdout.decode().strip()
    tracked = subprocess.run(["git", "ls-files", "-z", "src-tauri/src", "src-tauri/tests", "src", "package.json", "package-lock.json", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock"], cwd=ROOT, capture_output=True, check=True).stdout
    hashes = {name.decode("utf-8"): hashlib.sha256((ROOT / name.decode("utf-8")).read_bytes()).hexdigest() for name in tracked.split(b"\0") if name}
    receipt = {"mode": args.mode, "command": command, "cwd": str(cwd), "head": head, "review_baseline": BASE, "utc": datetime.now(timezone.utc).isoformat(), "exit_code": code, "timed_out": timed_out, "duration_seconds": round(time.monotonic() - start, 3), "stdout_sha256": hashlib.sha256(stdout).hexdigest(), "stderr_sha256": hashlib.sha256(stderr).hexdigest(), "source_sha256": hashes}
    (out / "receipt.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({k: v for k, v in receipt.items() if k != "source_sha256"}, ensure_ascii=True, indent=2))
    if args.mode in ("impact", "review"):
        print(stdout.decode("utf-8", errors="replace")[:12000])
    else:
        print(stdout.decode("utf-8", errors="replace")[-10000:])
        print(stderr.decode("utf-8", errors="replace")[-2500:])
    return code


if __name__ == "__main__":
    raise SystemExit(main())
