# Windows release binaries

Prebuilt Windows (x86_64) builds ship as **GitHub Release** zip archives plus
`SHA256SUMS` (not in the git tree — ~150 MB is over the regular git object
limit, and public-fork LFS uploads are blocked on GitHub).

`install.ps1` and `turbo update` require **exactly** these names:

| Wire version | Release | Asset |
| --- | --- | --- |
| `1.0.13-rc.4` | [v1.0.13-rc.4](https://github.com/danmsheets-dev/turbo-grok-build/releases/tag/v1.0.13-rc.4) | `turbo-1.0.13-rc.4-x86_64-pc-windows-msvc.zip` |
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

A raw `.exe` on the release is a convenience copy only; auto-update ignores it.

## Install

```powershell
irm https://github.com/danmsheets-dev/turbo-grok-build/releases/latest/download/install.ps1 | iex
turbo update --check
# → turbo 1.0.13-rc.3 (latest: 1.0.13-rc.3) [stable]
```

Or pin: `.\install.ps1 -Version v1.0.13-rc.3`

The zip must contain a root-level `turbo.exe` plus `bundled/`. The installer
activates the binary at `%USERPROFILE%\.turbo\bin\turbo.exe` and the bundle at
`%USERPROFILE%\.grok\bundled`.

## Verify

```text
turbo version
# → turbo 1.0.13-rc.3 (<commit>)
```

## Local packaging note

`release.yml` on tag `v*` builds `turbo-<version>-x86_64-pc-windows-msvc.zip`.
Do not pass `/DEBUG:LongSymbolTruncate` (GitHub `link.exe` 14.44 → LNK1117).
The path `releases/windows/*.exe` is gitignored; this README stays tracked.
