# Model isolation audit: provisional candidates

The first audit completed on 2026-09-05 with a partial, source-only report. Its model selection did not match the operator's requested review policy. These are hypotheses for a fresh Terra Medium audit, not a final acceptance or a claim of runtime reproduction. No application code was changed. The first audit retained twelve candidates and excluded six others at its claim cap.

Scope: current tree at `7ab727263`, including the existing uncommitted additional-directory changes. Preserve those changes. Compare against `d8c2e8313` where evidence is available. Separate independent Turbo processes from multiple sessions inside one process. Do not access real credential stores or exercise real provider endpoints.

## High-severity candidates

1. **C4: delayed persistence failure switches the newly focused session.** `crates/codegen/xai-grok-pager/src/app/dispatch/settings/ui.rs:1093-1137`. Session A's default-model persistence fails after navigation to B. Rollback reads `app.active_view` and emits a reverse SwitchModel for B rather than A. Trace `SettingPersistFailed` identity and the typed default setter; do not generalize this to ordinary successful switches.
2. **C8: old xAI refresh overwrites new provider credentials.** `crates/codegen/xai-grok-shell/src/session/acp_session_impl/sampler_turn.rs:1731-1762`. Credentials and route are captured before `get_valid_token().await`; writeback does not revalidate after a compatible switch to a static-BYOK endpoint. Check whether the interleaving is actually permitted and whether downstream auth policy rejects a mismatched key.
3. **C11: child inheritance combines different routing snapshots.** `crates/codegen/xai-grok-shell/src/agent/subagent/mod.rs:815-820,895-907`. Separate actor reads of sampling config and credentials can straddle a parent switch. Check whether an unpinned static-BYOK child can inherit P's URL and Q's key, including all downstream reconstruction guards.

## Medium-severity candidates

4. **C1: cross-process settings lost update.** `crates/codegen/xai-grok-shell/src/util/config/persist.rs:7-83,167-215`. Process-local mutex plus whole typed-config merge may let B's theme save overwrite A's completed model-default change. Atomic rename alone is not transaction locking.
5. **C2: optional classifier follows shared rather than session model.** `crates/codegen/xai-grok-shell/src/session/acp_session_impl/laziness.rs:347-360,544-608`. Check the opt-in detector's explicit request-model override after another window changes the shared default. Ordinary main inference may remain session-local.
6. **C3: clearing default does not remove the persisted key.** `crates/codegen/xai-grok-shell/src/util/config/persist.rs:163-203`, `campaigns.rs:465-466`, `settings_writes.rs:114-121`. None is omitted by serialization and merge may preserve the old default while the UI reports it cleared.
7. **C5: first overlapping switch completion releases queued prompts early.** `crates/codegen/xai-grok-pager/src/app/dispatch/session/lifecycle.rs:1479-1507,1544-1547`. M1 and M2 switches share a pending boolean without a request generation. Distinguish proven early local queue release from whether the backend actually runs M1.
8. **C6: ordinary failed switch leaves an optimistic model displayed.** `crates/codegen/xai-grok-pager/src/app/dispatch/session/lifecycle.rs:1537-1546`, `settings/setters.rs:1734-1764,1825-1883`. Check a pre-mutation ACP rejection after an optimistic typed setter and whether selecting M1 again wrongly hits its no-op check.
9. **C7: incompatible-switch question appears on another session.** `crates/codegen/xai-grok-pager/src/app/dispatch/session/lifecycle.rs:236-285,1529-1535`. Error cleanup uses A's captured ID, but the question helper chooses currently focused B. Include navigation and draft-stashing behavior.
10. **C9: reconstruction loses provider-qualified catalog identity.** `crates/codegen/xai-grok-shell/src/session/acp_session_impl/sampler_turn.rs:324-369,541-590`, `agent/config.rs:4565-4572,6177-6264`. Check the shipped Anthropic API vs Claude subscription collision for `claude-sonnet-4-6`, including catalog order, resolver restoration, and auth-header selection.
11. **C10: old request model sent through new endpoint.** `crates/codegen/xai-grok-shell/src/session/acp_session_impl/sampler_turn.rs:1666-1682`, `turn.rs:2255-2344`, `crates/codegen/xai-chat-state/src/actor/request_builder.rs:129-139`. Request construction precedes independently selected sampler config. Check switch interleavings, explicit model preservation, and turn guards.
12. **C12: queued implicit-model jobs bind at execution rather than enqueue.** `crates/codegen/xai-grok-shell/src/agent/mvp_agent/subagent_coordinator.rs:20-26,255-282`. This behavior may be deliberate live inheritance, not a defect. Establish the documented contract before assigning severity.

## Coverage gaps to address

- The first audit did not execute tests or compare historical commits; do not attribute these defects to RC3 without evidence.
- No real independent-Windows-process regression test was demonstrated. In-process handles, threads, and static HTTP fixtures do not prove cross-window end-to-end isolation.
- Recheck Codex OAuth refresh versus logout, token-family adoption, legacy-auth cleanup, outer operation timeouts, and Windows lock recovery; the first report's claim cap excluded additional candidates.
- Separate a model-only switch from explicit login/logout/account replacement. Shared account credentials are not proof that a model switch changes another window's account.

## Codex account-limit integration (separately reviewed by Sol Medium)

Native quota fetching is feasible using a shell-owned adapter and the existing paired Codex bearer/account resolver. Keep quota types distinct from xAI CreditBalance. Route the allowance display by effective main provider; reject stale results before mutation using session/request/account identity. Preserve server-provided window durations, resets, and additional buckets, with explicit stale/unavailable states. Do not send quota credentials to an arbitrary inference override URL. Do not route Codex `/usage manage` to Grok billing.

Official sources inspected by the parent:

- https://learn.chatgpt.com/docs/app-server (account/rateLimits/read, account/rateLimits/updated, generic window and bucket fields).
- https://raw.githubusercontent.com/openai/codex/469ce0db51af87a09d44e24992dc655068d47e81/codex-rs/backend-client/src/client/rate_limit_resets.rs (native read-only usage endpoint).
- https://raw.githubusercontent.com/openai/codex/469ce0db51af87a09d44e24992dc655068d47e81/codex-rs/backend-client/src/client.rs (raw payload conversion and additional quota buckets).

No authenticated quota fetch or implementation has occurred.
