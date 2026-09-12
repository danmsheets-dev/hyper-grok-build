# RC4 verification record

Status: implementation and integration in progress. No RC4 release binary has been built or published yet.

## Baseline and safety

- Source baseline: `7ab727263`, wire `1.0.13-rc.3`.
- Preexisting additional-directory changes and RC3 release artifacts are preserved.
- Debug cleanup completed: 243,918,015,570 logical bytes (227.2 GiB) of incremental/PDB files removed. Free space afterward: 330.7 GiB.
- Current release binary, archive, dependency caches, and release-dist caches were retained.
- A disk monitor checks for less than 60 GiB free during builds; it does not delete anything.
- Tests use synthetic credentials, loopback endpoints and temporary config stores. No real credential-store inspection is part of this verification.

## Parent settings persistence scope

Files: shell `util/config/persist.rs`, `persist_rc4_tests.rs`, `mcp.rs`, and `extensions/marketplace.rs`.

| Stage | Result |
|---|---|
| Initial RC4 regressions, before production changes | 1 helper passed; 2 intended assertions failed: clear-default retained the key, and an independent writer held no cross-process transaction lock. |
| Initial persistence fix | 59 tests passed, 0 failed. |
| Independent Terra High review | Found MCP mutation and startup marketplace bypasses; public-clear coverage and child cleanup also required improvement. |
| Added bypass/malformed regressions before extending the fix | 3 passed; 2 intended assertions failed: disabled-tools overwrote malformed config and completed while another process held the config lock. |
| Expanded persistence fix | 61 tests passed, 0 failed; includes the real child-process MCP/upsert/delete/startup transaction cases, malformed-config preservation, and public clear-default path. |
| Existing MCP, campaign and marketplace/purge regressions | 72 tests passed, 0 failed. |
| Follow-up Terra High review | GO for the four-file scope; no remaining high-confidence findings. |

Commands:

```powershell
$env:CARGO_INCREMENTAL = '0'
cargo test -p xai-grok-shell --lib util::config::persist -- --test-threads=4 --nocapture
```

The resulting libtest executable was reused for these OR-ed filters, avoiding repeated Cargo build-script invalidation:

```text
util::config::mcp::tests
util::config::campaigns::tests
extensions::marketplace::official_source_tests
extensions::marketplace::default_skills
```

The 61 and 72 runs are separate groups, totaling 133 passing tests for this implementation stage. They do not certify the unlanded routing, auth, pager or quota changes. Existing workspace warnings remain; no warning suppression or ignored regression was added to obtain these results.

## Remaining release gates

- Complete and independently review the isolated auth/quota backend, pager switch-ordering and atomic-routing implementations.
- Land only owned snapshots and re-run tests against the combined parent tree.
- Implement and verify provider-aware Codex quota presentation using the finalized wire and switch-generation contracts.
- Close every retained audit finding and resolve/refute each previously unverified candidate with evidence.
- Run final independent review and targeted regression groups, then perform the RC4 version bump, release-dist build, CLI/UI smoke checks and archive/checksum verification.

This record will be updated with actual integrated results and release artifact hashes; the listed remaining gates are not claimed complete.
