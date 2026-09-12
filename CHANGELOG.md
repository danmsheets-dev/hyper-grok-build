# Changelog

All notable changes to **Turbo Build** (`turbo` binary).

Format: [Keep a Changelog](https://keepachangelog.com/).  
Wire versions: [`VERSION`](./VERSION) (`1.0.0-rc.N` on Grok Build 1.0 core;
older community line was `0.2.119-rN`).

English-only product surface (UI and public docs) as of RC14.

---

## Pedigree (community line)

Turbo Build evolved from the Hyper community fork of
[xAI Grok Build](https://github.com/xai-org/grok-build). Multi-agent work
accelerated at r6; product rebrand to **Turbo** at r10. From **1.0.0-rc.1**
onward, official Grok Build is the permanent upstream core remote
(`upstream` → `xai-org/grok-build`).

| RC | Wire | Theme |
|----|------|--------|
| r6 | `0.2.114-r6` | Isolation + headless honesty |
| r7 | `0.2.114-r7` | Folder worktrees by default |
| r8 | `0.2.114-r8` | `/deepaudit`, land/diff/discard |
| r9 | `0.2.114-r9` | Baselines, Boot Card, Auto Developer Log |
| r10 | `0.2.114-r10` | Turbo brand / `turbo` CLI |
| r11 | `0.2.114-r11` | Game Mode, Feature Request Log |
| r12 | `0.2.114-r12` | Isolation write boundary, densify, MCP harden |
| r13 | `0.2.114-r13` | Workspace Tree inject, Game Mode perf |
| r14 | `0.2.114-r14` | **web_fetch**, **workflow routing**, English-only |
| r15 | `0.2.119-r1` | Upstream 0.2.119 sync, security + Windows correctness |
| r2 | `0.2.119-r2` | Game Mode overhaul, disk gates, tools list |
| r3 | `0.2.119-r3` | Full Disk Clean (taxonomy, multi-path, prune, telemetry) |
| **1.0 rc1** | **`1.0.0-rc.1`** | **Grok Build 1.0.0 core + Turbo product layer** |
| **1.0 rc2** | **`1.0.0-rc.2`** | Agent WebView, Grok 4.6, confinement harden |
| **1.0 rc2.1** | **`1.0.0-rc.2.1`** | Agent WebView named-pipe hotfix |
| **1.0 rc3** | **`1.0.0-rc.3`** | Agent WebView field-test pass |
| **1.0 rc4** | **`1.0.0-rc.4`** | Fathom-style `/meeting` notetaker |
| **1.0 rc5** | **`1.0.0-rc.5`** | Harness Q&A: spawn catalog, WebView save, logs/redaction |
| **1.0 rc6** | **`1.0.0-rc.6`** | Providers ACP, isolation, Poolside hosted Chat API |
| **1.0 rc7** | **`1.0.0-rc.7`** | Phase 5 control plane + Meeting Join Hardening |
| **1.0 rc8** | **`1.0.0-rc.8`** | Scheduled tasks, Meeting R2, Browser R3, GitHub log sync |
| **1.0 rc9** | **`1.0.0-rc.9`** | Meeting Tool v3: joined Teams notetaker bot |
| **1.0 rc10** | **`1.0.0-rc.10`** | Teams join hardening and incident log |
| **1.0 rc11** | **`1.0.0-rc.11`** | Security honesty |
| **1.0 rc11.1** | **`1.0.0-rc.11.1`** | Windows worktree isolation hotfix |
| **1.0 rc12** | **`1.0.0-rc.12`** | Subagent hardening + Turbo Build rename |

Older release notes (r1–r13 detail) are archived under
[`docs/archive/`](./docs/archive/).

---

## [1.0.13-rc.4] - 2026-09-12

### Added
- **`turbo mcp serve` — Turbo as an MCP server.** Serves a bounded subset of
  Turbo's own file tools to an outside client over Streamable HTTP, bound to
  loopback behind a random path segment and a bearer token that both change on
  every start. `--root` is required and repeatable (the server never adopts the
  current directory on its own); `--allow` defaults to `readonly`, and `edit` is
  for a client trusted to change your code. `--tunnel cloudflare` puts a public
  HTTPS edge in front. Every call still passes the same path boundary, in the
  tier the operator started, inside the roots they approved.
- **OAuth 2.1 for clients that cannot send a pasted bearer header** — ChatGPT
  Developer mode among them. RFC 9728 protected-resource metadata, RFC 8414
  authorization-server metadata, RFC 7591 dynamic client registration, PKCE
  (`S256` only; `plain` refused), exact redirect-URI matching, and RFC 8707
  audience-bound tokens. Consent binds to the console: Turbo prints an approval
  code that the browser must return, so reaching the approval page is not enough
  to obtain a token. Clients that *can* send their own header keep working
  unchanged. **Not yet exercised against a live ChatGPT connector** — see the
  status note in the README.

### Fixed
- **Consent could never be granted on a server that had been running five
  minutes.** The approval window was measured from process start rather than
  from the moment the approval page is served, so under `--tunnel cloudflare` it
  often closed before the code was even printed, and afterwards no correct code
  could be accepted — the only recovery was a restart that re-rolled every other
  credential. The window now opens when the page is served, and reopens each
  time it is served again.
- **The `401` challenge and the printed `OAuth:` line named loopback behind a
  tunnel,** so a remote client's discovery step resolved to its own host. Both
  are now derived from the live resource instead of a bind-time snapshot.
- **The OAuth routes had no body-read timeout.** They are merged after the
  timeout layer, and `axum` does not apply a router's layers to merged routes,
  so connections that sent a `Content-Length` they never satisfied could hold
  every connection slot for the life of the process — taking the existing bearer
  path down with them, since both share the budget. The timeout is now layered
  on the OAuth router itself.
- **32 unauthenticated registrations could lock out the operator's own connector**
  for the life of the process. The oldest registration with no live grant is now
  evicted to make room instead of refusing the newcomer.
- Access tokens are now refused past the hour their `expires_in` advertises
  (previously honoured for 14 days); `state` is percent-encoded into the
  redirect rather than interpolated; `userinfo` in a loopback redirect URI is
  refused; the consent page ships frame-busting, referrer and no-store headers
  with both displayed values bounded and stripped of bidi controls; spent
  refresh tokens are expired and cleared on rebind; and OAuth refusals now reach
  the operator through the same throttled line as a bad bearer token.
- **`clippy::collapsible_if`** in `xai-tool-types`, `xai-grok-tools/build.rs`
  (two sites) and `xai-grok-config/shell.rs` (two sites), plus a needless borrow
  and a redundant `trim()` before `split_whitespace()` in `xai-grok-config`.
  `FileIdentity::from_metadata` is gated `#[cfg(not(windows))]` to match its
  call sites, rather than allowed or deleted — it is live on every other target.

---

## [1.0.13-rc.3] - 2026-09-05

### Fixed
- **Codex model picks stay in the window that chose them.** Native
  `openai-codex/*` ids (Astra, Sol, Spark, …) were written to shared
  `~/.grok/config.toml` as `[models].default` because persist-skip still
  matched only the legacy `codex:` prefix. Other Turbo windows hot-reloaded
  that default and retargeted new sessions / some subagent fallbacks.
  Codex is session-only again. CLI `-m` / `GROK_DEFAULT_MODEL` still work.

### Added
- **Spark spawn aliases for subagents.** `spark`, `codex-spark`,
  `gpt-5.3-spark`, and `openai-codex/spark` resolve to
  `openai-codex/gpt-5.3-codex-spark` and are advertised on the spawn catalog
  when Spark is available.

---

## [1.0.13-rc.2] - 2026-09-05

### Added
- **ChatGPT Codex `gpt-6-astra`.** Catalog key `openai-codex/gpt-6-astra`
  (aliases `openai/gpt-6-astra`, `gpt-6-astra`, `gpt-6`). Thinking menu
  matches the Codex desktop picker: **Light**, **Medium**, **High**,
  **Extra High**, **Ultra** (wire tokens `low` / `medium` / `high` /
  `xhigh` / `ultra`→`max`). Context window 1,050,000. Default effort is
  Medium.
- **ChatGPT Codex Spark is a first-class picker model.**
  `openai-codex/gpt-5.3-codex-spark` sits with the current Codex lineup
  (after Luna), is listed after login, and accepts `gpt-5.3-spark` /
  `openai/gpt-5.3-spark` spawn aliases. Effort menu is low · medium ·
  high · xhigh (default low).

---

## [1.0.13-rc.1] - 2026-09-02

### Security
- **Links no longer run through `cmd.exe` on Windows.** `cmd /c start` splits on
  `&` and expands `%VAR%`, so a server- or agent-supplied URL such as
  `https://example.com/&calc.exe` executed a command. URLs now go straight to
  `ShellExecuteW` as a single argument.
- **MCP tool descriptions are stripped of invisible characters.** A server could
  hide model-directed text in a description using zero-width, bidi-override, or
  tag-block (`U+E0000..E007F`) characters; the sanitizer only collapsed
  whitespace. The full invisible/spoofing class is now flattened before the
  collapse, covering both the tool catalog and compaction.
- **Container-runtime sockets are denied in network-restricted profiles.**
  `/run`, `/var` and `$HOME` stay readable, so a docker/podman/containerd socket
  — a root-equivalent API — was path-reachable. System, rootless per-uid, and
  Docker Desktop endpoints are added to the deny set and bound over at launch.
  Applied on every resolve path, so the Landlock/Seatbelt capability set gets
  them too, not just bwrap.
- **Read-deny masks are verified as real mountpoints.** Containment was inferred
  from the reproducible `__GROK_INSIDE_BWRAP` env marker, which a child can set.
  It is now confirmed via `/proc/self/mountinfo` and `statx`, with a bound
  sentinel directory; a hostile symlink at the sentinel path is replaced before
  binding.
- **The child-network seccomp filter no longer keys on apply-state.** If
  Landlock was unsupported or `Sandbox::apply` failed inside bwrap, the
  per-spawn filter was silently disabled — exactly the degraded state where it
  is the only enforcement left. It now keys on the resolved config.
- **Folder trust no longer cascades across a git-root boundary.** A grant
  covered every subdirectory by path prefix, so a nested (or sibling) repo under
  a trusted parent inherited the grant. A grant now covers only descendants that
  share its workspace key.
- **The bundle archive's entry cap could be bypassed.** The counter was bumped
  after the non-regular-entry skip, so a flood of directory/symlink/PAX headers
  spun the extraction loop uncapped. Every header now counts. Path
  sanitization also rejects Windows-hostile components (`:` names an NTFS
  alternate data stream, so `notes:ads.md` wrote hidden stream content).
- **Allowed-write and confine checks no longer drop a `..` on Linux/macOS.**
  The permission canonicalizer walks up to the nearest existing ancestor and
  re-joins the missing tail; `Path::file_name` is empty for a trailing `..`,
  so `docs/../secret.txt` (with `docs` absent) was re-joined as
  `docs/secret.txt` and passed an allowlist scoped to `docs`. Windows never
  reached this because its path API collapses `docs\..` before the
  filesystem sees it. The walk now keeps `..` and collapses it lexically.
- **Tilde operands are never canonicalized against the cwd.** On a host that
  cannot resolve `~/..` through the filesystem, the lexical fallback
  collapsed `~/../key.pem` to the workspace file `key.pem` and an allow on
  `*.pem` leaked to it. Tilde paths are matched literally only, as the
  policy already documented.
- **The installer scripts accept a typed loopback `TURBO_UPDATE_BASE_URL`.**
  `install.sh` / `install.ps1` pinned the base to the GitHub hosts (rc.11)
  while the updater itself admits an exact loopback host for local mirrors
  and tests. Both scripts now apply the updater's rule (`127.0.0.1`,
  `localhost`, `[::1]` with a numeric port, no credentials); every other
  origin still fails closed.
- **`TURBO_UPDATE_BASE_URL` is validated by parsing, not prefix matching.** The
  override already compared `host_str()` so the userinfo trick
  (`http://127.0.0.1:9@evil.com`) was rejected, but the check now requires
  `http(s)`, refuses embedded credentials, and resolves the host to an
  `IpAddr` — which also fixes a dead `::1` arm (`host_str()` yields `[::1]`).

### Fixed
- **A symlinked container-socket endpoint no longer refuses the session.** The
  new runtime-socket deny treated a well-known endpoint that is itself a symlink
  (`/var/run/docker.sock` under colima, Rancher Desktop, OrbStack) as fatal, and
  `--sandbox strict` / `read-only` exited at start on those hosts. The link cannot
  be masked in place and its target is outside the bwrap handoff policy, so it is
  skipped with a warning; real endpoints alongside it are still masked and the
  per-spawn child network filter remains the guarantee.
- **Mangled X10 mouse reports no longer type into the composer.** A
  UTF-8-converting relay (ConPTY forwarding to WSL/SSH) splits the column byte
  above column 95, so crossterm emitted a bogus mouse event plus a stray key
  press. The pair is now held briefly and recombined into the real report.
- **`web_search` accepts a domain allowlist or blocklist.** The two lists are
  mutually exclusive and each is capped at five domains, validated at every
  deserialize ingress so a bad config fails to parse instead of erroring
  mid-turn.
- **Turbo builds on Linux again.** `err_message` in the Linux-only capture
  backend matched `VoiceError` exhaustively and was never updated when the TTS
  variants landed, so the crate failed to compile off Windows.
- **The settings screen no longer advertises `always_allow_all_sessions` as the**
  **permission-prompt default.** The settings registry declared it while the
  runtime resolves `allow_once`; the declaration now matches the (safer) runtime
  behaviour, so the screen shows what actually happens.
- **`.envrc` loads on Linux again.** The bash evaluator was detached from the
  terminal twice (once by the loader, once by the deadline runner), so the
  child ran two `setsid` hooks; the second failed with EPERM, its `setpgid`
  fallback failed the same way for a session leader, and the spawn itself
  failed. Windows only sets a creation flag, so it never noticed. The
  duplicate detach is gone and the hook now treats an already-detached child
  as success.
- **Upload-queue orphan cleanup is independent of directory order.** The
  sweep read a temp file's age from its sidecar while deleting as it went, so
  on ext4 (hashed order) an expired sidecar could be removed first and its
  temp file, now without a sidecar, fell back to a fresh mtime and survived.
  Ages are decided for every entry before anything is deleted.
- **The confine shell analyser classifies Windows-shaped program tokens the
  same on every host.** `C:\...\blender.exe` was reported as *hiding* an
  absolute path on Linux (host `is_absolute` is false there) and the
  PowerShell recovery failed closed; drive and UNC shapes are now recognised
  host-independently.
- **Signal handlers preserve `errno`.** `signal-hook-registry` moves to 1.4.8
  (upstream lockfile), whose dispatcher saves and restores `errno` around
  callbacks; the ported regression test now passes.
- **A git-status invalidation can no longer be lost while a walk is in
  flight.** `invalidate` resolves the root through a short-lived cache; once
  that entry had expired for a root that had never been invalidated before,
  the fallback bumped only existing epoch entries, the root stayed at epoch
  0, and the next caller joined the pre-invalidation walk and got its stale
  result. Every root with a live slot is now bumped.
- **Mixpanel requests are bounded by a 10s timeout**, so a wedged endpoint
  cannot keep a telemetry future alive indefinitely.
- **Session no longer panics on context-length errors that omit stream metadata.**
  `should_compact_on_error` recovers from CLE text even when `model_metadata`
  is missing; `handle_sampling_failure` now falls back to the session
  context window instead of `expect`ing it on the error.
- **`spawn_subagent` accepts `openai/gpt-5.6-terra`.** An OpenRouter routing-slug
  collision no longer rejects the slug while still listing it. Spawn prefers
  the credentialed `openai-codex/gpt-5.6-terra` alias.
- **`turbo disk report/clean` see Windows `{drive}:\t\w` isolation trees**
  (and `$GROK_WORKTREE_ROOT`), not only `~/.grok/worktrees`.

### Added
- **Attach extra folders** (`--add-dir`, `/folder add|remove|list`, ACP
  `additionalDirectories`). Relative paths still resolve against primary
  `--cwd`. Extra folders expand the **write** confine set (and workspace-tree
  overlay); reads stay unconfined. Attaching extras on an otherwise unconfined
  session installs a write boundary of `[cwd, extras]`. Title bar shows `+N`
  (or `+basename` for one extra). Claude `permissions.additionalDirectories`
  is auto-applied on **new** sessions; TUI resume resends the stored list.
  Isolation worktrees still clone only the primary repo; extra folders stay
  live on disk. LSP `workspaceFolders` and fsnotify include extra roots
  (live `/folder` updates send `didChangeWorkspaceFolders` and extra
  watchers).
- **`--confine` / `GROK_CONFINE` as a path list.** Repeatable `--confine PATH`
  (alias `--workspace-root`). `GROK_CONFINE` is `;`-separated (Unix also
  splits on `:` when no `;` is present; Windows never splits on `:`). Nested
  turbo may only tighten inherited roots — a sibling not under any inherited
  root is a startup error. Streaming-json `start` keeps `confineRoot` (first
  root) and adds `confineRoots` (full list).
- OpenRouter **MiniMax M3 free** (`openrouter/minimax/minimax-m3:free`),
  **Inkling free** (`openrouter/thinkingmachines/inkling:free`), and
  **Nemotron 3 Ultra** (`openrouter/nvidia/nemotron-3-ultra-550b-a55b` and
  `:free`) catalog keys for spawn.
- NVIDIA Integrate newest NIMs: **Kimi K3** (`nvidia/moonshotai/kimi-k3`),
  **DeepSeek V4 Pro 0813** (`nvidia/deepseek-ai/deepseek-v4-pro-0813`),
  **DeepSeek V4 Flash 0731** (`nvidia/deepseek-ai/deepseek-v4-flash-0731`).
  Nemotron 3.5 Lightning and Muse Glimmer are now `agent_ready` so they can
  spawn write-capable subagents. Ultra / hang Llama / gpt-oss stay chat-only.
  Wan2.2-Animate is a video model and is not in the text spawn catalog.

---

## [1.0.0-rc.12] - 2026-08-26

**Subagent hardening + Turbo Build.** Child boot cards tell the truth about
Windows short worktrees (`{drive}:\t\w\{hash}\subagent-…`). The parent boot
card teaches that path and drops duplicated ADL/FRL prose. User-facing product
name is **Turbo Build**; CLI remains `turbo`.

#### Subagents

- Boot-card `infer_isolation_label` accepts the rc.11 short root and
  `$GROK_WORKTREE_ROOT` (same patterns as the start-gate).
- Child card budget 320 tokens (measured ~146); parent short target ≤1200
  (measured ~1168; cap still 1650).
- Depth-1 build subagents can launch named read-only review workflows.
- xhigh/max/unbounded GP default wall-clock is 45 minutes.
- Disk gate names the dest volume; clean seed materializes from HEAD.
- Cancelled `resume_from` reuses a preserved live worktree.
- Residual: isolation label is still a CWD-path heuristic, not spawn metadata.

#### Brand

- Display name **Turbo Build** (`PRODUCT_DISPLAY_NAME`).
- Machine id `--version --json` `product` stays `turbo-grok-build`.
- GitHub repo, CLI binary `turbo`, `~/.grok` / `~/.turbo` unchanged.

#### Also in this RC (open log sweep)

- Windows grok_home credential **writes** fail-closed in policy (kernel sandbox still advisory).
- MCP handshake fail-soft; Blender health probes TCP 9876.
- NVIDIA 429 retries (6, capped jitter).
- Persist Agent WebView profile (`$GROK_HOME/agent-browser`); OAuth popups are host-owned policy-checked tabs.
- `/steer` `/rollback`; `turbo test --match`; `turbo pr` / `turbo pipeline`; `turbo secret get`.
- `.grok/policy.toml` enforced at tool dispatch.
- Teams guest/web join without Graph; optional `GROK_MEETING_TTS=1` local SAPI.

---

## [1.0.0-rc.11.1] - 2026-08-26

**Windows isolation hotfix.** rc.11 moved worktrees to `{drive}:\t\w\{hash}` so
`git worktree add` stays on the source volume under MAX_PATH. The start-gate
honesty check still only accepted `~/.grok/worktrees/…` and
`grok-subagent-worktrees/…`, so every default `isolation=worktree` spawn on
Windows created a real tree then refused to start:

`isolation=worktree claimed but resolved child CWD is not a subagent worktree`.

That was fail-closed (not a silent parent share). The detector now accepts the
short root and `$GROK_WORKTREE_ROOT`.

---

## [1.0.0-rc.11] - 2026-08-26

**Security honesty.** Fail-closed confine bypasses, permission mapping for
mutating tools that were classified as pathless reads, folder-trust for project
skills, meeting-QA workspace confine, Agent WebView called **beta** and
permission-gated.

Do not describe `--confine` as an OS jail. Meeting audio is transcribed by
xAI hosted STT. Report vulnerabilities via this fork's GitHub private
advisory flow, not xAI HackerOne.

#### Security (HIGH)

- **Project skills/commands load only when the folder is trusted** (F04).
  `list_skills` fails closed; session paths pass `project_scope_allowed`.
- **Project plugins cannot inherit a user-scope enable-by-name** (F03). User
  plugins outrank same-named project plugins; enable requires a fully-qualified
  `PluginId`.
- **`browser_eval` `confirm` is ignored** (F14). Mutating expressions are
  always refused; prefer `browser_click` / `browser_fill`.
- **`turbo dashboard --web` requires a per-launch token** (F05) and rejects
  non-loopback `Host`, cross-origin `Origin`, and `Sec-Fetch-Site: cross-site`.
- **Ripgrep tarballs are SHA-256 pinned before embed** (F09). Release actions
  are SHA-pinned; `contents: write` is limited to the publish job (F02).
- **Internal OTLP export no longer stamps live xAI credentials onto
  user-repointed collectors** (F13). Standard `OTEL_EXPORTER_OTLP_*` endpoints
  are treated as external.
- **`read_file` / credential `Read`/`Grep` of `$GROK_HOME` auth files is
  policy-denied** (F17), matching the bash write-deny.
- **Internal campaign / claims-ledger files are gone from the tree** (F26/F27).
  `marketing/` is gitignored. This is delete-from-tip, not a history rewrite.

#### Security (MEDIUM, this round)

- Nested children inherit `GROK_CONFINE_SHELL_MODE`; `GROK_CONFINE*` env
  prefixes are unmodelled (F65/F67).
- Bash credential-path matching survives quoting; `mcp_credentials.json` is
  a credential basename (F48/F49).
- Secret redactor covers Groq/Cerebras/NIM/Fireworks prefixes, prefixed
  env assignments, and DSN userinfo (F50/F51).
- `unified.jsonl` is created 0600/0700 on Unix (F55).
- Permission prompt default is allow-once, not always-approve (F44).
- Project marketplace paths cannot escape the git root (F38).
- `browser_navigate` checks URL policy before the direct-download broker (F57).
- Codex OAuth loopback requires `state` (F52).
- Installers reject non-GitHub `TURBO_UPDATE_BASE_URL` (F71).
- `keep-features.yml` is `contents: read` with SHA-pinned actions (F28/F72).

#### Security (MEDIUM, remaining gates)

- Session `deny_commands` matches a dequoted/whitespace-collapsed haystack so
  `cu''rl` / `rm  -rf` cannot dodge the rule (F59).
- `allowed_paths` and subagent land canonicalize and fail closed on symlink
  escapes (F66/F60).
- Release job attests `dist/*` (F29/F31); bootstrap installers are release
  assets, not the mutable `dev` branch tip (F32).
- Marketplace remote installs default to `require_sha` (F47).
- Meeting briefing `cap`/`tail` and OpenCode read/grep truncate on UTF-8
  char boundaries (F43/F61/F62).
- Terminal emulator bounds total cells against CSI cursor-forward
  amplification (F45).
- NOTICE/LICENSE name Turbo Grok Build; THIRD-PARTY-NOTICES is regenerated
  from the ship graph via `cargo about` (F34/F37). Direct SKILL.md
  registration already requires workspace + vendor-compat (F63).
- Marketplace-vendored plugin copies require a full git SHA when
  `require_sha` is on (default) (F47).
- Installers fail closed on `gh attestation verify` when GitHub CLI is
  present (`GROK_SKIP_ATTESTATION=1` to checksum-only) (F29/F31).

#### Security (LOW, this round)

- Test fixtures no longer embed the operator username (F83).
- Cursor rule bodies are framed as untrusted and tag-neutralized; folder-trust
  scans `.cursor/rules` (F86).
- Teams bot-profile (Chromium History) is deleted when the meeting stops (F88).
- README last-sync line matches `SOURCE_REV` (F74).

#### Incident follow-through

- Session `images/` / media copies under `$GROK_HOME/sessions` are no longer
  treated as grok-home credentials.
- Official `Godot_v*` Windows console exports are modelled under confine.
- `gh_*` tools pin `--repo` to `origin` (not a read-only `upstream` remote).
- `openrouter/openai/gpt-5.6-terra` aliases to ChatGPT Codex Terra.
- Meeting copy no longer says Fathom-style; Teams is a guest in the lobby or
  local WASAPI, and the result names which one ran.
- HTTP 404 HTML is a loaded page, not `browser_navigate` failure.
- Host injects env-resolved `PolicyParams` at session start.
- Worktree disk gate prunes every non-live tree before refusing spawn.
- `get_command_or_subagent_output` lists known subagent ids and resolves
  `child_session_id` aliases so spawned reviews are addressable.
- Windows worktrees prefer `{drive}:\t\w\{hash}` (same volume as the source
  git repo) so `git worktree add` shares objects and `cargo fmt` stays
  under MAX_PATH. Override with `GROK_WORKTREE_ROOT`.
- Poolside Laguna children compact at 40% context (not 85%) so long blender
  briefs do not die on `max_tokens_truncation`.
- Linux bwrap binds a placeholder over missing `$GROK_HOME` credential
  files so `auth.json` cannot be created under the writable home grant.


---

## [1.0.0-rc.10] - 2026-08-24

**Teams Join Hardening and Incident Log.** rc.9 sent a guest notetaker into the
meeting. On one machine it worked; on another the operator got a File Explorer
window, a join that timed out, and a transcript of their own speakers that
looked healthy. Two independent defects, plus a crash found on the way.

### Fixed

- **`meeting_join` no longer opens the join link when a guest bot is
  dispatched.** The link was handed to the OS unconditionally, before the
  transport was even chosen, via `explorer.exe <url>` — which opens the default
  browser when the `https` association resolves and *reveals a folder* when it
  does not. That single line produced both the working Chrome window on one
  machine and the stray File Explorer windows on the other. Local-capture paths
  still open the link, because there the operator does have to be in the
  meeting themselves.
- **Windows now opens join links with `ShellExecuteW(open)`,** matching the
  contract the pager already documents, instead of `explorer.exe` — which
  spawns a new Explorer window per call. A link with no handler is *reported*,
  never turned into a file-manager window.
- **A prompt containing any multi-byte character could abort the process.**
  `first_https_url` walked byte offsets and sliced on them, so a smart quote, em
  dash or emoji anywhere past byte 8 panicked — and `panic = "abort"` made that
  a hard process death. It ran on every prompt submit through
  `detect_join_request`, which is why it looked like a large-paste crash: a long
  paste almost always contains one. Now walks character starts.
- **`turbo issues sync --push` exited 0 after pushing nothing.** A run where
  every incident was skipped printed a cheerful summary and reported success.
  It now exits nonzero. Same for `turbo features sync`.

### Added

- **Teams web-join rewrite.** Teams redirects a `/meet/<id>` link to
  `/dl/launcher/launcher.html?…&msLaunch=true&suppressPrompt=true`, which fires
  the `ms-teams:` protocol immediately and never renders "Continue on this
  browser" — leaving the notetaker with no DOM to drive. The bot now navigates a
  query-only rewrite asking for the anonymous web client. Path, host and the `p`
  passcode are preserved untouched; unrecognised shapes fall back to the URL as
  pasted. Kill switch: `GROK_MEETING_TEAMS_WEB=0`.
- **Page-side protocol guard.** The injected tap refuses `ms-teams:`,
  `msteams:` and `teams:` navigations through `window.open`, `location.assign`,
  `location.replace` and a capture-phase anchor click, and reports each one.
- **The continue-on-web click is retried from the page's own poll loop.** The
  launcher redirects twice inside a second while the Rust side polls at 500 ms,
  so a single click could never win that race.
- **`BotState::Launcher` and `BotError::LauncherHandoff`.** A page parked on the
  launcher is now named in 20 seconds instead of being reported as a generic
  "join timed out" after 60. Every bot failure maps to a typed
  `JoinFailureStage` recorded in `meta.json`.
- **Navigation logging.** `Page::navigation_stream()` surfaces the redirect
  chain that was already flowing through the CDP connection and being discarded.
  Diagnosing this incident previously meant reading the browser profile's
  History file by hand.
- **Downloads are denied browser-wide** (`Browser.setDownloadBehavior`), so the
  launcher's `directDl=true` cannot pull an installer. Best-effort: a protocol
  mismatch warns, it never fails a join.
- **`gh repo view` preflight.** Sync now asks for `hasIssuesEnabled`,
  `viewerPermission`, `isFork` and `isArchived` *before* listing, and refuses
  with a remediation naming the exact GitHub settings page. GitHub disables
  Issues on new forks by default, which is how a configured sync landed nothing
  and reported an opaque API string. On refusal the maintainer bundle is
  exported locally and its path printed, so the log is never stranded.

### Changed

- **A failed guest join no longer reads as success.** The outcome is durable in
  `meta.json` as `NotetakerOutcome`, and `meeting_join`, `meeting_status` and
  `meeting_stop` all render it, so they cannot disagree. A failed join now leads
  with `NO GUEST IN THE MEETING …` naming the reason, instead of burying one
  honest sentence seventh of eight lines under "Notetaker started".
- **Work-folder recaps record the capture source.** A recap transcribed from one
  PC's speakers used to be indistinguishable from one taken inside the meeting.
- **A visible (`GROK_MEETING_BOT_WINDOW=1`) launch gets `--window-size`,** which
  only headless mode set, so the diagnostic window now renders the layout the
  headless run saw.
- CI covers `xai-grok-meetings`, `xai-grok-meeting-bot` and `xai-grok-cdp`,
  which had no gate. `xai-grok-developer-log`'s test target did not compile on
  `dev` (a missing `RemoteState` import), so its existing gate was red.

---

## [1.0.0-rc.9] - 2026-08-23

**Meeting Tool v3 — the notetaker joins the meeting.** rc.4 recorded the
operator's speakers and put nobody in the room. rc.9 sends a real guest
participant: "Turbo (Notetaker)" waits in the Teams lobby, is admitted like any
other attendee, hears the meeting instead of the machine, and answers `Turbo:`
questions in chat under its own name. Closes
`fr_01a030379de47ce1bf74fed2c32cb44b` and `fr_01a030361d877ce39a41ff8b933df228`.

### Added
- **Joined Teams notetaker.** `meeting_join` launches the Edge already installed
  on the machine (headless, throwaway profile), joins as an anonymous guest named
  **Turbo (Notetaker)**, camera and mic off, and reports lobby → admitted state.
  Teams' default `ExternalBotAccessMode=RequireApprovalWhenDetected` holds
  detected notetakers in the lobby for an explicit admit; Turbo surfaces that
  rather than working around it.
- **In-page audio tap.** A document-start script wraps `RTCPeerConnection`,
  mixes inbound tracks through Web Audio running natively at 16 kHz, and streams
  20 ms frames of mono 16-bit LE PCM over a loopback WebSocket (bound to
  `127.0.0.1`, random per-meeting token) into the existing Grok STT pipeline.
  The tap is in-page rather than on the sound card, so it keeps working with
  the operator's speakers muted, their headset unplugged, or the operator
  gone. Captured PCM is then **uploaded to xAI hosted STT** (`wss://api.x.ai/v1/stt`
  by default, overridable via `[voice].api_base`). This is not local-only
  transcription and not a third-party meeting-bot SaaS.
- **Chat Q&A as the bot.** Scraped meeting chat feeds `inbox.jsonl`; answers
  post to meeting chat as **Turbo (Notetaker)**. `GROK_GRAPH_TOKEN` is no longer
  required for coworker Q&A — Graph is now the fallback, not the primary path.
- **`xai-grok-cdp`** — minimal Chrome DevTools Protocol client (Target / Page /
  Runtime) over a local WebSocket. Launches through `ProcessScope::enroll`, so
  the whole Chromium tree is reaped with the session.
- **`xai-grok-meeting-bot`** — join choreography, selector table, injected tap,
  loopback audio server.
- **Selector overrides.** `GROK_MEETING_SELECTORS` (or
  `$GROK_HOME/teams-selectors.json`) replaces the Teams DOM selector table from
  disk, so a Teams UI change is repairable without a Turbo release. Join
  failures name the step that broke (`name_input`, `join_button`, …).
- Docs: [`docs/MEETING_NOTETAKER.md`](./docs/MEETING_NOTETAKER.md).

### Security
- **Meeting Q&A is confined to read-only tools, enforced at dispatch.** A
  `Turbo:` question is untrusted text from participants who may be outside the
  organization, with spoofable display names. The pager tags the prompt id with
  `meeting-qa-`, the shell parses it into `PromptOrigin::MeetingQuestion`, and
  anything outside the allowed set is refused **before it runs** — no write,
  edit, shell, or subagent spawn, and no MCP, `workspace_tree` or `resolve_path`
  (MCP reaches off-box and its read-only hints are server-self-reported).
  Unreadable classification **fails closed**. Previously this was only requested
  in the prompt text.
- **Confinement follows the data, not the entry point.** `/meeting ask` with no
  arguments drains a participant-authored question, so it is tagged and confined
  exactly like the automatic path; calling `meeting_ask` with no question from an
  ordinary turn is refused. Only the notetaker's read/answer tools
  (`meeting_ask`, `meeting_reply`, `meeting_transcript`, `meeting_status`) are
  exempt from the gate — `meeting_join`, `meeting_stop`, `meeting_notes` and
  `meeting_knowledge` stay blocked, so a coworker cannot start another
  recording, end this one, or rewrite the recap.
- A refused tool is **non-terminal** (`ToolLoop::PolicyDenied`): the refusal is
  fed back as a tool result so Turbo still answers with what it may use, instead
  of the turn dying and the coworker getting silence.
- The bot's outbound audio track is silent by construction (zero-gain Web Audio
  node), not Chromium's fake-device beep, and `getUserMedia` never reaches the
  operator's real microphone.
- Verification challenges are **never answered**. A challenge, a signed-in-only
  meeting, or a denial ends the bot join and falls back to local capture.

### Changed
- `CaptureSource` gains `MeetingBot`. `meeting_status` reports notetaker state
  and PCM frame count, and no longer claims Q&A has "full tools".
- Every fallback to local capture states what is actually being recorded and
  that **no participant joins the meeting**, so nobody waits to admit a bot that
  was never dispatched (`fr_01a03036`).
- Sitting in the lobby past `GROK_MEETING_LOBBY_TIMEOUT` (default 300s) marks
  the notetaker failed and says so. Turbo does not silently switch to recording
  the operator's speakers instead.

### Fixed
- **Windows: clicking a file link no longer opens an Explorer window.**
  `open_path` ran `explorer.exe /select,<path>` on every activation — a file
  link, a `file://` URL, or the media `[Open]` button — and `/select` spawns a
  *new* Explorer window each time, so they accumulated across a working
  session. The file was never actually opened, despite the toast saying
  "Opening in default app…" (macOS and Linux did open it). Now `ShellExecuteW`
  launches the file in its default application: no shell, so percent-encoded
  session paths still survive intact, and no Explorer window. A file with no
  registered association falls back to revealing it rather than raising the
  "How do you want to open this file?" chooser, and a missing file still opens
  its parent folder. `reveal_in_file_manager` stays public for an explicit
  "show in folder" action.
- Live meeting audio is **shed, not queued**, when STT stalls — in the page (a
  WebSocket backlog ceiling) and in Turbo (`try_send`), matching local capture.
  Previously an STT reconnect applied backpressure all the way into the browser
  and buffered stale audio. `meeting_status` reports `notetaker_audio_dropped`.
- The in-page audio graph is built behind a memoized promise. A boolean latched
  before `audioWorklet.addModule` resolved let every track that arrived during
  startup — i.e. everyone who joined at the same moment — past a still-undefined
  mixer, silently dropping them for the rest of the meeting.
- Pre-existing `clippy::approx_constant` error in Game Mode's `RACK_ANCHOR`
  blocked `cargo clippy` on the whole pager crate.
- `schema/tool_meta.schema.json` was stale since rc.8 added `ToolKind::Meeting`,
  failing `tool_meta_schema_is_up_to_date`. Regenerated.
- Flaky `disk_cmd` tests: `plugin_worktrees_skips_live_and_reclaims_old` set
  `GROK_BUILD_WORKTREE_ROOT` with a raw `set_var`, racing
  `plugin_worktree_roots_are_config_only`, which asserts it is unset. Both (and
  the semicolon-split test) now share a `serial_test` key and use `EnvVarGuard`.

---

## [1.0.0-rc.8] - 2026-08-23

**Scheduled tasks, Meeting R2, Browser R3, shared GitHub logs.** rc.7 made the scheduler a control-plane primitive and closed the first meeting-join holes. rc.8 turns that into standing jobs an operator actually uses, takes `/meeting` and the Agent WebView through another Q&A + harden pass, and stops leaving incidents and feature requests stranded on one disk.

### Added
- **`/schedule`** standing jobs (interval, `at` datetime, optional weekday clock). No 7-day expiry. Recipes: `search`, `stat`, `meeting join`. Results under `{workspace}/Schedules/`. Index: `{workspace}/.grok/schedules.json`. Headless: `turbo schedule list|show|cancel` (works with Turbo closed; fires only while the pager is up). `/loop` still expires at 7 days.
- **`turbo issues sync` / `turbo features sync`** — opt-in GitHub Issues upsert/pull (`github_repo` in developer-log.toml / feature-request-log.toml). Local JSON remains write-ahead; default is no cloud upload. Default private repo name `danmsheets-dev/turbo-field-logs`.
- Workflows: `.grok/workflows/bug-sweep.rhai`, `perf-optimize.rhai`, `feature-planning.rhai`, `security-sweep.rhai`, `test-gap.rhai`.

### Fixed
- **Meeting R2:** pin `meeting_*` early on the live tool handshake; process exit marks capture stopped; stale disk recording is not live; join `?p=` redacted from meta/status; NL join accepts “meeting link to test with”; spoken `Turbo, …` auto-ask; status `graph: configured|missing`.
- **Meeting untrusted Q&A:** coworker/spoken `Turbo:` text cannot authorize writes; Graph chat lookup falls back when `?p=` is redacted.
- **Meeting-join shell:** `meeting_*` is `ToolKind::Meeting` (ReadWrite, not All), so scheduled joins no longer get bash. First scheduled meeting-join requires `confirm=true`; writes jailed to `Meetings/` + `Schedules/`.
- **Browser R3:** NavigationStarting fail-closed, eval confirm, pane mirror from last snapshot, download jail helper, `docs/BROWSER-R3-QA.md`.
- **C9 OAuth popups:** host-owned HWND + `SetNewWindow` so later hops still hit NavigationStarting/DownloadStarting (no `SetHandled(false)` policy skip).
- **C12 chrome-devtools:** MCP tools omitted unless `GROK_CHROME_MCP=1`; Agent WebView remains the default headed browser.
- **`/schedule` jail:** search/stat fires get ReadWrite + `allowed_paths=["Schedules/"]` (host jail, not prompt-only).

### Known
- Standing `/schedule` jobs do **not** fire if the pager process is quit (no Windows service).
- Teams `[Turbo]` chat posts still need `GROK_GRAPH_TOKEN`.
- chrome-devtools daily Chrome is opt-in: `GROK_CHROME_MCP=1`.
- Restart Turbo after installing rc.8; an older pager will not have `/schedule`, Meeting R2, or GitHub log sync.

---

## [1.0.0-rc.7] - 2026-08-23

**RC6 Phase 5 + Meeting Join Hardening.** Control-plane v1 (steer, receipts, policy, gh, secrets, scheduler) plus spawn-catalog identity, land isolation source, browser save jail, Windows userspace credential write-deny, and the first round of `/meeting` join hardening.

Two join entry points, one implementation (`meeting_join`): pager slash never leaks as a coding `<user_query>`; natural-language “join this meeting” + a Teams/Zoom/Meet/Webex URL calls `meeting_join` in that turn. Opening Teams with `Start-Process` is not the feature.

### Added
- **Meeting Join Hardening (round 1):** `meeting_*` on every grok-build primary toolset (default, workspace, concise, plan, plan-no-subagents, ask-user, orchestrator, hashline). `meeting_join` accepts `title` or alias `name`. NL join detector (`detect_join_request`) treats a bare Teams/Zoom/Meet/Webex URL as join, and longer text only with join/listen/notes intent.
- Phase 5 tools: `steer`, `receipts`, `rollback`, `gh_pr_status`, `gh_ci_status`, `gh_ci_rerun` on default/workspace/concise toolsets.
- Session policy `GROK_POLICY_DENY_PATHS` / `DENY_COMMANDS` / `MAX_DIFF_LINES`.
- Resource-aware spawn gates (disk + live-children) with named errors.

### Fixed
- **`/meeting` never PassThrough:** typed `/meeting join <url>` is handled by the pager builtin even when `meeting_join` is missing from the advertised tool handshake (or advertised as `GrokBuild:meeting_join`). It is not forwarded as a coding `<user_query>`.
- **Live schema vs `turbo tools list`:** `meeting_join` is injected on grok-build primary toolsets so the model’s available tools match `turbo tools list --require meeting_join`.
- **NL join:** “Join this meeting: https://teams.microsoft.com/meet/…” injects `meeting_join` immediately (WASAPI+mic capture). Do not ask the user to retype a slash command. Do not `Start-Process` the URL.
- **Slash tool gate suffix match:** `required_tools()` names match advertised `Namespace:short` ids.
- **Spawn identity (C1–C5):** exact catalog key wins over Codex/NVIDIA aliases; `openai/` aliases are not advertised when that key already exists; suffix match requires a `/` boundary.
- **Land (C6–C9):** land git root prefers `display_cwd` / `child_cwd`; live/snapshot land refuse absolute/`..` paths.
- **Worktree source (C7–C8):** spawn cwd and `GROK_SUBAGENT_REPO_ROOT` must sit under the parent workspace; unique nested git before ancestor walk.
- **MCP inherit (C10):** `isolation=worktree` coerces default `mcpInheritance=all` to `none`.
- **Windows `$GROK_HOME` credentials:** userspace write-deny at LocalFs + write/search_replace/apply_patch/bash/monitor (kernel sandbox remains advisory).
- **browser_save:** size cap on `file:` copy and streamed HTTP bodies; reserved-device filenames; downloads folder symlink/canonical jail.
- **browser_set_file:** refuse symlink sources; host SetFile fail-closes canonicalize.
- **developer-log:** `sanitize_incident` covers component/tags/environment/cwd_hash; `set_status` re-sanitizes; MCP health reasons run `redact_secrets`.
- **gh:** `CREATE_NO_WINDOW`; no `--no-browser` argv; `--` before user ids.

### Known
- Windows kernel sandbox is still advisory (userspace credential deny is the jail).
- Linux bwrap cannot block *create* of a missing `auth.json`.
- Meeting Join Hardening is in this binary. Restart Turbo after installing rc.7 before live Teams Q&A; an rc.6 process will still Start-Process without capture.

- **Windows grok-home credential write-deny:** `auth.json` / keys under `$GROK_HOME` are refused at LocalFs, write/search_replace/apply_patch, bash, and monitor even when kernel sandbox is advisory. Same matcher as Seatbelt/bwrap.
- **gh/git `CREATE_NO_WINDOW`:** Phase 5 `gh_*` tools and branch probe detach via `xai_tty_utils::detach_command` (no console flash on Windows).
- **steer:** 16 KiB cap is now enforced at runtime, not only in the schema text.
- **receipts:** undo payloads skip `.key` / grok-home credential suffixes, not just `.pem` / `.env`.
- **search_replace:** new-file path reads the target once (was twice).

Phase 5 control-plane follow-up (post-audit):

- **Tools on stock toolsets:** `steer`, `receipts`, `rollback`, `gh_pr_status`, `gh_ci_status`, `gh_ci_rerun` are on default / workspace / concise grok-build toolsets (the running rc.5 binary still will not show them until this tree is built).
- **Policy fail-closed:** `max_diff_lines` is checked before `write_file`; `write`, `apply_patch`, and `monitor` honor `GROK_POLICY_*`.
- **Receipts:** undo payload is written first; secret-shaped / credential-path files are not stored as raw `.before`.
- **gh CLI:** dropped illegal `--no-browser` (use `GH_NO_BROWSER`/`CI` env); `kill_on_drop`; `--` before user ids; `gh_ci_status` filters `--branch` and fetches `jobs`.
- **Scheduler:** live-marker heartbeat stops on every completion; `retain_worktree` trees do not fill the live-children cap; resume/create serialize admission.

---

## [1.0.0-rc.6] - 2026-08-22

**Providers + isolation + browser policy.** rc.5 field tests found `/providers` ACP
dispatch holes, worktree keep-N/land fail-open, sandbox credential writes, and
NVIDIA catalog 400/410/hangs. This release lands those plus Poolside hosted Chat
API as a first-class `/providers` platform.

### Added
- **Poolside** (`/providers poolside <api_key>`). OpenAI-compatible Chat Completions
  at `https://inference.poolside.ai/v1`. Env: `POOLSIDE_API_KEY` /
  `GROK_POOLSIDE_API_KEY`. Offline catalog: Laguna S 2.1 (1M), XS 2.1 (256K),
  M.1 (256K). Thinking via `chat_template_kwargs.enable_thinking`; assistant
  `reasoning_content` is preserved on tool loops. Wire ids are
  `poolside/laguna-s-2.1` (catalog key `poolside/laguna-s-2.1`).
- **`turbo issues resolve --sha` / `turbo features ship --sha`** record a proving
  commit on the incident/request.
- **NVIDIA `agent_ready` spawn gate** — write-capable children cannot pin
  chat-only Integrate rows; glm-5.2 is catalog EOL (410). Extra `model_id` is
  stripped on NVIDIA Chat Completions.

### Fixed
- **`/providers` ACP** dispatches `x.ai/internal/set_platform_api_key` (OpenRouter
  and every other BYOK platform). rc.5 TUI saved keys; the agent match did not.
- **keep-N** never deletes `retain_worktree` or live-PID trees; land fail-closes
  on missing v1 `allowed_paths`; ReadOnly capability_mode clamps MCP and
  verbatim-fork tools.
- **Sandbox** write-denies `auth.json` / credential files under `~/.grok` on
  confining profiles (Windows enforcement remains advisory).
- **Agent WebView:** `browser_save` re-checks redirect policy; zip/pdf navigations
  broker into session downloads; PDF empty DOM falls back to AX; `wait_ms`
  clamped to 60s; HTML saves as `.html` not `download.bin`.
- **Land CLI** finds `--session` across cwd hashes and uses a nested git root
  when the umbrella workspace is not a repo.

### Known
- Windows sandbox credential write-deny is profile metadata only (no kernel jail).
- Named/required `tool_choice` is not supported on Poolside while thinking is on
  (Poolside API); Turbo uses auto tool_choice.

---

## [1.0.0-rc.5] - 2026-08-21

**Harness Q&A follow-through.** Isolated-worktree and Agent WebView field tests
on rc.4 produced a concrete list of spawn-catalog, download, log-redaction, and
timeout-finalize bugs. This release lands those product fixes.

### Added
- **`browser_downloads` on the live toolset** — the session inject list was
  missing the listing tool even though the host already brokered files.
- **`browser_save`** — save the current page URL (or an explicit https URL)
  into the session `downloads/` folder with sanitized names, size limits, and
  `browser_downloads` listing. Public PDF/guide save no longer depends on
  Chromium raising `DownloadStarting`.
- **`browser_downloads wait_ms`** — optional wait until a completed brokered
  file appears (JS download interstitials).

### Fixed
- **`openai/gpt-5.5` (and other `openai/gpt-5.*`) spawn slugs** alias to
  `openai-codex/…` the same way Luna/Sol already did. Spawn descriptions list
  only session-spawnable catalog keys plus those aliases.
- **Child boot card `Model:`** uses the child's resolved sampling model, not
  the parent `ModelsManager` current id.
- **Worktree DisplayCwd** remaps onto the **source git repo** (nested
  `turbo-grok-build` under an umbrella workspace), not the umbrella root.
- **HTTP error pages** (404 HTML) are a successful `browser_navigate`;
  download-shaped navigations that fire `DownloadStarting` are not reported as
  `-32000 navigation failed`.
- **`browser_set_file`** copies workspace/confine files into a session
  `uploads/` broker before the host path check (host remains session-folder
  only).
- **`mcp_server_health`** documents `failed` / overall `unknown`, and
  remediation names `turbo mcp doctor` (with `grok mcp doctor` as alias).
- **developer_log `snapshot_ref` redaction** and **feature_request evidence**
  sanitization.
- **Subagent turn-result `schema_version`** uses `GCS_SCHEMA_VERSION` (`v1.24`).
- **Explicit `timeout_ms` finalize grace** defaults to `timeout/6` clamped
  30–120s so children get a stop-and-summarize window before hard cancel.
- **`turbo tools list`** injects the same Windows `browser_*` set a live
  session gets (including `browser_downloads` / `browser_save`).
- **Windows release CI** — drop `/DEBUG:LongSymbolTruncate` (GitHub
  windows-2022 `link.exe` 14.44 rejects it with LNK1117). Keep
  `/DEBUG:FASTLINK` for PDB limits. Release.yml no longer blocks publishing
  on `cargo test --workspace`.

---

## [1.0.0-rc.4] - 2026-08-21

**Fathom-style meeting notetaker.** Join a Zoom/Teams/Meet/Webex link, transcribe all participants on Windows, auto-answer coworker `Turbo:` questions from the launch workspace, and save a work-only recap.

### Added
- **Fathom-style meeting notetaker** — `/meeting join <url> [name]` opens a Zoom/Teams/Meet/Webex **https** join URL. On Windows, captures **system playback (all participants) mixed with the mic** via WASAPI loopback, transcribed with Grok STT (falls back to mic if loopback cannot open, unless `GROK_MEETING_CAPTURE=loopback`). `/meeting stop` writes a **work-only** summary to `{workspace}/Meetings/YYYY-MM-DD - <Meeting Name>.md`. Set `GROK_MEETING_CAPTURE=mic` to force microphone-only.
- **Meeting Q&A** — Coworkers type or say `Turbo: …`. Turbo **auto-injects** a research turn (workspace files, MCP, web) and `meeting_reply` posts `[Turbo] …` to Teams when `GROK_GRAPH_TOKEN` is set. Coworker text is treated as untrusted data (not extra system instructions). Set `GROK_MEETING_AUTO_ASK=0` to queue only. `/meeting ask` still drains manually.
- **Meeting recap highlights** — `/meeting stop` summary includes **For you** (asks/actions for the operator) and **Projects** (matched to the launch workspace).

### Fixed
- Join URLs are parsed as real `https` URLs (host-based platform classify, no `cmd` metacharacters). Windows opens via `explorer.exe` instead of `cmd /c start`. `GROK_TEST_OPEN_URL_FILE` is test-only.
- `GROK_MEETING_CAPTURE=loopback` no longer falls back to the microphone when WASAPI mix fails.
- Recap writes stay inside `{workspace}/Meetings` (no `..` reuse, no symlinked Meetings folder). Meeting ids reject path separators.
- Graph `$filter` percent-encodes `JoinWebUrl` and matches the returned meeting; chat ids are path-encoded; Graph error bodies are truncated.
- WASAPI mix-format / event HANDLE leaks on loopback open failure; `CoUninitialize` only after a successful `CoInitializeEx`.
- Oracle execution-budget test matched the 24-turn / 48-tool definition (was still expecting 12/40).
- Nested-subagent depth test hung: default max depth is 2, so a depth-1 spawn waited forever on the mock backend.

---

## [1.0.0-rc.3] - 2026-08-19

**Agent WebView field-test pass.** rc.2.1 made the window usable. This release
is the evening QA round: new page-control tools, click/fill/eval policy that
matches job-hunt and 1:1 outreach, OAuth popups that do not hijack the only
tab, session-scoped profiles, and a compaction retry that recognizes more
context-window overflow wordings.

### Added
- **Brokered Agent WebView downloads** — page-initiated downloads are redirected into the session-scoped `downloads/` folder with sanitized collision-safe names; `browser_downloads` reports completed files without exposing arbitrary destination writes.
- **NVIDIA Integrate models** — Muse Glimmer (`meta/muse-glimmer-30b`, 131K),
  Poolside Laguna XS 2.1 (`poolside/laguna-xs-2.1`, 256K), and
  Mistral-Nemotron (`mistralai/mistral-nemotron`, 128K) via
  `https://integrate.api.nvidia.com/v1` / `$NVIDIA_API_KEY`. Catalog keys
  `nvidia/meta/…`, `nvidia/poolside/…`, `nvidia/mistralai/…` plus short
  `nvidia/<id>` aliases; same `request_compat` as other NIM chat models.
- **`browser_wait`** — poll until `text` is on the page or the URL contains
  `url_substring`. Timeout names what was waited for.
- **`browser_scroll` / `browser_press_key` / `browser_select` / `browser_hover` /
  `browser_set_file`** — below-the-fold Apply, combobox Enter, filters, hover
  menus, and workspace / session-folder file inputs (resume PDFs). Downloads
  stay blocked.
- **`browser_raise`** — bring the hidden window back for human login.
- **`browser_snapshot include_text`** — truncated main/article text so LinkedIn
  Experience and Indeed job bodies survive the 200-node cap.
- **Session-scoped WebView profile** — default
  `$GROK_HOME/agent-browser/sessions/<id>`. `GROK_BROWSER_PROFILE=durable` opts
  into a shared job-hunt profile.
- **LinkedIn 1:1 playbook** in the `agent-browser` skill (Connect vs Message,
  200-char notes, Pending stop, close the messaging overlay first).

### Fixed
- **Umbrella isolation source repo** — `isolation=worktree` plus `cwd` selects
  the source git checkout (child still runs in a new worktree). Unique nested
  git discovery and `GROK_SUBAGENT_REPO_ROOT` remain as fallbacks. Multiple
  nested git dirs without an explicit `cwd` still fail-closed.
- **Nested spawn default `max_depth=2`** — first-level children can spawn one
  grandchild. Operators can still set `GROK_SUBAGENTS_MAX_DEPTH=1`. Child boot
  card says when spawn is stripped at the ceiling.
- **Sol Medium spawn aliases** — `openai/gpt-5.6-sol` (and `-pro`, short
  `gpt-5.6-sol`) resolve to `openai-codex/gpt-5.6-sol` like Luna.
- **Explore/oracle wall-clock** — explore 5 min / 24 turns; oracle 10 min /
  24 turns so read-only children finish instead of 120–180s stalls.
- **Spawn start honesty** — background spawn result includes
  `isolation_requested` and the DisplayCwd-remap note.
- **Worktree `cargo` under confine** — `cargo` / `rustc` / `rustfmt` / `rustup`
  run|show|which` are modelled. Implicit `target/` plus `--target-dir` /
  `--root` are confine operands so `cargo test -p …` works and `cargo install`
  to `~/.cargo` still fail-closes.
- **CMake/MSBuild FileTracker FTK1011** — worktree children pin
  `CARGO_TARGET_DIR` to the real `{worktree}/target` instead of inheriting or
  remapping onto the parent `H:\…\target`.
- **Detached HEAD after worktree commit** — `git worktree add` uses
  `-B grok/<dest-basename>` so commits stay on a named child branch.
- **NVIDIA child wall-clock** — default 10 min → 1 hour; stall 10 min → 30 min.
  Explicit `timeout_ms` is not capped at 30 minutes.
- **Reviewer stall** — agents whose name contains `review` default to 48 tool
  calls and a 10 min budget unless the definition overrides them.
- **`review-current-branch`** — no longer hardcodes `origin/dev`. Scope uses
  `capability_mode=execute` so git works; empty baseline resolves
  `@{upstream}` then `HEAD`.
- **`allowed_paths` refusals** name the prefix to re-spawn with. Root
  `.gitattributes` / `.gitignore` are writable and landable even when the
  allowlist is crate-scoped.
- **`location.href` reads no longer need `confirm=true`**. Assignment / replace /
  assign / `el['click']` / `document.write` / `.src=` still do. The host
  re-checks mutating eval.
- **Denied `browser_navigate` no longer drops the snapshot**, so the next click
  is not "call browser_snapshot first".
- **Click reports a cancelled navigation** instead of success-on-the-old-page.
  Result includes the post-click URL.
- **Overlapping writes are refused** (`browser_busy`) instead of last-write-wins
  on the single tab.
- **Click confirm** gates Apply / Connect / Follow / Invite / Message; **Sign in**
  does not (the human types secrets in the window).
- **Snapshots** pick up `role=status|option|listbox|dialog`, `<label for>`,
  overlay Close, and keep Experience/About headings under the cap. Same-origin
  iframes are walked.
- **contenteditable / Lexical fill** uses `insertText` + composed InputEvents
  (and a paste-shaped fallback) so LinkedIn Send enables.
- **Google/Microsoft OAuth popups** are real windows. GSI is no longer
  Navigated into the only tab.
- **Iframe navigations** go through the same URL policy as the top-level frame.
- **Pipe binds before WebView2 init**; ensure wait is 45s. The host is enrolled
  in a Job Object / process scope so leftover `msedgewebview2.exe` dies with
  the pager.
- **Uid schema examples** are epoch-index (`4-17`), not positional `"2"`.
- **Boot card** documents close-hides. **chrome-mcp** skill now says: use
  `browser_*` unless the user asked for daily Chrome.
- **Compaction** treats more overflow phrasings (`context window`, `token
  limit`, `too many tokens`, `input too long`) as deterministic context-length
  errors so the input ladder can step down instead of failing closed.
  `should_compact_on_error` now recovers from CLE text even when stream
  metadata is missing or the token estimate sits under the advertised window.
  StreamError overflow is no longer retried as a blip. Checkpoint is written
  after the forked history is resolved, and the jsonl marker is not written if
  the checkpoint file cannot be queued. Truncated unclosed `<summary>` blocks
  are degenerate and retried.

### Policy
- Agent WebView is HITL assist, not a bot. Skill forbids bulk Indeed apply and
  LinkedIn connect/message loops.

### Changed
- Wire version **`1.0.0-rc.3`**.

## [1.0.0-rc.2.1] - 2026-08-19

**Agent WebView hotfix.** rc.2 shipped the Agent WebView with a defect that made
it unusable: the window opened and stayed white. Everything here is that bug and
the field report that followed it.

### Fixed
- **`browser_*` calls hung forever (release blocker)** — the named-pipe server
  created `SPARE_INSTANCES + 1` instances but called `connect()` on exactly one.
  Windows hands an incoming client to *any* listening instance, so a client that
  landed on one of the four nobody awaited was accepted and then blocked forever.
  A fresh `browser_navigate` had a **four-in-five chance of never returning**, which
  is why the window opened and stayed blank. Reproduced by driving the pipe
  directly: four hangs, one reply, then pool exhaustion. The server now runs one
  acceptor task per instance, each awaiting its own `connect()` and re-arming after
  it serves. Regression test included — verified to fail against the old shape.
- **A wedged host produced no reply at all** — the pipe side awaited the UI
  thread's response with no deadline, so a request the UI thread never reached
  left the connection open with nothing written on it and the caller saw a bare
  75s transport timeout. There is now a middle rung between `NAV_TIMEOUT` (60s)
  and the client's `CALL_TIMEOUT` (75s) that answers with a real JSON-RPC error on
  the caller's own id.
- **First paint was an empty white rectangle**, which reads as a crash. The host
  now paints a card naming itself, the profile path, and what it is waiting for.
  Written into the blank document rather than via `NavigateToString` (which the
  navigation policy would cancel as a `data:` URI), and `aria-hidden` so neither
  snapshot path mistakes it for page content.
- **Every window was titled `Turbo Agent Browser`**, so a leftover host was
  indistinguishable from the live one and could sit on top of it. The caption is
  now `Turbo Agent Browser — <host> [<session>]`, set before the load starts and
  again when it settles. Host extraction rejects userinfo spoofing
  (`https://bank.test@evil/`), treats a backslash as an authority terminator per
  WHATWG, and strips control characters.
- **The close button killed the host with no telemetry** — one stray click looked
  exactly like a crash. `X` now hides the window and marks the WebView2 controller
  invisible; the host keeps serving and any `browser_*` call re-shows it. Host exit
  and close-to-hide both log to stderr, which the shell already drains into tracing.
- **`file:` URLs were refused by the host** — the spawn never passed
  `--session-folder`, so the host had nothing to measure them against while the
  client-side policy allowed them (audit finding C1).
- **The Agent Boot Card omitted the browser** even with `browser_*` registered, so
  the agent had no idea the window existed and went looking for a command to open
  it. There is none — the window appears on the first tool call. The card now
  carries a gated launch line (guaranteed by test); the click/fill/uid loop stays
  in the `agent-browser` skill.
- **The boot card reported the wrong version** (`1.0.0-rc.1` while `turbo --version`
  said `1.0.0-rc.2`). Three crates carried independent version strings and
  `xai-grok-version` was never bumped. Both build scripts now resolve
  `GROK_VERSION` → workspace `VERSION` file → crate version, so the `VERSION` file
  is the single source of truth, with a test asserting they agree.

## [1.0.0-rc.2] - 2026-08-19

**Agent WebView, Grok 4.6, and a confinement hardening pass.** A week of RC2
work folded into the release: a product-owned WebView2 browser the agent drives
directly, the Grok 4.6 default catalog, MCP disk-wins merge, full Disk Clean,
and remediation of two P0 findings from the RC2 audit
([`docs/RC2_UNRELEASED_AUDIT.md`](./docs/RC2_UNRELEASED_AUDIT.md)).

### Added
- **Grok 4.6 default catalog** — compiled-in `default` / `web_search` / `image_description` / `session_summary` now `grok-4.6` (advertises `xhigh` reasoning effort; hosted backend search on). `grok-4.5` remains selectable. Config / env / CLI overrides still win. Resolved-checkpoint display (`requested (resolved)`) now treats all `grok-4.*` slugs as coding models, not only `grok-4.5`.
- **Prompt `work_policy` + `browser_verification`** — official completion discipline merged into the primary and subagent templates without dropping Turbo `<action_safety>`. The `<browser_verification>` block renders only when the finalized toolset includes `browser_*` tools (not plan-builtin-only). AGENTS.md user reminders are not suppressed.
- **Agent WebView** (`browser_*` tools + `turbo browser-host`) — product-owned WebView2 window on `~/.grok/agent-browser`. Ctrl+Shift+B mirrors URL/snapshot in the TUI. Not chrome-devtools MCP.

### Security
- **`--confine` write-boundary escape (P0)** — `cmd /c <engine>` handed the whole
  compound command to the Windows recovery path and returned early, so sibling
  invocations were never classified: `cmd /c blender -b ; powershell -c "Set-Content
  C:\outside\x 1"` was allowed. The escape survived a first fix that counted bash
  command nodes, because `cmd /c "A & B"` is a single node. The recovery now fails
  closed on any newline, on any statement separator (`&`, `&&`, `|`, `||`, `;`, `^`)
  at every recursion depth, and on late-expanded tokens (`%VAR%`, `$env:`, `$(…)`,
  backticks, `~`) whose target cannot be range-checked. Also closes the glued
  redirect form `>C:\path`, which resolved as a *relative* path under the root.
- **Snapshot uid forgery** — `data-turbo-uid` is page-writable and `elByUid` resolved
  it with `querySelector`, so a page could stamp a live uid onto a control of its
  choosing and capture the next `browser_click` / `browser_fill`. Resolution now runs
  through a registry held in the CDP isolated world. This also fixes uid lookup for
  nested shadow roots, which the old document-level query only reached one level deep.
- **`turbo disk clean` deleting non-product data** — `plugin-worktrees` hardcoded
  `H:\gb` / `H:\gb-work` and removed *every* child directory. Roots are now
  configuration-only (`GROK_BUILD_WORKTREE_ROOT` / `GROK_PLUGIN_WORKTREE_ROOT`, split
  on `;` only, since a comma is a legal path character), and a child must carry a
  product-shaped name and not be an ordinary clone. Separately, the shared temp-root
  filter no longer matches the generic `tmp.*` / `.tmp*` / `uid-*` / `kg-*` spellings
  that `mktemp -d` and `tempfile` produce for unrelated applications — reachable from
  a bare `turbo disk clean --safe`.

### Fixed
- **`disk clean --safe --if-low-space` was a no-op on a healthy disk** — the temp-grok
  always-sweep retained `TempGrok`, then fell into a volume filter whose low-volume set
  was empty and which stripped it back out, reporting `ok: false`. Subagent dispose runs
  exactly this command.
- **Windows path separators** — `get_worktree_info` returned libgit2's forward slashes,
  silently defeating the `collapse_home_path` prefix match; and `path_not_found_hint`
  joined a POSIX display path with the host separator, handing the model
  `/home/user/project\src`.

### Harness polish (first RC2 drop, 2026-08-11)

Identity, permission-rule compatibility, and job-object ergonomics so delegates
stop dying at launch against wrong `turbo` binaries or legacy deny prefixes. No interactive TUI redesign —
focus is identity, permission-rule compatibility, and job-object ergonomics so
delegates stop dying at launch against wrong `turbo` binaries or legacy deny
prefixes.

### Added
- **`turbo version --json` identity card** — besides `currentVersion` /
  `channel`, now emits:
  - `product` (`turbo-grok-build` | `grok-build`)
  - `binary` (`turbo` | `grok`)
  - `cliFamily: "grok-build"`
  - `agentCompatible: true`
  - `features` (`headless`, `confine`, `jsonSchema`, `jobObject`, …)
  - `permissionToolPrefixes` (stable list for harness filters)
  Harnesses can distinguish Turbo Grok Build from Vercel Turborepo without
  scraping `--help`.
- **`GROK_JOB_OBJECT=1`** env alias (alongside `TURBO_JOB_OBJECT` /
  `HYPER_JOB_OBJECT`) for Windows Job Object opt-in.
- **`turbo subagent land --json-union-by=<key>`** — union-merge landed JSON
  arrays of objects by that key (child wins). `assets/manifest/*.json` always
  merges by `name` so parallel densify lands keep sibling rows. Fail closed
  if a targeted file is not an array of objects or an object map.
- **`land_subagent.json_union_by`** — same merge on the tool path.
- **Imagine-web skill** — `bundled/skills/imagine-web` drives grok.com Imagine
  through chrome-devtools MCP (snapshot → fill → save). Pair with a user MCP pin
  (`turbo mcp add chrome-devtools`) using `~/.grok/browser-profile` and
  `--allow-unrestricted-paths`. Login is human-in-the-loop; not the Imagine API.
- **`turbo mcp restart <name>`** — disable then enable so a live session
  re-merges from disk.
- **`/chrome-mcp` skill** — chrome-devtools loop, `--autoConnect` daily Chrome,
  Cloudflare SSO warning, draft-not-send on social sites.
- **MCP merge: disk beats session snapshot** — TUI `session/new` injects
  `load_mcp_servers()` as the client list; a client `insert` froze
  chrome-devtools argv across `config.toml` edits. Client extras still
  survive; TOML/plugin args win on the same name.
- **Nemotron 3.5 Lightning** — spawnable catalog slugs
  `nvidia/nvidia/nemotron-3.5-lightning-30b-a3b` and
  `nvidia/nemotron-3.5-lightning-30b-a3b` (NVIDIA Integrate, 1M ctx,
  same request_compat as Ultra/Super).
- **Luna slug aliases** — `openai/gpt-5.6-luna` (and `-pro`) resolve to
  `openai-codex/gpt-5.6-luna` so keep-N / densify spawn does not 400.
- **`turbo disk` nested `target/` + plugin worktrees** — report/clean walk
  child `Cargo.toml` / `CACHEDIR.TAG` trees, and `--include plugin-worktrees`
  scans `GROK_BUILD_WORKTREE_ROOT` plus `H:\gb` / `H:\gb-work`. Live markers
  and recent unlanded dirs are skipped.
- **Keep-N land artifacts** — `meta.json` + `changes.patch` copied to
  `~/.grok/subagent-artifacts/<id>/`. `land_subagent` / `diff_subagent` fall
  back to that store, then `refs/grok/subagents/<id>`, when session meta is
  gone.

### Changed
- **Claude-compat deny prefixes** — `NotebookEdit` / `MultiEdit` → Edit,
  `NotebookRead` → Read. Older Grok Build plugins that still emit
  `--deny NotebookEdit(**)` no longer hard-abort headless starts with
  `unsupported tool prefix`. `EnterWorktree` remains unsupported.
- Wire version **`1.0.0-rc.2`**.

### Fixed (RC2 harness closeout)
- **Absolute worktree writes remapped to parent (P0)** — isolated
  `…/.grok/worktrees/…/subagent-…` paths are no longer DisplayCwd-folded
  onto the shared checkout.
- **Subagent 0-tool stalls** — first-progress timeout 60s after spawn
  (worktree setup excluded); `allowed_paths` stall 3 min so scoped
  children finish and the parent can land.
- **godot-docs-mcp pipe closed** — docker stdio wait + one respawn;
  handshake error names the Windows 232 / pin workaround.
- **Windows densify confine** — PowerShell `& "C:\\Program Files\\…\\blender.exe"`
  and `cmd /c` wrapping the same is now modelled (script/export operands only).
  Random Program Files exes stay fail-closed `shell-unparseable`.
  Session-configured `GROK_BLENDER` / `GROK_GODOT` paths (even renamed
  basenames) are also modelled.
- **Land + `allowed_paths`** — `.grok-subagent-live` and `.grok/` are harness
  markers, not payload. Land no longer refuse-closes on them and does not
  copy them into the parent.
- **`turbo disk temp-grok`** — report and `--safe` clean now include aged
  TEMP-root leftovers (`grok-*`, `nest-*`, `goal-*`, `kg_*`, empty `.tmp*`,
  …) not just `%TEMP%/grok`. Official `grok/sessions` still age-prunes with
  a fail-closed newest-mtime scan. Post-subagent `--if-low-space` still
  sweeps `temp-grok` when space is OK.
- **`turbo subagent list`** — default is the newest session for this cwd;
  discarded+cleaned leftovers are hidden; `running` + cleaned is shown as
  `stale`. Pass `--all` for the old dump.
- **Worktree seed HEAD race** — reset prefers the source commit SHA and
  retries once on `Could not parse object 'HEAD'`.

### Harness notes
- Prefer `turbo version --json` → `agentCompatible` / `cliFamily` for CLI
  selection on PATH.
- Isolated Windows delegates: pass `--job-object` or set `TURBO_JOB_OBJECT=1`
  so stop can tear down the whole process tree via the job handle.
- Confined blender-artist / Godot workers may invoke the installed engine
  binary even when it lives under Program Files; quote the path.
- Plugin / cargo tests should nest temps under `%TEMP%/grok/{plugin-tests,tests}`
  so `turbo disk clean --safe --include temp-grok` can reclaim leaks.

---

## [1.0.0-rc.1] - 2026-08-09

**Turbo Grok Build 1.0 RC1.** Merges official **xAI Grok Build 1.0.0**
(`75e73f3d6`, monorepo `SOURCE_REV` `a61c32b12…`) as the shared core while
preserving Turbo’s product overlay (`turbo` binary, disk clean, Game Mode,
workflows/deep-audit, ADL/FRL, isolation/land, English-only, multi-provider).

### Upstream core (adopted)
- Product ladder **0.2.120 → 0.2.121 → 1.0.0** (dashboard turn summaries,
  extensions modal, `/feedback` report box, SSH/tmux auto theme, table reflow,
  permission full-script + Ctrl-F, MCP image fixes, queue/cancel hardening,
  remote resume defaults, session fork memory, and related stability work).
- Rust toolchain pin **1.94.0**.
- Auth `bearer_fragment`, computer-hub connection work, tool-types task model
  updates, workspace restore hang fixes.

### Preserved (Turbo product layer)
- CLI **`turbo`**, `community-build`, install/update under `~/.turbo`.
- Disk clean / multi-path free-space gates / `turbo disk|issues|features|tree|tools`.
- Game Mode, workflows + stock deep-audit/deep-research/continuous-improve,
  Auto Developer Log + Feature Request Log, workspace tree inject, boot card.
- Headless **streaming-json schemaVersion 2** (subagent lifecycle, confine,
  tool_denied) — upstream reducer subtree not reintroduced.
- Windows correctness: line-ending CI, process-tree, `RUST_MIN_STACK`, CRT notes.

### Changed
- Wire version **`1.0.0-rc.1`** (semver pre-release on the 1.0.0 core).
- Tracking branch for this sync: `sync/1.0.0-rc1` (merge base `e5478eff1`).

### Fixed (RC1 hardening pass)
- **`turbo tree prune --execute`** — dry-run by default (parity with
  `subagent prune` / `disk prune`); pass `--execute` to delete.
- **Windows bare `bash` / `*.sh`** — agent terminals route through Git Bash when
  installed (never WSL `System32\bash.exe`); PowerShell remains default for
  native `/flag` toolchains. Opt out: `GROK_PREFER_GIT_BASH_FOR_SCRIPTS=0`.
- **Land allowlist case-fold on Windows** — `assets/Mini Games` matches
  allowlist `assets/mini games/` (NTFS); write-time and land share the rule.
- **Worktree seed honesty** — `<worktree_seed>clean|dirty</worktree_seed>` on
  completion; clean seed documents missing parent WIP.
- **Isolation CWD assert** — isolation=worktree without a real subagent worktree
  path fails closed (unless `GROK_SUBAGENT_ALLOW_SHARED_FALLBACK=1`).

### Added (RC1 hardening pass)
- **`turbo disk recover --safe`** — closed-loop check → clean `--if-low-space` →
  re-check; exit 1 if still under the free-space gate.
- **Post-subagent disk clean** — after dispose, best-effort
  `disk clean --safe --if-low-space` (5‑minute debounce). Disable with
  `GROK_POST_SUBAGENT_DISK_CLEAN=off`.

### Notes
- Full package-scoped compile/test campaign is part of this RC; treat as
  integration candidate until Round A–D gates are green on Windows.

---

## [0.2.119-r3] - 2026-08-07

**Turbo Grok Build RC3.** Full **Disk Clean** feature for post-agent Rust/cache
reclaim — category taxonomy, multi-path free-space gates, unified prune, and
JSON reclaim telemetry. Default `--safe` behavior remains RC2-compatible.

### Added
- **`turbo disk clean --safe --include <cats>`** — opt-in reclaim categories:
  `debug`, `debug-pdbs`, `debug-incremental`, `release`, `release-dist-caches`,
  `worktrees`, `tree-store`, `temp-grok`, `cargo-home` (requires
  `--i-accept-redownload`). Omitted `--include` keeps the RC2 default set
  (`debug` + aged worktrees + tree store).
- **`turbo disk clean --json`** — machine-readable `reclaimed_bytes` by category
  and `total_reclaimed_bytes` (agents / continuous-improve).
- **`release-dist-caches`** — removes only
  `incremental` / `deps` / `build` / `examples` / `.fingerprint` under the
  resolved target root’s `release-dist`; **keeps** ship binaries (`turbo.exe`).
  Safety is Cargo’s profile `.cargo-lock` (exclusive lock held through deletes);
  existence of the lock file alone is not treated as “active.” Optional mtime
  hints use `--active-build-grace-secs` (default 120).
- **`CARGO_TARGET_DIR`** — report/clean sizes and reclaim use this path when set
  (not only `<workspace>/target`).
- **`turbo disk prune`** — unified `--worktrees` / `--tree-store` /
  `--session-meta` / `--all` with dry-run default and `--execute`.
- **Multi-path free-space report/check** — gates workspace root, worktrees base,
  `CARGO_TARGET_DIR` (when set), and `GROK_HOME` (when set); fail closed with
  path-labeled remediation.
- **Report category breakdown** — debug PDBs, debug incremental, release-dist
  caches, cargo-home registry/git cache sizes.

### Safety (unchanged defaults)
- Clean still requires `--safe`; cargo-home requires a second consent flag.
- Fresh live worktrees (`.grok-subagent-live` within
  `GROK_SUBAGENT_LIVE_MARKER_MAX_SECS`, default 12h) are protected; **stale**
  markers are reclaimable (parity with spawn soft-preserve).
- Ship profile binaries are never deleted by default safe clean.
- Multi-path free-space also gates **cargo-home** and **TEMP** volumes.
- `--if-low-space` only reclaims categories on **failing volumes** (no wiping
  H:`target/debug` when only C: worktrees are low); gate disabled
  (`GROK_MIN_FREE_GB=0`) no longer forces a full clean.
- Profile locks for `debug` / `release` / `release-dist-caches` refuse reclaim
  while cargo holds the profile lock.
- Session-meta prune skips active statuses (`running`/`starting`/…).
- JSON clean results include `free_bytes_before`/`after`, stable `skipped_reason`,
  and report emits `suggested_clean` ranked by size for agents.

---

## [0.2.119-r2] - 2026-08-05

**Turbo Grok Build RC2.** Second RC on the `0.2.119` wire line. Headline is a
full Game Mode overhaul — every confirmed bug and performance finding from the
[RC2 Game Mode audit](./docs/RC2_GAME_MODE_AUDIT.md) fixed, two new hover
tooltips, and eleven new sprite animations. Game Mode test coverage went 25 →
132.

### Added
- **Game Mode — Supervisor hover tooltip:** model, phase, turn elapsed, context
  window used/total, seat + overflow counts, wall state, git branch.
- **Game Mode — MCP server rack:** the rack sprite is now composed into the
  office, with a hover tooltip listing each server's status, tool count and
  failure detail. Backed by a new per-agent MCP status cache so status is live
  outside the `/mcps` modal (pushes were previously dropped when it closed).
- **Game Mode — eleven sprite animations:** debug-rage fail pose with a red
  error monitor, arms-up celebrate with confetti, papers flying during handoff,
  monitor glow + compile flash, swinging door on spawn/exit, MCP rack LED bursts
  keyed to real tool calls, coffee-sip idle (which revives the previously dead
  idle steam and thinking-bubble blink), real day/night wall clock with hour
  tint, office-wide success wave on WORK FINISHED, typing cadence driven by
  token throughput, and a floor robot that patrols only while the office is busy.
- `turbo tools list [--json] [--require NAME]` — headless schema assert for registered model-facing tools (respects `GROK_SUBAGENTS` / `[subagents] enabled`).
- `turbo disk check [--min-free-gb N]` — fail closed under free-space gate (default `GROK_MIN_FREE_GB=40`).
- `turbo disk clean --safe --if-low-space` — reclaim only when under the free-space gate.
- Disk report surfaces **min free** threshold status and **keep-N** vs live `subagent-*` count.

### Fixed
- **Game Mode tier off-by-one:** the tick path re-peeled the status strip, so at
  a 19-row paint area every tick snap-cleared walks, celebrates and handoffs
  while the office still painted them — those animations were impossible at that
  terminal height. Tick and paint tiers now match by construction.
- **Game Mode animation cadence:** the 90 ms animation gate sat above the 83 ms
  Slow tick interval and dropped every other tick, so the office ran at ~6 Hz
  and the wall clock at half real time. The gate is now derived from
  `SLOW_TICK_INTERVAL` so the two cannot drift apart.
- **Exit walk:** after a handoff the developer teleported backward and walked
  into the supervisor again instead of leaving. It now exits through the door.
- Nine further Game Mode correctness bugs: hover popup clipping, failure-status
  vocabulary drift against the dashboard classifier, stale tool-call counts on
  finished agents, stale overflow badge, Unicode art alignment, token unit
  rounding at boundaries, and a dead supervisor-phase write.

### Changed
- **Game Mode idle cost:** an open office no longer pins the event loop at
  ~12 Hz. A frozen room parks (Compact/Unicode at zero wakeups; the pixel office
  falls through to a budgeted ~0.33 Hz ambient tick that drives the idle
  animations). Per-tick snapshot rebuilds over the insert-only subagent map are
  change-gated, image caches are released when the view closes (~8-10 MB that
  previously leaked for the process lifetime), and paint buffers are reused
  in place instead of reallocated per animation step.
- Soft-preserve keep-N default **3** (`GROK_SUBAGENT_KEEP_N`; alias `GROK_SUBAGENT_SOFT_PRESERVE_KEEP_N`). **`0` = age-only** prune (`GROK_SUBAGENT_KEEP_MAX_AGE_SECS`, default 24h).
- Pre-spawn free-space default **40 GiB** (`GROK_MIN_FREE_GB`; alias `GROK_SUBAGENT_MIN_FREE_BYTES`). Set `0` to disable.
- Safe clean skips live-marked worktrees (`.grok-subagent-live`).

### Known gaps
- Supervisor pacing (audit animation #10) is deferred: the supervisor sprite
  bakes his desk into the same image as the figure, so it needs an asset split
  rather than a code change. Reasoning is recorded in the audit doc.

---

## [0.2.119-r1] - 2026-08-04

**Turbo Grok Build RC15.** The wire version jumps `0.2.114` → `0.2.119`: Turbo's
upstream base was actually **0.2.112** (newest bundled release notes were
`0.2.112.md`) while `VERSION` advertised `0.2.114`, so `--version` and the
What's-New surface were both wrong. RC15 syncs seven upstream releases and
re-stamps everything to one value.

### Sync point (read this before the next sync)

RC15 is where Turbo re-synced with the **Hyper community line**
(`DaviRain-Su/hyper-grok-build`, remote `community`) and with **xAI upstream**:

| Ref | Commit | Meaning |
|-----|--------|---------|
| xAI upstream | `e5478eff1` (`SOURCE_REV` `27d2088ae…`) | 0.2.119, merged in full |
| Hyper fork point | `c260695cc` | where Turbo and Hyper diverged (2026-07-29) |
| Hyper compared at | `7a48dd755` | Hyper `community/dev` head at audit time |

Cherry-picked from Hyper: `9cd8dffca` (sampler privacy + WASM decode),
`783989c01` (credential rotation, voice), `2c68d9d26` (model config reload),
`d49c3feb3` + `a831b5620` (DeepSeek V4). Hand-ported from Hyper's merge
`488edc10d`, which is reachable from neither branch tip: the circuit-breaker
probe fix.

**Deliberately NOT taken** (evaluated and declined): the Comet desktop app (does
not run on Windows), Hypercore (default-off, would add a second turn engine),
the `packages/*` reorg (git rename detection already bridges the layouts at
2,945 renames, so declining costs nothing), upstream's headless reducer rewrite
(Turbo's `streaming-json` schemaVersion 2 contract is richer), and the Codex
turn-state / remote-compaction work.

### Security

- **`x-grok-*` headers no longer leak to third-party providers.** Turbo stamped
  `x-grok-deployment-id` / `x-grok-user-id` / `x-grok-client-identifier` on every
  sampling request with no base-URL check, and set no redirect policy, so
  reqwest followed up to 10 hops including cross-origin. As the heavier
  multi-provider fork (NVIDIA/Nemotron, OpenRouter, Ollama, Kimi, Azure-style
  proxies) Turbo leaked xAI product/session identity on ordinary traffic. Now
  gated on an HTTPS-only, suffix-safe first-party allowlist, stripped from the
  *finalized* header map so a late injector cannot reintroduce them, and
  redirects follow only same-origin HTTPS.
- **Allow-rule traversal escape** (introduced by the sync, caught by upstream's
  own tests): `./../../etc/passwd` textually matched the glob `./**` while
  denoting `/etc/passwd`. Allow rules no longer match a raw spelling containing
  a parent-dir component; deny/ask keep the full multi-spelling union.
- **Subagent path-segment validation.** Model-supplied task/session/agent ids
  reach filesystem paths in more places in Turbo than upstream and were
  unvalidated. Now fail-closed against traversal, separators, drive/UNC
  prefixes, NUL/control chars, alternate data streams, Windows reserved device
  names, and trailing dot/space.
- **Strict WASM guest decode** — over-long or invalid UTF-8 from a guest is
  rejected rather than silently truncated or lossily decoded.
- **MCP** credential storage hardened against multi-process wipe (via sync).

### Fixed

- **Self-update was broken on both platforms.** `release.yml` ships the whole
  `bundled/` tree; the extractor hard-bailed `"archive entry is nested"` on the
  second path component, so every `bundled/skills/*.md` aborted the update. The
  extractor now accepts `bundled/**` at any depth while rejecting zip-slip,
  absolute/rooted paths, drive prefixes, symlinks, reserved device names and
  depth > 32, with entry/byte caps. Activation is a compensating transaction
  (bundle → binary → state) so a crash leaves either the old bundle or the new
  one, never a merge.
- **Circuit breaker double-admitted half-open probes** — a zero-valued claim
  timestamp was ambiguous with "no claim", and reservation was not atomic
  against state transitions.
- **Credential rotation** now keys on the refresh-token *family* rather than
  access-token expiry, and four paths no longer discard a freshly minted token.
  Fires on concurrent refresh — Turbo's `spawn_many` / worktree profile.
- **`/compact` no longer orphans running background tasks and subagents**, and
  recovers instead of failing when the summarizer input exceeds the window.
- **DeepSeek V4 on Ollama Cloud** advertised 256k context / 32k max output
  against a real **1M / 384k** — users compacted at a quarter of the true window
  and were capped at a twelfth of the real output limit.
- **Windows: bundled skills shipped as CRLF** while Unix got LF, so the two
  platforms shipped different bytes for the same runtime.
- **Windows: mixed CRT.** The workspace sets `+crt-static` but had no CMake
  toolchain file, so a CMake-built native dep (bundled Opus) selected `/MD`
  against a `/MT` Rust runtime.
- **Windows: POSIX shell commands kept working.** The sync replaced the
  always-`sh -c` helper with hardcoded `cmd /C`, which would have silently
  broken any configured `auth_provider_command` using `$VAR`, `exit N` or
  pipes. Now routed through the shared shell detector (Git Bash when present,
  else pwsh/cmd, honouring `GROK_SHELL`).
- Stale RC14 rename leftovers: the published `streaming-json` schema still
  identified as Hyper, and the GitHub Release body still pointed at
  `DaviRain-Su/hyper-grok-build`'s install scripts.

### Added (from upstream 0.2.113–0.2.119)

- **LSP pull diagnostics** — the language server is no longer torn down on
  every edit (headline symptom: C# diagnostics never arriving).
- **`GROK_EXTRA_CA_BUNDLE`** — corporate TLS roots for inspecting proxies.
- **`/model` re-reads `config.toml`** on invocation, replacing only
  config-owned catalog rows so remote/provider/default rows survive.
- **`x.ai/task_completed` frames bounded to 32 KiB** so ACP clients with a
  line-length limit (Python asyncio, most Node readline setups) stop hard-failing
  on a large task log.
- File watching skips nested checkouts; `git-head-changed` fires on same-branch
  commits; session `/delete`; slash-command mode-support matrix; broad TUI
  polish across six releases.

### Changed

- **Rust 1.93.0** (required by upstream 0.2.118+ code and lint config).
- `RUST_MIN_STACK` is set for cargo-run processes: the 0.2.119 prompt-turn
  future exhausts the default 2 MiB Rust thread stack (the PE main thread's
  16 MiB `/STACK` does not cover libtest/tokio threads).
- Clippy bans unenrolled `Command::spawn` — an unenrolled child outlives its
  session.

### Known

- **The project picker is inert.** Upstream removed its own picker in 0.2.119
  and the sync accepted that in the non-conflicted files; Turbo's
  `project_picker` module and `AppView` state remain but nothing triggers them.
  Left in place rather than deleted or re-implemented.
- **The test suite is not green on Windows and never was.** 477 of ~5,900 tests
  fail on `dev` for POSIX reasons (test support hardcodes `/tmp`; some tests
  shell out with `printf` / `${VAR:-default}`). RC15's differential against that
  baseline shows **zero regressions**.
- `--include-partial-messages` is accepted but rejected with a pointer to
  `--output-format streaming-json`: it only ever applied to upstream's
  `streaming-messages-json` reducer, which Turbo does not carry.

---

## [0.2.114-r14] - 2026-08-04

**Turbo Grok Build RC14** — production `web_fetch`, workflow routing so free-text
deep audits launch real Rhai recipes, English-only UI, docs reset, and product
rename prep (`turbo-grok-build`).

### Added
- **`web_fetch`** — URL → clean markdown for agents (article/full/raw extract,
  SSRF + DNS pin, challenge detection, token-aware windowing/links, browser-shaped
  client, apex↔www redirects, doctor status).
- **Workflow routing** — boot card catalog + system-prompt policy; natural-language
  host soft-match (“run a deep audit on …”) launches stock recipes.
- **Game Mode screenshot** — `docs/assets/screenshot-game-mode.png` for README.

### Changed
- Wire version **`0.2.114-r14`**.
- **English-only UI** — non-English locale packs and zh-CN user guide removed;
  language setting resolves to `en` only.
- **Docs cleanup** — public docs slimmed; historical RC/Q&A material under
  `docs/archive/`; changelog starts fresh from this RC with pedigree table.
- Stock **deep-audit** is the only recommended audit path (no `deep-audit-fixed`).

### Fixed
- Full `web_fetch` security/token hardening suite (allowlist enforcement, non-2xx
  errors, body streaming limits, charset decode, cache purge, overflow budget).

### Notes
- CLI: **`turbo`**. Install: `~/.turbo/bin`. Config/auth still `~/.grok`.
- Rebuild for binary:  
  `cargo build -p xai-grok-pager-bin --profile release-dist --bin turbo`
- Tracking remotes (post-rename):
  - `origin` → your Turbo fork
  - `upstream` → `xai-org/grok-build` (official)
  - `community` → `DaviRain-Su/hyper-grok-build` (Hyper community, fetch-only)
- For **Turbo vs Hyper** compare: `git fetch community` then
  `git log --oneline HEAD..community/dev` (do not merge casually).
