# MCP Server (`turbo mcp serve`)

`turbo mcp serve` exposes a small, bounded set of Turbo's file tools to an
external [Model Context Protocol](https://modelcontextprotocol.io) client. The
client supplies the reasoning; Turbo supplies the tools, confined to directories
you approve.

This is the reverse of `turbo mcp add`, which connects Turbo *to* other MCP
servers.

---

## Quick start

```bash
turbo mcp serve --root /path/to/project
```

Turbo prints the endpoint and waits:

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

Point your MCP client at the URL and configure it to send the header on every
request. Press **Ctrl-C** to stop.

The URL and the token together are a credential. Anyone holding both can use
the listed tools inside the listed roots. Both change every time the server
starts.

A client that cannot send a header of its own — ChatGPT is one — uses the
`OAuth:` address instead and asks you for the `Approve:` code. That code is a
credential too. See [Using it with ChatGPT](#using-it-with-chatgpt).

---

## Options

| Flag | Default | Meaning |
|---|---|---|
| `--root <PATH>` | *(required)* | An approved directory. Repeat for several. The server never adopts the current directory on its own. Relative paths are resolved against the directory you run the command from. See [Roots](#roots). |
| `--allow readonly\|edit` | `readonly` | What the client may do inside the roots. `edit` is only for a client you trust to change your code. See [Tiers](#tiers). |
| `--port <PORT>` | ephemeral | Loopback port to bind. |
| `--tunnel none\|cloudflare` | `none` | Start a public tunnel in front of the server. See [Tunnels](#tunnels). |
| `--tunnel-bin <PATH>` | search `PATH` | Location of the tunnel helper binary. A relative path is resolved against the current directory. Without this flag, only absolute `PATH` entries are searched. |
| `--json` | off | Print the endpoint as one JSON object: `url`, `public_url`, `bearer_token`, `oauth_metadata_url`, `oauth_approval_code`, `allow`, `roots`, `tools`. Cautions go to standard error. |

---

## Roots

Approve the narrowest directory that does the job. As a guard-rail against
approving far too much, startup fails if a root is:

- a filesystem root, such as `/` or `C:\`;
- a Windows drive mounted under WSL, wherever it is mounted (`/mnt/c`, or `/c`
  with `[automount] root = /`). A mount of a drive's `Users` folder, or of one
  profile, counts as the homes it holds; a mount of any other Windows folder is
  an ordinary directory;
- a home directory, or a directory that contains one. A root contains a home
  when it is the home or a folder above it, compared as folders rather than by
  name (so a symlink, junction or bind mount to one counts); when the home's
  real path lies inside the root's; or when the home is reachable inside the
  root under the end of its own path, as `<root>/Users/alice` is for
  `/Users/alice`. On Linux, a home also counts at every other place the
  filesystem holding it is mounted: a bind mount of the home or of a folder
  above it, under any name, or a second mount of the same disk or subvolume.
  On macOS, `/System/Volumes/Data` counts as a home.

Home directories, for this check, are yours; every folder beside yours, unless
your home sits directly under `/` as `/root` does; on Linux and macOS, the
accounts in `/etc/passwd` with user ID 0 or at least 1000; on macOS, every
folder in `/Users`; and under WSL, every Windows profile on a mounted drive.
Past 256 homes in one folder, that folder counts as one home, and so does every
folder directly inside it.

Startup also fails if no client path could name a file under a root. The tools
print paths under a root's real path, so that is the spelling judged, even when
you gave the root through a link. It fails when the real path has a quote
character, a colon inside a name, a reserved device name such as `aux` or
`con`, a name ending in a dot or space, an invisible formatting character, or a
name that is not valid Unicode; when, on Windows, it holds the replacement
character U+FFFD, which grep cannot tell from a name it could not decode; or
when it is on a network share (`\\server\share`, or a drive letter mapped to
one) or too long for Windows' plain spelling (about 260 characters). Turbo
prints the cause on standard error.

Checking the roots reads the disk, which can take a while when home folders are
on a network. A root that is itself an automount point is mounted first; a
home below an automount point nothing is mounted on yet is judged by its path
alone, so it is not mounted. Ctrl-C still stops Turbo while it checks.

This check is not a list of every sensitive place on the machine. Everything
inside an approved root is available to the client, apart from the
[always-refused files](#always-refused-even-inside-a-root).

The client must name files the way Turbo printed the root on its `Root:` line,
or the way you gave it to `--root` if that spelling passes the rules under
[Paths](#paths). Another spelling of the same folder, such as a path through a
symlinked parent or a Windows short name (`PROGRA~1`), is refused. On Windows,
letter case does not matter.

---

## Tiers

| Tier | Tools |
|---|---|
| `readonly` | `read_file`, `list_dir`, `grep` |
| `edit` | everything in `readonly`, plus `search_replace` |

**There is no shell tier.** A shell command's arguments cannot be checked
against the approved roots, and Turbo's command tool does not confine itself,
so serving it would give the remote client an unrestricted shell on your
machine.

Every tool advertises MCP annotations (`readOnlyHint`, `destructiveHint`,
`openWorldHint`) derived from the tool's kind.

### `edit` is for trusted clients only

Changing files in a code project is, in practice, a way to run code: the next
time you build, test, or run the project, edited build scripts, package
manifests and source files execute on your machine. `edit` refuses the files
that Turbo and other tools load or run **without** you doing anything (see
[Files refused for editing](#files-refused-for-editing)), but it cannot make
ordinary source edits safe. It is not a sandbox for a client you do not trust.
Turbo prints this caution whenever you start the `edit` tier.

Served edits keep no copy of a file's previous content: the client has no way
to undo with one. The server's session folder, which only your account can open
(mode `0700` on Linux and macOS, an owner-only access list on Windows), holds no
file content. It is deleted when the server stops, including after a panic and
when you close the console window on Windows. After a forced kill, a crash that
is not a panic (for example running out of memory), or a Windows logoff or
shutdown, the empty folder can remain in your temporary folder.

---

## What the boundary enforces

### Transport

- **Loopback only.** The server binds `127.0.0.1`, never `0.0.0.0`, and speaks
  HTTP/1.1.
- **Bearer token.** Every MCP request needs `Authorization: Bearer <token>`,
  compared in constant time — either the token Turbo printed, or an access token
  this server issued for this URL. A missing or wrong token gets `401` before any
  of the request body is read, and the `401` says where the OAuth metadata is.
- **Secret path.** The URL contains a random path segment. With a valid token, a
  wrong path gets `404`; without one every MCP path gets `401`, since the token
  is checked before any route is matched.
- **The OAuth endpoints answer first.** Five paths sit outside that check,
  because a client cannot be asked to authenticate at the endpoints that tell it
  how to authenticate: the two metadata documents, registration, the approval
  page and the token endpoint. Each is bounded on its own — bodies of at most
  64 KiB that must arrive within the same 30 seconds, caps on registered clients,
  pending codes and issued grants, and every refusal reported to you on the same
  throttled line as a bad bearer token.
- **Host header.** `Host` must name `127.0.0.1`, `localhost` or `::1`, on any
  port. Any other host gets `403`, which defends against DNS rebinding; a
  missing or malformed `Host` gets `400`.
- **Connections.** At most 64 connections are open at once; more are closed as
  they arrive. A connection has 10 seconds to send its request headers, which
  may total about 64 KiB (`431` beyond that, or `414` when the request line
  alone is too long).
- **Request bodies.** At most 16 authenticated requests are handled at once;
  more wait their turn. A body must arrive within 30 seconds (`408`), be at
  most 8 MiB whether or not it declares its length (`413`), and hold at most
  100,000 JSON values (`413`).
- **Browsers.** A cross-origin browser request must send a preflight first;
  the preflight carries no token, gets `401`, and receives no CORS permission.
- **Arguments.** A call may use only the arguments its tool advertises; any
  other argument is refused with a message naming it. A call with more than 64
  strings in its path arguments is refused.
- **Concurrency.** At most 8 tool calls run at once; further calls are refused
  as busy. Checking a call's paths counts as part of the call.
- **Timeout.** The server stops waiting for a call after 90 seconds and tells
  the client so. A call that is still running keeps its place among the 8 until
  the tool actually finishes, and a client that disconnects mid-call keeps its
  request slot until the call ends, so abandoned work cannot pile up.

### Paths

- **Absolute paths only.** Relative paths, `~`, `..` segments, UNC and `\\?\`
  paths, NTFS alternate data streams, reserved device names (`CON`, `NUL`,
  `CONIN$`, `CON .txt`, ...), trailing dots or spaces, control characters,
  invisible formatting characters, quote characters, and leading or trailing
  whitespace are all refused before any file is touched.
- **One spelling per path.** `.` segments (`/a/./b`), doubled separators
  (`/a//b`) and a trailing separator (`/a/b/`) are refused, as are paths over
  4,096 bytes or 256 segments. Name a directory without a trailing separator.
- **Roots bound reads and writes.** A path outside every approved root is
  refused, whether it is being read or written, and whether or not it exists.
- **Checked again at the file actually opened.** `read_file` and
  `search_replace` resolve symlinks and fall back to Unicode look-alike
  filenames before opening a file. The resolved path is checked again at that
  point, so a symlink inside a root cannot be used to read or write outside it.
  Documents, oversized files and special files (below) are recognised there too.
  A missing name in a folder of more than 4,096 entries is refused rather than
  compared with every entry.
- **Symlinks on writes.** Writes are checked against the path with symlinks
  resolved. A write through a dangling symlink, whose target cannot be
  resolved, is refused.
- **No write after a refused read.** If a call was refused reading a file, the
  same call cannot then write it, so an edit can never recreate a file it was
  not allowed to see.
- **Names count as written.** A path is also judged by the names it is spelled
  with, so a link named like a refused file or folder (`.env`, `.envrc`, a
  `.claude` folder) is refused under that name, wherever it leads. What such a
  link inside a root leads to is refused under its own name too: for every
  access when the link's name is always refused, for a walk when its name is one
  no walk may target (`.ssh`, `.gnupg`, `.aws`, `.docker`, `.kube`, git
  metadata), and for writes when it is refused for editing. A link named like the first half of a rule that needs two
  names (`.kube`, `.aws`, `.docker`, `.cargo`, `.github`, `hooks`, `.git`) is
  judged with the second name joined on, so what `.kube/config` leads to is
  refused as well. A link that leads nowhere yet refuses the target it names too,
  so the file it will lead to cannot be created. A link below a folder another
  link leads to is followed as well, and so are the links in the folders Turbo
  loads from outside the roots (your Grok home, and `.agents`, `.claude` and
  `.cursor` in your home directory) when they lead into a root.
- **No skill loading.** In a Turbo session, a tool that reads, lists or edits a
  path also loads skill files from the folders above it, which can lie above a
  root or inside folders these rules refuse. The server turns that off. It also
  starts no scheduler, so no `/schedule` job is run, or recorded as run, while
  it serves.

### Always refused, even inside a root

| Category | Locations |
|---|---|
| Grok configuration | your Grok home (`$GROK_HOME` and `~/.grok`), any `.grok` directory, any `grok.toml` or `policy.toml` |
| Credentials | `.ssh`, `.gnupg`, `.aws/credentials`, `.docker/config.json`, `.kube/config`, `.cargo/credentials` and `.cargo/credentials.toml`, `.netrc`, `_netrc`, `.git-credentials`, `.npmrc`, `.pypirc`, private keys named `id_rsa`, `id_dsa`, `id_ecdsa` or `id_ed25519` (their `.pub` public keys are not refused), and Terraform state (`*.tfstate`, `*.tfstate.backup`) |
| Environment secrets | `.env` and `.env.*`, except `.env.example`, `.env.sample`, `.env.template` and `.env.dist` |
| Git metadata | writing anywhere inside it; reading its hooks and any `config` or `config.worktree` inside it. Git metadata is any `.git` directory and any directory git would open as a repository (one holding `HEAD`, `objects` and `refs`), whatever it is named. |
| Documents | PDF and PowerPoint files are not read, whatever the requested name: the decision is made on the file actually opened and its first bytes. Their parsers run on untrusted bytes, and release builds stop the whole process if a parser crashes. |
| Large files | a file over 32 MiB is not read whole or written, and an edit that would produce one is refused. A read, in either tier, returns a file read whole (such as a skill file) only up to 8 MiB unless `read_file` recognises the file as an image, and a single line window over 8 MiB is never returned. Ordinary text files of any size are still read in line windows. |
| Special files | FIFOs, sockets and devices are not opened, and a search or listing aimed at one is refused. |

Names in these rules match in any letter case.

### Files refused for editing

The `edit` tier also refuses to write the files that Turbo, other agents,
editors, git hook managers and plugin loaders load or run without you doing
anything:

- anything under `.agents`, `.claude`, `.claude-plugin`, `.cursor`,
  `.git-hooks`, `.githooks`, `.grok-plugin`, `.hooks`, `.husky`, `.idea`,
  `.vscode`, `.github/workflows` or `.github/actions`;
- `AGENTS.md`, `AGENT.md`, `CLAUDE.md`, `CLAUDE.local.md`, `.cursorrules`,
  `.mcp.json`, `.lsp.json`, `.pre-commit-config.yaml`, `.ignore`, `.rgignore`,
  `.gitignore`, `plugin.json`, `extension.wasm`, `HEAD`, and a plugin's
  `hooks/hooks.json`;
- lefthook's configuration and local override (`lefthook.yml`,
  `.lefthook-local.toml`, and the other names and formats lefthook reads);
- `.envrc` and any file whose name starts with `.envrc`, and every file an
  `.envrc` in a root, or in a folder above it, loads: through `source`, `.`,
  `source_env`, `dotenv` and their `_if_exists` forms, also after words such as
  `if` and `then`; the file `source_up` looks for in the folders above; and
  `flake.nix` and `flake.lock`, `shell.nix` and `default.nix`, or the devenv
  files, for `use flake`, `use nix` and `use devenv`. A linked `.envrc` counts,
  and a byte that is not UTF-8 does not hide the lines around it;
- plugin and skill folders declared by `[plugins] paths`, or by `[skills]
  paths`, `server_skill_dirs` or `bundled_skill_dirs`, in `.grok/config.toml`
  at a root, in any folder above it or under it, in your Grok home's
  `config.toml`, `managed_config.toml` or `requirements.toml`, or in the system
  ones (`/etc/grok`), including the `[[version_overrides]]` and `[[campaigns]]`
  patches in those files and in `GROK_CAMPAIGNS_OVERRIDE`; local marketplaces
  declared in `.claude/settings.json` at those same places; and the
  marketplaces recorded in `~/.claude/plugins/known_marketplaces.json`; the
  plugins recorded in `~/.claude/plugins/installed_plugins.json`; and the
  sources, snapshots and install folder recorded in Turbo's own plugin install
  registry (`registry.json` under `[plugins] install_dir`, or
  `installed-plugins` in your Grok home), because Turbo re-copies a local
  install's source folder and loads it again at every session. Entries
  are expanded the way Turbo expands them (`$VAR` and `${VAR}`, from the
  server's own environment, and `~/`). Turbo reads a relative entry from the
  folder it runs in, which can be any folder, so a relative entry counts
  wherever its names appear under a root, at or below the folder the entry's
  leading `..` steps reach from the configuration's own folder (that folder
  itself when the entry has none), for a project configuration. So do the names after a variable, and a
  `~/` entry also counts as a folder named `~`, which is how loaders that do not
  expand it read it;
- every repository's hooks folder, the git hook directory named by
  `core.hooksPath`, and every file pulled in by `include.path` or `includeIf`,
  following includes of includes, in the repository's configuration and in your
  global and system git configuration (on Windows, the global configuration is
  also looked for under `%HOME%`, as Git for Windows does). This covers every
  repository under a root, a bare repository inside one, and the repository a
  root is a folder inside, since git runs that repository's hooks for a commit
  made anywhere in it. Values are read as git reads them: an unquoted `#` or `;`
  starts a comment, quotes and backslash escapes count, a backslash at the end of
  a line continues the value, and a key can follow its section header.
  `includeIf` conditions are not evaluated: every one counts. What a link in a
  repository's hooks folder, or in the `core.hooksPath` folder, leads to is
  refused too.

This always covers every instruction file, rules directory and skill directory
Turbo itself loads, and every repository file Turbo's folder trust treats as
code.

The list is **best effort**. Declared locations (`.envrc` sources, plugin and
skill paths, git hooks and includes) are read once, when the server starts,
only from regular files of at most 1 MiB, and the search under a root, for
declarations and for links, stops after 50,000 folders or 12 levels. At most
4,096 plugin, skill and marketplace locations are kept: a configuration naming
more makes the `edit` tier refuse to start, so the command fails and says so on
standard error, and the same roots still serve read-only. What an `.envrc` or a
git configuration names is bounded instead by the 1 MiB limit on each file and
by that same walk. The search for links in the folders Turbo loads from outside
the roots stops after 20,000 folders. Campaign
patches Turbo downloads, and on macOS requirements set by device management,
are not read, and a variable can hold something else when Turbo runs. No list
can cover every tool that runs code from a project, which is why `edit` is for
trusted clients only.

### Directory walks

`grep` and `list_dir` recurse, so a rule that only looked at the path you name
would not stop them from reaching a denied file further down. Therefore:

- `grep` and `list_dir` cannot be pointed at git metadata, `.grok`, `.ssh`,
  `.gnupg`, `.aws`, `.docker` or `.kube`.
- When `grep` is given no path, it searches the first root, and that search is
  checked the same way. `list_dir` always needs a directory.
- `grep` never returns a result from a file the server would refuse to read,
  however it reached that file, and its reply is the same whether or not such a
  file matched, including when other files matched too. It also excludes every
  always-refused name while it searches, in any letter case, ahead of `.ignore`
  and `.gitignore` rules; it ignores your ripgrep configuration file; it does
  not follow symlinks; and it returns nothing from a file whose name is not
  valid UTF-8, whose path holds a line break, or, on Windows, whose path holds
  the replacement character U+FFFD, which ripgrep prints for a name it cannot
  decode. Paths are read whole, so a folder name holding a newline cannot pass
  for a matching line. ripgrep's messages about files it could not read, or
  ignore files it could not parse, are never shown: a search that met one still
  returns what it found. Only an error in the pattern, glob or file type you
  gave is reported, including the one for a pattern that could match a line
  break, which says to search again with `multiline`. A pattern or file type
  holding a NUL, and a pattern, file type or glob over 8 KiB, is refused
  instead: ripgrep cannot be given one.
- A directory that contains a relocated Grok home (one whose path has no
  `.grok` component) cannot be searched recursively.
- `list_dir` can show the **names** of entries inside a root, including hidden
  ones. It does not show their contents.

### Refusals

Every refusal returns the same message, so a client cannot use refusals to
learn whether a file exists or where your home directory is. Turbo prints the
specific reason to its standard error, for example:

```text
refused: tool="read_file" reason=OutsideRoots
refused: tool="search_replace" reason=WorkspacePolicy
rejected: request without a valid bearer token
```

The tool name is the client's text, so Turbo escapes it and cuts it short.
Rejected tokens are reported at most once a second; the ones not shown are
counted on the next such line, on a line of their own once the rejections stop,
or when Turbo exits. One thread writes these lines, and Turbo's own log messages go
through it too: if nothing reads Turbo's standard error, lines are dropped
rather than holding up the server, and the next line written, or a last line
when Turbo stops, says how many were not shown.

Turbo's workspace policy (`grok.toml` or `.grok/policy.toml` in the first root)
still applies to served calls, including the checks `search_replace` makes on
the file a path resolves to. Its refusals return the same message and are
printed as `reason=WorkspacePolicy`; start Turbo with `RUST_LOG=warn` to see the
policy's own message. A policy that requires confirmation refuses every served
edit, since a remote client has no way to confirm one. Under a `max_diff_lines`
limit, counting the lines a served edit adds is limited in time and memory: an
edit too large to count exactly is judged by an upper bound, and the policy's
message says so. A line ends at a newline, at `\r\n`, or at a lone carriage
return, as Turbo's diff counts lines.

### `--confine`

Under `turbo --confine A mcp serve`, a `--root` outside `A` is refused at
startup. The check is repeated against the roots the server actually adopts.

## What it does not protect against

- **Everything else inside an approved root is available.** Approve the
  narrowest directory that does the job.
- **Edits are code.** The `edit` tier lets the client change code you will run,
  and its list of refused files is best effort.
- **Check-then-open timing.** Paths are checked immediately before the tool
  opens them, not atomically with the open. A local process that can change
  files inside the root while the server runs could race a check.
- **Hard links.** A hard link inside a root to a file elsewhere is served like
  any other file in the root, because its path is inside the root. Package
  managers create hard links routinely, so they are not refused. The served
  tools cannot create links; only someone who can already write inside the root
  can plant one.
- **Image decoding.** Image files are still decoded when read. A crafted image
  can make the decoder use far more memory than the file's size, or crash it,
  which would stop the server.
- **What the client and the tunnel see.** File contents the client reads leave
  your machine and are handled under that client's own terms. A tunnel provider
  can see them too (see below).

---

## Tunnels

A remote client, such as a hosted AI service, can only reach your machine
through a public HTTPS address.

```bash
turbo mcp serve --root /path/to/project --tunnel cloudflare
```

This starts a [Cloudflare quick tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/do-more-with-tunnels/trycloudflare/)
with `cloudflared`, which must be installed. Turbo adds a `Public:` line to the
output. The token is still required at the public address.

- **Cloudflare can read the traffic.** TLS for the public address ends at
  Cloudflare, so Cloudflare sees the token, the secret path and every file the
  client reads. Turbo prints this caution whenever a tunnel is running.
- If `cloudflared` is missing, or reports no address within 45 seconds, startup
  fails. Turbo never falls back to a different transport. A missing helper is
  noticed before the server binds its port.
- Your own cloudflared setup does not shape this tunnel. `cloudflared` reads an
  empty configuration file of Turbo's, so a `config.yml` in `~/.cloudflared` or
  `/etc/cloudflared` does not apply, and it starts without any `TUNNEL_*`
  variable from your environment, in any letter case.
- Quick-tunnel addresses change on every start.
- The public address is reachable by anyone on the internet who has it. Treat
  it, together with the token, like a password.

### Stopping

Ctrl-C (and Ctrl-Break on Windows, or `SIGTERM` and `SIGHUP` on Linux and
macOS) stops the server:

1. The tunnel is stopped, so the public address goes away at once.
2. New calls are refused and new connections are no longer accepted.
3. Running calls get up to ten seconds to finish. Calls still running then are
   cancelled and get up to five seconds more to stop; a cancelled call's reply
   says it changed no file. An edit that has begun writing a file is not
   cancelled: Turbo waits for it to finish however long that takes, so the file
   is never left half-written.
4. Any process a tool started is stopped.
5. Connections get up to fifteen seconds to deliver the replies. A local client
   receives its result; a client behind the tunnel does not, because the
   tunnel is already gone.

Stopping takes at most about 40 seconds, unless an edit is still writing a
file. Once calls are cancelled, no edit begins writing. Press Ctrl-C again to
stop at once without waiting for anything; an edit still writing can then be
left incomplete. A repeat of the first signal within a second counts as the
same one, and so does `SIGHUP`, which a closed terminal sends more than once.
When a hangup started the stop, every signal but a console close is ignored for
five seconds after it, so a session manager tearing the session down does not cut
the stop short.
Ctrl-C also works while Turbo is still checking its roots at startup.

On Windows, closing the console window leaves Turbo only a few seconds, so it
stops the tunnel and deletes the session folder first, then stops as above for
as long as Windows allows, including when an earlier Ctrl-C is still stopping.
Logging off or shutting down is different: Windows does not tell Turbo, and
Turbo ends as if it were killed (below).

If Turbo panics, it still stops the tunnel and deletes its session folder. If
it is killed outright, or crashes without a panic (for example when it runs out
of memory), the tunnel still stops on **Windows** (the process is in a job that
closes with Turbo) and on **Linux** (the kernel signals it when Turbo exits),
but the empty session folder can remain. On **macOS** there is no equivalent:
`cloudflared` can keep running until you stop it.

### Your own tunnel or reverse proxy

With `--tunnel none`, you can front the loopback URL yourself. Your proxy must
rewrite the `Host` header to `127.0.0.1:<port>`; the server refuses any other
host with `403` to defend against DNS rebinding.

---

## Using it with ChatGPT

ChatGPT Developer mode connects to a custom MCP app by discovering the server's
OAuth metadata and registering itself, so a static bearer token is not a path it
offers. The server implements that flow: OAuth 2.1 with protected resource
metadata (RFC 9728), authorization server metadata (RFC 8414), dynamic client
registration (RFC 7591), PKCE (`S256` only) and audience-bound tokens (RFC 8707).

A client that sends its own `Authorization: Bearer` header still works exactly as
before. The two are alternatives, not replacements: the bearer token Turbo prints
keeps working while OAuth is available.

### How the approval works

Turbo has no accounts, so approving a connection binds to the terminal Turbo is
running in. Start the server with a tunnel, since the specification wants the
authorization endpoints over HTTPS:

```bash
turbo mcp serve --root /path/to/project --tunnel cloudflare
```

Give ChatGPT the **public** URL. It fetches the metadata, registers itself, and
sends you to an approval page. That page asks for the `Approve:` code from
Turbo's own output — without it nothing is issued, so reaching the page is not
enough to obtain a token.

The code is good for five minutes **from the moment the approval page is
served**, not from when the server started. If it lapses, load the page again:
that reopens the window, and the same code still works. Inside the window the
code may be used more than once, which is deliberate — when a tunnel reports its
public URL every token issued against the old one stops being accepted, so a
client has to authorize again, and a single-use code would force a restart.

### What a token is worth

An access token lasts an hour and is refreshed by the client; a refresh token is
replaced each time it is used, and presenting a spent one is treated as theft,
revoking what it became and reporting it on Turbo's standard error.

A token is bound to the URL it was issued for. One minted before a tunnel came up
is not accepted afterwards, and a token from another server is never accepted at
all. None of this widens what a client may do: every call still passes the same
boundary, in the tier you started, inside the roots you approved.

---

## Troubleshooting

| Symptom | Cause |
|---|---|
| `invalid roots: a root may not be a filesystem root, a home directory, ...` | Approve a project directory instead of `/`, a drive root, a home directory, or a folder that contains one. See [Roots](#roots). |
| `invalid roots: no client path could name a file under this root` | The root's real path cannot be named by a client: it has a quote character, a colon inside a name, a reserved device name, a name ending in a dot or space, an invisible character, a name that is not valid Unicode, or on Windows the replacement character U+FFFD, or it is on a network share or too long for Windows' plain spelling. Turbo prints the cause just above. Rename or move the folder; for a share, work on a local copy. See [Roots](#roots). |
| `invalid roots: the configuration under these roots declares more plugin, skill and marketplace locations ...` | The `.grok` and `.claude` configuration under the roots names more than 4,096 plugin, skill and marketplace locations, which the `edit` tier cannot check every write against. Serve the same roots read-only, or trim the declarations. |
| The connection closes with no response | The request headers took more than 10 seconds, or 64 connections were already open. |
| `400 Bad Request` | The `Host` header is missing or malformed, the request body could not be read, or the `MCP-Protocol-Version` header names a version the server does not know or disagrees with the `initialize` request. |
| `401 Unauthorized` | On an MCP path: the `Authorization: Bearer` header is missing, or the token is wrong, or an OAuth access token has passed its hour, or it was issued for a different URL than the one the request arrived at. The token changes on every start. The response names the OAuth metadata address. |
| `{"error":"access_denied"}` from the approval page | The `Approve:` code was wrong, or its window had closed. Load the approval page again — that reopens the window — and enter the code from Turbo's output. |
| `{"error":"temporarily_unavailable"}` | More grants or pending authorization codes are outstanding than the server keeps at once. Retry shortly. |
| `403 Forbidden` | The `Host` header is not `127.0.0.1`, `localhost` or `::1`. A self-hosted proxy must rewrite it to `127.0.0.1:<port>`. |
| `404 Not Found` | The URL path is wrong. Copy the whole URL, including the random segment. |
| `405 Method Not Allowed` | The server accepts `POST` only. |
| `406 Not Acceptable` | The client's `Accept` header must include both `application/json` and `text/event-stream`. |
| `408 Request Timeout` | The request body did not finish arriving within 30 seconds. |
| `413 Payload Too Large` | The request body is over 8 MiB or holds more than 100,000 JSON values. |
| `414 URI Too Long` | The request line is longer than the server reads (about 64 KiB). |
| `415 Unsupported Media Type` | `Content-Type` is not `application/json`, or the body is not a single JSON-RPC message: malformed JSON, or a batch, which the server does not accept. |
| `431 Request Header Fields Too Large` | The request headers exceed about 64 KiB or 100 fields. |
| A tool call returns "the request is not permitted" | The path is relative, spelled with `.` segments, doubled or trailing separators, spelled differently from the printed root, outside every root, or in an always-refused location, or the workspace policy refused the call. Turbo prints the reason on its standard error (`WorkspacePolicy` for the workspace policy). |
| "unknown argument ..." | The call used an argument the tool does not advertise. The message lists the ones it accepts. |
| "the server is busy" | Eight calls are already running, possibly including calls the client stopped waiting for. Retry shortly. |
| "the tool call timed out" | The call ran for more than 90 seconds. For an edit, the change may still be applied: read the file before retrying. |
| "the server is shutting down" | Turbo is stopping. Calls are refused from the moment it starts to stop, and a call it cancelled says it changed no file. |
| "document formats ... are not available" | PDF and PowerPoint files are not served. |
| "the file is too large to read or change over this MCP server" | The file is over 32 MiB and had to be read whole (an image, or a file being edited), a line window is over 8 MiB, or a file read whole for a read (such as a skill file) is over 8 MiB and is not an image. |
| "this edit would make ... larger than the 32 MiB this server edits" | A replacement, usually a `replace_all`, would produce a file over 32 MiB. Replace fewer occurrences at a time. |
| "only regular files and directories can be opened" | The path is a FIFO, socket or device. |
| `cloudflared not found on PATH` | Install `cloudflared`, or pass `--tunnel-bin <PATH>`. |
| `cloudflared not found at <PATH>` | The `--tunnel-bin` path does not exist. A relative path is resolved against the directory you ran Turbo from. |
| `--root ... is outside the inherited --confine root` | Turbo was started with `--confine`; every `--root` must be inside it. |
