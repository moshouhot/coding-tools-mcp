# Current-HEAD verification before returning for approval

## Decision

**HOLD: the six unresolved behavior defects in REPORT.md remain reproducible.**

This addendum does not modify or replace the earlier report, manifest, logs, or advisor output.
It records a fresh source inspection and test replay on the actual local audit HEAD:

```text
branch: audit/phase2-rereview-20260926
tested HEAD: 880c5149eb52d68f161809a42e794dcb39d9118e
candidate source version: 0.2.4-custom.5
original review baseline: 3adfb99f9f7fbc0ad029d7c835daa28da416441a
live connector version observed: 0.2.4-custom.4
```

No production changes were made during this final replay. The existing minor-fix commit
`3d3134f725c3f87092b43d29e578f3cedab849b8` was inspected: its production diff consists of the
two public permission-response sanitizations, plus tests and documentation. The previously
identified functional fixes were not implemented without approval.

## Fresh evidence

| Evidence directory under checks/ | Command | Result |
| --- | --- | --- |
| final-head-confirmation | cargo test --locked --test phase2_rereview_probe -- --test-threads=1 --nocapture | exit 101; six safe-expectation assertions failed; one deliberately vulnerable-behavior confirmation passed |
| final-head-rust | cargo test --locked | exit 0; 352 passed, 0 failed, 0 ignored |
| final-head-frontend | npm run check | exit 0; svelte-check: 0 errors, 0 warnings |

Each directory contains unmodified stdout/stderr and a receipt with the command, tested HEAD,
exit code and SHA-256 values. Normal-suite receipts additionally record tracked source hashes.
The red-probe runner recorded an empty uncommitted production diff and confirmed removal of
only its own temporary integration test. No actual user project was used as a test fixture.

The normal suite passing does not supersede the six red probes. The deliberately vulnerable
confirmation is not a product acceptance test. The symlink probe failed its safety assertion
after successfully creating a fixture symlink; it did not take the UNTESTED early return.

## Source and result reconciliation

- A / R1-R2: context.rs still persists a cloned in-memory map without a shared read-modify-write
  transaction. Its load-error flag is not serialized. A second restart after repairing another
  session still loses the old session's fail-closed state; two contexts still lose one binding.
- B / R3-R5: History resolve_scope still registers the requested directory for every caller,
  including search, before checking archive ownership. A source-directory search still exempts
  real code. A blocked managed-metadata directory still breaks read-only search. Root-project
  Harness construction still drops registered custom exclusions.
- C / R6: validate_bound_project still checks only that the current canonical destination is a
  directory inside the pool, not that it equals the destination originally bound. A fixture
  replacement link still makes the old conversation read beta/name.txt.

For R4, the observed Windows error 183 comes from the fixture's ordinary file occupying the
managed-project-paths directory location. It is not evidence of an ACL denial or a rename race.
For R2, two ToolContext objects sharing a store are not the same as two chats sharing one
ToolContext. For R6, a local filesystem replacement is a prerequisite. These qualifications
from REPORT.md remain part of the conclusions.

The existing independent advisor report was read alongside its limitations. The fresh main
reviewer source inspection and command receipts supply the direct evidence; the advisor's
opinion is not substituted for a fresh execution or a product-level acceptance result.

## Release boundary

No push, merge, package build, installation, live restart, or live binding-store manipulation
was performed for this final verification. Keep the review and evidence on the local audit
branch. Obtain approval for groups A/B/C before functional remediation and a new candidate.
The user is not being asked to install .5 as a formally accepted candidate at this stage.
