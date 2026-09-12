# Multi-window model isolation and Codex account limits

Date: 2026-09-05. Baseline: `7ab727263` (1.0.13-rc.3), with the preexisting uncommitted additional-directory changes preserved.

## Verdict

RC3 addresses the specific **Codex selection leaking into the shared startup default**. It does not establish complete multi-window or concurrent model-switch isolation.

The replacement `deep-audit-terra` recipe used explicitly pinned Terra workers with a Medium effort definition, including independent verifiers. It retained **nine source-verified findings: four high and five medium**. Four further candidates remain unverified. The report is therefore **partial**, not a clean bill of health. No observed production credential disclosure is claimed.

Native **OpenAI Codex account quota display is feasible**. Official Codex sources expose usage percentages, reset times, and multiple quota buckets. A separate Sol Medium review checked the proposed integration against Turbo's code. It has not been implemented or tested against the user's account.

## What the recent fix does

The parent inspected the actual `7ab727263` diff, rather than attributing every current defect to that commit:

- Native `openai-codex/*` and legacy `codex:*` identifiers are classified as session-scoped.
- Codex selections skip both the typed default-model persistence path and the preferred-model persistence path.
- The shell's persistence helper refuses to save these identifiers as shared `models.default`.
- Configuration reload does not retarget the process default to a leaked Codex setting. Startup resolution also ignores a Codex default from configuration; explicit CLI/environment selections remain allowed.
- Spark aliases resolve to the native Codex catalog entry.

Evidence: `crates/codegen/xai-grok-models/src/platforms.rs:1197-1204`; `crates/codegen/xai-grok-pager/src/acp/router.rs`; pager `app/dispatch/settings/setters.rs:1848-1866` and `app/dispatch/session/lifecycle.rs:1520-1527`; shell `agent/models.rs:920-951`, `agent/models/resolution.rs:115-136`, and `util/config/campaigns.rs:455-467`.

Other providers' default settings can still be shared. A shared startup preference is not the same thing as an already-running session's model; the findings below distinguish those cases.

## Retained findings

Paths below are relative to `crates/codegen/`. IDs are from the replacement Terra audit, not the original audit.

### High

| ID | Finding and trigger | Source | Recommended correction |
|---|---|---|---|
| C3 | A delayed default-setting save failure for session A rolls back whichever session is focused when it arrives. If B is focused, the code can emit an actual reverse model switch for B. | `xai-grok-pager/src/app/dispatch/settings/ui.rs:1093-1137`; `app/dispatch/task_result.rs:1587-1596` | Bind persistence results and rollback to the originating session and operation generation. Do not reverse a different session because focus changed. |
| C7 | An xAI refresh captures the old credentials/route, awaits refresh, and writes them back after a compatible switch installed a third-party route. The old token can replace the new route's credential. | `xai-grok-shell/src/session/acp_session_impl/sampler_turn.rs:1733-1754`; `model_switch.rs:48-81` | Generation-check refresh writeback and make route/auth changes atomic. Test detached/background work overlapping a switch. |
| C10 | Child startup reads parent routing and credentials through separate actor queries while a switch publishes them separately. It can pair one provider's endpoint with another provider's key. | `xai-grok-shell/src/agent/subagent/mod.rs:815-820,895-939`; `session/acp_session_impl/model_switch.rs:48-81` | Read and publish one coherent model/provider/endpoint/auth snapshot. Test forced interleavings during queued-child promotion. |
| C12 | Codex refresh captures a refresh token before locking. Another process logs out and deletes the scope. Refresh later acquires the lock, interprets absence as no sibling to adopt, and can recreate the logged-out credential. | `xai-grok-shell/src/auth/openai_codex/login.rs:592-639,672-739,775-800`; `auth/storage.rs:703-730` | Treat scope deletion or changed login generation as a barrier to earlier refresh operations; revalidate under the lock before exchange and persistence. |

C7 and C10 are potential wrong-origin credential transmission paths, not evidence that a real key has leaked. A receiver rejecting an invalid credential would not undo disclosure of a transmitted header. Until corrected and tested, avoiding provider changes while background work or child startup is active is a prudent precaution, not a proven complete mitigation.

### Medium

| ID | Finding and trigger | Source | Recommended correction |
|---|---|---|---|
| C1 | Two processes share config. B reads the old model default, A successfully saves a new default, then B saves only a theme but merges its stale typed model section back over A's update. | `xai-grok-shell/src/util/config/persist.rs:7-11,32-37,166-180,205-214` | Use an interprocess lock across the entire read-modify-write and field-scoped updates. Atomic rename does not prevent lost updates. |
| C4 | An incompatible-switch result for A cleans up A, then opens its question using current focus B, stashing B's draft and using B's context for the answer. | `xai-grok-pager/src/app/dispatch/session/lifecycle.rs:236-285,1477-1547,1554-1571` | Carry explicit origin session/binding identity into the question and its answer. |
| C6 | The typed default setter optimistically displays M1. An ordinary switch error before remote mutation leaves that display intact, clears the pending gate, and can release work to the still-M0 session. | `xai-grok-pager/src/app/dispatch/settings/setters.rs:1830-1883`; `app/dispatch/session/lifecycle.rs:1479-1547` | Separate confirmed selection from pending selection; reconcile all failure paths before releasing work. |
| C9 | The optional laziness classifier gets its model from shared ModelsManager but its client from the individual session. Another session's switch can produce an unintended request-model/transport combination. | `xai-grok-shell/src/session/acp_session_impl/laziness.rs:347-350,544-578`; `sampler_turn.rs:947-956` | Use the same session routing snapshot for the classifier model and client. This finding is limited to the opt-in classifier. |
| C13 | Windows auth publication removes the old file before renaming its replacement. An unlocked Codex reader in that interval sees no credential and produces an empty bearer/account resolution. | `xai-grok-shell/src/auth/storage.rs:370-419,670-675`; `auth/openai_codex/login.rs:503-519` | Use a Windows replacement primitive that preserves visibility, with deterministic concurrent-reader and failure tests. |

All nine retained issues have structured Auto Developer Log entries. Logging is not a fix or acceptance of residual risk.

## Unresolved candidates and scope limits

These are not counted as confirmed findings:

- **C2: clearing a default may preserve the old disk key.** Evidence points to `None` being omitted during serialization and the merge retaining absent keys; the replacement verification did not produce an accepted confirming result. Needs a fixture-backed clear-and-reload test.
- **C5: overlapping switches may release the queue after the first completion.** There is one pending boolean and no completion generation. Needs a deterministic two-request completion-order test.
- **C8: sampler reconstruction may combine separately read route and credential state.** Closely related to C10, but the precise detached-call timing and downstream guards still need a controlled test.
- **C11: child setup may combine an earlier catalog identity with later live routing.** Verify the exact handle/update path and OAuth dialect selection. Ordinary enqueue-time versus execution-time model inheritance is a product contract choice, not automatically a defect; current code deliberately prefers live inheritance.

The earlier seed audit also raised Claude subscription/API-key catalog-name collisions and an already-built request using a newer transport. The replacement report did not close those exact hypotheses. They remain coverage gaps, not confirmed defects or refutations. See `MODEL_ISOLATION_AUDIT_SEEDS_2026-09-05.md` for their original locations and conditions.

Neither audit demonstrated an end-to-end regression test with two independent Windows Turbo processes sharing a temporary home. The high-risk interleavings need mock credentials and controlled scheduling, not tests against live accounts. Findings describe current source; their individual introduction dates were not established.

## Codex account limits

### Why Grok usage still appears

`xai-grok-pager/src/app/effects/mod.rs:4391-4447` unconditionally sends `x.ai/billing`. The shell implementation at `xai-grok-shell/src/extensions/billing.rs:200` explicitly requires xAI authentication. The modal and prompt warnings render `CreditBalance`, and `/usage manage` opens Grok billing. None of these paths chooses an allowance source from the active model provider.

This is an xAI-specific data path, not a failure to refresh the displayed percentage. Reusing it for Codex would also preserve incorrect team/consumer gating and stale-response risks.

### What can be fetched

The official Codex app-server documents `account/rateLimits/read` and `account/rateLimits/updated`. Its underlying native client uses a read-only request to `https://chatgpt.com/backend-api/wham/usage`.

Available fields can include:

- Primary and secondary quota windows, with percentages used and reset timestamps.
- Actual window durations, rather than a guaranteed fixed weekly or five-hour period.
- Additional metered-feature buckets, which may include model-specific limits.
- Plan and credit information when the service supplies it.

These are account/workspace quota snapshots, not a separate allowance per Turbo window. Do not infer a number of remaining prompts or tokens from a percentage. Do not treat a missing window as unlimited or zero usage.

### Recommended implementation

1. **Shell-owned, read-only Codex adapter.** Reuse `ensure_openai_codex_auth()` for paired bearer/account resolution. Expose a typed extension to the pager rather than putting credentials in UI state. No external Codex CLI dependency is needed.
2. **Separate quota types.** Represent provider/account identity, freshness/status, and arbitrary buckets/windows separately from xAI `CreditBalance`, subscription tier, spending cap, and auto-top-up.
3. **Session-aware attachment.** Select the displayed allowance from the effective main provider. Reject results from old sessions, model switches, requests, or account generations before mutating state. Never display Grok quota as a Codex fallback.
4. **Safe refresh lifecycle.** Fetch on successful Codex selection and `/usage`; throttle and coalesce turn-end/focus refreshes. Label same-account cached data as stale on transient failure; detach it on logout/account replacement or authentication failure. Allow one forced refresh and retry for `401`, not an unbounded loop.
5. **Fixed credential destination.** Do not derive the quota URL from an arbitrary inference endpoint override. Disable redirects or enforce same-origin redirects. Use injected endpoints and synthetic auth only in tests.
6. **Honest UI.** Show provider name, actual window labels, percentages used/remaining with unambiguous wording, resets, and freshness. Preserve unknown/additional buckets. Keep session token/cost accounting separate. Hide or reject Codex `/usage manage` until a deliberate official management destination is supported.

The HTTP endpoint is an internal implementation interface, not a stable public billing API. Isolate its schema and handle unsupported/unavailable responses explicitly. The documented app-server interface is an alternative, but restoring an external process dependency is not recommended for this native integration.

### Verification before release

- Force the four high-severity interleavings with synthetic credentials and mock endpoints.
- Exercise two independent Windows processes against a temporary config/auth store.
- Verify success/failure/overlapping-switch behavior, navigation during errors, queue release, and queued-child promotion.
- Check Codex to xAI to Codex switching, logout during refresh, account replacement, out-of-order results, absent buckets, Spark/unknown buckets, `401`, `403`, `429`, timeouts, and malformed payloads.
- Verify full TUI and minimal `/usage`, prompt warnings, session/context tabs, and `/usage manage`; unrelated xAI billing behavior must remain intact.
- Perform an authorized live Codex quota smoke test without exposing or recording secrets.

## Evidence and work status

- The model-isolation conclusions are independent source reviews, not executed concurrency reproductions.
- The earlier pager billing test build was cancelled at the operator's stop request; it did not report passing tests.
- `cargo test -p xai-grok-models --lib platform_roundtrip -- --test-threads=4` passed: **1 passed, 0 failed, 51 filtered out**. This checks the catalog assertions, including native/legacy Codex session-scoped IDs; it does not test cross-window persistence or the concurrency races.
- No live account quota request, application fix, provider login/logout, commit, or push was performed.
- Changes from this task are the project-local audit worker definition and audit notes. Existing application changes are untouched. The edited workflow projection is session-scoped; it does not alter the global default or the registered built-in recipe.

## Recommended next decision

Approve an implementation pass that first repairs session/credential atomicity, logout/publication, persistence, and switch error ownership; resolves the remaining candidate tests; then adds the Codex-only allowance adapter and display. Do not declare the isolation work complete on the basis of the Codex-default guard alone.

## Official Codex sources inspected

- [Codex App Server](https://learn.chatgpt.com/docs/app-server): documented account limit fields and notifications.
- [Native limit read implementation, pinned revision](https://raw.githubusercontent.com/openai/codex/469ce0db51af87a09d44e24992dc655068d47e81/codex-rs/backend-client/src/client/rate_limit_resets.rs).
- [Native payload conversion and additional buckets, pinned revision](https://raw.githubusercontent.com/openai/codex/469ce0db51af87a09d44e24992dc655068d47e81/codex-rs/backend-client/src/client.rs).
