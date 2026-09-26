"""Verify frozen evidence without editing it or its original manifest."""
from __future__ import annotations

import hashlib
import json
import subprocess
from pathlib import Path

here = Path(__file__).resolve().parent
root = here.parents[2]
old = root / "docs/verification/phase2-static-review-2026-09-26"
manifest = json.loads((old / "manifest.json").read_text(encoding="utf-8"))
checks = []
for item in manifest["files"]:
    data = (old / item["path"]).read_bytes()
    actual = hashlib.sha256(data).hexdigest()
    checks.append({"path": item["path"], "expected_bytes": item["bytes"], "actual_bytes": len(data), "expected_sha256": item["sha256"], "actual_sha256": actual, "matches": actual == item["sha256"], "original_reconstructed_by_adding_one_LF": hashlib.sha256(data + b"\n").hexdigest() == item["sha256"]})
for item in manifest["sources"]:
    data = subprocess.run(["git", "show", manifest["source_fix_commit"] + ":" + item["path"]], cwd=root, capture_output=True, check=True).stdout
    checks.append({"path": item["path"], "revision": manifest["source_fix_commit"], "representation": "git_blob", "expected_sha256": item["sha256"], "actual_sha256": hashlib.sha256(data).hexdigest(), "matches": hashlib.sha256(data).hexdigest() == item["sha256"]})
result = {"original_manifest_unchanged": True, "checks": checks, "mismatches": sum(not item["matches"] for item in checks)}
target = here / "previous-manifest-verification.json"
with target.open("x", encoding="utf-8") as stream:
    json.dump(result, stream, ensure_ascii=False, indent=2)
    stream.write("\n")
print(json.dumps(result, ensure_ascii=True, indent=2))
