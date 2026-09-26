# Independent advisor: Phase 2 re-review

## User request and scope
Re-review Coding Tools MCP at baseline `3adfb99f9f7fbc0ad029d7c835daa28da416441a` (0.2.4-custom.5). The user permits minor fixes, but significant functionality changes require approval. You are the independent reviewer/test worker, NOT the production implementer. Main agent owns final conclusions.

Design invariants: one Connector + Workspace Pool; each host conversation has an independent Active Project; explicit project designation binds automatically; incidental references do not switch it. History and Harness follow the selected project. Never silently fall back after losing an established binding. History bookkeeping must not invalidate code baselines, without exempting arbitrary real code. Keep standalone mode, permission modes and legacy history intact.

## Hard boundaries
- Production sources and existing tests are READ ONLY for you. No commit, push, merge, install, service restart or real user configuration writes. No process kills. No external project changes.
- Allowed writes: this advisor directory, sibling `repro` evidence directory, and ONE temporary new integration test `src-tauri/tests/phase2_rereview_probe.rs`. Do not overwrite a preexisting file. All test data must use tempfile outside real project data.
- Do not run cargo concurrently with another cargo process; main agent will not run cargo until you finish. Avoid dependency changes/network installs.
- Preserve raw stdout/stderr, actual exit codes, baseline SHA and test source. Use a small Python subprocess runner (not shell pipelines) if useful. Run focused repro tests with a bounded timeout. Remove ONLY your temporary integration test after capturing its source/evidence so the normal test suite is not left broken. Do not remove other files.

## Work
Independently read AGENTS.md, relevant source and the preceding remediation report. Investigate the changed persistence, History baseline exclusions, dispatcher snapshots and public response contracts. Do not rubber-stamp the previous PASS.

Focus hypotheses to confirm OR disprove with source locations and isolated deterministic reproductions:
1. A corrupt session-binding store is detected; repairing one session overwrites it; after another context recreation, do other lost sessions silently use default_cwd? The prior test only checks before the second restart.
2. Two long-lived ToolContext instances using the same workspace/harness store: can sequential writes of different sessions overwrite each other's durable bindings? Inspect production constructors to assess whether this is reachable (do NOT require real concurrent timing to reproduce).
3. History read/search/validate resolve_scope registers any history_dir before verifying archive validity. Can history_dir='src' or '.' exempt real code from a subsequently created Harness baseline, even when no valid History exists? Can read-only History calls cause persistent writes or become dependent on writable metadata?
4. Registered custom History exclusions when Active Project equals Workspace root (or no session project): does the early clone of ctx.harness drop those exclusions? Test actual checkpoint -> exec.
5. Does replacing a bound directory with an in-pool symlink/junction to another project silently retarget a conversation? Only test within temp dirs, no administrative privileges or real junctions. Mark untested if OS disallows.
6. Other high-confidence defects you find in this diff, especially user-visible contracts and recovery behavior. Do not expand to an unrelated rewrite.

Write safe-expectation regression probes: a confirmed product defect should fail its safe assertion and be reported as a RED repro, not labeled a product PASS. Separate baseline unmodified-suite results from red probes. No ignoring tests to claim success. A passing reproduction that asserts the vulnerable behavior may be included only if explicitly labeled as vulnerability confirmation, never an acceptance gate.

## Deliverables
- REPORT.md: findings severity, exact file:line, minimal sequence, observed vs expected, impact in plain Chinese, recommendation, small-fix vs approval-needed classification, limitations.
- Raw logs + a JSON manifest containing command, exit_code and SHA; retain probe source in sibling repro/.
- RESULT.json with status, findings and files_changed (advisor/test evidence only). State clearly whether each hypothesis is confirmed, disproved or untested.
- Prefer finish within 10 minutes; prioritize 1, 3 and 4. No production fixes in this task.
