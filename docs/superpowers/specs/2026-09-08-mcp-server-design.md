# `turbo mcp serve` — Turbo Build as an MCP server

Design record for exposing a bounded subset of Turbo's tools to external MCP
clients. Rewritten 2026-09-11 to describe what was built, and updated the same
day for the round-3, round-4 and round-5 audit fixes. Implementation plan and
progress:
`docs/superpowers/plans/2026-09-08-mcp-serve-completion.md`.

## Status (2026-09-11)

| Component | File | State |
|---|---|---|
| Containment boundary | `xai-grok-mcp-server/src/guard.rs` | Built |
| Filesystem-layer re-check and call scope | `src/read_confined_fs.rs` | Built |
| Private session folder | `src/private_dir.rs` | Built |
| Served toolset, resource bounds, shutdown | `src/toolset.rs` | Built |
| MCP annotations | `src/annotate.rs` | Built |
| `rmcp::ServerHandler` | `src/handler.rs` | Built |
| HTTP transport | `src/http.rs` | Built |
| Tunnel supervision (Cloudflare) | `src/tunnel.rs` | Built; live end-to-end test passes |
| Served grep policy | `xai-grok-tools/.../grok_build/grep/mod.rs` (`ServedGrepPolicy`) | Built |
| Served edit policy | `xai-grok-tools/.../grok_build/search_replace/mod.rs` (`ServedEditPolicy`) | Built |
| `turbo mcp serve` | `xai-grok-pager/src/mcp_serve_cmd.rs` | Built |
| User guide | `xai-grok-pager/docs/user-guide/32-mcp-server.md` | Built |

Not built, and not claimed anywhere: an `[mcp_server]` config section, a JSONL
audit log, `--tunnel openai` (OpenAI `tunnel-client`), and a TUI mount.

## Problem

Turbo was an MCP client only. Nothing let an external agent call into Turbo's
toolchain. The intended consumer is a ChatGPT Developer-mode custom app reached
through a tunnel, so that a ChatGPT subscription drives Turbo's tools. That path
uses OpenAI's documented connector mechanism and involves no subscription-OAuth
handling on Turbo's side.

## Decisions (with the operator)

| Decision | Choice |
|---|---|
| Direction | Inbound only. Unrelated to `openai-codex/*` outbound model access. |
| Approval model | Sandbox-trust: inside an approved root is allowed, outside is refused, no interactive prompt. |
| Structure | New crate `xai-grok-mcp-server`; first consumer is `turbo mcp serve`. |
| Protocol | `rmcp` 2.1 server features. |
| Tiers | `readonly` (default) and `edit`, which is for trusted clients only. **No shell tier**: shell operands cannot be enumerated and the bash tool self-enforces nothing. |
| Transport auth | `Authorization: Bearer`, constant-time compare via `subtle`. |
| Tunnel | `--tunnel none\|cloudflare`. |

## Why a separate crate, and rmcp features

`xai-grok-mcp` quarantines `rmcp` + `reqwest 0.13` for its client transports.
The server needs neither `auth` nor any reqwest transport. Verified with
`cargo tree -p xai-grok-mcp-server -e features`: active rmcp features are
`default` (`base64`, `macros`, `server`) plus `transport-streamable-http-server`.
`reqwest 0.13` does appear in this crate's tree, via `gcloud-storage` in
`xai-grok-tools`, unrelated to MCP.

## Why the boundary had to be written

`FinalizedToolset::call` runs one gate, `enforce_session_policy` ->
`PolicyParams`. `CompiledPolicy`, `confine_access_outside_root` and
`edit_target_protection` are driven from ACP session setup and never run on this
path. `ConfinedFs` guards writes only. The bash tool enforces nothing. On
Windows `xai-grok-sandbox` is a no-op.

`PolicyParams` cannot serve as the boundary: it loads `<cwd>/grok.toml` and
`<cwd>/.grok/policy.toml`, files the exposed agent could otherwise write, and
its overlay replaces deny lists rather than unioning them. The guard therefore
hard-denies writes to those files and relies on `PolicyParams` for nothing. The
policy still runs; its refusals are reported as the opaque refusal (see
Reporting).

## Architecture

```
client --HTTPS--> tunnel (optional) --HTTP/1.1--> 127.0.0.1:<port>/<secret>/mcp
  accept loop: 64 connections, headers within 10 s into at most 64 KiB
  require_bearer (401, reads no body)
  limit_concurrency (16 request slots, waits)
  limit_body (30 s -> 408; 8 MiB -> 413; 100,000 JSON values -> 413)
  mcp_entry: rmcp's StreamableHttpService on a spawned task that owns the slot
    rmcp Host check (loopback names, any port; else 403, missing Host 400)
    TurboMcpHandler (list_tools, call_tool)
      ServedToolset::call
        unknown-argument check          <- only advertised arguments
        permit (8, try) -> blocking-pool thread owning the permit, 90 s timeout
          PathGuard::check_call          <- argument check (bounded)
          PathGuard::canonical_walk_args <- walk targets rewritten to canonical form
          call scope (task-local) + ToolBridge::call,
          dropped if shutdown cancels it before the call has begun writing
            read_file / list_dir / grep / search_replace
              ReadConfinedFs           <- resolved-path re-check, documents,
                                          size, special files; records decisions
                ConfinedFs             <- write re-check
                  LocalFs
            grep: DenyReadGlobs         <- case-folded ripgrep excludes
                  ServedGrepPolicy      <- no ripgrep config, no link following,
                                           per-result check of the source file,
                                           exit status judged by kept output
            search_replace: ServedEditPolicy <- result size, detail count, no
                                                receipts, policy refusals as errors
        scope record -> Refused / Failed; policy denial -> Refused;
        else the tool's result
```

## Security model

### Argument checks (`guard.rs`, `toolset.rs`)

1. **Judge the string the tool uses.** Tools resolve
   `sanitize_model_path_arg(raw)`, which trims Unicode whitespace and strips
   quotes. `admit` refuses any argument the sanitiser would change, and any
   quote or invisible formatting character.
2. **Admit before I/O.** Relative paths, `~`, UNC and verbatim paths, `..`,
   alternate data streams, reserved device names (including `CONIN$`,
   `CONOUT$`, `CLOCK$`, superscript COM/LPT, and a stem with trailing spaces
   such as `CON .txt`), trailing dots or spaces and control characters are
   refused before any filesystem call.
3. **One spelling per path.** `.` segments, empty segments and a trailing
   separator are refused. `Path::components` drops them, but a walker builds
   child paths from the spelling it was given: a walk of `<root>/.aws/.` yields
   `.aws/./credentials`, which no name exclude matches. Arguments are limited to
   4,096 bytes and 256 segments; an over-long one is logged by length only.
4. **Only advertised arguments, and a bounded amount of checking.** The toolset
   refuses an argument the tool's live schema does not advertise, with a message
   that names it and lists the accepted ones; this also stops a client from
   satisfying a workspace policy's `confirm: true`. The guard examines at most
   64 strings in a call's path arguments, and a missing name in a folder of more
   than 4,096 entries is refused rather than compared with every entry for the
   Unicode fallback. The whole check runs on the call's blocking thread, under
   its permit and timeout, so it can neither occupy a runtime worker nor bypass
   the concurrency bound.
5. **Containment, lexical first.** The normalized argument must start with a
   spelling of a root the guard knows (the operator's absolutized spelling or
   the canonical one; letter case is ignored on Windows), or it is refused as
   `OutsideRoots` before any filesystem access, so a refusal never probes
   whether an outside path exists. The resolved path must then satisfy both
   `path_is_under_confine_root` and an exact prefix match of its canonical
   display form. The tools print paths under the canonical form, so that is
   the spelling judged and shown: a root is refused when the guard is built
   (`Reason::Inadmissible`), with the cause logged at error level, when that
   form keeps a verbatim prefix (a UNC share, a drive mapped to one included, or
   a path too long for the plain form), when `admit` would refuse every file
   under it (a quote, a colon inside a name, a reserved device name, a trailing
   dot or space, an invisible character), when its path is not valid Unicode
   (no JSON string can name it), or on Windows when it holds U+FFFD (grep
   refuses such paths there). Round 6 fell back to the operator's own spelling
   in the first two cases; tool output then named files in a spelling no client
   path could use.
6. **Over-broad roots.** Refused when the guard is built
   (`Reason::OverBroadRoot`): a filesystem root, a WSL drive mount
   (`/mnt/<letter>`), or any directory that is, by directory identity, an
   ancestor of a candidate home (so symlinks, junctions and bind mounts count),
   that a candidate home's canonical path lies under, or below which a
   candidate home is reachable under a tail of its own path (`<root>/Users/<u>`
   for `/Users/<u>`). Identity comes from `fstatat` with `AT_NO_AUTOMOUNT` on
   Linux (device and inode, without opening the directory, so a FIFO cannot
   block), from metadata on other Unix systems, and from a `same_file` handle on
   Windows; a root whose identity cannot be read is still judged by its path.
   Each root is stat'ed first, which mounts a root that is an automount point, so
   mountinfo and identities describe what it serves. `AT_NO_AUTOMOUNT` does not
   stop autofs mounting a name not yet looked up below an indirect map, so a home
   that looking at could mount is judged by its path alone and never probed: one
   below an `autofs` mount with nothing mounted on it yet, as spelled or once the
   links above it are followed one name at a time, with no later mount between
   that point and the home. A mounted key of an indirect map is judged like any
   other folder, so a second mount of one account's home is still recognised. Candidate homes are the current home; every folder
   beside it, when its parent is not a filesystem root, typed from the listing;
   on Unix, accounts in `/etc/passwd`, read as bytes line by line, with uid 0 or
   at least 1000; past 256 homes in one folder, that folder counts as one home
   and is recorded as crowded, and a root directly inside a crowded folder is
   refused too; on Linux, from `/proc/self/mountinfo` read as bytes (a
   non-UTF-8 line is skipped), every mount of a whole Windows drive (drvfs,
   `aname=drvfs`, or 9p/virtiofs from a drive letter, judged by its raw `path=`
   option or its unescaped source, plus the mount's subtree) with the profiles
   inside it, every mount of a drive's `Users` folder or of one profile, and
   every other path at which a home's filesystem is mounted again, worked out
   from mountinfo's device and root fields alone (a bind mount of the home or of
   a folder above it under any name, or a second mount of the same filesystem or
   subvolume), where an NFS mount is placed by the folder its source names, since
   NFS prints `/` as every mount's root and shares one superblock across a
   server's mounts; every mount of one folder directly inside a folder too
   crowded to list, which is an account's home under another name; and on macOS,
   every folder in `/Users` and
   `/System/Volumes/Data`. The guard is built on a blocking thread, so a stop
   signal still ends startup. This is a guard-rail, not a complete list of
   sensitive places.
7. **Hard deny** for every access: Grok homes (the user's and the default) by
   absolute prefix, `.grok`, `grok.toml`, `policy.toml`, credential stores (via
   `xai_grok_sandbox::is_sensitive_credential_store`), `.git-credentials`,
   `.kube/config`, `.cargo/credentials` and `.cargo/credentials.toml`,
   `.npmrc`, `.pypirc`, private keys named `id_rsa`, `id_dsa`, `id_ecdsa` or
   `id_ed25519`, Terraform state (`*.tfstate`, `*.tfstate.backup`), and dotenv
   files. Names compare case-insensitively. The name rules also run on the
   lexically normalised spelling a client sent, and the canonical target of
   every link under a root whose own name, or a folder it sits in, is refused
   for every access joins the hard-deny prefixes (the walk runs in both tiers).
   A link named like the first half of a two-name rule (`PAIR_RULE_NAMES`) is
   judged with each second name joined on, and a link that resolves to nothing
   contributes the target it names as well, so a client cannot create the file it
   would lead to. Links in the loader folders outside every root are followed
   too, bounded to 20,000 folders and depth 8: each Grok home and, in every home
   `~` can mean, `.agents`, `.claude` and `.cursor`, with the folders inside them
   the trust-marker and `CompatConfig` tables name. A Grok home's links lead to
   hard-denied locations, the others' to write-refused ones.
8. **Git metadata by content.** The git rules apply under any `.git` and any
   directory holding `HEAD`, `objects` and `refs`: writes and walks anywhere
   inside are refused; reads of `hooks/` and of any `config` or
   `config.worktree` inside are refused. A not-yet-existing `<name>.git/config`
   is refused by name.
9. **Auto-run files (writes; best effort).** Anything under `.agents`,
   `.claude`, `.claude-plugin`, `.cursor`, `.git-hooks`, `.githooks`,
   `.grok-plugin`, `.hooks`, `.husky`, `.idea`, `.vscode`, `.github/workflows`
   and `.github/actions`; a plugin's `hooks/hooks.json`; the names
   `.cursorrules`, `.gitignore`, `.ignore`, `.lsp.json`, `.mcp.json`,
   `.pre-commit-config.yaml`, `.rgignore`, `AGENT.md`, `AGENTS.md`,
   `CLAUDE.local.md`, `CLAUDE.md`, `extension.wasm`, `HEAD` and `plugin.json`;
   lefthook's configuration and local override in every format it reads; and
   any name starting with `.envrc`. The list is unioned at startup with
   `CompatConfig`'s agent filenames, rules directories and skill directories.
   Locations declared elsewhere are resolved once at startup and refused as
   prefixes:
   - git: every repository's hooks folders (its git directory's and, for a
     linked worktree, its `commondir`'s), `core.hooksPath` (relative to the
     folder git runs the hooks in) and every
     `include.path` and `includeIf.*.path` target (relative to the including
     file), followed recursively to depth 10 with a cycle guard, starting from
     the repository config, a worktree's `commondir` config and
     `config.worktree`, `GIT_CONFIG_GLOBAL`, `~/.gitconfig` and the XDG git
     config (honouring `XDG_CONFIG_HOME`) under every home `~` can mean (on
     Windows also `%HOME%` and `%HOMEDRIVE%%HOMEPATH%`, which Git for Windows
     uses), `GIT_CONFIG_SYSTEM`, `/etc/gitconfig`, and on Windows Git's
     `etc/gitconfig` under `ProgramFiles` and its config under `ProgramData`;
     `includeIf` conditions are not evaluated. The repositories are every folder
     the walk found a `.git` in, every folder whose content makes it a bare
     repository, and the worktree holding the root, found by walking its
     ancestors as git does (stopping where another filesystem starts unless
     `GIT_DISCOVERY_ACROSS_FILESYSTEM` says otherwise), so a root below a
     repository's top is covered too. Configuration is parsed as git parses it:
     a key may follow its section header, an unquoted `#` or `;` ends a value,
     quotes and the `\n`, `\t`, `\b` escapes are resolved, a backslash at the
     end of a line continues the value, and a `]` inside a quoted subsection does
     not close the header;
   - Turbo: `[plugins].paths`, and `[skills].paths`, `server_skill_dirs` and
     `bundled_skill_dirs` (a skills entry naming a file counts as its folder),
     at the top level and in every `[[version_overrides]]` and `[[campaigns]]`
     patch, in each Grok home's and the system folder's `config.toml`,
     `managed_config.toml` and `requirements.toml`, in the patches of
     `GROK_CAMPAIGNS_OVERRIDE`, and in `.grok/config.toml` at the root, every
     ancestor, and every folder under it; the `GROK_WORKSPACE_SERVER_SKILLS_DIR`
     and `GROK_WORKSPACE_BUNDLED_SKILLS_DIR` variables; local marketplaces in
     `.claude/settings.json` at the same places; and the `installLocation` of
     every marketplace in `~/.claude/plugins/known_marketplaces.json`; every
     `installPath` in `~/.claude/plugins/installed_plugins.json`; and, from
     Turbo's plugin install registry (`registry.json` in `[plugins].install_dir`
     from a global layer, or `installed-plugins` in a Grok home), each repo's
     `path`, each `Local` install's whole `source_path`, which refresh re-copies
     at every session spawn, and the install folder itself.
     `GROK_CAMPAIGNS_OVERRIDE` is read in the flat form Turbo uses, where every
     key but the campaign id belongs to the patch. Entries
     are expanded with Turbo's own `expand_env_vars_in_string` in the serve
     process; one that expands to nothing declares nothing, since a location of
     no names would be every location. An absolute entry is a prefix. Turbo resolves a relative entry
     against its process cwd, which can be any folder, so a relative entry
     becomes a `RelativeDeclaration`: its names, with leading `..` counted,
     match as a contiguous run of lowercase path names that starts at or below
     the configuration's folder (anywhere, for a global file). A `~/` entry is
     also joined to every home `~` can mean, `%USERPROFILE%` included, and taken
     literally as a folder named `~` (the plugins loader does not expand it), and
     the names after the last variable in an entry count from anywhere.
     Declarations are deduplicated, each configuration folder is canonicalized
     once however many entries it holds, and every declaration keeps the anchor
     and name count a check needs; past `MAX_DECLARED_LOCATIONS` (4,096) plugin,
     skill and marketplace locations the guard refuses the edit tier with
     `Reason::TooManyDeclarations` (what `.envrc` and git name is bounded by the
     1 MiB file limit and the walk instead);
   - direnv: files that `.envrc` loaders pull in, from `.envrc` files in a root
     and in every folder above it: `.`, `source`, `dotenv` and
     `dotenv_if_exists` (a file); `source_env` and `source_env_if_exists` (a
     file; a folder names its `.envrc`, refused by name already);
     `source_up`/`source_up_if_exists` with a file name (that name in every
     folder above, inside the root); `use flake` (`flake.nix` and `flake.lock`
     in a local flake's folder), `use nix` (its file, or `shell.nix` and
     `default.nix`) and `use devenv` (`devenv.nix`, `devenv.yaml`,
     `devenv.lock`). The file is split into commands the way a shell splits
     them: quotes and backslashes resolved, comments dropped, a backslash before
     a line break joining two lines, and `;`, `&`, `|`, `(`, `)` and line breaks
     ending a command. Leading shell keywords and grouping tokens are stepped
     past, the file is decoded lossily, and a linked `.envrc` counts;
   - links: the canonical target of every link under a root whose own name, or a
     folder it sits in, is refused for writes; of every link in a repository's
     hooks folder and in a `core.hooksPath` folder; of every link a loader meets
     on the way to a location declared relative to where Turbo runs (the declared
     folder itself, or any name above it), with the rest of the declared names
     joined on; and of every link that lies inside a location already refused,
     repeated until nothing new appears, since a location reached through one
     link can hold another.
   Folders under a root are found by one walk bounded to 50,000 directories and
   depth 12 that skips `.git`, `node_modules`, `target`, `.venv` and `venv`,
   follows no links, and runs in both tiers. It records every link (up to
   100,000), the repositories it meets, and the paths through a link that a name
   rule refuses. A link is judged by its own names only: whether a folder above
   it is a repository is answered once per folder and never through the link, so
   a link to an unreachable share or an automount point is never stat'ed. Every
   declaration file is read only if it is a regular file (a link to one is
   followed) of at most 1 MiB, so a link to a device or FIFO
   cannot exhaust or hang startup. `FOLDER_TRUST_MARKER_SAMPLES` names a sample
   file for every
   kind folder trust gates on; one test extracts the `hit!("kind")` literals
   from `folder_trust.rs` and fails on an uncovered kind or a sample that is not
   write-refused, and a pager test proves every sample is really detected by
   `repo_config_kinds`.
10. **Walks** (`grep`, `list_dir`) are a separate access kind: they cannot
    target git metadata, `.grok`, `.aws`, `.docker`, `.gnupg`, `.kube`, `.ssh`,
    a FIFO, socket or device (`Reason::SpecialFile`), a directory containing
    a Grok home that has no `.grok` component, or where a link named like one of
    those folders leads. A grep with no named target is
    checked against the first root, which is where it walks; `list_dir` requires
    its target. The toolset then rewrites every walk target to its canonical
    spelling, so the paths the walker builds are the ones the name rules see.
11. **Unicode fallback mirror.** For a missing target, siblings that match after
    U+00A0/U+202F normalization pass the same checks.
12. **Symlinks on writes.** A symlink left in the canonicalized form (a dangling
    link in the not-yet-created tail) refuses the write. Probing skips the drive
    prefix and root, which are drive-relative on Windows.
13. **Opaque refusals.** One message for every cause; the reason goes to the
    operator via `tracing` and the toolset's event observer. The CLI escapes and
    shortens the client's tool name (`log_preview`) before printing it, and
    throttles rejected-credential lines to one a second with a count of those
    held back; the transport's own trace of each rejection is at debug level, so
    `RUST_LOG=warn` does not flood the operator queue.

### Resolved-path checks (`read_confined_fs.rs`)

`read_file` and `search_replace` do all file content I/O through
`AsyncFileSystem`. The decorator, on the path actually opened:

- re-runs containment and hard-deny rules;
- refuses FIFOs, sockets and devices;
- refuses documents with `read_file`'s own predicate (`is_pdf_file` on the
  extension and leading bytes, or a `pptx` extension), so symlinks and the
  Unicode fallback cannot route a document to its parser;
- refuses whole-file reads and writes over `MAX_WHOLE_FILE_BYTES` (32 MiB); in
  a read-only call, refuses a whole-file read over `MAX_LINE_WINDOW_BYTES`
  (8 MiB) unless `read_file`'s own detector (`bytes_to_metadata` on the first
  64 KiB) finds an image, because `read_file` reads skill
  markdown whole and copies it several times; and serves line windows with its
  own bounded reader, refusing one over 8 MiB, so a newline-free file is never
  buffered whole;
- refuses any write in a call that already had something refused, because
  `search_replace` treats an unreadable file as absent and would recreate it;
- reduces errors from the layers beneath to their kind, because their text can
  name roots or the resolved target; the full text goes to the operator log.

Each decision is recorded in the call's `CallScope`, a task-local installed
around `ToolBridge::call`, which also carries whether the call only reads,
whether it has begun a write or delete, and whether one has completed. Tools rewrite these errors into their own text, so the
toolset reports the recorded decision and never scans tool output. Once the
call has written, a later document or size refusal is not recorded:
`search_replace` re-reads what it wrote to hash it, and a completed edit must be
reported as completed. A boundary refusal is always recorded. Every
`AsyncFileSystem` call the served tools make targets the call's own resolved
path (instruction discovery and rule loading use `tokio::fs`, and a served edit
keeps no receipt), so a recorded decision is always about the call's target.

Turbo's system reminders are off (`SystemRemindersEnabled(false)`). One of
them, skill discovery, runs after every read, listing and edit: it walks up
from the path toward the session's working directory, to the filesystem root
for a path outside the first root, and parses every skill file it finds with
`std::fs`, past every check, `.grok` folders included. Its front-matter reader,
which Turbo's own sessions share, now reads at most `MAX_FRONTMATTER_BYTES + 1`
bytes of a file rather than a whole first line, and a description taken from a
skill's body reads at most its first 64 KiB. Turbo's scheduler is not started:
the toolset passes a `SchedulerHandle` whose receiver is already dropped, so
`finalize` spawns no `SchedulerActor`, which would otherwise reload and rewrite
`.grok/schedules.json` in the first root, in either tier.

### Reporting (`toolset.rs`)

- A refusal recorded by the filesystem layer, or by the argument check (or its
  repeat after a failed edit), is the opaque refusal.
- A `ToolError` whose `details.code` is Turbo's `policy_denied` is the opaque
  refusal too: the policy engine's text can quote `grok.toml` or
  `.grok/policy.toml`, including a TOML parse error's offending line. With a
  `ServedEditPolicy` installed, `search_replace` reports its own policy checks
  on the resolved path (`deny_paths`, Grok-home credentials, and
  `max_diff_lines` on the real diff) the same way instead of as text. The
  operator event carries `Reason::WorkspacePolicy`; the detail is logged at
  warn level.
- A timed-out edit tells the client the edit may still be applied. A call that
  shutdown cancels says it changed no file; a call that has begun writing is
  not cancelled, because the file may already have changed.

### Recursive tools

`grep` runs ripgrep and does not read through `AsyncFileSystem`. It is bounded
three ways.

- **Name excludes.** `DenyReadGlobs` mirrors the name rules, with every letter
  rewritten as a two-case class because ripgrep globs are case-sensitive and the
  guard is not. grep appends them after any caller glob, so they win over
  caller globs and `.ignore` whitelists. A caller `glob` is validated as a
  pattern: no absolute, home-relative, drive or `..` forms.
- **Served policy.** The toolset installs a `ServedGrepPolicy` resource, which
  interactive sessions never have. With it, grep passes `--no-config` and
  removes `RIPGREP_CONFIG_PATH` (an operator config could add `--follow` or
  re-include files), passes `--no-follow`, marks ripgrep `kill_on_drop` so a
  cancelled call stops it, passes `--null` so every printed path ends in a NUL
  byte, passes `--no-messages` and `--no-ignore-messages` so ripgrep names no
  file it could not read and no ignore file it could not parse (one above the
  root included), and filters the output by the file each result came from: in content
  mode by tracking `--heading` blocks whose path runs to its NUL, in
  files-with-matches mode one NUL-terminated path per file, in count mode a
  NUL-terminated path and its count. A path is read whole, so a name holding a
  newline cannot pass for a path and a numbered line, and kept output is
  written back in the newline form the tool parses. A result is kept only if
  its file's path is valid UTF-8, holds no line break (the reply is read line
  by line, and no client could name it), holds no replacement character on Windows
  (ripgrep there prints an undecodable name with U+FFFD; elsewhere U+FFFD is an
  ordinary character), and
  `PathGuard::check_resolved(file, Read)` accepts it. Inside a content block,
  any line that is neither numbered nor a separator (such as ripgrep's
  binary-file notice, which names the file) may narrow the block's decision but
  never reopen a refused block. ripgrep writes a blank line before each file
  after the first; the filter holds it back and writes it only before the next
  kept file, so a dropped last block leaves no trailing blank line. Because
  ripgrep's exit status still counts
  dropped files, a filtered search that kept no output is reported exactly as
  one that matched nothing, so the reply does not reveal whether a refused file
  matched. The early-exit probe counts only kept output. ripgrep exits 2 when
  it could not read a file or parse an ignore file: with kept output the search
  counts as successful, and without it as no match, unless standard error is
  about the pattern, glob or file type the client sent (`rg: regex parse
  error`, `rg: error parsing glob '`, `rg: unrecognized file type`, `rg:
  compiled regex exceeds size limit`, `rg: the literal `, which a pattern that
  could match a line break produces and which says to retry with multiline),
  which quote only the client's input. A pattern or file type holding a NUL, or a
  pattern over 8 KiB, is refused by the argument check instead, since ripgrep
  cannot be given one; a spawn that fails anyway is answered with a fixed line
  that does not name where ripgrep is installed. A
  search of one named binary file prints only `<path>: binary file matches
  (...)` with no NUL; at the end of the output that line is judged by the path
  before the notice.
- **Canonical targets.** Walk targets reach grep in canonical form (argument
  check 10).

### Edits (`search_replace`)

The toolset installs a `ServedEditPolicy`. With it, `search_replace` computes the
size of the file an edit would produce from the match count before building it
and refuses one over 32 MiB (a small `replace_all` request could otherwise demand
gigabytes), skips the whole-file line diff unless a `max_diff_lines` policy
needs it and skips the line-count telemetry diff of the client's strings
altogether (diffing a large rewrite can take very long), builds edit details for
at most one replacement (each detail scans and copies from the whole file, and
the client never sees them), and records no receipt (`record_receipts: false`),
so no copy of a file's previous content is written anywhere. Under a
`max_diff_lines` limit, a served edit and the dispatcher's check of a served
call (`enforce_dispatch_with(.., bounded: true)`, chosen when the toolset holds
a `ServedEditPolicy`) count added lines with `line_diff_bounded`: it sets aside
the lines both texts share at the start and the end, counts what is left as all
removed and all added past 500,000 lines, and otherwise diffs for at most two
seconds before approximating. Its counts can only be higher than the smallest
diff's, so the limit still holds, and a refusal based on an inexact count says
so. Interactive sessions keep the exact `line_diff`. Neither check diffs at all
when no limit is set.

### Transport (`http.rs`)

- Loopback bind; bearer with `subtle::ConstantTimeEq`, the scheme name matched
  case-insensitively; 64-hex-character token; 32-hex-character secret path.
  `ServeHandle::addr` is the bound address and the URL is built from it.
  Dropping a `ServeHandle` stops its server.
- An own accept loop over hyper's HTTP/1 connection builder, not `axum::serve`,
  so an unauthenticated peer's cost is bounded: at most 64 connections (more
  are closed on accept), 10 s to finish the request headers (hyper also applies
  this to idle keep-alive connections), a 64 KiB read buffer (hyper answers 431
  beyond it or past 100 header fields, and 414 for an over-long request line).
- Order: authentication (no body read), a request slot (16, waiting), then the
  body is buffered within 30 s (408), up to 8 MiB (413 on a declared or
  streamed overflow), and refused if it holds more than 100,000 JSON values,
  counted as opening brackets and separators outside strings (413): parsing
  builds two trees at tens of bytes per value, so bytes alone do not bound
  memory. `DefaultBodyLimit` only affects axum extractors, and rmcp reads the
  raw body with an unbounded `collect()`.
- Each request runs on a spawned task that owns the request slot. In stateless
  mode rmcp's per-request loop never exits if the response receiver is dropped
  mid-call; owning the receiver on a spawned task means a client disconnect
  cannot strand it, and owning the slot means a disconnect cannot free a slot
  while rmcp still holds the request.
- rmcp's default `allowed_hosts` (`localhost`, `127.0.0.1`, `::1`, any port) is
  kept: another `Host` gets 403, a missing or malformed one 400. rmcp also
  answers 400 for an unknown `MCP-Protocol-Version` header or one that
  disagrees with the `initialize` request, and 415 for a body that is not a
  single JSON-RPC message (batches included). The Cloudflare tunnel passes
  `--http-host-header` so the Host check holds end to end. rmcp's Origin
  validation is off (empty `allowed_origins`); a browser request carrying
  `Authorization` forces a preflight, which gets `401` without CORS headers.

### Resource bounds and shutdown (`toolset.rs`, `http.rs`)

- Eight call permits, taken without waiting (busy refusal).
- Each call runs on a blocking-pool thread via `Handle::block_on`, and the
  permit moves into that closure, which also runs the argument check.
  `list_dir` walks synchronously inside an async body, which a tokio timeout
  cannot interrupt; on its own thread the client gets its timeout answer after
  `CALL_TIMEOUT` (90 s, above grep's 60 s WSL limit), the permit covers the work
  until it really ends, and a tool's child process is reaped by the tool rather
  than orphaned by a dropped future.
- `shutdown_gracefully` refuses new calls, stops accepting connections (open
  ones stay open), lets running calls finish for up to 10 s, cancels what is
  left (the tool future is dropped inside the call's `select!`, which also stops
  a served grep's ripgrep; a call that has begun writing is left to finish) and
  waits up to 5 s for their permits, then waits with no deadline for every call
  that has begun writing (the toolset tracks each running call's scope weakly),
  so a file is never left half-written, and stops tool processes. Only then
  does it ask open connections to close after their current request, through a
  token separate from the accept loop's, so each gets its full 15 s grace from
  that point. It waits, up to 15 s, until every request slot is back and every
  connection has closed, so a finished call's response leaves rmcp and is
  written, and only then ends requests still waiting in rmcp. A stop takes at
  most about 40 s, including up to 10 s for the accept loop to end, unless an
  edit is still writing. Each call's `CallScope` holds the shutdown token:
  once it is cancelled, `begin_write` refuses a call that has not begun writing,
  under the scope's lock, which `calls_writing` takes too, so no write begins
  after the wait's check, and the refused call reports that it changed no file.

### Process lifecycle (`mcp_serve_cmd.rs`, `tunnel.rs`)

- The tunnel is enrolled in the global process scope, bound to Turbo's death on
  Linux (`PR_SET_PDEATHSIG`), and spawned with `CREATE_NEW_PROCESS_GROUP |
  CREATE_NO_WINDOW` on Windows. `creation_flags` is a set, not an or, so both
  flags are applied together after `prepare`. It is started with `--config`
  naming an empty configuration file of its own (kept for the tunnel's life),
  so no `config.yml` from the operator's home or `/etc/cloudflared` applies, and
  with every `TUNNEL_*` environment variable removed, matched in any letter case
  because Windows treats variable names that way. An explicit `--tunnel-bin` is
  made absolute and must exist; there is no fallback. Otherwise `find_on_path`
  searches only absolute `PATH` entries. The helper is resolved before anything
  binds.
- SIGINT, SIGTERM and SIGHUP (Ctrl-C, Ctrl-Break and console close on Windows)
  are registered before startup: `run_with` takes registration and startup as
  separate steps, and a test proves the order. A stop during startup drops the
  startup future; a dropped `ServeHandle` or `RunningTunnel` stops what it
  owned. Logoff and shutdown are not registered: Windows delivers
  `CTRL_LOGOFF_EVENT` and `CTRL_SHUTDOWN_EVENT` only to console processes that
  have not loaded user32, and turbo.exe imports it, so those end Turbo like a
  forced kill.
- Teardown stops the tunnel first, so the public exposure ends at once, then
  runs `shutdown_gracefully`, so an edit in progress is not cut off part-way.
  For a console close, after which Windows ends the process within seconds,
  teardown also removes the session folder before draining. Another stop
  signal during teardown counts by its kind: `SIGHUP` (a closed terminal sends
  it twice), every signal for five seconds after a `SIGHUP` began the stop (a
  session manager tearing the session down sends its own), and a repeat of the
  first signal within a second are ignored; a
  console close removes the session folder and teardown goes on; any other
  drops teardown at once, after removing the session folder, so the operator is
  never held waiting on a slow edit. While serving, a one-second tick reports
  the rejected-credential lines the throttle held back once a burst is over, and
  the exit path reports any still held.
- The endpoint is written to standard output with its errors handled; if it
  cannot be written, the server is torn down and the command fails. Operator
  lines go to a bounded queue drained by one writer thread, and for `mcp serve`
  the binary gives its tracing subscriber a writer that queues each event there
  too: no request path or stop path writes to standard error itself, so a pipe
  nobody reads cannot block runtime workers, signal delivery or teardown. A full
  queue drops lines; each push takes the drop count atomically and reports it on
  the next line queued, and the exit flush writes a last line with any count
  still pending.
- A panic hook reaps every child (`kill_all`) and then removes the live session
  folders; every exit path of `run` also calls `kill_all`.

### Private state (`private_dir.rs`)

The session folder is a per-process `tempfile::TempDir`, removed on drop. On
Unix it is created with mode `0700` by `mkdir` itself. On Windows it gets a
protected DACL granting only the current user `FILE_ALL_ACCESS`, inherited by
its contents; creation fails if anything appeared in it before the DACL was
applied. (An inheritable `GENERIC_ALL` grant is split by Windows into an
effective and an inherit-only entry, so specific rights are used.) Live folders
are registered so the panic hook can remove them, because an abort skips the
destructor. A fixed shared name under `/tmp` let other local users pre-create
it and plant symlinks, and kept what tools wrote there outside every root
indefinitely. Served edits write nothing into it: `ServedEditPolicy` turns
receipts off.

## Residual risks

- Everything else inside an approved root is available; `edit` is code
  execution by proxy through ordinary source and build files, so it is for
  trusted clients only. Its auto-run list is best effort, and declared
  locations are read once, at startup: downloaded campaign patches and macOS
  device-management requirements are not read, variables are expanded in the
  serve process, and links past the walk's bounds are not seen.
- Paths are checked immediately before open, not atomically with it.
- A hard link inside a root to a file elsewhere passes every check, because its
  path is inside the root. Refusing files with several links would break pnpm
  and cargo stores. Only someone who can already create files inside the root
  can plant one; the served tools cannot create links.
- Image files (up to 32 MiB) are decoded when read, on untrusted bytes: a
  decoder crash would stop the server, and decoding can need far more memory
  than the file's size.
- A forced kill, a crash that is not a panic (out of memory, stack overflow),
  or a Windows logoff or shutdown leaves the empty session folder behind,
  private to the user.
- A tunnel provider terminates TLS and sees the token and file contents.
- On macOS a forcibly killed Turbo can leave `cloudflared` running (no
  parent-death equivalent).
- `list_dir` can reveal entry names inside a root, including hidden ones.
- The over-broad root check finds homes it can enumerate; a home that is
  neither listed nor beside the current one is not recognised, and neither is a
  home reachable inside a root only through a link or junction with a different
  name (on Linux, other mounts of a home's filesystem are found through
  mountinfo).

## Decided: OAuth 2.1 + RFC 9728 for the ChatGPT path

ChatGPT's custom-connector form discovers a server's OAuth metadata and
registers itself; a static `Authorization: Bearer` header is not a mode it
offers. The operator chose the standards-correct route (2026-09-11): implement
OAuth 2.1 as the MCP authorization specification requires, rather than a
secret-path-only mode whose credential would sit in every URL a proxy logs.

What that obliges this server to do, as an OAuth 2.1 resource server:

- serve OAuth 2.0 Protected Resource Metadata (RFC 9728). The resource
  identifier has a path, so the document lives at
  `/.well-known/oauth-protected-resource/<segment>/mcp`, and it must carry
  `resource` and at least one entry in `authorization_servers`;
- answer every 401 with
  `WWW-Authenticate: Bearer resource_metadata="<that URL>"`;
- validate that an access token was issued **for this server** (RFC 8707
  audience binding), reject anything else, and never pass a token downstream.

Hosting the authorization server here additionally obliges OAuth 2.0
Authorization Server Metadata (RFC 8414), authorization-code with PKCE (S256),
exact redirect-URI matching, refresh-token rotation for public clients, and
Dynamic Client Registration (RFC 7591), which is how ChatGPT obtains a client
id without the operator pasting one. Consent has no user database to draw on, so
it binds to console access: the operator approves a connection with a code Turbo
prints, the same trust boundary the bearer token has today. The tunnel supplies
the HTTPS the specification requires for authorization endpoints.

`rmcp` 2.1 ships OAuth support for the **client** side only
(`transport::auth`, behind its `auth` feature: `AuthClient`,
`AuthorizationManager`, credential and state stores, PKCE). It offers nothing
server-side, so the metadata documents, registration, authorization and token
endpoints are this crate's to write and to audit.

## OAuth design, and what it is trusted to do

The server keeps its bearer token exactly as it is: a client that can send a
header keeps working, and every route below `/<segment>/mcp` still requires a
credential. OAuth is added beside it, so the failure of one is not the failure of
the other.

### What is served, and where

| Endpoint | Auth | Purpose |
|---|---|---|
| `/.well-known/oauth-protected-resource/<segment>/mcp` | none | RFC 9728 metadata: `resource`, `authorization_servers`, `bearer_methods_supported: ["header"]`, `scopes_supported: ["mcp"]`. |
| `/.well-known/oauth-authorization-server` | none | RFC 8414 metadata: issuer, the three endpoints below, `code_challenge_methods_supported: ["S256"]`, `grant_types_supported: ["authorization_code", "refresh_token"]`, `token_endpoint_auth_methods_supported: ["none"]`. |
| `/oauth/register` | none | RFC 7591 registration. Public clients only, no client secret issued. |
| `/oauth/authorize` | console code | The consent step. Serves a minimal form; approves only on the code Turbo printed. |
| `/oauth/token` | PKCE | Authorization-code and refresh-token grants. |

These four sit **outside** the bearer layer, because a client has no credential
before it has a token. That is new unauthenticated surface reachable by anyone
who can reach the port, so each is bounded on its own (below) and audited as its
own attack surface before the ChatGPT path is called usable.

### The resource identifier

The `resource` a client names must be the URL it actually used, so metadata is
built from the public tunnel URL when a tunnel is running and from the loopback
URL otherwise. A token records the resource it was issued for, and every MCP
request checks it: a token minted for another server, or for the loopback URL and
replayed through the tunnel, is refused (RFC 8707 audience binding, which the MCP
specification makes a MUST).

### Consent, with no user database

Turbo has no accounts, so consent binds to the console, which is the same trust
boundary the bearer token already has: the operator sees the token on their own
terminal. Turbo prints the code when the server starts and `/oauth/authorize`
serves a form; the browser must return that code. The code is 8 characters from
a CSPRNG and compared in constant time.

Its window is five minutes measured **from the moment the approval page is
served**, not from process start: a window anchored to startup closes while the
tunnel is still coming up, and then refuses every correct code the operator ever
enters, with a restart that re-rolls the path segment, the bearer token, the
console code and the tunnel hostname as the only way out. Loading the page again
reopens the window, so a setup that takes a few attempts is not locked out.

Inside that window the code may be used more than once. That is deliberate: when
a tunnel reports its public URL the resource changes and every grant issued
against the old one is cleared, so a client has to authorize again, and a
single-use code would force a restart to do it. The window is what bounds the
code, not a use count. Without the code the endpoint approves nothing, so
reaching the endpoint is not enough to mint a token.

### Tokens

Opaque random strings, never JWTs: there are no signing keys to manage or leak,
and the server that issues a token is the only one that validates it. They live
in memory only and die with the process, exactly as today's bearer token does —
a restart invalidates everything, and nothing is written to disk. Access tokens
last one hour; refresh tokens rotate on every use (OAuth 2.1 requires rotation
for public clients) and are bound to the client and resource they were issued
for. A refresh token presented twice is treated as theft: both it and its
successor are revoked.

### Bounds on the unauthenticated endpoints

- Bodies on these routes are capped far below the 8 MiB MCP limit (64 KiB).
- At most 32 registered clients and 32 pending authorization codes at once, each
  entry expiring; past that, registration is refused rather than growing without
  bound.
- Authorization codes are single-use, expire in 60 seconds, and are bound to the
  client id, the redirect URI and the PKCE challenge.
- `code_challenge_method` must be `S256`; `plain` is refused.
- Redirect URIs must be `https` or a loopback host, matched exactly against what
  was registered, as OAuth 2.1 requires.
- Rejections are counted through the same throttled operator line as bad bearer
  tokens, so a scan does not flood the terminal.

### What this does not do

- It does not authenticate a *person*. Anyone holding the console code, or the
  bearer token, is the operator as far as this server is concerned.
- It does not make `--tunnel none` remotely usable: the specification requires
  authorization endpoints over HTTPS, which the tunnel edge provides and plain
  loopback does not. With no tunnel the flow is for local clients only.
- It does not widen what a token may do. Every call still passes the same guard,
  in the same tier; a token is a way in, not a permission.

## Testing

```
cargo test -p xai-grok-mcp-server --lib -- --test-threads=2
cargo test -p xai-grok-pager --lib mcp_serve_cmd -- --test-threads=2
cargo test -p xai-grok-tools --lib grep
cargo test -p xai-grok-tools --lib search_replace
cargo test -p xai-grok-tools --lib line_diff
```

CI runs the server and pager tests on Linux
(`.github/workflows/keep-features.yml`, which installs ripgrep first) and the
server tests on Windows (job `mcp-server-windows`, ripgrep from Chocolatey). The
grep tests assert that ripgrep ran and that hidden files were searched, so they
cannot pass vacuously. Symlink tests fail rather than silently pass on a host
that cannot create symlinks, unless `TURBO_ALLOW_SYMLINK_SKIP=1` is set.
Concurrency, timeout, disconnect, shutdown and swap-after-check tests hold calls
at test-only filesystem gates: an async gate, a gate that blocks its thread the
way synchronous tool work does, and an entry counter, so a test acts only once
a call has really reached the filesystem layer. The live Cloudflare test is
`#[ignore]`d and run manually.
