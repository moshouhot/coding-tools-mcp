# Bounded final independent opinion (read-only)

You have at most 180 seconds. Do NOT run shell commands, cargo, git, discovery, filesystem scans, installs or skills. Read ONLY the following three files using the read tool, then write REPORT.md and RESULT.json beside this TASK.md. No other writes. The main agent runs tests separately.

1. `src-tauri/src/tools/context.rs` (416 lines).
2. `src-tauri/src/tools/dispatch.rs` lines 180-315 only.
3. `docs/verification/phase2-rereview-2026-09-26/checks/independent-red-probes/stderr.log`.

Context: .5 baseline 3adfb99f9f7fbc0ad029d7c835daa28da416441a had 350 passing normal tests. New independently executed safe-expectation tests fail for: corrupt binding store -> rebind another session -> SECOND restart allows old session default_cwd fallback; two separate contexts overwrite each other's whole-map store; readonly History search(history_dir=src) exempts real code from baseline; readonly History search requires metadata write access; root Active Project drops custom exclusions; in-pool symlink replacement reads beta instead of original alpha. The first advisor built these probes but its final filesystem search timed out, so do not claim that run completed successfully.

Only production change now is two lines in request_permissions echoes: `strip_internal_context(effective_args.clone())` for both granted and unsupported replies. Helper strips top-level `_host_session_key` and `_active_project_root`. It leaves permission decisions and nested user arguments unchanged. Two tests failed before this fix; post-fix testing is underway.

User explicitly permits small fixes but requires approval for major functional changes. Design: one Connector + pool, per-conversation project binding, no silent cross-project fallback, History bookkeeping must not weaken real code checks, no task requirement for standalone use. We propose HOLD .5 live acceptance, keep small fixes on local audit branch, no push/merge/deploy. Require approval for persistent recovery/quarantine + shared-store transactional writes, History directory ownership/read-only separation + root-scope parity, and rejection of changed canonical binding targets. Do not prescribe a heavyweight DB or a new connector per project.

REPORT.md in Chinese, at most 500 words: assess whether small fix is reasonable; whether HOLD is justified by observed evidence; flag any overstatement, especially two contexts is not the same as two chats in one shared context and directory retarget requires local filesystem change. State any inability to verify History source directly (only logs provided). RESULT.json with verdict, small_fix_approved, major_findings_require_user_approval, limitations. Finish immediately after these two files. No full repo audit claim.
