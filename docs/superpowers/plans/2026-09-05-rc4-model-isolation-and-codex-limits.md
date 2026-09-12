# RC4 Model Isolation and Codex Limits Implementation Plan

> **For agentic workers:** Use subagent-driven development to execute the assigned scope. Write regression tests before production changes, verify their failure, implement, then rerun targeted tests. The parent owns integration and the release; do not commit, push, publish, change real credentials, or clean caches independently.

**Goal:** Resolve every retained or unresolved item in the 2026-09-05 audit, add native Codex account limits, and build a tested Windows 1.0.13-rc.4 release.

**Architecture:** Session model identity, transport, and authentication must be read and updated coherently; stale asynchronous results must not mutate a newer session binding or selection. Shared settings and credentials need cross-process transaction protection and Windows-safe publication. Codex account quota is a separate typed shell service and provider-specific pager presentation, never an alias of xAI billing.

**Tech Stack:** Rust 1.94, Tokio actors, ACP extensions, serde, reqwest, ratatui, Windows filesystem APIs, package-scoped Cargo tests, release-dist.

---

## Constraints and ownership

- Preserve all preexisting additional-directory edits and current RC3 release artifacts.
- Native read/write tools must never access actual auth stores or secrets. Tests use temporary files and synthetic credentials; application-authenticated smoke tests must not expose credentials.
- Isolated builders own disjoint paths. The parent lands snapshots using product land/diff controls, never by copying a worktree.
- No simultaneous release-dist link and test builds. Check disk before the ship build. Use `CARGO_INCREMENTAL=0` for one-shot isolated builds to avoid recreating the removed incremental bloat.
- Small verification tasks use Terra High; tough implementation uses Sol Medium; the atomic-routing architecture and implementation use Astra Medium. Audit panels use Terra Medium. No Nemotron.
- User approved implementation, testing, cache cleanup and a local RC build. Remote publication, pushing, tagging and installation into a running user's binary remain separate actions.

## Task 1: Cache cleanup (parent)

- [x] Inspect active cargo/rustc/link processes and disk report.
- [x] Dry-run debug incremental and PDB cache cleanup.
- [x] Delete only those previewed categories using `turbo disk clean --safe --include debug-incremental,debug-pdbs --json`.
- [x] Verify successful cleanup: 243,918,015,570 logical bytes reclaimed; 330.7 GiB free. Keep dependency caches and current release binaries.

## Task 2: Settings persistence (parent)

**Primary files:** `crates/codegen/xai-grok-shell/src/util/config/persist.rs`, `settings_writes.rs`, `campaigns.rs`; existing config-lock callers in `util/config/mcp.rs` and `extensions/marketplace.rs` if their shared lock contract changes.

- [ ] Add temporary-home regressions for C1 (independent settings writers preserve both changes) and C2 (clear-default actually removes the persisted key and reloads without it).
- [ ] Verify the existing implementation fails the intended assertions, without invoking real config paths.
- [ ] Serialize the complete read-modify-write across processes. Re-read the user document under the lock, preserve unmodeled fields, propagate lock/read errors rather than pretending an unreadable file is empty, and support explicit default deletion.
- [ ] Preserve campaign dismissal ordering and Codex-default refusal; preserve settings writes that intentionally pin a default-valued value.
- [ ] Add a real child-process fixture sharing one temporary config rather than relying only on two threads.
- [ ] Run targeted shell config/persistence/campaign tests, then review the field-preservation and lock/error paths.

## Task 3: Auth lifecycle and Codex quota service (Sol Medium worker)

**Owned paths:** shell `src/auth/`, new `src/extensions/codex_usage.rs`, `src/extensions/mod.rs`, and the extension dispatch registration in `src/agent/mvp_agent/acp_agent.rs`. Request parent coordination before editing any other file. Do not change model-switch handlers or pager files.

- [ ] Add a failing refresh-versus-logout test (C12) using synthetic expired/rotated tokens and a controlled exchange.
- [ ] Revalidate the live credential scope under the lock before refresh and before persistence. Missing/deleted scope must abort an older refresh; a replaced account/token family must not be overwritten. Preserve legitimate sibling-token adoption and forced-401 refresh behavior.
- [ ] Add a failing Windows concurrent-reader/publication test (C13), then replace remove-then-rename with atomic replacement semantics. Verify failed publication retains the prior file and protections.
- [ ] Investigate legacy cleanup, refresh outer timeouts and Windows lock recovery identified in the earlier seed report; add regression coverage and fix demonstrated defects in this owned scope.
- [ ] Define a public, serde-compatible Codex quota response with explicit status, opaque account identity, freshness, plan, and arbitrary named quota buckets. Never serialize bearer/refresh tokens.
- [ ] Add fixture parser tests for primary/secondary windows, seconds-based durations and reset timestamps, additional metered features (including unknown/Spark buckets), absent/null/malformed fields and credits when supplied.
- [ ] Implement a read-only fixed-origin native fetch using `ensure_openai_codex_auth()`, paired bearer/account headers, bounded response size/time, redirect refusal, and at most one forced refresh/retry on 401. Ignore arbitrary inference URL overrides for quota routing. Inject auth/HTTP dependencies for mock-only tests.
- [ ] Expose `x.ai/codex-usage` through the shell ACP extension router, with no xAI subscription requirement, no external codex CLI, and no billing mutations.
- [ ] Report the exact Rust wire types and extension request/response contract for the pager integration worker.
- [ ] Run targeted shell auth/Codex quota tests and capture commands/results.

## Task 4: Pager model-switch ownership and ordering (Sol Medium worker)

**Owned paths:** `crates/codegen/xai-grok-pager/src/` excluding Codex quota presentation work, which begins after this worker lands. Preserve initial dirty app_view.rs content by three-way landing; never replace the whole file.

- [ ] Add regressions for C3 (A's save failure while B focused), C4 (A's incompatible-model question while B focused), C5 (two overlapping switches with a queued prompt), and C6 (ordinary rejection after optimistic typed selection).
- [ ] Bind asynchronous model-switch/persistence results to origin session/binding and a monotonically increasing request generation. Ignore stale results before changing any mirror, pending gate, modal, persisted preference, or queue.
- [ ] Ensure an obsolete completion cannot release work before the user's latest selection resolves. Serialize/coalesce overlapping requests as needed to align actual ACP state and displayed state, not merely hide old UI results.
- [ ] Keep confirmed and requested selections distinguishable. Restore the appropriate model and effort on all pre-mutation failure paths; selecting a rejected model again must retry.
- [ ] Target mismatch questions and their answers to their origin rather than current focus. Do not stash or clear another session's draft.
- [ ] Preserve session-local Codex behavior and existing non-Codex default-setting intent. A settings-save failure must not reverse an unrelated successful session switch.
- [ ] Run targeted pager settings/session/task-result/queue tests, including unchanged success behavior and session reuse/navigation.
- [ ] Hand off the switch-generation and session-binding API to the Codex UI worker.

## Task 5: Atomic session routing and inheritance (Astra Medium worker)

**Owned paths:** `crates/codegen/xai-chat-state/`; shell model/session routing sources in `src/session/acp_session_impl/`, `src/session/acp_session.rs`, `src/session/handle.rs`, `src/agent/config.rs`, `src/agent/handlers/model_switch.rs`, `src/agent/subagent/`, and `src/agent/mvp_agent/subagent_coordinator.rs`. Do not edit auth storage/login, extension dispatch, settings persistence, or pager files. Preserve existing additional-directory changes at integration.

- [ ] Use the independent Astra oracle's concrete API recommendation before changing state ownership.
- [ ] Add deterministic regressions for C7/C8/C10/C11 and the two earlier coverage gaps: provider-qualified Claude subscription identity versus API-key collisions, and already-built request model versus newly selected transport.
- [ ] Add one coherent read snapshot and one atomic update boundary for selected catalog identity, wire model, endpoint/backend and credentials. Preserve old serialized session compatibility, and keep secrets out of Debug/log/transport representations.
- [ ] Apply auth refresh only if its captured routing/login generation is still current. Cover both success writeback and failure clearing so old refreshes cannot erase the new credential.
- [ ] Use coherent snapshots for sampler reconstruction, detached side calls and child startup. Keep a request's model and transport selection consistent across scheduling and retries.
- [ ] Fix C9 by choosing the optional classifier's request model from its session snapshot, not the shared ModelsManager. Retain classifier generation cancellation semantics.
- [ ] Preserve deliberate execution-time inheritance for unpinned queued jobs, but resolve catalog identity/effort/dialect/auth from the same captured state. Explicit model pins stay explicit.
- [ ] Keep catalog identity separate from bare wire model slugs so equal model names cannot select another provider's signer or OAuth resolver.
- [ ] Run xai-chat-state and targeted shell routing/subagent/auth-memo tests. Add controlled mock wire checks proving no cross-provider key/header pairing.

## Task 6: Provider-aware Codex quota presentation (Sol Medium worker, after Tasks 3 and 4 land)

**Primary files:** pager `src/app/actions.rs`, `app_view.rs`, `agent_view/mod.rs`, `agent_view/render.rs`, `app/effects/`, `app/dispatch/{status.rs,billing.rs,auth.rs,prompt.rs,session/lifecycle.rs}`, `app/event_loop.rs`, `src/slash/commands/usage.rs`, `src/views/usage_modal.rs`; user guide `docs/user-guide/28-openai-codex.md`.

- [ ] Add reducer and rendering tests first using the actual shell quota response from Task 3 and origin/switch tokens from Task 4.
- [ ] Add dedicated Codex fetch/result/cache state; never mutate xAI credit balance, plan, paywall, auto-top-up, or warning state with Codex data.
- [ ] Route account allowance by effective main provider. Codex works independently of xAI team/consumer billing visibility. Unsupported providers show an honest unavailable state instead of Grok limits.
- [ ] Reject obsolete session, binding, model-switch, request-sequence, or account responses before cache/presentation mutation. On logout/account replacement detach old data; do not present a previous account's cached quota.
- [ ] Refresh on successful Codex selection and explicit `/usage`; coalesce/throttle background turn-end and focus refreshes. Stop irrelevant polling after switching away.
- [ ] Render actual duration/reset information and all server-supplied buckets; show freshness and unavailable/auth/error states. Do not invent remaining messages/tokens or label every secondary window weekly.
- [ ] Update full TUI, minimal `/usage`, prompt warnings, context/session tabs, welcome-versus-session behavior, and `/usage manage` consistently. Codex manage must not open Grok billing.
- [ ] Verify switch A to B to A, out-of-order completions, retries, missing fields, expired/reset windows and repeated commands. Preserve existing xAI billing tests.

## Task 7: Integration and independent review (parent)

- [ ] Inspect each isolated snapshot, land only its allowed scope using product diff/land, and preserve all unrelated dirty changes.
- [ ] Re-run targeted packages against the integrated parent tree; no child-only green result substitutes for this.
- [ ] Independently review scope coverage and code quality with explicit approved model/effort pins. Run the Terra Medium audit recipe against changed areas and the complete audit checklist.
- [ ] Fix and retest every residual in-scope finding. Record a refutation with evidence for any original candidate shown not to be a defect; no candidate silently disappears.
- [ ] Verify the TUI's interaction paths through existing reducer/render/PTTY test surfaces and appropriate local smoke runs. Browser verification applies if any web surface is changed.
- [ ] Record exact test counts, failures/fixes and any genuinely blocked live account smoke check without reading secrets.

## Task 8: RC4 release (parent)

- [ ] Set `VERSION`, pager-bin and version crate wire versions to `1.0.13-rc.4`, update the lockfile's local package versions, release notes, and current-version documentation. Do not create a fake published release link.
- [ ] Check free disk and ensure no test builds are still running.
- [ ] Build with `$env:GROK_VERSION = (Get-Content VERSION -Raw).Trim(); cargo build -p xai-grok-pager-bin --profile release-dist --bin turbo`.
- [ ] Wait for successful build, verify `target/release-dist/turbo.exe version` reports RC4, and run available non-mutating CLI smoke checks.
- [ ] Package the verified binary at the zip root with the required `bundled/` payload using the existing Windows release layout. Produce a new RC4 archive and checksum without discarding the existing RC3 archive/checksum information.
- [ ] Verify archive contents, embedded version, SHA256, final diff and line endings. Report exact local artifact paths; do not claim publication or installation.
