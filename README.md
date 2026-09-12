<div align="center">

<h1>Turbo Build</h1>

<img src="docs/assets/turbo-banner.jpg" alt="Turbo Build — multi-agent terminal coding" width="720">

<p>
  <a href="https://github.com/danmsheets-dev/turbo-grok-build/releases"><img src="https://img.shields.io/github/v/release/danmsheets-dev/turbo-grok-build?display_name=tag" alt="Release"></a>
  <a href="https://github.com/danmsheets-dev/turbo-grok-build/actions/workflows/release.yml"><img src="https://img.shields.io/github/actions/workflows/release.yml/badge.svg?branch=dev" alt="Release CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License"></a>
  <img src="https://img.shields.io/badge/version-1.0.13-rc.3-blue" alt="1.0.13-rc.3">
  <img src="https://img.shields.io/badge/rust-1.94.0-orange?logo=rust" alt="Rust 1.94">
  <img src="https://img.shields.io/badge/platform-macOS%20%C2%B7%20Linux%20%C2%B7%20Windows-lightgrey" alt="Platforms">
  <img src="https://img.shields.io/badge/UI-English-brightgreen" alt="English UI">
</p>

**Turbo Build** (CLI: **`turbo`**) is a heavily extended multi-agent coding
CLI forked from [xAI Grok Build](https://github.com/xai-org/grok-build). It keeps
the Rust TUI core and multi-provider stack, then layers production-grade
**folder worktrees**, recovery tooling, deep-audit workflows, Game Mode, field
logging, and agent orientation that the upstream product does not ship.

Current release line: **Grok Build 1.0 core** · wire **`1.0.13-rc.3`** — GitHub
**Latest** with `SHA256SUMS` so managed `~/.turbo` installs auto-update.

CLI binary: **`turbo`** (installs to `~/.turbo/bin`). Product name: **Turbo Build**.

[What changed](#what-makes-turbo-different) ·
[Feature overview](#feature-overview) ·
[RC history](#community-rc-history) ·
[Install](#installation) ·
[Providers](#providers) ·
[Subagents & worktrees](#subagents--worktrees) ·
[Workflows & deep audit](#workflows--deep-audit) ·
[Web fetch](#web-fetch) ·
[Auto Developer Log](#auto-developer-log) ·
[Build](#building-from-source) ·
[Docs](#documentation) ·
[License](#license)

</div>

---

## What makes Turbo different

Upstream Grok Build is a strong single-session coding agent. **Turbo Build
is a multi-agent development runtime** built on that foundation.

| Area | Upstream Grok Build | **Turbo Build** |
|------|---------------------|----------------------|
| Product focus | Official agent CLI | Community multi-agent platform |
| Subagents | Present | Isolation by default, land/diff/discard, soft-preserve, restore |
| Folder worktrees | Optional / fragile | Default `isolation=worktree`, FS confine, baseline agent-only patches |
| Worktree recovery | Ephemeral / hard to find | Snapshot + baseline refs + `turbo subagent …` CLI |
| Dirty-parent pollution | Diff vs HEAD can explode | Spawn **baseline** refs → agent-only patches; land fails closed if huge |
| Deep audit | — | **`/deepaudit`** recipe (investigate → verify → report) |
| Workflows | Limited | Rhai recipes + NL soft-match + boot-card routing |
| Web content | Search / raw HTTP | **`web_fetch`** URL → clean markdown (token-aware) |
| Agent orientation | System prompt + project rules | **Agent Boot Card** + Workspace Tree atlas |
| Product field signal | `/feedback`, crashes | **Auto Developer Log** + **Feature Request Log** |
| Providers | xAI-centric | Multi-provider (Grok, NVIDIA Integrate, Codex, Kimi, OpenAI, Anthropic, …) |
| UX | TUI | TUI + **Game Mode** (`Ctrl+G` pixel office) + **`/meeting` notetaker** |
| Branding / binary | `grok` · `~/.grok` | Product **Turbo Build** · CLI **`turbo`** · binary under `~/.turbo` |

### Prebuilt binaries and auto-update

Production builds ship as **GitHub Release** archives (not git blobs). The
community updater and `install.ps1` / `install.sh` require:

- `turbo-<version>-<target-triple>.zip` (Windows) or `.tar.gz` (macOS/Linux)
- `SHA256SUMS`

| Wire | Release | Windows asset |
| --- | --- | --- |
| `1.0.13-rc.3` | [v1.0.13-rc.3](https://github.com/danmsheets-dev/turbo-grok-build/releases/tag/v1.0.13-rc.3) | `turbo-1.0.13-rc.3-x86_64-pc-windows-msvc.zip` |
| `1.0.13-rc.2` | [v1.0.13-rc.2](https://github.com/danmsheets-dev/turbo-grok-build/releases/tag/v1.0.13-rc.2) | `turbo-1.0.13-rc.2-x86_64-pc-windows-msvc.zip` |
| `1.0.13-rc.1` | [v1.0.13-rc.1](https://github.com/danmsheets-dev/turbo-grok-build/releases/tag/v1.0.13-rc.1) | `turbo-1.0.13-rc.1-x86_64-pc-windows-msvc.zip` |
| `1.0.0-rc.12` | [v1.0.0-rc.12](https://github.com/danmsheets-dev/turbo-grok-build/releases/tag/v1.0.0-rc.12) | `turbo-1.0.0-rc.12-x86_64-pc-windows-msvc.zip` |
| `1.0.0-rc.11.1` | [v1.0.0-rc.11.1](https://github.com/danmsheets-dev/turbo-grok-build/releases/tag/v1.0.0-rc.11.1) | `turbo-1.0.0-rc.11.1-x86_64-pc-windows-msvc.zip` |
| `1.0.0-rc.11` | [v1.0.0-rc.11](https://github.com/danmsheets-dev/turbo-grok-build/releases/tag/v1.0.0-rc.11) | `turbo-1.0.0-rc.11-x86_64-pc-windows-msvc.zip` |
| `1.0.0-rc.10` | [v1.0.0-rc.10](https://github.com/danmsheets-dev/turbo-grok-build/releases/tag/v1.0.0-rc.10) | `turbo-1.0.0-rc.10-x86_64-pc-windows-msvc.zip` |
| `1.0.0-rc.9` | [v1.0.0-rc.9](https://github.com/danmsheets-dev/turbo-grok-build/releases/tag/v1.0.0-rc.9) | `turbo-1.0.0-rc.9-x86_64-pc-windows-msvc.zip` |
| `1.0.0-rc.8` | [v1.0.0-rc.8](https://github.com/danmsheets-dev/turbo-grok-build/releases/tag/v1.0.0-rc.8) | `turbo-1.0.0-rc.8-x86_64-pc-windows-msvc.zip` |
| `1.0.0-rc.7` | [v1.0.0-rc.7](https://github.com/danmsheets-dev/turbo-grok-build/releases/tag/v1.0.0-rc.7) | `turbo-1.0.0-rc.7-x86_64-pc-windows-msvc.zip` |

```powershell
# Managed install (enables `turbo update` / launch auto-update)
irm https://github.com/danmsheets-dev/turbo-grok-build/releases/latest/download/install.ps1 | iex
turbo update --check
```

Keep `[cli] auto_update = true` in `~/.grok/config.toml`. Do **not** mark
GitHub Releases as prerelease if they should become `/releases/latest`.

Packaging notes: [`releases/windows/README.md`](./releases/windows/README.md).
Full notes: [`CHANGELOG.md`](./CHANGELOG.md).

### Highlights (1.0 line)

**1.0.0-rc.1** merges official **xAI Grok Build 1.0.0** as the permanent upstream
core while keeping Turbo’s product layer (worktrees, deep-audit, Game Mode,
multi-provider, English-only).

**1.0.0-rc.2** adds the **Agent WebView** (**beta**) — a product-owned WebView2
window the agent drives through first-class `browser_*` tools, mirrored in the
TUI with `Ctrl+Shift+B` — plus the **Grok 4.6** default catalog, MCP disk-wins
config merge, full Disk Clean, and harness polish for the Grok Build Claude
plugin (identity card, permission aliases, job-object ergonomics). It also
carries a confinement hardening pass: a `--confine` write-boundary escape and a
snapshot-uid forgery vector were found and closed before release
([`docs/RC2_UNRELEASED_AUDIT.md`](./docs/RC2_UNRELEASED_AUDIT.md)). The WebView
is a dedicated session profile, not the operator's daily Chrome; mutating
`browser_eval` / navigate are permission-gated as of 1.0.0-rc.11.

**1.0.0-rc.2.1** is the Agent WebView hotfix. rc.2 shipped it with a named-pipe
defect that gave a fresh `browser_navigate` a four-in-five chance of never
returning, so the window opened and stayed white. That is fixed, along with the
field report it produced: a wedged host now returns a real error instead of a
transport timeout, first paint explains itself, the title carries the page host
and session, and the close button hides the window instead of killing the host.

**1.0.0-rc.3** is the Agent WebView field-test pass (page-control tools, OAuth
popups, session-scoped profiles). See [`CHANGELOG.md`](./CHANGELOG.md).

**1.0.0-rc.4** adds the **Fathom-style meeting notetaker**: `/meeting join <url>
[name]` opens Zoom/Teams/Meet/Webex, taps inbound audio in-page (or WASAPI
loopback + mic on Windows fallback), and transcribes **all participants** via
**xAI hosted STT** (`wss://api.x.ai/v1/stt` by default). Auto-answers coworker
`Turbo:` questions from the launch workspace (MCP ok), and writes a work-only
recap to `Meetings/YYYY-MM-DD - <name>.md` with **For you** and **Projects**
sections. Audio leaves the machine for transcription.

**1.0.0-rc.11** is **Security honesty**: fail-closed confine bypasses, folder-trust
for project skills, marketplace `require_sha`, signed GitHub release attestations,
and Agent WebView called **beta**. `--confine` is a write boundary, not an OS jail.
Meeting audio is transcribed by xAI hosted STT. Internal campaign / claims-ledger
files are gone from the tree.

**1.0.0-rc.10** is **Teams Join Hardening and Incident Log**: `meeting_join` no
longer hands the link to the OS when a guest bot is dispatched (which is what put
a File Explorer window on screen), the notetaker asks Teams for the anonymous web
client instead of following the desktop-app launcher, and a guest join that fails
says so in its first line rather than passing a recording of your own speakers off
as the feature you asked for. `turbo issues sync` now preflights the remote and
exports a local bundle when GitHub refuses. It also fixes a crash on any prompt
containing a smart quote or emoji.

**1.0.0-rc.9** is **Meeting Tool v3**: the notetaker stops recording your PC and
**joins the meeting**. `/meeting join <teams-url>` sends a guest participant
named **Turbo (Notetaker)** that waits in the Teams lobby, is admitted like any
other attendee, and taps audio inside the meeting page — so it works with your
speakers muted, your headset unplugged, or you gone. Coworker `Turbo:` questions
are answered in chat under the bot's own name, on a **read-only toolset enforced
at dispatch**. See [`docs/MEETING_NOTETAKER.md`](./docs/MEETING_NOTETAKER.md).

**1.0.0-rc.8** productizes **scheduled tasks** (`/schedule` + `turbo schedule`),
hardens **Meeting R2** (live tool pin, honest stop after crash, passcode redact,
spoken `Turbo,` auto-ask, untrusted coworker Q&A) and **Agent WebView R3**
(hop policy, OAuth host-owned popups, chrome-devtools opt-in), and adds
**opt-in GitHub Issues sync** so Auto Developer Log and Feature Request Log
can leave one laptop (`turbo issues sync` / `turbo features sync`).

Earlier community highlights (Game Mode, disk clean, tools list) from the
`0.2.119-rN` line remain in the product; see [`CHANGELOG.md`](./CHANGELOG.md).

- **Windows voice crash fixed (release blocker)** — `cpal`'s process-global WASAPI
  device enumerator outlived the COM apartment that created it, so the **second**
  push-to-talk dictation in a session hard-crashed the process (exit 139, no
  panic, unsent draft lost). Voice mode is on by default, so this was reachable
  by anyone dictating twice. All in-process `cpal` work now runs on one
  long-lived audio host thread
- **Game Mode overhaul** — hover tooltips on the **Supervisor** (model, phase,
  turn elapsed, context usage, seats, branch) and on the **MCP server rack**
  (per-server status, tool counts, failure detail, backed by a live status cache);
  eleven new sprite animations; and the office no longer pins the event loop at
  ~12 Hz while idle. Full audit: [`docs/RC2_GAME_MODE_AUDIT.md`](./docs/RC2_GAME_MODE_AUDIT.md)
- **Test suite** — `cargo check --workspace --all-targets` is clean and
  **28 414 tests pass** (`cargo test --workspace --lib` is fully green). Nine
  integration tests are known to fail on a developer machine that has real MCP
  servers, hooks, or auth configured — they read live user state instead of a
  fixture; see [`docs/RC2_UNRELEASED_AUDIT.md`](./docs/RC2_UNRELEASED_AUDIT.md).
  The `xai-grok-pager-pty-harness` crate is excluded: its ConPTY teardown can
  spin on Windows
- **Line endings made deterministic** — 34 files carried CRLF **in the git index**,
  so Windows and Linux builders shipped different bytes. Now a `.gitattributes`
  + a CI guard that *derives* the embedded-asset inventory rather than trusting a
  hand-written list
- **`turbo tools list [--require …]`** — prove `spawn_subagent` (and peers) are
  registered after config resolve; no model turn. Honors `GROK_SUBAGENTS=0`
- **`turbo disk report|check|clean --safe|recover|prune`** — multi-path free-space gate,
  category reclaim (`--include debug-pdbs|release|release-dist-caches|…`),
  JSON `reclaimed_bytes`, unified prune (RC3 full Disk Clean)
- **Keep-N + free gate defaults** — `GROK_SUBAGENT_KEEP_N=3` (age-only when `0`);
  `GROK_MIN_FREE_GB=40` fail-closed before isolation=worktree create
- **Agent cargo policy** — package-scoped tests, `CARGO_INCREMENTAL=0` one-shots,
  clean debug after ship builds (see [`AGENTS.md`](./AGENTS.md))

Three bugs were found only because the suite could finally run: Codex Live's
Windows speaker output was **silently dead** (the stream dropped the instant it
was created), Windows and macOS users were shown **Linux** paste instructions, and
`locales/en.yml` was compiled into the binary without an LF pin.

Still ships from earlier RCs: **`web_fetch`** + workflow routing (RC14), isolation
write boundary (RC12; policy classifier, not an OS sandbox), Workspace Tree inject
(RC13), Game Mode (RC11), baselines + Boot Card + ADL (RC9–10), **`/deepaudit`**
(RC8).

Not affiliated with xAI. Based on Apache-2.0 Grok Build source.

---

## Feature overview

| Feature | What it does | Since |
|---------|----------------|-------|
| **Folder worktrees** | Subagents default to isolated git worktrees under `~/.grok/worktrees/…` | RC7 (harden RC6–RC12) |
| **Land / diff / discard** | Promote or drop child work via tools + `turbo subagent …` | RC8–RC9 |
| **Agent-only baselines** | Diff/land = `baseline..snapshot`, not dirty parent vs HEAD | RC9 |
| **Soft-preserve + keep-N** | Live worktrees kept for review; `GROK_SUBAGENT_KEEP_N` (default 3; `0` = age-only) | RC9 / RC12 / RC2 |
| **Free-space gate** | Pre-spawn + `turbo disk check` (`GROK_MIN_FREE_GB`, default 40) | RC12 / RC2 |
| **`turbo tools list`** | Headless schema assert (`--require spawn_subagent`) without a model turn | RC2 |
| **`turbo disk`** | Report / check / category clean / recover / prune (multi-path free space) | RC15 / RC2 / **RC3** / **1.0-rc1** |
| **FS confine (worktree)** | Fail-closed **write boundary** (path-prefix + program classifier; not an OS sandbox). See [KNOWN_ISSUES](./docs/KNOWN_ISSUES.md) | RC12 |
| **`/deepaudit`** | Parallel investigate → independent verify → verified report | RC8 |
| **`/deep-research`** | Bounded research with claim cross-check | earlier + RC14 routing |
| **Workflow tool + NL** | Rhai recipes; free-text maps to stock launches | RC14 |
| **`web_fetch`** | URL → clean text for the model (tokens down, content up) | RC14 |
| **Workspace Tree** | `workspace_tree` / `resolve_path` + session inject card | RC12–RC13 |
| **Agent Boot Card** | Ops brief: tools, isolation, recovery, logging, workflows | RC9 / RC14 |
| **Auto Developer Log** | Structured product issues (`developer_log` + `turbo issues`) | RC9 |
| **Feature Request Log** | Missing capability surface (`feature_request_log` + `turbo features`) | RC11 |
| **Game Mode** | `Ctrl+G` pixel office of supervisor + subagent desks; hover tooltips (Supervisor / MCP rack), 11 animations, parks when idle | RC11 / RC2 |
| **Multi-provider** | Grok, NVIDIA Integrate, Codex, Kimi, OpenAI, Anthropic, … | r2+ |
| **Headless honesty** | Streaming-json tool/subagent events, confine, trust gates | RC6 |
| **Agent WebView (`browser_*`)** | **Beta.** Product-owned WebView2 (not daily Chrome); `browser_*` tools + `Ctrl+Shift+B` TUI mirror. Mutating eval/navigate prompt. | **1.0-rc.2** / gate **1.0-rc.11** |
| **`/meeting` notetaker** | **Joins Teams as a guest bot** ("Turbo (Notetaker)" in the lobby); in-page audio tap → xAI hosted STT; coworker `Turbo:` Q&A on workspace-confined reads; work-only recap in `Meetings/` | **1.0-rc.4** / v3 **1.0-rc.9** |
| **`/schedule`** | Standing jobs (search / stat / meeting join); no 7-day expiry; `turbo schedule list` | **1.0-rc.8** |
| **GitHub log sync** | Opt-in `turbo issues sync` / `turbo features sync` to a private Issues repo | **1.0-rc.8** |
| **`turbo mcp serve`** | Turbo *as* an MCP server: bounded file tools to an outside client over loopback, optional Cloudflare tunnel, OAuth 2.1 for clients that cannot send a pasted bearer header | **1.0.13-rc.4** |

---

## Community RC history

| RC | Wire | Theme |
|----|------|--------|
| **r1–r5** | `0.2.114-r1` … `r5` | Community fork: providers, extensions, OMP resume, Linux glibc 2.17 floor |
| **r6** | `0.2.114-r6` | Isolation + headless honesty (confine is a boundary; isolation fails closed) |
| **r7** | `0.2.114-r7` | **Folder worktrees by default** (`isolation=worktree`) |
| **r8** | `0.2.114-r8` | **Deep audit**, land/diff/discard tools, NVIDIA harden, continuous-improve |
| **r9** | `0.2.114-r9` | Baselines, **Boot Card**, **Auto Developer Log**, soft-preserve |
| **r10** | `0.2.114-r10` | Deep-audit + ADL ship fixes; **Turbo** brand / `turbo` CLI |
| **r11** | `0.2.114-r11` | **Game Mode** + Feature Request Log |
| **r12** | `0.2.114-r12` | Isolation **write boundary**, densify lifecycle, MCP harden, Game Mode polish |
| **r13** | `0.2.114-r13` | **Workspace Tree** inject, densify engines, Game Mode performance |
| **r14** | `0.2.114-r14` | **`web_fetch`** + **workflow routing** |
| **r15** | `0.2.119-r1` | **Upstream 0.2.119 sync**, security + Windows correctness |
| **r2** | **`0.2.119-r2`** | **Windows voice-crash fix**, **Game Mode overhaul**, green suite, LF normalization, disk keep-N / free gate, **`turbo tools list`** |
| **1.0-rc.1** | **`1.0.0-rc.1`** | **Grok Build 1.0.0 core** + Turbo product layer |
| **1.0-rc.2** | **`1.0.0-rc.2`** | **Agent WebView**, Grok 4.6, confinement harden |
| **1.0-rc.2.1** | **`1.0.0-rc.2.1`** | Agent WebView named-pipe hotfix |
| **1.0-rc.3** | **`1.0.0-rc.3`** | Agent WebView field-test pass |
| **1.0-rc.4** | **`1.0.0-rc.4`** | **Fathom-style `/meeting` notetaker** (loopback STT, `Turbo:` Q&A, work-only recap) |
| **1.0-rc.5** | **`1.0.0-rc.5`** | **Harness Q&A:** spawn catalog aliases, `browser_save`/`browser_downloads`, log redaction |
| **1.0-rc.6** | **`1.0.0-rc.6`** | **Providers:** `/providers` ACP, isolation keep-N/land, **Poolside** hosted Chat API |
| **1.0-rc.7** | **`1.0.0-rc.7`** | Phase 5 control plane, spawn identity, Meeting Join Hardening, Windows credential jail |
| **1.0-rc.8** | **`1.0.0-rc.8`** | **`/schedule`**, Meeting R2, Browser R3, GitHub Issues log sync, stock workflows |
| **1.0-rc.9** | **`1.0.0-rc.9`** | **Meeting Tool v3:** joined Teams notetaker bot, in-page audio tap, read-only meeting Q&A |
| **1.0-rc.10** | **`1.0.0-rc.10`** | **Teams Join Hardening:** web-join rewrite, protocol guard, honest fallback, GitHub sync preflight |
| **1.0-rc.12** | **`1.0.0-rc.12`** | **Subagent hardening** + display name **Turbo Build** (CLI still `turbo`) |
| **1.0.13-rc.4** | **`1.0.13-rc.4`** | **`turbo mcp serve`:** Turbo as an MCP server — loopback + optional tunnel, OAuth 2.1 for ChatGPT Developer mode |
| **1.0.13-rc.3** | **`1.0.13-rc.3`** | **Codex window isolation** + Spark spawn aliases (`spark`, `codex-spark`) |
| **1.0.13-rc.2** | **`1.0.13-rc.2`** | **Codex catalog:** GPT-6 Astra (Light→Ultra) + GPT-5.3 Codex Spark as a first-class picker model |
| **1.0.13-rc.1** | **`1.0.13-rc.1`** | **Upstream sync 1.0.13:** multi-folder (`--add-dir`, `/folder`), `--confine` path list, upstream security ports, Windows + Linux suites green |
| **1.0-rc.11.1** | **`1.0.0-rc.11.1`** | **Windows worktree isolation hotfix** (`H:\t\w\{hash}` recognized by start gate) |
| **1.0-rc.11** | **`1.0.0-rc.11`** | **Security honesty:** confine HIGH bypasses, permission mapping, skills trust, meeting-QA confine; Agent WebView **beta** |

> RC numbering follows the **wire version**, which restarted at `r1` when RC15
> synced upstream `0.2.119`. The 1.0 line starts at **`1.0.0-rc.1`**.

Full per-release detail: [`CHANGELOG.md`](./CHANGELOG.md).

---

## Screenshots

| Welcome (English) | Game Mode |
| ----------------- | --------- |
| ![Turbo TUI](docs/assets/screenshot-welcome-en.png) | ![Game Mode](docs/assets/screenshot-game-mode.png) |

---

## Names at a glance

| | Official | This project |
|---|---|---|
| Product | Grok Build | **Turbo Build** |
| CLI binary | `grok` | **`turbo`** |
| Install root | `~/.grok` | **`~/.turbo`** (binary only) |
| Config / auth / sessions | `~/.grok` | **same `~/.grok`** (shared) |
| Release line | upstream cadence | **1.0.13-rc.3** |
| Upstream | [xai-org/grok-build](https://github.com/xai-org/grok-build) | This fork (+ multi-provider / multi-agent patches) |

GitHub repo: **`turbo-grok-build`**. Product name is **Turbo Build**; CLI is **`turbo`**.

---

## Installation

Prebuilt binaries (macOS arm64/x86_64, Linux arm64/x86_64 glibc 2.17+, Windows x86_64) on
[GitHub Releases](https://github.com/danmsheets-dev/turbo-grok-build/releases):

```sh
# macOS / Linux
curl -fsSL https://github.com/danmsheets-dev/turbo-grok-build/releases/latest/download/install.sh | bash
```

```powershell
# Windows PowerShell
irm https://github.com/danmsheets-dev/turbo-grok-build/releases/latest/download/install.ps1 | iex
```

```sh
turbo --version
turbo login          # xAI / Grok session (browser OAuth)
turbo                # start the TUI
```

Pin a release:

```sh
curl -fsSL https://github.com/danmsheets-dev/turbo-grok-build/releases/latest/download/install.sh | bash -s -- --version v1.0.13-rc.3
```

```powershell
# Windows — pin this release
.\install.ps1 -Version v1.0.13-rc.3
```

Installer verifies `SHA256SUMS`, installs to `~/.turbo/bin/turbo`
(`%USERPROFILE%\.turbo\bin\turbo.exe` on Windows), and activates `bundled/`
under `~/.grok/bundled`. Managed installs auto-update from GitHub `/latest`
when `[cli] auto_update = true`.

### Install with Nix

```sh
nix run github:danmsheets-dev/turbo-grok-build#turbo-grok-build -- --version
nix profile install github:danmsheets-dev/turbo-grok-build#turbo-grok-build
```

From a clone:

```sh
git clone https://github.com/danmsheets-dev/turbo-grok-build
cd turbo-grok-build
nix run .#turbo-grok-build -- --version
nix develop
```

---

## Providers

| Platform | Auth | Notes |
| -------- | ---- | ----- |
| xAI / Grok | `turbo login` or `XAI_API_KEY` | First-party models |
| NVIDIA Integrate | platform key | Large catalog; Turbo adds agent-ready hardening |
| Kimi Code / Moonshot | OAuth / API key | `kimi-code/*`, open platform |
| ChatGPT Codex | ChatGPT OAuth | GPT-6 Astra, GPT-5.3 Codex Spark, GPT-5.6 Sol/Terra/Luna + experimental live voice |
| OpenCode Go | subscription key | Chat Completions + Messages |
| OpenAI / Anthropic / DeepSeek-style | BYOK | Catalog platforms |
| OpenRouter | BYOK (`/providers openrouter`) | OpenAI-compatible aggregator |
| Poolside | `POOLSIDE_API_KEY` or `/providers poolside` | Hosted Laguna Chat API (`inference.poolside.ai`) |
| Z.AI Coding Plan | platform key | International plan |
| Ollama Cloud | API key | Live roster sync |

Model ids look like `{platform}/{model}`. Config and credentials stay under
**`~/.grok`** (shared with upstream Grok Build).

---

## Subagents & worktrees

Turbo treats subagents as first-class workers with **folder worktrees** and
recovery (isolation-by-default since **RC7**, fail-closed + FS confine through
**RC12**):

```text
spawn (isolation=worktree)
  → spawn baseline  refs/grok/subagent-baselines/<id>
  → agent works in ~/.grok/worktrees/<slug>/subagent-<id>
  → complete snapshot refs/grok/subagents/<id>
  → soft-preserve live tree (or clean if GROK_SUBAGENT_SOFT_PRESERVE=0)
  → agent-only patch: baseline..snapshot
```

```bash
turbo subagent list
turbo subagent open <id>
turbo subagent open <id> --restore          # materialize snapshot
turbo subagent diff <id>
turbo subagent land <id>                    # refuses huge dirty-tree patches
turbo subagent discard <id>
```

Optional:

| Env | Effect |
|-----|--------|
| `GROK_SUBAGENT_SOFT_PRESERVE=0` | Delete live tree immediately after snapshot |
| `GROK_SUBAGENT_KEEP_N` | Max soft-preserved live trees (default **3**; `0` = age-only). Alias: `GROK_SUBAGENT_SOFT_PRESERVE_KEEP_N` |
| `GROK_SUBAGENT_KEEP_MAX_AGE_SECS` | Age cutoff when `KEEP_N=0` (default 86400 = 24h) |
| `GROK_MIN_FREE_GB` | Free-space floor before worktree create / `turbo disk check` (default **40**; `0` disables). Alias: `GROK_SUBAGENT_MIN_FREE_BYTES` |
| `GROK_SUBAGENT_WORKTREE_SEED=clean` | HEAD-only sandbox (default; no parent dirty copy). Completion tag: `<worktree_seed>clean</worktree_seed>` |
| `GROK_SUBAGENT_WORKTREE_SEED=dirty` | Copy parent WIP into the worktree (preserve working tree) |
| `GROK_POST_SUBAGENT_DISK_CLEAN=off` | Disable auto `disk clean --safe --if-low-space` after subagent dispose (default: enabled, 5‑min debounce) |
| `GROK_PREFER_GIT_BASH_FOR_SCRIPTS=0` | Windows: do not rewrite bare `bash` / `*.sh` to Git Bash |
| `retain_worktree=true` on spawn | Keep path until land/discard |

Headless schema assert (no model turn):

```bash
turbo tools list
turbo tools list --require spawn_subagent --json
turbo disk check            # exit 1 if free space under GROK_MIN_FREE_GB
turbo disk report           # shows keep-N + min-free threshold status
turbo disk recover --safe   # check → clean --if-low-space → re-check
turbo tree prune --execute  # apply tree-store prune (default is dry-run)
```

Details: [`docs/KNOWN_ISSUES.md`](docs/KNOWN_ISSUES.md),
[`CHANGELOG.md`](./CHANGELOG.md) (RC7–RC2), user-guide `16-subagents.md`.

---

## Workflows & deep audit

Stock Rhai workflows (background; progress in `/workflows`):

| Recipe | Slash / NL | What it does |
|--------|------------|--------------|
| **deep-audit** | `/deepaudit`, `/deep-audit`, `/ultracode` · “run a deep audit on …” | Parallel find → independent verify → verified-only report (read-only) |
| **deep-research** | `/deep-research <query>` · “deep-research on …” | Bounded research shards + claim cross-check + citations |
| **continuous-improve** | `/workflow continuous-improve` | Research → plan → implement (worktree) → verify loop |

Agents with the `workflow` tool are instructed (Boot Card + system prompt) to
**launch these recipes** instead of spawning two ad-hoc explore/review
subagents. Host soft-match also intercepts clear free-text audit/research
requests when workflows are enabled (`GROK_WORKFLOWS=0` to disable).

```text
/deepaudit --size medium crates/codegen/xai-grok-tools
Can you run a deep audit on the security app
/deep-research Compare Postgres 17 vs MySQL 9 migration risks
```

User/project recipes: `~/.grok/workflows/*.rhai` and `.grok/workflows/*.rhai`
(filename should match `meta.name`).

---

## Web fetch

**`web_fetch`** turns a URL into **agent-usable clean text** (HTML → markdown,
article extract, windowing) with SSRF protection, DNS pin on direct egress,
challenge detection, and token-aware defaults (links off by default).

```text
# Agent tool (when enabled — default on)
web_fetch url=https://example.com/docs extract_mode=article
```

Disable: `GROK_WEB_FETCH=0` or `[features] web_fetch = false`. Optional
enterprise allowlist: `[toolset.web_fetch] allowed_domains = […]`. See user
guide configuration and [`CHANGELOG.md`](./CHANGELOG.md) RC14.

---

## Auto Developer Log

Agents are instructed (Boot Card) to **always** file product friction with the
`developer_log` tool. Humans triage with:

```bash
turbo issues list
turbo issues show <id>
turbo issues export --severity p0 --out ./turbo-issues-pack
turbo issues path
turbo issues set-dir D:/TurboLogs/developer-log   # persist custom root
turbo issues clear-dir
```

| Precedence | Log directory source |
|------------|----------------------|
| 1 | `turbo issues set-dir` (session + `~/.grok/developer-log.toml`) |
| 2 | `GROK_DEVELOPER_LOG_DIR` |
| 3 | `~/.grok/developer-log.toml` |
| 4 | Default `~/.grok/developer-log` |

Disable: `GROK_DEVELOPER_LOG=0`. Full writeup: [`docs/AUTO_DEVELOPER_LOG.md`](docs/AUTO_DEVELOPER_LOG.md).

---

## Agent Boot Card

On each **new** session (and by default on resume), Turbo injects a short system
briefing (`<turbo_boot_card>`) covering tools, **workflows catalog**, subagent
lifecycle, recovery commands, land safety, and **required** `developer_log` /
`feature_request_log` usage. Subagents get a tiny child stub only.

```text
GROK_BOOT_CARD=off|short|full    # default short
GROK_BOOT_CARD_ON_RESUME=0       # disable inject on resume
```

Workspace Tree inject (RC13) adds a budgeted `<workspace_tree_card>` atlas after
the boot card when indexing is enabled.

---

## Game Mode

`Ctrl+G` opens a pixel **office** view of the Supervisor (main agent) and
subagent desks (added **RC11**, polished RC12–RC13, overhauled **RC2**). Chat
composer stays available. Compact terminals fall back to a simpler layout. Tasks
pane: `Ctrl+Shift+G`.

**Hover tooltips (RC2).** Hover the **Supervisor** for model, phase, turn elapsed,
context window used/total, seat + overflow counts and git branch. Hover the **MCP
server rack** for per-server status, tool counts and failure detail — backed by a
live status cache, so it stays current outside the `/mcps` modal. `Tab` /
`Shift+Tab` cycle desks; `Esc` clears.

**Animations (RC2).** Eleven added: debug-rage pose on failure, arms-up celebrate
with confetti, papers flying during handoff, monitor glow + compile flash, a door
that swings on spawn/exit, MCP rack LEDs bursting on **real tool calls**,
coffee-sip idle, a real day/night wall clock, an office-wide success wave, typing
cadence driven by token throughput, and a floor robot that patrols only while the
office is busy.

**Idle cost (RC2).** An open office used to wake the event loop ~12×/sec forever.
A frozen room now parks — Compact and Unicode tiers at zero wakeups, the pixel
office at a budgeted ~0.33 Hz ambient tick that drives the idle animations.
Closing the view now also releases ~8–10 MB of image caches that previously
leaked for the life of the process.

![Game Mode](docs/assets/screenshot-game-mode.png)

---

## MCP server (`turbo mcp serve`)

Turbo can also act as an MCP **server**, exposing a bounded subset of its own
file tools to an outside client over Streamable HTTP.

```sh
turbo mcp serve --root /path/to/project
```

It binds loopback only, and prints what a client needs:

```text
Turbo MCP server listening (loopback only)
  URL:     http://127.0.0.1:52817/3f9c.../mcp
  Header:  Authorization: Bearer 8a41...
  OAuth:   http://127.0.0.1:52817/.well-known/oauth-protected-resource/3f9c.../mcp
  Approve: K7M2PQR9   (a client using OAuth asks for this)
  Allow:   readonly
  Root:    /path/to/project
  Tools:   read_file, list_dir, grep
```

The URL, the bearer token and the approval code are each credentials, and all
three change on every start. `--allow` defaults to `readonly`; `edit` is for a
client you trust to change your code.

### ChatGPT Developer mode (OpenAI custom MCP connector)

ChatGPT's connector discovers a server's OAuth metadata and registers itself —
it never sends a bearer header you paste in. Turbo implements that flow as an
OAuth 2.1 resource server: RFC 9728 protected-resource metadata, RFC 8414
authorization-server metadata, RFC 7591 dynamic client registration, PKCE
(`S256` only; `plain` is refused), exact redirect-URI matching, and RFC 8707
audience-bound tokens.

The specification wants the authorization endpoints over HTTPS, so run a tunnel:

```sh
turbo mcp serve --root /path/to/project --tunnel cloudflare
```

1. Add the **public** tunnel URL in ChatGPT as a custom MCP connector.
2. It fetches the metadata, registers itself, and sends you to an approval page.
3. Enter the `Approve:` code from Turbo's own terminal. Without it nothing is
   issued, so reaching that page is not enough to obtain a token.

The code is good for five minutes **from the moment the approval page is
served** — not from when the server started. If it lapses, load the page again:
that reopens the window, and the same code still works.

> **Status.** This flow is written against the MCP authorization specification
> and covered by the crate's own wire-level tests. It has **not** yet been
> exercised against a live ChatGPT connector, so treat a first connection as a
> test rather than a supported path.

An access token lasts an hour and the client refreshes it. A refresh token is
replaced on every use, and presenting a spent one is treated as theft: the grant
it became is revoked, and the event is reported on Turbo's standard error. A
token is bound to the URL it was issued for, so one minted before the tunnel came
up is not accepted afterwards, and a token from another server is never accepted.

None of this widens what a client may do. Every call still passes the same
boundary, in the tier you started, inside the roots you approved. The full
boundary — path rules, always-refused locations, connection and body caps, and
troubleshooting — is in the
[MCP Server guide](crates/codegen/xai-grok-pager/docs/user-guide/32-mcp-server.md).

Any MCP client that *can* send a custom `Authorization: Bearer` header still
works exactly as before; OAuth is an addition, not a replacement.

---

## Building from source

Requirements: Rust (`rust-toolchain.toml`), [DotSlash](https://dotslash-cli.com)
for `bin/protoc`, CMake 3.5+.

```sh
cargo run -p xai-grok-pager-bin              # TUI (binary name: turbo)
GROK_VERSION=$(cat VERSION) cargo build -p xai-grok-pager-bin --profile release-dist
./target/release-dist/turbo --version
```

Windows PowerShell:

```powershell
$env:GROK_VERSION = (Get-Content VERSION -Raw).Trim()
cargo build -p xai-grok-pager-bin --profile release-dist --bin turbo
# Binary: target\release-dist\turbo.exe  (do not install over ~/.turbo until ready)
```

Community branding / updater: `--features community-build` (default on this tree).

### Disk hygiene (important on Windows)

This monorepo’s `target/` can grow past **100–200 GB** (debug incremental + PDBs,
plus independent `release-dist` caches). Prefer package-scoped `cargo check` /
`cargo test`. Before large builds, free space should be ≥40 GB (see
[`AGENTS.md`](./AGENTS.md)). After a successful ship binary:

```powershell
# Keep turbo.exe; drop release-dist rebuild caches (optional)
Remove-Item -Recurse -Force target\debug -ErrorAction SilentlyContinue
# Or only: target\release-dist\{incremental,deps,build,.fingerprint}
```

`turbo disk report|check|clean --safe` shipped in **RC2** — see
[`CHANGELOG.md`](./CHANGELOG.md).

---

## Changelog & known issues

- [`CHANGELOG.md`](./CHANGELOG.md) — **official changelog** · **1.0.13-rc.3** is current
- [`docs/KNOWN_ISSUES.md`](./docs/KNOWN_ISSUES.md)
- [`docs/workspace-tree.md`](docs/workspace-tree.md) — Workspace Tree (RC12–RC13)
- [`docs/archive/RC11_RELEASE_NOTES.md`](docs/archive/RC11_RELEASE_NOTES.md) — Game Mode (historical)
- [`docs/archive/RC9_FEATURES.md`](docs/archive/RC9_FEATURES.md) — worktrees, Boot Card, ADL (historical)
- [`docs/archive/Q&A/rc9/RC10_HARNESS_FIX_PLAN.md`](docs/archive/Q&A/rc9/RC10_HARNESS_FIX_PLAN.md) — RC10 harness matrix (historical)

---

## Releasing

1. Set root [`VERSION`](VERSION) to the monorepo lockstep client version (stamp
   `x-grok-client-version`; xAI rejects clients below **0.1.202**).
2. Update `CHANGELOG.md`, commit on `dev`.
3. Tag and push:

```sh
VERSION=$(tr -d '[:space:]' < VERSION)
git tag "v${VERSION}"
git push origin "v${VERSION}"
```

Workflow: [`.github/workflows/release.yml`](.github/workflows/release.yml).
Artifacts ship as `turbo-<version>-<target>.tar.gz` / `.zip` + `SHA256SUMS`.

---

## Coexistence with official `grok`

| Surface | Official | Turbo (`turbo`) |
|---------|----------|-----------------|
| Binary | `grok` | `turbo` |
| Install root | `~/.grok/bin` | `~/.turbo/bin` |
| Config / auth / sessions | `~/.grok` | **same** |
| Leader IPC | under `~/.grok` | **same namespace** |

- Sessions and OAuth are shared — log in once for both.
- Updater is isolated: Turbo never overwrites `~/.grok/bin/grok`.
- Not affiliated with xAI / SpaceXAI.

---

## Documentation

| Doc | Content |
|-----|---------|
| [User guide](crates/codegen/xai-grok-pager/docs/user-guide/) | Product how-to (CLI is `turbo`) |
| [MCP Server](crates/codegen/xai-grok-pager/docs/user-guide/32-mcp-server.md) | `turbo mcp serve`: roots, tiers, the boundary, tunnels, and the ChatGPT OAuth flow |
| [Changelog](./CHANGELOG.md) | RC14 + pedigree table |
| [Workspace Tree](docs/workspace-tree.md) | Atlas / inject / CLI |
| [Auto Developer Log](docs/AUTO_DEVELOPER_LOG.md) | Field logging for maintainers |
| [Feature Request Log](docs/FEATURE_REQUEST_LOG.md) | Missing-capability log |
| [Archive](docs/archive/) | Historical RC plans / Q&A (not product surface) |
| Official Grok Build | [docs.x.ai/build](https://docs.x.ai/build/overview) |

`SOURCE_REV` records the last monorepo sync point.

### Tracking remotes (for cherry-picks / Turbo vs Hyper compare)

| Remote | URL | Use |
|--------|-----|-----|
| `origin` | `danmsheets-dev/turbo-grok-build` | This Turbo fork (`dev` default) |
| `upstream` | `xai-org/grok-build` | Official Grok Build (fetch-only) |
| `community` | `DaviRain-Su/hyper-grok-build` | Hyper community (fetch-only) |

Turbo and Hyper share a common history but diverge after the rebrand. Use
fetch + log for compare; do not merge casually.

**Last sync: 1.0.0-rc.1.** Turbo merged xAI Grok Build **1.0.0** (`75e73f3d6`,
`SOURCE_REV` `a61c32b12…`) on `sync/1.0.0-rc1` (shared base `e5478eff1` /
0.2.119). The Turbo/Hyper fork point is `c260695cc` (2026-07-29); Hyper was
compared at `7a48dd755`. Anything already listed in the RC15 changelog under
"Deliberately NOT taken" was evaluated and declined — re-read that before
proposing it again. Note that two Hyper fixes Turbo carries (the circuit-breaker
probe race and the `auth.json` scope-lock question) exist **only inside Hyper's
merge commits**, so a `--no-merges` log will not show them; use
`git show --cc <merge>` when surveying.

```sh
git fetch origin
git fetch upstream
git fetch community
# Inspect without merging:
git log --oneline HEAD..community/dev | head
git log --oneline community/dev..HEAD | head
git rev-list --count HEAD..community/dev   # commits Hyper has that Turbo lacks
git rev-list --count community/dev..HEAD   # commits Turbo has that Hyper lacks
git show community/dev:VERSION
git show HEAD:VERSION
```

---

## Repository layout

| Path | Contents |
|------|----------|
| `crates/codegen/xai-grok-pager-bin` | Composition root → `turbo` binary |
| `crates/codegen/xai-grok-pager` | TUI |
| `crates/codegen/xai-grok-shell` | Agent runtime / subagents |
| `crates/codegen/xai-grok-developer-log` | Auto Developer Log store |
| `crates/codegen/xai-grok-agent` | Prompt assembly / Boot Card |
| `install.sh` / `install.ps1` | Release installers |
| `.github/workflows/release.yml` | Multi-target release CI |

> [!IMPORTANT]
> Root `Cargo.toml` is often treated as monorepo-generated — prefer editing
> per-crate manifests for durable local changes.

---

## License

Apache-2.0. See [`LICENSE`](LICENSE), [`NOTICE`](NOTICE), and
[`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES).

Based on [xai-org/grok-build](https://github.com/xai-org/grok-build).
**Turbo Build** is an independent community fork — not an official xAI product.
