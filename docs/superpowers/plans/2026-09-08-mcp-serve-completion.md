# `turbo mcp serve` Completion Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish `turbo mcp serve` so an external MCP client can drive a bounded subset of Turbo's tools over loopback, with Windows and Linux both green.

**Architecture:** A new crate `xai-grok-mcp-server` owns the containment boundary (`PathGuard`, already built), constructs a `ToolBridge` over a read-and-write-confined filesystem, and exposes it through `rmcp`'s `ServerHandler` mounted as an axum `route_service` behind a bearer token on 127.0.0.1. A `turbo mcp serve` subcommand wires it up and optionally supervises a tunnel child.

**Tech Stack:** Rust 1.94.0 · `rmcp` 2.1 (`server` already on via `default`; add `transport-streamable-http-server`) · axum 0.8 · tokio · `xai-grok-tools` `ToolBridge` · `xai-tty-utils` `ProcessScope`.

## Progress (updated 2026-09-11)

| Task | State |
|---|---|
| 0. Reclaim disk | Moved to Task 10. `target/debug` lives on H: (337 GB free); the WSL constraint is C:, so wiping it first would only cost a full rebuild. |
| 1. Confined toolset | **Done.** Drift guard runs against the live `ToolBridge` schemas. |
| 2. Annotations | **Done.** Derived from `ToolKind`. |
| 3. `ServerHandler` | **Done.** Refusals are tool-level errors. |
| 4. HTTP + bearer | **Done.** `subtle::ConstantTimeEq` (ring's comparison is deprecated with no side-channel promise). |
| 5. `turbo mcp serve` | **Done.** 30 CLI tests on the round-5 revision: parsing, tiers, confine widening, tunnel flags, fail-fast on a missing helper with the port left unbound, signals registered before startup, stop during and after startup, teardown when the endpoint cannot be printed, the JSON endpoint shape, escaped and throttled operator lines that never wait for a stuck standard error (tracing output included) and report every dropped line exactly once, a console close removing the session folder first, and the folder-trust marker samples. Wire-level JSON-RPC tests live in the server crate. |
| 6. Tunnel | **Cloudflare done and live-verified end to end, again on the round-4 revision.** Round 4: cloudflared runs with `--config` naming an empty file of its own, so the operator's `config.yml` does not apply, and `TUNNEL_*` variables are removed in any letter case. Round 3: an explicit `--tunnel-bin` is made absolute and never falls back, and `cloudflared` starts with every `TUNNEL_*` variable removed. (Verified route: public HTTPS -> Cloudflare -> loopback; `--http-host-header` keeps rmcp's Host check passing; bearer still 401s at the far end). `--tunnel cloudflare` and `--tunnel-bin` wired into the CLI. OpenAI `tunnel-client` **blocked on an operator decision** — see below. |
| 7. Docs | **Done; updated for round 5 (2026-09-11).** `32-mcp-server.md` (roots, spelling rules, transport limits and status codes, the edit-tier list marked best effort, grep guarantees, hard-link and image residuals, the stopping sequence) and the design record. `USER_GUIDE` entry next to `07-mcp-servers.md`; README row. No zh-CN mirror needed (none exists for guides 25+). 21 pager docs tests pass on Windows. |
| 8. Audit + fix loop | **Round 9 fixes applied (2026-09-11). Windows green on the round-9 revision; the Linux run is in progress.** Round 9: 48 agents over nine review lenses, every finding adversarially verified; 38 raw findings, 21 confirmed, 17 refuted. The critic's verdict was that **no confirmed finding blocks user testing**: every containment defect it upheld is reachable only in the `--allow edit` tier (the write-deny lists are empty when read-only, and no write tool is served), and the readonly tier had no verified hole this round. All 21 were fixed anyway, since AGENTS.md forbids deferring. The one the round-8 self-review had missed: `WalkedLink::leads_to` resolved a **dangling** link to the link's own path, so the link closure, the relative declarations and the outside-root loader scan all missed the target an edit-tier client could still create — `dangling_link_target` existed but was wired only into the name-based rule. Also fixed: a repository is now judged the way git judges one (`is_worktree_top` and `is_git_dir`), so a stray `.git` no longer shadows the repository that holds a root and a bare repository whose `objects` is a link is still found; a global git config that lies inside a root is refused like the files it includes; an empty or `.` `core.hooksPath` no longer collapses to the worktree and refuses every write in it; `[plugins].install_dir` is read from version and campaign patches and a relative or `~/` value now counts wherever Turbo runs; an NFS mount keeps its own subtree (the export folder joined with the mount's root field), so a bind mount out of an NFS home neither invents a home alias nor refuses a legitimate root; the outside-root link closure follows chains rather than one hop, with a visited set; the crowded-folder check normalises the root spelling; and a `glob` is capped at 8 KiB like a pattern. From the critic's own list: the client is now told which roots it may name, which is the difference between a first session that works and a string of identical opaque refusals. Tests: nine new cases, each failing if its fix is reverted, plus one grep case per search-argument prefix and a thread-local seam so guard tests stop walking the operator's real `~/.claude` (about 8,000 listings per guard build, roughly 120 builds per suite). Three pre-existing git-hooks fixtures failed after the repository fix and were the fixtures' fault: each created only `.git/config` with no `HEAD`, `objects` or `refs`, exactly the stray-`.git` case git itself walks past, so they are now real repositories rather than the rule being weakened back. Docs: nine corrections, including two the critic added — a wrong path gets `404` only with a valid token (without one every path gets `401`, since the token is checked before any route is matched), and the 4,096 cap covers plugin, skill and marketplace declarations only, with `.envrc` and git bounded by the 1 MiB file limit and the walk. Windows on the round-9 revision: server crate 249 passed + 1 ignored (the manual live-tunnel test) with no skipped tests and all nine new cases running; tools skill discovery 43, grep 111, search_replace 110, policy 41, line_diff 4, resources 101, confined_fs 6; pager serve 35, pager docs 21; no clippy warnings in any file this work touches in the server, tools and pager crates or in the turbo binary. The gate's one server clippy hit was a `cmp_owned` in a new test, fixed, and a re-run gave 0 hits with 249 still passing. Windows on the round-8 revision: server crate 240 passed + 1 ignored (the manual live-tunnel test), with every round-8 test running and none skipped; tools skill discovery 43, grep 111, search_replace 110, policy 41, line_diff 4, resources 101, confined_fs 6; pager serve 35, pager docs 21; no clippy warnings in any file this work touches in the server, tools and pager crates or in the turbo binary. The gate's two server clippy hits were cosmetic (a range loop and a needless borrow) and were fixed, along with three defects this round's own review of the new code found: the walk recorded one link past its cap, the declaration dedup key dropped a second spelling of one location (so a link along that spelling went unfollowed), and the git config parser read `\b` as deleting the character before it rather than as git's backspace. A re-run on the final revision: fmt clean, server clippy 0 hits, 240 tests passing. Round 8: 16 agents (8 review lenses, verifiers, a critic), with every finding adversarially verified. The critic blocked on one must-fix, now fixed: a served grep answered "No matches found" for a pattern ripgrep refuses because it could match a line break, hiding the message that says to retry with multiline. Every other confirmed finding was fixed too. Git: hooks folders and `core.hooksPath` are read for the repository a root sits inside, for every repository under a root and for a bare one, and configuration is parsed as git parses it (inline `#` and `;` comments, quotes, escapes, line continuations, a key on the section-header line). Links: a declared plugin or skill folder that is a link, or a link on a name above it, leaves what it leads to write-refused, and the spelling a client sends is judged against the relative declarations too; a link named like the first half of a two-name rule (`.kube`, `.aws`, `.docker`, `.cargo`, `.github`, `hooks`, `.git`) is probed with the second name joined on; a link that leads nowhere yet refuses the target it names, so a client cannot create it; links below a folder another link leads to are followed, and so are links in the loader folders outside the roots (each Grok home, and `.agents`, `.claude` and `.cursor` in every home). Declarations: Turbo's plugin install registry (a local install's whole source folder, its snapshot and the install folder), Claude's `installed_plugins.json`, and `GROK_CAMPAIGNS_OVERRIDE` in the flat form Turbo actually uses are read; an entry whose variable expands to nothing declares nothing instead of every location; declarations are deduplicated, each configuration folder is canonicalized once, and more than 4,096 of them refuse the edit tier with a new `TooManyDeclarations` reason. Homes: a home under an indirect autofs map is judged per home rather than per trigger (a mounted key is examined, an unmounted one is not, and a spelling through links is resolved one name at a time), mounts are placed by the folder their NFS source names rather than by the `/` every NFS mount prints, and a bind mount of one home inside a folder too crowded to list counts as that home. Also: `.envrc` arguments are split the way a shell splits them (quoted spaces, backslash continuations, comments); a grep `pattern` or `type` holding a NUL, or a pattern over 8 KiB, is refused instead of failing the spawn with a message naming where ripgrep is installed; after a hangup starts the stop, a session manager's SIGTERM no longer cuts it short for five seconds; and the walk never stats through a link, so a link to an unreachable share or an automount point cannot stall startup. Windows on the round-7 revision: server crate 219 passed + 1 ignored; tools skill discovery 43, grep 111, search_replace 110, policy 41, line_diff 4, resources 101, confined_fs 6; pager serve 35, pager docs 21; no clippy warnings in any file this work touches in the server, tools and pager crates or in the turbo binary. Round 7: 16 agents (6 fix checks, 2 fresh-review lenses, 7 verifiers, a critic). The critic found two defects that blocked user testing, both fixed: the served toolset started Turbo's scheduler, which rewrote `.grok/schedules.json` in the first root even in the readonly tier (it now gets a scheduler handle that leads nowhere, so none starts); and a link named like a refused file or folder left its differently named target writable, or for `.grok` readable (name rules now also judge the spelling a client sends, and what such links lead to, and links in git hooks folders, is refused in both tiers). Every other confirmed finding was fixed too: a served grep passes `--no-messages` and `--no-ignore-messages`, never returns ripgrep's standard error on exit 2 unless it is about the client's own pattern, glob or file type, keeps the notice for a named binary file, and refuses paths holding a line break; relative plugin and skill entries count wherever Turbo could run (which also fixes a round-6 regression for a Grok home inside a root); `requirements.toml`, `server_skill_dirs`, `bundled_skill_dirs`, `GROK_CAMPAIGNS_OVERRIDE` and the launcher skill variables are read, `~/` entries also count literally, and the names after a variable count from anywhere; `.envrc` parsing reads past shell keywords, handles `source_up`, `use flake`, `use nix` and `use devenv`, reads `.envrc` files above a root, decodes lossily, and no longer refuses a whole folder for `source_env ..`; `/etc/passwd` is read as bytes, a root directly inside a crowded account folder is refused, the Windows-profile scan runs only for WSL drive mounts, a root is mounted before homes are read, and homes below an unmounted automount are judged by path alone; a root whose real path no client can name is refused again rather than printed under the operator's spelling (network shares and over-long Windows paths included); another stop signal while stopping counts by its kind (a hangup and a quick repeat are ignored, a console close removes the session folder and stopping goes on); no write begins once shutdown has cancelled a call; held-back rejection counts are reported once a burst ends and at exit; `max_diff_lines` counts a lone carriage return as a line end; a skill description reads at most the first 64 KiB of its file. The verifiers refuted an autofs regression claim and an Inadmissible-message omission. Windows on the round-6 revision: server crate 206 passed + 1 ignored; tools skill discovery 42, grep 109, search_replace 110, policy 41, line_diff 2, resources 101, confined_fs 6; pager serve 31, pager docs 21; no clippy warnings in any file this work touches in the server, tools and pager crates or in the turbo binary. The first run caught two CLI tests failing because their test stop source fired again once its channel closed, which a real signal source never does (the test source now waits, as a real one does); it also flagged `cap_account_homes`, which only Unix calls and is now compiled only there and in tests, and two `nonminimal_bool` lints that predate this work in files it touches (the deny-path component match in `policy/mod.rs` and the version check at `main.rs:2860`), both rewritten to equivalent minimal forms. Round 6: the verification audit of round 5 found no code defect that must block user testing, but found documentation claims the code did not yet make true, and a critic addition. All addressed: a served grep reads ripgrep's `--null` output, so a path holding a newline is judged whole, a path that never ends is dropped, and U+FFFD in a path is refused only on Windows, where ripgrep substitutes it; a printed root tries the canonical spelling, then the operator's; the over-broad root check drops its identity early return, stats without triggering automounts on Linux, finds every other mount of a home's filesystem through mountinfo, and treats an account folder with more than 256 homes as a home; relative configuration entries count from the folders between the configuration and the root; `[[version_overrides]]` and `[[campaigns]]` patches and Claude's `known_marketplaces.json` install locations are write-refused; a linked `.envrc` counts; shutdown waits however long an edit that has begun writing takes, and a second stop signal ends the command at once; `max_diff_lines` is diffed only when set, and a served diff is bounded in lines and time and never undercounts; Turbo's system reminders are off for served calls, so skill discovery no longer reads skill files above a root or inside `.grok` folders, and its front-matter reader stops one byte past its limit; rejected-token traces are debug level; the guide and design record were corrected. Round 5: 51 agents. 23 fix checks on round 4 found 9 gaps (1 high, 3 medium, 5 low); the fresh review confirmed 15 findings (6 medium, 9 low) and refuted 2; the critic's one release blocker was the high gap. All addressed, both refuted items included: a served grep holds back the blank line ripgrep writes between files and writes it only before the next kept file, so a refused last block leaves no trace (the high), and names ripgrep printed with U+FFFD are refused; search_replace's own policy checks on the resolved path return `policy_denied` when served, reported to the operator as `WorkspacePolicy`; served edits keep no receipt and skip the line-count telemetry diff, and `line_diff` stops searching after two seconds with an approximation that never undercounts; shutdown no longer stops an edit whose write has begun; the read-only whole-read cap uses `read_file`'s own image detector; the over-broad root check compares directory identity by metadata, also refuses a root holding a home's real path, finds bind-mount points and macOS's data volume, classifies Windows mounts by the folder they expose, caps account folders at 256, and runs off the runtime so Ctrl-C works during startup; declaration files are read only as regular files of at most 1 MiB, with `$VAR` expansion, relative entries counted from every folder where Turbo could start, `managed_config.toml`, and Git for Windows' `%HOME%`; a root no client path could name is refused at startup with its cause; operator drop counts are taken atomically and reported at exit; tracing output for `mcp serve` goes through the operator queue; the logoff and shutdown handlers, which Windows never delivers to turbo.exe, were removed and documented; connections get their close grace only after the drain. Windows on the round-5 revision: server crate 199 passed + 1 ignored, tools grep 106, search_replace 110, line_diff 1, resources 101, confined_fs 6, pager serve 30, pager docs 21; clippy clean for the new code in the server, tools and pager crates and in the turbo binary (its one warning, `nonminimal_bool` at main.rs:2860, predates this work, as do three `collapsible_if` lints in `xai-tool-types` and the tools build script that stop a `-D warnings` run); the line-ending gate is clean. Round 4 fixes applied (2026-09-11). Round 4: 45 agents, 24 findings, 18 upheld (1 high, 6 medium, 11 low), plus 5 critic additions (1 high, 2 medium, 2 low), all addressed. Fixes: a filtered grep that kept nothing now reads exactly as no match, closing an oracle over refused files; a line inside a refused grep block can no longer reopen it, and non-UTF-8 paths are refused; the argument check runs on the call's blocking thread under its permit and timeout, arguments a tool does not advertise are refused with a message naming them (which also stops `confirm: true` satisfying a workspace policy), and the sweep and Unicode sibling scan are bounded; workspace-policy refusals are opaque; a served edit policy bounds replace_all output, skips the post-edit diff and limits edit details, and writes over 32 MiB are refused; read-only whole reads of text are capped at 8 MiB; git includes are followed recursively and Turbo's global and nested plugin and skill declarations are write-refused; WSL drive mounts are found wherever mounted, macOS `/Users` is enumerated, and a home reachable under another path makes a root over-broad; printed roots use a spelling the guard accepts; a timeout or shutdown no longer misreports an edit that has written; graceful shutdown waits for responses and connections; operator lines go through a non-blocking writer thread; a Windows console close tears down and removes the session folder first; the user guide and design record were corrected (roots rule, crash behaviour, stopping, 400, 415 and 431). Windows on the round-4 revision: server crate 187 passed + 1 ignored, pager serve 28, pager docs 21, tools grep 104, tools search_replace 110, live Cloudflare tunnel test passed, clippy clean for the new code in the server and tools crates; the one pager warning, in a test helper, is fixed. That run caught one mistaken test, which passed an argument the served grep does not advertise. Round 3 fixes applied (2026-09-11); Windows and Linux green on that revision. Windows on the round-3 revision: server crate 172 passed + 1 ignored (the manual live-tunnel test), pager serve 25 passed, pager docs 21 passed, tools grep 100 passed, fmt clean, clippy clean for the new code in both crates. That run caught two defects in the round-3 code, both fixed (private helpers re-exported to the tests, and a borrow of a temporary in a new CLI test), plus one stale test that expected `HardDenied` where the new lexical check answers `OutsideRoots` first; it now proves the Grok-home rule from a root nested inside a Grok home. Round-3 critic: readonly trustworthy except grep; edit to be described as best effort. Round-3 fixes: grep results filtered by source file, with `--no-config`, `--no-follow` and canonical walk targets; one spelling per path (no `.` or empty segments, bounded length); lexical containment before any filesystem access plus an exact canonical prefix; over-broad roots by file identity against every candidate home, including folders beside the home (which also closed a macOS gap found while documenting); credential and walk-denied additions; special-file walks refused; edit-tier additions including declared hook, include, plugin and `.envrc` source locations; bounded line windows; completed edits reported as completed; an own HTTP/1 accept loop with connection, header and body bounds and a JSON value cap; request slots held until rmcp finishes; graceful shutdown that lets running calls return; the panic hook removes session folders; tunnel environment scrub; CLI output that cannot panic; a Windows CI job. Round 2 fixes applied (2026-09-11). Windows: server crate 136 passed + 1 ignored (the manual live-tunnel test), pager serve 18 passed, pager docs 21 passed, fmt clean, clippy clean for the new code. The first Windows run caught two defects in the round-2 code itself, both fixed: a borrow error in the new stop-ordering test, and a wrong DACL assumption (an inheritable `GENERIC_ALL` grant is split into two entries; the folder now gets `FILE_ALL_ACCESS` and the test asserts every entry is the current user). Round 2: 144 agents, 45 findings after dedup, 34 upheld. Critic: readonly trustworthy for project-scoped roots once fixed; edit defensible only as a trusted-client tier. Fixes: owner-only session folder (Unix mode at `mkdir`, Windows protected DACL); filesystem-layer decisions recorded in a task-local call scope instead of matching tool text; document, 32 MiB whole-read and special-file refusals on the resolved path; no write after a refused read; git metadata by content; edit-tier deny unioned with `CompatConfig` plus a folder-trust marker table with a source-extracting drift test; case-folded grep excludes; walks with no target checked against the first root; calls on blocking threads that own their permit (90 s client timeout); body buffered after auth with 16 request slots; over-broad roots refused; bound-address loopback assertion; device stem trim; symlink probe skips prefix and root; absolute-only `PATH` search; edit tier relabelled trusted-client-only in CLI and docs; tests for busy, timeout, disconnect, SIGHUP and stop ordering; ripgrep installed in CI. Round 1: Windows after fixes: server crate 117 passed + 1 ignored, pager serve 13 passed. Fixes: sanitiser-equality admission, resolved-path `ReadConfinedFs`, walk access kind + `DenyReadGlobs`, real body limit, spawned request task (rmcp stateless leak), per-process private session dir, concurrency/timeout bounds, Unix signal + parent-death + panic-hook teardown, composed Windows creation flags, credential/dotenv/git/auto-run denies, PDF/PPTX refusal, operator event observer on stderr, honest tests, user guide and spec rewritten. Round 1 summary: | 139 agents, 43 findings after dedup, 33 upheld (3 high, 14 medium, 16 low). No dispatch bypass: every non-`tools/call` MCP method is inert, task-augmented calls error out, batches cannot deserialize, and `ToolKind` comes from the server. Critic: readonly fixable structurally; edit is code execution by proxy for an untrusted client. Root cause of the highs: the guard checks the raw argument while the tools act on a sanitized, symlink-resolved string. |
| 9. Linux via WSL | **Green on the round-9 revision (2026-09-11).** Server crate 251 passed + 1 ignored (the manual live-tunnel test), no clippy warnings in the server crate; pager serve 35 and pager docs 21 passed, no clippy warnings in `mcp_serve_cmd.rs`; tools grep 113 passed (Unix-only cases account for the difference from Windows), search_replace 110, policy 41, line_diff 4, resources 96, confined_fs 5, skill discovery 43; no clippy warnings in any tools file this work touches. ripgrep present, so the grep tests ran decisively; C: had 51 GB free, and every file this round touched was CR-free before the run. Earlier: green on the round-8 revision (2026-09-11). Server crate 242 passed + 1 ignored (the manual live-tunnel test), no clippy warnings in the server crate; pager serve 35 and pager docs 21 passed, no clippy warnings in `mcp_serve_cmd.rs`; tools grep 113 passed (Unix-only cases account for the difference from Windows), search_replace 110, policy 41, line_diff 4, resources 96, confined_fs 5, skill discovery 43; no clippy warnings in any tools file this work touches. ripgrep present, so the grep tests ran decisively; C: had 51 GB free, and every file this round touched was CR-free before the run. Earlier: green on the round-7 revision (2026-09-11). Server crate 221 passed + 1 ignored (the manual live-tunnel test), no clippy warnings in the server crate; pager serve 35 and pager docs 21 passed, no clippy warnings in `mcp_serve_cmd.rs`; tools grep 113 passed (Unix-only cases account for the difference from Windows), search_replace 110, policy 41, line_diff 4, resources 96, confined_fs 5, skill discovery 43; no clippy warnings in any tools file this work touches. ripgrep present, so the grep tests ran decisively; C: had 52 GB free. Earlier: green on the round-6 revision (2026-09-11). Server crate 208 passed + 1 ignored (the manual live-tunnel test), no clippy warnings in the server crate; pager serve 31 and pager docs 21 passed, no clippy warnings in `mcp_serve_cmd.rs`; tools grep 111 passed (Unix-only cases account for the difference from Windows), search_replace 110, policy 41, line_diff 2, resources 96, confined_fs 5, skill discovery 42; no clippy warnings in any tools file this work touches (grep, search_replace, policy, registry, `output.rs`, `resources.rs`, skill discovery). ripgrep present, so the grep tests ran decisively; C: had 52 GB free. Earlier: green on the round-5 revision (2026-09-11). Server crate 199 passed + 1 ignored (the manual live-tunnel test), clippy clean for the new code; pager serve 30 and pager docs 21 passed, no clippy warnings in `mcp_serve_cmd.rs`; tools grep 108 passed (Unix-only cases account for the difference from Windows), search_replace 110, line_diff 1, resources 96, confined_fs 5; no clippy warnings in the grep module, search_replace or `output.rs`. The one warning in `resources.rs` was a `std::fs::canonicalize` call in a Unix-only test from the concurrent containment change; it now uses `dunce::canonicalize`, which behaves the same on Unix, and a Linux recheck shows no warning there and the test passing. ripgrep present, so the grep tests ran decisively. Earlier: green on the round-3 revision (2026-09-11). Server crate 172 passed + 1 ignored, clippy clean for the new code; pager serve 25 and pager docs 21 passed, pager clippy exit 0 with no warnings in `mcp_serve_cmd.rs`; tools grep 102 passed (Unix-only cases account for the difference from Windows), no clippy warnings in the grep module and none in the `ServedGrepPolicy` lines of `resources.rs` (its three hits are the pre-existing `std::fs::canonicalize` calls at lines 978 and 1005). ripgrep present, so the grep tests ran decisively. Earlier: green on the round-2 revision (2026-09-11). Server crate 137 passed + 1 ignored (the Unix-only FIFO test is the extra one), clippy clean for the new code; pager serve 19 passed (the extra one raises a real SIGHUP); pager clippy exit 0 with no warnings in `mcp_serve_cmd.rs`; ripgrep present, so the grep test ran decisively. Round-1 revision: 1 real failure, fixed in round 2. Server crate 116 passed, 1 failed, 1 ignored; clippy clean for the new code; pager serve 13 passed; all symlink tests ran. The failure is a genuine defect the test was written for: on Linux the per-process session folder is created `0755`, not `0700`, so other local users could list it and read `search_replace` receipt backups. Windows cannot observe this. Fix held until the round-2 audit finishes so its agents read stable code. (Earlier: the pre-fix revision was green, 81 + 12; the very first attempt never started because Git Bash rewrote the `/mnt/h/...` path, caught from the output.) |
| 10. Final verify, cleanup, release build | **In progress.** Windows gate green on the round-5 revision (row 8) and the line-ending gate is clean. Cache cleanup and the release-dist build follow the Linux run and the round-6 audit. |

### Deviations from the original task text

- **No `--allow full`.** The crate never serves a shell, so a `full` tier would be a name with nothing honest behind it. Tiers are `readonly` (default) and `edit` (adds `search_replace`, bounded by the guard and by `ConfinedFs`).
- **No separate `--read-only` flag.** `--allow readonly` is that mode; two switches for one property is a way to set them inconsistently.
- **`--tunnel openai` not offered yet.** It depends on the open auth question below.
- **Commit steps stay open.** Every task's work is delivered and tracked in the table above, but nothing has been committed: commits wait for the operator to ask.

### Open operator decision: auth mode for ChatGPT

The server requires `Authorization: Bearer <token>`. Public guides describe ChatGPT's custom connector auth as OAuth or none, with one adding "API key"; none of them state that a static bearer header can be configured. The working reference implementation uses only a secret URL path. If ChatGPT cannot send the header, the default configuration cannot be used from ChatGPT at all. The OpenAI `tunnel-client` path has the same dependency. Resolving this needs either confirmation from a ChatGPT workspace with Developer mode, or an explicit operator choice to add a secret-path-only mode with that trade-off documented.

## Global Constraints

- Binary is **`turbo`**, not `grok`. Subcommand is `turbo mcp serve`.
- `std::fs::canonicalize` / `Path::canonicalize` are **clippy-banned**; use `dunce::canonicalize` or the `xai_grok_tools` helpers.
- `std::process::Command::spawn` / `tokio::process::Command::spawn` are **clippy-banned**; children must go through `xai_tty_utils::ProcessScope::enroll`.
- Name the clap args struct **`McpServeArgs`** — `ServeArgs` is taken by `agent serve` (`cli.rs:514`).
- Every new file is LF. Files already CRLF in the worktree (root `Cargo.toml`, `.github/workflows/*.yml`) must keep CRLF on edit.
- New crates need `[lints] workspace = true`, `publish = false`, `license = "Apache-2.0"`, `edition.workspace = true`.
- Package-scoped cargo only. No `cargo test --workspace` (disk).
- **Never serve a shell.** `run_terminal_cmd` and friends stay out of `declared_path_fields`. The guard's `shell_is_never_declared` test enforces this.
- Every task ends with `cargo fmt -p <crate>`, `cargo clippy -p <crate> --all-targets`, and `cargo test -p <crate> --lib`, all clean, before commit.

## File Structure

| File | Responsibility |
|---|---|
| `crates/codegen/xai-grok-mcp-server/src/guard.rs` | **Done.** Containment boundary. |
| `.../src/toolset.rs` | Build `ToolBridge` from a `SessionContext` over a confined FS; expose `list()` / `call()` gated by `PathGuard`. |
| `.../src/annotate.rs` | `ToolKind` -> `rmcp::ToolAnnotations`. |
| `.../src/content.rs` | Turbo `ToolOutput` -> `Vec<rmcp::model::ContentBlock>`. |
| `.../src/handler.rs` | `TurboMcpHandler: ServerHandler`. |
| `.../src/http.rs` | axum router, bearer auth, secret path, body cap. |
| `.../src/tunnel.rs` | Tunnel child supervision via `ProcessScope`. |
| `.../src/config.rs` | `McpServeConfig`. |
| `crates/codegen/xai-grok-pager/src/mcp_cmd.rs` | `McpCommand::Serve` variant + handler. |
| `crates/codegen/xai-grok-pager/docs/user-guide/32-mcp-server.md` | User docs. |

---

### Task 0: Reclaim disk before anything compiles

`target/debug` is 88 GB and `target/release-dist` 27 GB. The Linux leg needs headroom.

- [ ] **Step 1: Record current free space**

```bash
df -h /c /h | head -3
```

- [ ] **Step 2: Wipe the debug profile only**

`release-dist` is independent and must survive (it holds the ship binary).

```bash
rm -rf "H:/Apps/grok build/turbo-grok-build/target/debug"
```

- [ ] **Step 3: Confirm reclaim and that release-dist survived**

```bash
df -h /h | head -3 && ls "H:/Apps/grok build/turbo-grok-build/target"
```
Expected: `release-dist` still present, `debug` gone.

---

### Task 1: Confined toolset construction

**Files:** Create `src/toolset.rs`; modify `src/lib.rs`, `Cargo.toml`.

**Interfaces:**
- Consumes: `guard::{PathGuard, Access, Denial, Reason, validate_tool_schema}`.
- Produces: `ServedToolset::new(roots: Vec<PathBuf>, read_only: bool) -> anyhow::Result<ServedToolset>`; `ServedToolset::list(&self) -> Vec<ServedTool>`; `ServedToolset::call(&self, name: &str, args: Value) -> Result<String, CallFailure>`; `pub struct ServedTool { pub name: String, pub description: String, pub schema: Value, pub kind: ToolKind }`; `pub enum CallFailure { Refused(Denial), Failed(String) }`.

- [x] **Step 1: Add dependencies**

```toml
anyhow = { workspace = true }
tokio = { workspace = true, features = ["rt", "sync", "macros"] }
xai-grok-agent = { workspace = true }
xai-tool-runtime = { workspace = true }
```

- [x] **Step 2: Write the failing test**

```rust
#[tokio::test]
async fn served_toolset_lists_only_declared_tools() {
    let root = tempfile::tempdir().unwrap();
    let ts = ServedToolset::new(vec![root.path().to_path_buf()], true).await.unwrap();
    let names: Vec<String> = ts.list().iter().map(|t| t.name.clone()).collect();
    assert!(names.iter().any(|n| n == "read_file"), "read_file must be served");
    assert!(!names.iter().any(|n| n.contains("run_terminal_cmd")), "shell must never be served");
}
```

- [x] **Step 3: Run it and watch it fail**

```bash
cargo test -p xai-grok-mcp-server --lib served_toolset_lists
```
Expected: FAIL, `ServedToolset` not found.

- [x] **Step 4: Implement**

Construct the FS stack confined for **writes** by `ConfinedFs`, with reads bounded by `PathGuard` at dispatch:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use xai_grok_tools::bridge::ToolBridge;
use xai_grok_tools::computer::local::{ConfinedFs, LocalFs, LocalTerminalBackend};
use xai_grok_tools::computer::types::{AsyncFileSystem, TerminalBackend};
use xai_grok_tools::notification::ToolNotificationHandle;
use xai_grok_tools::registry::types::{SessionContext, ToolServerConfig};

let fs: Arc<dyn AsyncFileSystem> = Arc::new(ConfinedFs::with_roots(
    Arc::new(LocalFs),
    roots.clone(),
));
let backend: Arc<dyn TerminalBackend> = Arc::new(LocalTerminalBackend::new());
let ctx = SessionContext {
    backend, fs,
    cwd: roots[0].clone(),
    session_folder: std::env::temp_dir().join("turbo-mcp-serve"),
    session_env: Arc::new(HashMap::new()),
    notification_handle: ToolNotificationHandle::noop(),
    owner_session_id: None, subagent: None, parent_scheduler_handle: None,
    skills: vec![],
    state_path: std::env::temp_dir().join("turbo-mcp-serve").join("state.json"),
    memory_backend: None,
    web_search_config: Default::default(),
    web_fetch_config: Default::default(),
    lsp: None,
    image_gen_config: Default::default(),
    video_gen_config: Default::default(),
    app_builder_deployer_config: Default::default(),
    api_key_provider: None, auth_provider: None, attribution_callback: None,
    system_reminder_tag: xai_grok_tools::reminders::DEFAULT_REMINDER_TAG,
};
```

Build the config with **only** the servable tools, asserting id uniqueness first (`registry/types.rs:1207` does `remove(&id).unwrap()` and panics on duplicates):

```rust
use xai_grok_tools::implementations::grok_build;
let cfg = ToolServerConfig {
    tools: vec![
        (&grok_build::ReadFileTool).into(),
        (&grok_build::ListDirTool).into(),
        (&grok_build::GrepTool).into(),
    ],
    behavior_preset: None,
};
let mut ids = std::collections::BTreeSet::new();
for t in &cfg.tools {
    anyhow::ensure!(ids.insert(format!("{t:?}")), "duplicate tool id in served config");
}
let bridge = ToolBridge::finalize_builder(ToolBridge::get_builder(), cfg, ctx).await?;
bridge.set_confine_root(roots[0].clone()).await;
bridge.set_additional_directories(roots[1..].to_vec()).await;
```

Then validate every advertised schema and **drop** any tool that fails — the drift guard the audit demanded:

```rust
let mut served = Vec::new();
for def in bridge.tool_definitions().await {
    let name = def.function.name.clone();
    if crate::guard::validate_tool_schema(&name, &def.function.parameters).is_err() {
        tracing::warn!(tool = %name, "not served: schema failed declaration validation");
        continue;
    }
    let Some(kind) = bridge.tool_kind(&name) else { continue };
    served.push(ServedTool {
        name,
        description: def.function.description.unwrap_or_default(),
        schema: def.function.parameters,
        kind,
    });
}
```

`call()` gates through the guard **before** dispatch:

```rust
pub async fn call(&self, name: &str, args: Value) -> Result<String, CallFailure> {
    let Some(kind) = self.served.iter().find(|t| t.name == name).map(|t| t.kind) else {
        return Err(CallFailure::Refused(Denial::undeclared_tool()));
    };
    self.guard.check_call(name, kind, &args).map_err(CallFailure::Refused)?;
    let id = format!("mcp-{}", self.next_id());
    match self.bridge.call(name, args, &id).await {
        Ok(r) => Ok(r.prompt_text),
        Err(e) => Err(CallFailure::Failed(e.to_string())),
    }
}
```

- [x] **Step 5: Run tests**

```bash
cargo test -p xai-grok-mcp-server --lib -- --test-threads=4
```
Expected: PASS.

- [x] **Step 6: Add the refusal test**

```rust
#[tokio::test]
async fn call_outside_root_is_refused_before_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("id_rsa");
    std::fs::write(&secret, b"KEY").unwrap();
    let ts = ServedToolset::new(vec![root.path().to_path_buf()], true).await.unwrap();
    let err = ts.call("read_file", serde_json::json!({"target_file": secret.to_string_lossy()})).await.unwrap_err();
    assert!(matches!(err, CallFailure::Refused(_)));
}
```

- [ ] **Step 7: fmt, clippy, test, commit**

```bash
cargo fmt -p xai-grok-mcp-server && cargo clippy -p xai-grok-mcp-server --all-targets && cargo test -p xai-grok-mcp-server --lib
git add crates/codegen/xai-grok-mcp-server && git commit -m "feat(mcp-server): confined toolset construction"
```

---

### Task 2: Annotations and content bridge

**Files:** Create `src/annotate.rs`, `src/content.rs`.

**Interfaces:**
- Produces: `annotate::for_kind(kind: ToolKind) -> rmcp::model::ToolAnnotations`; `content::from_prompt_text(text: String) -> Vec<rmcp::model::ContentBlock>`.

- [x] **Step 1: Add the rmcp dependency**

`server` is already on via `default`; only the HTTP transport is additive. This pulls **no reqwest**.

```toml
rmcp = { version = "2.1", features = ["transport-streamable-http-server"] }
```

- [x] **Step 2: Write the failing test**

```rust
#[test]
fn read_only_kinds_are_annotated_read_only() {
    assert_eq!(for_kind(ToolKind::Read).read_only_hint, Some(true));
    assert_eq!(for_kind(ToolKind::Edit).read_only_hint, Some(false));
    assert_eq!(for_kind(ToolKind::Edit).destructive_hint, Some(true));
    assert_eq!(for_kind(ToolKind::WebFetch).open_world_hint, Some(true));
}
```

- [x] **Step 3: Run it and watch it fail**

```bash
cargo test -p xai-grok-mcp-server --lib read_only_kinds
```

- [x] **Step 4: Implement**

Derived from `ToolKind`, never hand-maintained, so a new tool cannot ship mislabelled:

```rust
pub fn for_kind(kind: ToolKind) -> ToolAnnotations {
    let ro = kind.is_read_only();
    ToolAnnotations {
        title: None,
        read_only_hint: Some(ro),
        destructive_hint: Some(!ro),
        idempotent_hint: None,
        open_world_hint: Some(matches!(
            kind,
            ToolKind::WebFetch | ToolKind::WebSearch | ToolKind::DeployApp
        )),
    }
}
```

`content.rs` maps Turbo's own three-variant `ToolOutput` to rmcp's five-variant `ContentBlock`; they are structurally incompatible and no conversion exists in-tree:

```rust
pub fn from_prompt_text(text: String) -> Vec<ContentBlock> {
    vec![ContentBlock::text(text)]
}
```

- [ ] **Step 5: Run, fmt, clippy, commit**

---

### Task 3: `ServerHandler`

**Files:** Create `src/handler.rs`.

**Interfaces:**
- Produces: `TurboMcpHandler::new(toolset: Arc<ServedToolset>) -> Self` implementing `rmcp::ServerHandler`.

- [x] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn list_tools_advertises_annotations_for_every_tool() {
    let root = tempfile::tempdir().unwrap();
    let ts = Arc::new(ServedToolset::new(vec![root.path().to_path_buf()], true).await.unwrap());
    let h = TurboMcpHandler::new(ts);
    let tools = h.tools_for_test();
    assert!(!tools.is_empty());
    assert!(tools.iter().all(|t| t.annotations.is_some()), "every tool must carry annotations");
}
```

- [x] **Step 2: Run it and watch it fail**

- [x] **Step 3: Implement**

```rust
impl ServerHandler for TurboMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "Turbo Build".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    async fn list_tools(
        &self,
        _r: Option<PaginatedRequestParams>,
        _c: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult { tools: self.tools_for_test(), next_cursor: None })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _c: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        match self.toolset.call(&request.name, args).await {
            Ok(text) => Ok(CallToolResult::success(content::from_prompt_text(text))),
            // A refusal is a tool-level error the model can act on, not a
            // protocol error. The message is the guard's opaque one.
            Err(CallFailure::Refused(d)) => Ok(CallToolResult {
                content: content::from_prompt_text(d.to_string()),
                structured_content: None,
                is_error: Some(true),
                meta: None,
            }),
            Err(CallFailure::Failed(msg)) => Ok(CallToolResult {
                content: content::from_prompt_text(msg),
                structured_content: None,
                is_error: Some(true),
                meta: None,
            }),
        }
    }
}
```

- [x] **Step 4: Add the refusal-shape test**

```rust
#[tokio::test]
async fn refused_call_is_a_tool_error_not_a_protocol_error() {
    // A guard refusal must come back as is_error, so the model can adapt,
    // and must not leak which cause triggered it.
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let ts = Arc::new(ServedToolset::new(vec![root.path().to_path_buf()], true).await.unwrap());
    let h = TurboMcpHandler::new(ts);
    let res = h.call_tool_for_test("read_file", serde_json::json!({
        "target_file": outside.path().join("x").to_string_lossy()
    })).await.unwrap();
    assert_eq!(res.is_error, Some(true));
}
```

- [ ] **Step 5: Run, fmt, clippy, commit**

---

### Task 4: HTTP transport with bearer auth

**Files:** Create `src/http.rs`, `src/config.rs`.

**Interfaces:**
- Produces: `McpServeConfig { roots, read_only, port, token, path_segment }`; `serve(config, toolset) -> anyhow::Result<ServeHandle>`; `ServeHandle { pub url: String, pub shutdown: oneshot::Sender<()> }`.

rmcp's `StreamableHttpService` performs **no authentication**; its only control is `allowed_hosts`, defaulting to loopback and 403ing otherwise. The bearer layer is therefore ours, and the tunnel must pass `--http-host-header 127.0.0.1:<port>` so the loopback default still holds.

- [x] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn missing_bearer_is_rejected() {
    let (h, _g) = spawn_test_server().await;
    let res = reqwest::Client::new().post(&h.url).json(&serde_json::json!({})).send().await.unwrap();
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn wrong_secret_path_is_404() { /* same shape, bad path segment */ }

#[tokio::test]
async fn oversized_body_is_rejected() { /* 9 MiB body -> 413 */ }
```

- [x] **Step 2: Run and watch them fail**

- [x] **Step 3: Implement**

```rust
let service = StreamableHttpService::new(
    move || Ok(TurboMcpHandler::new(toolset.clone())),
    Arc::new(LocalSessionManager::default()),
    StreamableHttpServerConfig::default(),
);
let app = axum::Router::new()
    .route_service(&format!("/{path_segment}/mcp"), service)
    .layer(axum::middleware::from_fn(require_bearer))
    .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024));
let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
```

Constant-time compare, so the token cannot be recovered byte-by-byte:

```rust
fn token_matches(given: &[u8], expected: &[u8]) -> bool {
    ring::constant_time::verify_slices_are_equal(given, expected).is_ok()
}
```

- [ ] **Step 4: Run, fmt, clippy, commit**

---

### Task 5: `turbo mcp serve`

**Files:** Modify `crates/codegen/xai-grok-pager/src/mcp_cmd.rs` (variant at `:65`, dispatch at `:157`); modify `crates/codegen/xai-grok-pager/Cargo.toml`.

- [x] **Step 1: Write the failing parse test**

```rust
#[test]
fn serve_parses_roots_and_tier() {
    let cli = Cli::try_parse_from(["turbo","mcp","serve","--root","/tmp/a","--allow","readonly"]).unwrap();
    // assert the McpServeArgs fields
}
```

- [x] **Step 2: Add the variant**

```rust
/// Serve Turbo's tools over MCP to an external client
Serve(McpServeArgs),
```

```rust
#[derive(Debug, clap::Args, Clone)]
pub struct McpServeArgs {
    /// Approved root. Repeatable. Required — the server never adopts cwd.
    #[arg(long = "root", value_name = "PATH", required = true)]
    roots: Vec<PathBuf>,
    /// Capability tier.
    #[arg(long, value_enum, default_value = "readonly")]
    allow: AllowTier,
    /// Refuse every non-read-only tool regardless of tier.
    #[arg(long)]
    read_only: bool,
    /// Tunnel helper.
    #[arg(long, value_enum, default_value = "none")]
    tunnel: TunnelKind,
    #[arg(long, value_name = "PATH")]
    tunnel_bin: Option<PathBuf>,
    #[arg(long)]
    port: Option<u16>,
}
```

- [x] **Step 3: Dispatch**

```rust
McpCommand::Serve(args) => run_serve(args).await,
```

`run_serve` must refuse to widen an inherited confine, mirroring `apply_confine_roots`:

```rust
if xai_grok_tools::types::resources::is_process_confined() {
    for r in &args.roots {
        anyhow::ensure!(
            xai_grok_tools::types::resources::path_is_under_any_root(r, &process_confine_roots()),
            "--root {} is outside the inherited --confine root; refusing to widen", r.display()
        );
    }
}
```

- [x] **Step 4: Print the URL and token, then hold**

```rust
println!("MCP endpoint: {}", handle.url);
println!("Authorization: Bearer {}", token);
tokio::signal::ctrl_c().await?;
toolset.shutdown().await;
```

- [ ] **Step 5: Run, fmt, clippy, test, commit**

```bash
cargo test -p xai-grok-pager --lib mcp_cmd -- --test-threads=4
```

---

### Task 6: Tunnel supervision

**Files:** Create `src/tunnel.rs`.

Template: `crates/codegen/xai-grok-cdp/src/launch.rs:224-265`.

- [x] **Step 1: Write the failing test**

```rust
#[test]
fn missing_tunnel_binary_is_an_error_not_a_silent_fallback() {
    let err = resolve_tunnel_bin(TunnelKind::Cloudflare, Some("does-not-exist".into())).unwrap_err();
    assert!(err.to_string().contains("not found"));
}
```

- [x] **Step 2: Implement**

Enrolled, never raw-spawned — an unenrolled child outlives the session:

```rust
let mut cmd = tokio::process::Command::new(bin);
cmd.arg("tunnel").arg("--url").arg(format!("http://127.0.0.1:{port}"))
   .arg("--http-host-header").arg(format!("127.0.0.1:{port}"))
   .stdout(Stdio::piped()).stderr(Stdio::piped());
let child = xai_tty_utils::global_process_scope().spawn(cmd)?;
```

Scrape the URL under a timeout; a slow tunnel is an error, never an unbounded wait.

- [ ] **Step 3: Run, fmt, clippy, commit**

---

### Task 7: User docs

**Files:** Create `crates/codegen/xai-grok-pager/docs/user-guide/32-mcp-server.md`; register in `docs.rs` (`guide!` macro).

- [x] **Step 1: Write the doc**

Must state plainly, per the spec's honesty section: `--allow readonly`/`edit` are root-bounded; `--allow full` is not root-bounded on any platform where the OS sandbox is a no-op (Windows included) and means "a shell on this machine, started in this directory". Must state that the tunnel URL plus bearer token is a credential.

- [x] **Step 2: Verify registration**

```bash
cargo test -p xai-grok-pager --lib docs -- --test-threads=2
```

- [ ] **Step 3: Commit**

---

### Task 8: Audit and fix loop

- [x] **Step 1: Run the adversarial audit workflow over the whole crate plus the CLI wiring**

Scope must include the invocation path, which the previous audit called out as unexamined: that the guard runs on every dispatch, on the same name and JSON the toolset sees, and cannot be bypassed by a sibling MCP path (`resources/*`, prompts, completion, batched or notification-shaped calls).

- [x] **Step 2: Fix every surviving finding**

AGENTS.md forbids deferring. Fix, retest, re-audit until residual findings are fixed or explicitly accepted by the operator.

- [x] **Step 3: Re-run the audit and confirm the verdict flipped**

The prior verdict was "not yet trustworthy". Do not claim otherwise without a fresh run saying so.

Round 9 (2026-09-11) is that fresh run, and it said so: 48 agents, 38 raw findings,
21 confirmed after adversarial verification, and a critic whose verdict was that
**no confirmed finding blocks the operator from testing this feature** — every
containment defect it upheld is reachable only in the `--allow edit` tier, and the
readonly tier had no verified hole.

Two things that verdict does **not** cover, stated plainly:

1. It was rendered on the round-8 revision. The 21 fixes landed after it, so the
   code that exists now has been tested (both platforms green) but not audited. A
   round-10 pass over the round-9 changes is what would close that gap.
2. The critic's own blocking list was about reaching the feature at all, not about
   the boundary: authentication (now Task 11), `grep` silently never running when
   ripgrep is absent, and the client not being told its roots (fixed in round 9).

Updated 2026-09-11: item 2's first entry is closed — Task 11 implemented OAuth and
round 10 audited it, with all 45 confirmed findings fixed and both platforms green.
Item 1 still stands, and now covers round 10 as well: neither the round-9 guard
fixes nor the round-10 OAuth fixes have been audited on the revision that exists
now. Round 10 audited the OAuth surface as it stood *before* those fixes, and it
caught three defects in this plan's own tests while doing so — so the residual risk
here is not hypothetical.

---

### Task 9: Linux green via WSL

Windows cannot cross-compile the sandbox-gated code.

- [x] **Step 1: Check C: free space first**

The WSL image lives on C: and fills during Linux builds; `LNK1102` and WSL I/O errors are that symptom.

```bash
df -h /c | head -3
```
Require > 40 GB free before starting.

- [x] **Step 2: Build and test in WSL**

```bash
wsl.exe -d Ubuntu -- bash -lc "cd '/mnt/h/Apps/grok build/turbo-grok-build' && cargo test -p xai-grok-mcp-server --lib -- --test-threads=4"
```

- [x] **Step 3: Fix any Linux-only failures**

Expect divergence in: path admission (no drive letters, no DOS device names, different symlink semantics), `dunce` being a no-op, and `xai-grok-sandbox` actually applying.

- [x] **Step 4: Confirm clippy and fmt on Linux too**

---

### Task 10: Final verification and cleanup

- [x] **Step 1: Full gate on Windows**

```bash
cargo fmt -p xai-grok-mcp-server -p xai-grok-pager -- --check
cargo clippy -p xai-grok-mcp-server -p xai-grok-pager --all-targets
cargo test -p xai-grok-mcp-server --lib -- --test-threads=4
cargo test -p xai-grok-pager --lib mcp_cmd -- --test-threads=4
```

Run on the round-8 revision (2026-09-11): clippy clean in both crates, server tests
240 passed + 1 ignored, pager `mcp_cmd` 23 passed. `fmt --check` over the whole
pager crate exits 1 on two files this work never touched: `src/app/cli.rs`, whose
committed copy in `HEAD` already fails the same check, and `src/app/app_view.rs`,
inside the operator's own uncommitted edit. Every file this work does touch passes
`rustfmt --check` on its own (`guard.rs`, `guard_tests.rs`, `toolset.rs`,
`toolset_tests.rs`, `mcp_serve_cmd.rs`, `grep/mod.rs`); reformatting the
operator's files is out of scope.

- [x] **Step 2: Line-ending gate as CI runs it**

```bash
git ls-files --eol | grep -E '^i/(crlf|mixed)'
```
Expected: no output.

Re-run on the final audit-fix revision (2026-09-11): `CRLF_IN_INDEX=0`. Every file
this work touched is LF in the worktree too — `oauth.rs`, `http.rs`,
`http_tests.rs`, `mcp_serve_cmd.rs`, the user guide, the spec and this plan all
count zero CR bytes. That was checked *before* editing rather than after: the
repo's CRLF files have to be rewritten by byte-level scripts, and `mcp_serve_cmd.rs`
was only edited directly once it had been confirmed LF.

- [x] **Step 3: Reclaim build caches**

Deferred until Task 11 is green (2026-09-11): `target/debug` is 177 GB and the WSL
target another 56 GB, but wiping either now would make every OAuth build start from
scratch, and a release-dist build would be invalidated by that work anyway. Reclaim
and build once, at the end.

```bash
rm -rf target/debug
df -h /c /h | head -3
```

Done once both platforms were green on the audit-fix revision, and not before: the
round-10 fix loop rebuilt the server crate a dozen times, and every one of those
runs would have started cold. `target/debug` had grown to **185 GB** by then, above
the 177 GB recorded when this step was written. H: went from 231 GB to **415 GB**
available (76% used to 56%).

Only the `debug` profile was removed. `release-dist` is kept, since Step 4's build
writes there. C: was deliberately left alone at 51 GB free — the WSL image lives on
it, and a full C: is the recorded cause of `LNK1102` and WSL I/O failures during
Linux builds, so that headroom is what any further WSL run depends on. The WSL
target (`$HOME/turbo-linux-target`, ~56 GB) is likewise still in place: Linux was
re-verified on this revision and wiping it would cost a cold rebuild to learn
nothing new.

- [x] **Step 4: Report Windows and Linux status honestly**

State per-platform: what ran, what passed, what was skipped and why. A skipped test is not a pass.

Final state, 2026-09-11, on the audit-fix revision (round 9 guard fixes + Task 11
OAuth + all 45 round-10 findings):

| | Windows | Linux (WSL Ubuntu) |
|---|---|---|
| `xai-grok-mcp-server` tests | 271 passed, 0 failed, 1 ignored, 0 filtered out | 273 passed, 0 failed, 1 ignored, 0 filtered out |
| `xai-grok-mcp-server` clippy | 0 hits | 0 hits |
| `xai-grok-tools` (7 tracked suites) | discovery 43, grep 111, search_replace 110, policy 41, line_diff 4, resources 101, confined_fs 6 | discovery 43, grep 113, search_replace 110, policy 41, line_diff 4, resources 96, confined_fs 5 |
| `xai-grok-tools` clippy (7 tracked files) | 0 hits | 0 hits |
| `xai-grok-pager` `mcp_serve_cmd` / `docs` | 35 / 21 | 35 / 21 |
| `xai-grok-pager` clippy, `turbo` bin check | 0 hits, exit 0 | — |
| release-dist `turbo` binary | built, 148 MiB, `1.0.13-rc.3` | — |

One caveat on that table: the `xai-grok-tools` rows were measured on the revision
*before* the round-10 OAuth fixes, during the Windows gate and the first Linux run.
They are carried forward because that crate was not touched afterwards — round 10
changed only `oauth.rs`, `http.rs`, `http_tests.rs` and `mcp_serve_cmd.rs` — so the
results still describe the code that exists. Every other row was measured on the
final revision. The distinction is recorded rather than smoothed over, because a
number carried from an older run is a weaker claim than one just observed.

The binary was also run, not merely linked: `turbo --version` reports
`1.0.13-rc.3 (7ab727263)` and `turbo mcp serve --help` renders the subcommand with
`--root`, `--allow readonly|edit` and `--port`.

Every cross-platform count difference was diffed **by name**, not assumed:

- Server, +2 on Linux: five unix-only tests against three windows-only ones.
- `resources`, +5 on Windows; `confined_fs`, +1: Windows case-sensitivity and
  long-path tests, exactly the category that should be absent on Linux.
- `grep`, +2 on Linux: `embedded_search_tools::grep_prepends_ugrep_defaults`
  (its module is `#[cfg(unix)]` in the *parent* `mod.rs`, which is why three
  searches inside the file itself found nothing) and `opencode::exit_code_2_with_output`
  (`#[tokio::test]` **before** `#[cfg(unix)]`, so an attribute-order assumption
  missed it). Nothing Windows covers is missing from Linux.

**What was skipped, and why it is not a pass.** One test is ignored on both
platforms: `live_cloudflared_quick_tunnel_reaches_the_server`, which starts a real
Cloudflare tunnel and is run by hand. The Linux side was not given the release-dist
build or the `turbo` binary clippy check — Windows is the release target here.
`cargo fmt --check` over the whole pager crate still cannot pass, for the reason
Task 8 records: `src/app/cli.rs` fails it in `HEAD` and `src/app/app_view.rs` is
inside the operator's uncommitted edit. Formatting was therefore scoped to
`-p xai-grok-mcp-server`.

**What is untested by anything here.** The OAuth consent flow has never been
exercised against a real ChatGPT connector, only against this suite. And the
round-9 and round-10 fixes have not themselves been audited — round 10 audited the
OAuth surface as it stood *before* its own fixes landed. That is the same gap
Task 8 records for round 9, now inherited by round 10, and it is worth taking
seriously: round 10 found three defects in this plan's own tests.

Nothing is committed. Task 7 Step 3 stays unticked for the same reason: the
standing instruction is to commit only when asked.

---

### Task 11: OAuth 2.1 + RFC 9728 for the ChatGPT path

Chosen by the operator on 2026-09-11, after round 9's critic found that
authentication, not the guard, is what blocks a real ChatGPT test: ChatGPT's
connector discovers a server's OAuth metadata and registers itself, and never
sends a static bearer header. The alternative considered and rejected was a
secret-path-only mode, whose credential would sit in every URL a proxy logs.

The MCP authorization specification makes these obligations, as an OAuth 2.1
resource server:

- **Protected resource metadata (RFC 9728).** The resource identifier has a path,
  so the document lives at `/.well-known/oauth-protected-resource/<segment>/mcp`
  and carries `resource` plus at least one `authorization_servers` entry.
- **Challenge.** Every `401` answers with
  `WWW-Authenticate: Bearer resource_metadata="<that URL>"`.
- **Audience binding (RFC 8707).** A token is accepted only if it was issued for
  this server; anything else is refused and never passed downstream.

Hosting the authorization server here adds RFC 8414 metadata, authorization-code
with PKCE (S256), exact redirect-URI matching, refresh-token rotation for public
clients, and RFC 7591 dynamic client registration. `rmcp` 2.1 ships OAuth for the
**client** side only (`transport::auth`, behind its `auth` feature), so every
endpoint here is this crate's to write and to audit.

- [ ] **Step 1: Write the design section and its threat model**

Consent has no user database to draw on. It binds to console access: Turbo prints
a one-time approval code, the operator enters it in the browser form the
authorization endpoint serves. Decide and record: token lifetime and rotation,
where credentials live (never on disk in the clear), what the tunnel exposes, and
what happens when `--tunnel none` makes the redirect non-HTTPS.

- [x] **Step 2: Metadata and challenge, with the bearer path unchanged**

The metadata endpoints must sit outside the bearer layer (they are unauthenticated
by definition) while every MCP route keeps its current check. Existing bearer
clients must keep working: the token stays valid, so this is additive.

- [x] **Step 3: Registration, authorization and token endpoints**

RFC 7591 registration issuing a public client id; authorization with PKCE S256,
exact redirect matching and the console-bound consent step; token exchange with
short-lived access tokens and rotating refresh tokens; audience validation on
every MCP request.

- [x] **Step 4: Tests**

Eleven wire-level tests (2026-09-11), all passing: the metadata document and its
RFC 9728 path, the `WWW-Authenticate` challenge on an unauthenticated call, an
authorization server offering `S256` only, registration refusing a redirect that
is neither HTTPS nor loopback, `plain` PKCE and an unknown client and an
unregistered redirect all refused, consent refusing three wrong console codes, an
authorization code that no failed attempt survives, an issued token opening the
MCP route while the operator's own token still works, refresh rotation with the
wrong client refused and a replay revoking what it became, a token refused once
the resource changes, and an oversized body on an unauthenticated route. Server
suite 260 passed + 1 ignored; pager serve 35.

A filter caught me first: `cargo test --lib oauth` matched two test *names* and
reported "ok. 2 passed", which would have read as a green suite while ten cases
never ran. Report the whole suite, not a convenient filter.

Wire-level tests for each endpoint and each refusal: no PKCE, wrong verifier,
replayed code, unknown client, mismatched redirect, a token issued for another
resource, and an expired token. A test that a `401` carries the challenge.

- [x] **Step 5: Audit the new surface, then re-verify both platforms**

New unauthenticated endpoints are new attack surface reachable before any
credential exists. Audit them on their own terms, fix what it finds, and re-run
the Windows gate and the Linux WSL run before claiming the ChatGPT path works.

Audited 2026-09-11 with 56 agents over six lenses (pre-credential surface,
protocol conformance, tokens and consent, redirects and HTML, integration, test
honesty), then adversarial verification, then a high-effort critic. **45
confirmed: 15 blocks-user-testing, 26 medium, 4 low**, deduping to about fifteen
distinct defects — several lenses found the same thing independently, which is
the useful signal, not the count.

The critic's verdict was *do not put this in front of a ChatGPT connector yet* —
explicitly **not** because an unauthenticated party could obtain a token (PKCE
S256, exact redirect matching, codes removed before validation, and the
constant-time console-code compare all held) but because the flow could not
survive a first-run setup, and one unauthenticated party could wedge the whole
server, taking the existing bearer path down with it through the shared
connection budget.

The four blockers, and what each is now:

1. **Consent measured from process start** (six lenses). `consent_started` was
   stamped in `OauthState::new`, so OAuth was permanently dead five minutes after
   boot — under `--tunnel cloudflare` up to 45s of that window elapsed before the
   code was even printed, every retry was guaranteed to fail, and the only way
   out was a restart that re-rolled the path segment, token, console code and
   tunnel hostname. Now `Option<Instant>`, armed by `begin_consent()` when the
   consent page is served and re-armed on every visit, with `ConsentExpired` kept
   apart from `ConsentRefused` so the operator is told which one happened.
2. **The 401 challenge frozen at loopback** (five lenses). `metadata_url` was
   snapshotted from the bound address into an `Arc<String>`, so behind a tunnel
   the metadata *document* was right while the pointer to it, the printed
   `OAuth:` line and the `--json` field all said `127.0.0.1`. This was the same
   ordering mistake already solved for `resource` and not carried across. Now
   derived per request from the live resource; `ServeHandle::metadata_url` is a
   method, and the stale snapshot is gone from `serve_with_options`.
3. **No body-read timeout on the OAuth routes** (three lenses). `axum`'s `merge`
   copies already-built routes, so `limit_body` layered on the `mcp` router never
   reached them; `DefaultBodyLimit` is a byte cap, not a time cap, and hyper's
   header timer disarms once the head parses. 64 sockets sending a complete head
   with a `Content-Length` they never satisfy held connection permits for the
   life of the process. `limit_body` is now layered on the OAuth router itself.
4. **32 registrations wedging `/oauth/register`.** Clients were retained against
   the 14-day refresh TTL and never evicted, so 32 unauthenticated POSTs locked
   the operator's own connector out permanently. Now the oldest **grantless**
   client is evicted to make room, and `TooMany` is returned only when every slot
   holds a client with a live grant — a real capacity limit rather than a scan.

Also fixed: `ACCESS_TOKEN_TTL` is enforced in `accepts` (tokens advertised as
lasting an hour were accepted for fourteen days, because `expire`'s disjunction
made the access arm dead); `grants` gained the cap it alone lacked;
`spent_refresh` carries timestamps, is pruned in `expire` and cleared by
`set_resource` so a rebind cannot look like theft; replay detection keeps its
record instead of erasing its own evidence and reports whether a revocation
actually happened; that report moved to `error` level, because `turbo mcp serve`
installs an `"error"` filter and was swallowing the module's only theft signal;
userinfo in a loopback redirect is refused (`http://127.0.0.1:80@evil/cb` reads
as `evil` to every real parser); the consent page ships `X-Frame-Options`,
`frame-ancestors 'none'`, `no-referrer` and `no-store`, with both displayed
values bounded and stripped of bidi and zero-width controls; and OAuth refusals
now reach the operator observer through the same throttled line as a bad bearer
token, which required threading the toolset through the OAuth handlers.

Two things were decided rather than deferred. The console code **stays reusable
inside its window**, and the spec was corrected instead of the code: a tunnel
reporting its public URL clears every grant, so a client must authorize again,
and a single-use code would force a restart to do it. The window, now short and
operator-initiated, is what bounds it. And eviction never drops a client with a
live grant, so a burst of registrations cannot displace a working connector.

Three of these findings were defects in *this plan's own tests*, which is the
part worth remembering:

- Every OAuth test drove `state=xyz` — three unreserved characters that round-trip
  identically whether or not escaping exists — so no test could detect the
  unescaped `state` the audit found. Now `HOSTILE_STATE` = `a+b/c=d&code=injected#frag`.
- `an_overlong_state_is_refused_rather_than_shortened` was passing on serde's
  duplicate-key rejection, not the length cap: the helper already appended a
  `state`, and the test appended a second one. It only surfaced when the new
  consent GET failed on the same URL.
- `a_token_issued_for_another_url_is_not_accepted_here` was vacuous, as the
  critic showed: `set_resource` clears `grants`, so the audience comparison never
  ran and deleting it outright would have left the suite green. It now also mints
  a token at the new resource, and a `#[cfg(test)]` seam tests `accepts` directly
  against a grant carrying a foreign resource.

That is five occasions in this work where a green result was not evidence of what
it appeared to prove. The lesson from Step 4 generalises past test filters: a
passing assertion is only worth what its inputs can distinguish.

Ten tests were added for the fixes, each written to fail if its fix were reverted:
the window arming on the page GET and closing and **re-arming**; the challenge
following a tunnel's public URL; registration surviving a 40-request burst;
userinfo redirects refused; an access token dying at the hour it advertises; the
audience check reached directly; a stalled body on an unauthenticated OAuth route
answered `408`; and an OAuth refusal reaching the operator observer.

**Verified on both platforms, 2026-09-11, on the audit-fix revision:**

| | Windows | Linux (WSL) |
|---|---|---|
| `xai-grok-mcp-server` tests | 271 passed, 0 failed, 1 ignored, **0 filtered out** | 273 passed, 0 failed, 1 ignored, **0 filtered out** |
| `xai-grok-mcp-server` clippy | 0 hits | 0 hits |
| `xai-grok-pager` `mcp_serve_cmd` | 35 passed | 35 passed |
| `xai-grok-pager` `docs` | 21 passed | 21 passed |
| `cargo fmt -p xai-grok-mcp-server` | clean | — |

The 2-test gap is the platform split, diffed by name rather than assumed: five
unix-only tests against three windows-only ones in this crate. The one ignored
test is `live_cloudflared_quick_tunnel_reaches_the_server`, which starts a real
tunnel and is run by hand. `0 filtered out` is recorded deliberately — a filtered
suite is what produced the Step 4 near-miss.

**What this does not cover.** These fixes have not themselves been audited; the
same gap Task 8 records for round 9 now applies to round 10. The consent flow has
not been exercised against a real ChatGPT connector — only against this suite —
so the claim is "the defects the audit found are fixed and tested", not "the
ChatGPT path is confirmed working end to end".
