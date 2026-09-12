//! The escape matrix.
//!
//! These tests are the product. `guard.rs` and `read_confined_fs.rs` are the
//! only boundary between an external MCP client and the operator's filesystem,
//! so each case here stands in for a way that boundary could be wrong.
//!
//! Cases prefixed `audit_` were added after an adversarial audit confirmed a
//! real hole.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use xai_grok_tools::types::tool::ToolKind;

use crate::guard::testing::{is_lefthook_config, lexical_normalize, read_git_config};
use crate::guard::{
    Access, Denial, FOLDER_TRUST_MARKER_SAMPLES, PathGuard, REFUSAL_TEXT, Reason, admit,
    candidate_homes, declared_path_fields, fold_glob_case, is_over_broad_root, log_preview,
    symlink_probe_paths, validate_tool_schema,
};

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn fixture() -> (PathGuard, tempfile::TempDir, tempfile::TempDir) {
    let root = tempfile::tempdir().expect("root tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let guard = PathGuard::new(vec![root.path().to_path_buf()], Vec::new(), false)
        .expect("guard with one root");
    (guard, root, outside)
}

fn reason(guard: &PathGuard, p: &Path, access: Access) -> Option<Reason> {
    guard.check_path(&s(p), access).err().map(|d| d.reason)
}

fn put(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// Symlink tests prove security properties, so they must never pass silently.
/// A host that cannot create symlinks fails them unless the operator opts out
/// with `TURBO_ALLOW_SYMLINK_SKIP=1`.
fn symlinks_or_skip(created: bool, what: &str) -> bool {
    if created {
        return true;
    }
    if std::env::var("TURBO_ALLOW_SYMLINK_SKIP").as_deref() == Ok("1") {
        eprintln!("SKIPPED ({what} symlink): creation unavailable and TURBO_ALLOW_SYMLINK_SKIP=1");
        return false;
    }
    panic!(
        "cannot create a {what} symlink on this host. This test proves a security property and \
         must not pass silently: enable symlink creation (Windows Developer Mode) or set \
         TURBO_ALLOW_SYMLINK_SKIP=1 to skip it explicitly."
    );
}

#[cfg(unix)]
fn make_file_symlink(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}
#[cfg(unix)]
fn make_dir_symlink(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}
#[cfg(windows)]
fn make_file_symlink(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_file(target, link).is_ok()
}
#[cfg(windows)]
fn make_dir_symlink(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_dir(target, link).is_ok()
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

#[test]
fn empty_roots_is_rejected_not_treated_as_unconfined() {
    let err = PathGuard::new(Vec::new(), Vec::new(), false).unwrap_err();
    assert_eq!(err.reason, Reason::NoRoots);
}

#[test]
fn relative_root_is_rejected() {
    let err = PathGuard::new(vec![PathBuf::from("relative/dir")], Vec::new(), false).unwrap_err();
    assert_eq!(err.reason, Reason::NoRoots);
}

#[test]
fn nonexistent_root_is_rejected() {
    let base = tempfile::tempdir().unwrap();
    let err = PathGuard::new(vec![base.path().join("nope")], Vec::new(), false).unwrap_err();
    assert_eq!(err.reason, Reason::NoRoots);
}

#[test]
fn roots_snapshot_is_a_copy_not_a_writable_handle() {
    let (guard, _root, _o) = fixture();
    let mut snapshot = guard.roots();
    snapshot.push(PathBuf::from("/"));
    assert_eq!(guard.roots().len(), 1);
}

#[test]
fn every_reason_renders_the_same_wire_text() {
    for r in [
        Reason::NoRoots,
        Reason::OverBroadRoot,
        Reason::Inadmissible,
        Reason::OutsideRoots,
        Reason::Unresolvable,
        Reason::HardDenied,
        Reason::SymlinkComponent,
        Reason::SpecialFile,
        Reason::UndeclaredTool,
        Reason::UndeclaredProperty,
        Reason::ReadOnlyServer,
        Reason::MalformedArgument,
        Reason::WorkspacePolicy,
        Reason::TooManyDeclarations,
    ] {
        assert_eq!(Denial::for_reason(r).to_string(), REFUSAL_TEXT, "{r:?}");
    }
}

// ---------------------------------------------------------------------------
// Admission (before any filesystem I/O)
// ---------------------------------------------------------------------------

#[test]
fn audit_relative_paths_are_inadmissible() {
    for raw in ["../x", "sub/file.txt", "./x", "..", "~/.ssh/id_rsa"] {
        assert_eq!(
            admit(raw).unwrap_err().reason,
            Reason::Inadmissible,
            "{raw}"
        );
    }
}

#[test]
fn audit_unc_paths_are_inadmissible_without_touching_the_network() {
    for raw in [
        r"\\evil.tld\share\x",
        r"\\?\C:\Windows\System32\config\SAM",
        "//evil.tld/share/x",
    ] {
        assert_eq!(
            admit(raw).unwrap_err().reason,
            Reason::Inadmissible,
            "{raw}"
        );
    }
}

#[test]
fn audit_parent_dir_segment_is_inadmissible() {
    let (_g, root, _o) = fixture();
    let raw = format!("{}/../escaped.txt", s(root.path()));
    assert_eq!(admit(&raw).unwrap_err().reason, Reason::Inadmissible);
}

#[test]
fn audit_dot_segments_empty_segments_and_trailing_separators_are_inadmissible() {
    // A walker builds child paths from the spelling it is given, so
    // `<root>/.aws/.` would yield `.aws/./credentials`, which no exclude matches.
    let (_g, root, _o) = fixture();
    let base = s(root.path());
    for spelled in [
        format!("{base}/.aws/."),
        format!("{base}/./x"),
        format!("{base}//x"),
        format!("{base}/x/"),
        format!("{base}\\x\\"),
    ] {
        assert_eq!(
            admit(&spelled).unwrap_err().reason,
            Reason::Inadmissible,
            "{spelled:?}"
        );
    }
    assert!(admit(&format!("{base}/x")).is_ok());
}

#[test]
fn audit_path_length_and_segment_count_are_bounded() {
    let (_g, root, _o) = fixture();
    let deep = format!("{}{}", s(root.path()), "/a".repeat(300));
    assert_eq!(admit(&deep).unwrap_err().reason, Reason::Inadmissible);
    let long = format!("{}/{}", s(root.path()), "a".repeat(5000));
    assert_eq!(admit(&long).unwrap_err().reason, Reason::Inadmissible);
}

#[test]
fn audit_alternate_data_stream_is_inadmissible() {
    let (_g, root, _o) = fixture();
    let raw = format!("{}/policy.toml::$DATA", s(root.path()));
    assert_eq!(admit(&raw).unwrap_err().reason, Reason::Inadmissible);
}

#[test]
fn audit_trailing_dot_or_space_is_inadmissible() {
    let (_g, root, _o) = fixture();
    for suffix in ["grok.toml.", "dir /x", "dir./x"] {
        let raw = format!("{}/{suffix}", s(root.path()));
        assert_eq!(
            admit(&raw).unwrap_err().reason,
            Reason::Inadmissible,
            "{raw:?}"
        );
    }
}

#[test]
fn audit_reserved_device_names_are_inadmissible() {
    let (_g, root, _o) = fixture();
    for name in [
        "NUL",
        "CON",
        "COM1",
        "nul.txt",
        "CONIN$",
        "CONOUT$",
        "CLOCK$",
        "COM\u{b9}",
        "LPT\u{b3}",
        // Windows trims spaces before the extension when matching devices.
        "CON .txt",
        "COM1 .log",
    ] {
        let raw = format!("{}/{name}", s(root.path()));
        assert_eq!(
            admit(&raw).unwrap_err().reason,
            Reason::Inadmissible,
            "{raw:?}"
        );
    }
}

#[test]
fn audit_control_characters_are_inadmissible() {
    assert_eq!(admit("C:\\a\0b").unwrap_err().reason, Reason::Inadmissible);
    assert_eq!(admit("/tmp/a\nb").unwrap_err().reason, Reason::Inadmissible);
}

#[test]
fn audit_invisible_formatting_characters_are_inadmissible() {
    let (_g, root, _o) = fixture();
    for c in ['\u{202E}', '\u{200B}', '\u{FEFF}', '\u{2066}'] {
        let raw = format!("{}/a{c}b.txt", s(root.path()));
        assert_eq!(
            admit(&raw).unwrap_err().reason,
            Reason::Inadmissible,
            "{raw:?}"
        );
    }
}

#[test]
fn audit_strings_the_sanitiser_would_change_are_inadmissible() {
    // The tools act on `sanitize_model_path_arg(raw)`: it trims Unicode
    // whitespace and strips quotes. Each of these once bypassed the name rules.
    let (_g, root, _o) = fixture();
    let base = format!("{}/grok.toml", s(root.path()));
    for spelled in [
        format!("{base}\""),
        format!("{base}'"),
        format!("{base}\u{00A0}"),
        format!("{base}\u{3000}"),
        format!(" {base}"),
        format!("\"{base}\""),
        format!("{base} "),
    ] {
        assert_eq!(
            admit(&spelled).unwrap_err().reason,
            Reason::Inadmissible,
            "{spelled:?}"
        );
    }
}

#[test]
fn audit_interior_quote_is_inadmissible() {
    let (_g, root, _o) = fixture();
    let raw = format!("{}/a\"b.txt", s(root.path()));
    assert_eq!(admit(&raw).unwrap_err().reason, Reason::Inadmissible);
}

#[test]
fn log_preview_escapes_and_bounds_client_text() {
    let preview = log_preview("line one\nline two");
    assert!(!preview.contains('\n'), "{preview}");
    let long = "x".repeat(10_000);
    let preview = log_preview(&long);
    assert!(preview.len() < 200, "{}", preview.len());
    assert!(preview.contains("10000 bytes"), "{preview}");
}

// ---------------------------------------------------------------------------
// Admission is actually wired into the guard, not only the free function
// ---------------------------------------------------------------------------

#[test]
fn audit_check_path_refuses_an_inadmissible_spelling() {
    let (guard, root, _o) = fixture();
    let raw = format!("{}/grok.toml\"", s(root.path()));
    assert_eq!(
        guard.check_path(&raw, Access::Read).unwrap_err().reason,
        Reason::Inadmissible
    );
}

#[test]
fn audit_check_call_refuses_a_relative_declared_argument() {
    let (guard, _root, _o) = fixture();
    assert_eq!(
        guard
            .check_call(
                "read_file",
                ToolKind::Read,
                &json!({"target_file": "sub/file.txt"})
            )
            .unwrap_err()
            .reason,
        Reason::Inadmissible
    );
}

#[test]
fn audit_sweep_refuses_a_unc_string_in_an_undeclared_field() {
    let (guard, _root, _o) = fixture();
    assert_eq!(
        guard
            .check_call(
                "read_file",
                ToolKind::Read,
                &json!({"extra": r"\\evil.tld\share\x"})
            )
            .unwrap_err()
            .reason,
        Reason::Inadmissible
    );
}

#[test]
fn audit_a_path_spelled_outside_every_root_is_refused_before_touching_the_disk() {
    let (guard, _root, outside) = fixture();
    // Nothing along this path exists; the refusal must not depend on that.
    let missing = outside
        .path()
        .join("no")
        .join("such")
        .join("dir")
        .join("x.txt");
    assert_eq!(
        reason(&guard, &missing, Access::Read),
        Some(Reason::OutsideRoots)
    );
}

#[test]
fn a_root_can_be_named_by_the_operators_own_spelling_or_its_canonical_one() {
    let base = tempfile::tempdir().unwrap();
    let real = base.path().join("real");
    fs::create_dir_all(&real).unwrap();
    fs::write(real.join("f.txt"), "x").unwrap();
    let alias = base.path().join("alias");
    if !symlinks_or_skip(make_dir_symlink(&real, &alias), "directory") {
        return;
    }
    let guard = PathGuard::new(vec![alias.clone()], Vec::new(), true).unwrap();
    assert_eq!(
        reason(&guard, &alias.join("f.txt"), Access::Read),
        None,
        "operator spelling"
    );
    assert_eq!(
        reason(&guard, &real.join("f.txt"), Access::Read),
        None,
        "canonical spelling"
    );
}

#[cfg(windows)]
#[test]
fn a_root_named_in_different_letter_case_still_admits_its_files() {
    let (guard, root, _o) = fixture();
    fs::write(root.path().join("File.txt"), "x").unwrap();
    let upper = PathBuf::from(s(root.path()).to_uppercase()).join("FILE.TXT");
    assert_eq!(reason(&guard, &upper, Access::Read), None);
}

// ---------------------------------------------------------------------------
// File creation and declared argument names
// ---------------------------------------------------------------------------

#[test]
fn audit_creating_a_file_outside_the_roots_is_denied() {
    let (guard, _root, outside) = fixture();
    let target = outside.path().join("pwn.bat");
    assert!(!target.exists());
    let err = guard
        .check_call(
            "search_replace",
            ToolKind::Edit,
            &json!({"file_path": s(&target), "old_string": "", "new_string": "@echo off"}),
        )
        .unwrap_err();
    assert_eq!(err.reason, Reason::OutsideRoots);
}

#[test]
fn audit_search_replace_declares_its_real_argument_name() {
    let fields = declared_path_fields("search_replace").expect("servable");
    assert!(fields.iter().any(|f| f.name == "file_path"));
}

#[test]
fn audit_creating_a_nonexistent_policy_file_in_dot_grok_is_denied() {
    let (guard, root, _o) = fixture();
    let target = root.path().join(".grok").join("policy.toml");
    assert!(!target.exists());
    assert_eq!(
        reason(&guard, &target, Access::Write),
        Some(Reason::HardDenied)
    );
}

// ---------------------------------------------------------------------------
// Hard deny
// ---------------------------------------------------------------------------

#[test]
fn audit_policy_file_outside_dot_grok_is_denied() {
    // No `.grok` component: only the policy-file name rule can decide this.
    let (guard, root, _o) = fixture();
    let nested = root.path().join("sub").join("policy.toml");
    assert_eq!(
        reason(&guard, &nested, Access::Write),
        Some(Reason::HardDenied)
    );
    assert_eq!(
        reason(&guard, &nested, Access::Read),
        Some(Reason::HardDenied)
    );
    let top = root.path().join("grok.toml");
    assert_eq!(
        reason(&guard, &top, Access::Write),
        Some(Reason::HardDenied)
    );
}

#[test]
fn audit_relocated_grok_home_inside_a_root_is_denied() {
    // A relocated GROK_HOME with no `.grok` component, placed INSIDE the root so
    // OutsideRoots cannot stand in for HardDenied.
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("cfg");
    fs::create_dir_all(&home).unwrap();
    let guard = PathGuard::with_grok_homes(
        vec![root.path().to_path_buf()],
        vec![home.clone()],
        Vec::new(),
        false,
    )
    .unwrap();
    let auth = home.join("auth.json");
    assert_eq!(
        reason(&guard, &auth, Access::Read),
        Some(Reason::HardDenied)
    );
    assert_eq!(
        reason(&guard, &auth, Access::Write),
        Some(Reason::HardDenied)
    );
}

#[test]
fn audit_the_guard_takes_its_grok_homes_from_the_configuration() {
    let homes = crate::guard::grok_homes();
    assert!(
        homes.contains(&xai_grok_config::default_grok_home()),
        "{homes:?}"
    );
    if let Some(configured) = xai_grok_config::user_grok_home() {
        assert!(homes.contains(&configured), "{homes:?}");
    }
}

#[test]
fn audit_walk_that_would_enter_a_relocated_grok_home_is_denied() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("cfg");
    let src = root.path().join("src");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&src).unwrap();
    let guard = PathGuard::with_grok_homes(
        vec![root.path().to_path_buf()],
        vec![home],
        Vec::new(),
        true,
    )
    .unwrap();
    assert_eq!(
        reason(&guard, root.path(), Access::Walk),
        Some(Reason::HardDenied)
    );
    assert_eq!(reason(&guard, &src, Access::Walk), None);
}

#[test]
fn audit_default_grok_home_is_denied() {
    // No admissible root contains `~/.grok` (every ancestor of a home is an
    // over-broad root), so from a project root it is refused lexically, before
    // any filesystem access.
    let (guard, _root, _o) = fixture();
    let auth = xai_grok_config::default_grok_home().join("auth.json");
    assert_eq!(
        reason(&guard, &auth, Access::Read),
        Some(Reason::OutsideRoots)
    );

    // The one way to reach a Grok home is a root inside it, and there the Grok
    // home rule wins over containment.
    let base = tempfile::tempdir().unwrap();
    let grok_home = base.path().join("grok-home");
    let skills = grok_home.join("skills");
    fs::create_dir_all(&skills).unwrap();
    fs::write(skills.join("SKILL.md"), "x").unwrap();
    let nested =
        PathGuard::with_grok_homes(vec![skills.clone()], vec![grok_home], Vec::new(), true)
            .unwrap();
    assert_eq!(
        reason(&nested, &skills.join("SKILL.md"), Access::Read),
        Some(Reason::HardDenied)
    );
    assert_eq!(
        reason(&nested, &skills, Access::Walk),
        Some(Reason::HardDenied)
    );
}

#[test]
fn audit_credential_stores_are_denied_even_inside_a_root() {
    let (guard, root, _o) = fixture();
    for rel in [
        ".ssh/id_rsa",
        ".aws/credentials",
        ".docker/config.json",
        ".netrc",
        ".npmrc",
        "tools/.pypirc",
        ".kube/config",
        ".cargo/credentials.toml",
        ".cargo/credentials",
        "infra/prod.tfstate",
        "infra/prod.tfstate.backup",
        "keys/id_ed25519",
    ] {
        let p = root.path().join(rel);
        assert_eq!(
            reason(&guard, &p, Access::Read),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    assert_eq!(
        reason(
            &guard,
            &root.path().join("keys/id_ed25519.pub"),
            Access::Read
        ),
        None,
        "public keys stay readable"
    );
}

#[test]
fn audit_dotenv_files_are_denied_but_examples_are_not() {
    let (guard, root, _o) = fixture();
    for rel in [".env", ".env.production", "app/.env.local"] {
        let p = root.path().join(rel);
        assert_eq!(
            reason(&guard, &p, Access::Read),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    let example = root.path().join(".env.example");
    assert_eq!(reason(&guard, &example, Access::Read), None);
}

#[test]
fn audit_writing_git_config_is_denied() {
    let (guard, root, _o) = fixture();
    let p = root.path().join(".git").join("config");
    assert_eq!(reason(&guard, &p, Access::Write), Some(Reason::HardDenied));
}

#[test]
fn audit_writing_anywhere_in_git_is_denied_but_ordinary_git_reads_stay_allowed() {
    let (guard, root, _o) = fixture();
    let p = root.path().join(".git").join("HEAD");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, b"ref: refs/heads/main\n").unwrap();
    assert_eq!(reason(&guard, &p, Access::Write), Some(Reason::HardDenied));
    assert_eq!(reason(&guard, &p, Access::Read), None);
}

#[test]
fn audit_reading_any_git_config_is_denied() {
    let (guard, root, _o) = fixture();
    for rel in [
        ".git/config",
        ".git/modules/sub/config",
        ".git/worktrees/w/config.worktree",
        ".git/hooks/pre-commit",
        "mirror.git/config",
        ".git-credentials",
    ] {
        let p = root.path().join(rel);
        assert_eq!(
            reason(&guard, &p, Access::Read),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
}

#[test]
fn audit_walking_denied_directories_is_denied() {
    let (guard, root, _o) = fixture();
    // `.aws`, `.docker` and `.kube` are decided by the walk rule alone: the
    // credential-store rule only names files inside them.
    for rel in [
        ".git", ".grok", ".ssh", ".gnupg", ".aws", ".docker", ".kube",
    ] {
        let p = root.path().join(rel);
        fs::create_dir_all(&p).unwrap();
        assert_eq!(
            reason(&guard, &p, Access::Walk),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    assert_eq!(reason(&guard, root.path(), Access::Walk), None);
}

#[cfg(unix)]
#[test]
fn audit_a_fifo_cannot_be_a_walk_target() {
    let (guard, root, _o) = fixture();
    let fifo = root.path().join("pipe");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo runs");
    assert!(made.success());
    assert_eq!(
        reason(&guard, &fifo, Access::Walk),
        Some(Reason::SpecialFile)
    );
}

#[test]
fn audit_writing_files_other_tools_run_automatically_is_denied() {
    let (guard, root, _o) = fixture();
    for rel in [
        ".envrc",
        ".envrc.local",
        ".vscode/tasks.json",
        ".idea/workspace.xml",
        ".github/workflows/ci.yml",
        ".github/actions/setup/action.yml",
        "AGENTS.md",
        "CLAUDE.md",
        ".cursorrules",
        ".ignore",
        ".rgignore",
        ".gitignore",
        ".mcp.json",
        "AGENT.md",
        "CLAUDE.local.md",
        "sub/Agents.md",
        ".claude/settings.json",
        ".claude/settings.local.json",
        ".claude/CLAUDE.md",
        ".claude/rules/style.md",
        ".claude/commands/deploy.md",
        ".cursor/hooks.json",
        ".cursor/mcp.json",
        ".cursor/rules/style.mdc",
        ".agents/skills/s/SKILL.md",
        ".husky/pre-commit",
        ".githooks/pre-push",
        ".git-hooks/pre-commit",
        ".hooks/pre-commit",
        ".pre-commit-config.yaml",
        "lefthook.yml",
        "lefthook.yaml",
        ".lefthook.toml",
        "lefthook-local.yml",
        ".lefthook-local.json",
        "HEAD",
        "vendor/p/plugin.json",
        "tools/x/.lsp.json",
        "plugins/y/extension.wasm",
        "plugins/y/hooks/hooks.json",
        "plugins/z/.claude-plugin/plugin.json",
    ] {
        let p = root.path().join(rel);
        assert_eq!(
            reason(&guard, &p, Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
}

#[test]
fn ordinary_edits_and_reads_of_agent_files_stay_allowed() {
    let (guard, root, _o) = fixture();
    for rel in ["src/main.rs", "docs/lefthook-guide.md", "hooks/README.md"] {
        assert_eq!(
            reason(&guard, &root.path().join(rel), Access::Write),
            None,
            "{rel}"
        );
    }
    assert_eq!(
        reason(&guard, &root.path().join("AGENTS.md"), Access::Read),
        None
    );
}

#[test]
fn lefthook_configs_are_recognised_in_every_format() {
    for name in [
        "lefthook.yml",
        ".lefthook.yaml",
        "lefthook.toml",
        "lefthook.json",
        "lefthook.jsonc",
        "lefthook-local.yml",
        ".lefthook-local.toml",
    ] {
        assert!(is_lefthook_config(name), "{name}");
    }
    for name in ["lefthook.md", "mylefthook.yml", "lefthook"] {
        assert!(!is_lefthook_config(name), "{name}");
    }
}

#[test]
fn deny_read_globs_cover_the_name_rules_in_every_letter_case() {
    let (guard, _root, _o) = fixture();
    let globs = guard.deny_read_globs();
    for pattern in [
        "**/.git/**",
        "**/*.git/**/config",
        "**/*.git/**/hooks/**",
        "**/.bare/**/config",
        "**/.bare/**/hooks/**",
        "**/.git-credentials",
        "**/.grok/**",
        "**/grok.toml",
        "**/policy.toml",
        "**/.ssh/**",
        "**/.gnupg/**",
        "**/.aws/credentials",
        "**/.docker/config.json",
        "**/.kube/config",
        "**/.netrc",
        "**/_netrc",
        "**/.npmrc",
        "**/id_rsa",
        "**/*.tfstate",
        "**/.env",
        "**/.env.*",
    ] {
        let folded = fold_glob_case(pattern);
        assert!(globs.contains(&folded), "missing {pattern} (as {folded})");
    }
    assert_eq!(
        fold_glob_case("**/_netrc.X"),
        "**/_[nN][eE][tT][rR][cC].[xX]"
    );
}

// ---------------------------------------------------------------------------
// Locations a root's own configuration runs automatically (edit tier)
// ---------------------------------------------------------------------------

#[test]
fn audit_plugin_roots_a_root_declares_are_write_refused() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    put(
        r,
        ".grok/config.toml",
        "[plugins]\npaths = [\"tools/my-plugin\"]\n",
    );
    put(
        r,
        ".claude/settings.json",
        r#"{"extraKnownMarketplaces": {"local": {"source": {"path": "market"}}}, "enabledPlugins": {"p@local": true}}"#,
    );
    let guard = PathGuard::new(vec![r.to_path_buf()], Vec::new(), false).unwrap();
    for rel in [
        "tools/my-plugin/commands/deploy.md",
        "tools/my-plugin/scripts/run.sh",
        "market/plugins/p/skills/s/SKILL.md",
    ] {
        assert_eq!(
            reason(&guard, &r.join(rel), Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    assert_eq!(
        reason(&guard, &r.join("tools/other/main.rs"), Access::Write),
        None
    );
}

#[test]
fn audit_a_configured_git_hooks_directory_and_config_includes_are_write_refused() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    put_repository(
        r,
        "[core]\n\thooksPath = scripts/git-hooks\n[include]\n\tpath = ../team.gitconfig\n\
         [includeIf \"gitdir:~/work/\"]\n\tpath = ../work.gitconfig\n",
    );
    let guard = PathGuard::new(vec![r.to_path_buf()], Vec::new(), false).unwrap();
    for rel in [
        "scripts/git-hooks/pre-commit",
        "team.gitconfig",
        "work.gitconfig",
    ] {
        assert_eq!(
            reason(&guard, &r.join(rel), Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    assert_eq!(
        reason(&guard, &r.join("scripts/build.sh"), Access::Write),
        None
    );
}

#[test]
fn audit_files_an_envrc_sources_are_write_refused() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    put(
        r,
        ".envrc",
        "source_env_if_exists env/local.sh\ndotenv config/dev.env\n",
    );
    put(r, "svc/.envrc", "source_env ../shared/secrets.sh\n");
    let guard = PathGuard::new(vec![r.to_path_buf()], Vec::new(), false).unwrap();
    for rel in ["env/local.sh", "config/dev.env", "shared/secrets.sh"] {
        assert_eq!(
            reason(&guard, &r.join(rel), Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    assert_eq!(reason(&guard, &r.join("env/other.sh"), Access::Write), None);
}

#[test]
fn git_config_parsing_finds_hooks_paths_and_includes() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    fs::write(
        &config,
        "# comment\n[core]\n\tHooksPath = \"x y\"\n[includeIf \"gitdir:/w/\"]\n\tpath = inc.cfg\n",
    )
    .unwrap();
    let entries = read_git_config(&config);
    assert!(
        entries.contains(&("core".into(), "hookspath".into(), "x y".into())),
        "{entries:?}"
    );
    assert!(
        entries.contains(&("includeif".into(), "path".into(), "inc.cfg".into())),
        "{entries:?}"
    );
}

#[test]
fn lexical_normalization_resolves_dots_without_the_disk() {
    let base = if cfg!(windows) {
        PathBuf::from(r"C:\a")
    } else {
        PathBuf::from("/a")
    };
    assert_eq!(
        lexical_normalize(&base.join("b").join("..").join(".").join("c")),
        base.join("c")
    );
}

// ---------------------------------------------------------------------------
// Symlinks
// ---------------------------------------------------------------------------

#[test]
fn symlink_pointing_outside_root_is_denied() {
    let (guard, root, outside) = fixture();
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, b"classified").unwrap();
    let link = root.path().join("innocent.txt");
    if !symlinks_or_skip(make_file_symlink(&secret, &link), "file") {
        return;
    }
    assert_eq!(
        reason(&guard, &link, Access::Read),
        Some(Reason::OutsideRoots)
    );
}

#[test]
fn audit_write_through_a_dangling_symlink_is_denied_by_the_symlink_rule() {
    // The link's target does not exist, so canonicalization cannot follow it and
    // the containment check alone would accept the path. Only the symlink rule
    // can decide this case, and it reports its own reason.
    let (guard, root, outside) = fixture();
    let missing_target = outside.path().join("not-yet-created");
    let link = root.path().join("dangling");
    if !symlinks_or_skip(
        make_dir_symlink(&missing_target, &link),
        "dangling directory",
    ) {
        return;
    }
    let target = link.join("pwn.txt");
    assert_eq!(
        reason(&guard, &target, Access::Write),
        Some(Reason::SymlinkComponent)
    );
}

#[test]
fn audit_unicode_sibling_of_a_missing_name_is_checked() {
    // The tools' Unicode fallback opens `a<NBSP>b.txt` when `a b.txt` is missing.
    let (guard, root, outside) = fixture();
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, b"classified").unwrap();
    let sibling = root.path().join("a\u{00A0}b.txt");
    if !symlinks_or_skip(make_file_symlink(&secret, &sibling), "file") {
        return;
    }
    let requested = root.path().join("a b.txt");
    assert_eq!(
        reason(&guard, &requested, Access::Read),
        Some(Reason::OutsideRoots)
    );
}

#[test]
fn audit_walk_targets_are_rewritten_to_their_canonical_spelling() {
    let (guard, root, _o) = fixture();
    let real = root.path().join("real");
    fs::create_dir_all(&real).unwrap();
    let link = root.path().join("link");
    if !symlinks_or_skip(make_dir_symlink(&real, &link), "directory") {
        return;
    }
    let args = guard.canonical_walk_args("grep", json!({"pattern": "x", "path": s(&link)}));
    assert_eq!(
        PathBuf::from(args["path"].as_str().unwrap()),
        xai_grok_tools::types::resources::canonicalize_for_permission(&real).display
    );
    assert_eq!(args["pattern"], "x");
}

// ---------------------------------------------------------------------------
// Core boundary
// ---------------------------------------------------------------------------

#[test]
fn read_inside_root_is_allowed() {
    let (guard, root, _o) = fixture();
    let f = root.path().join("ok.txt");
    fs::write(&f, b"hi").unwrap();
    assert_eq!(reason(&guard, &f, Access::Read), None);
}

#[test]
fn read_outside_root_is_denied() {
    let (guard, _root, outside) = fixture();
    let secret = outside.path().join("id_rsa_copy");
    fs::write(&secret, b"PRIVATE KEY").unwrap();
    assert_eq!(
        reason(&guard, &secret, Access::Read),
        Some(Reason::OutsideRoots)
    );
}

#[test]
fn write_to_nonexistent_tail_inside_root_is_allowed() {
    let (guard, root, _o) = fixture();
    let target = root.path().join("nested").join("new.txt");
    assert_eq!(reason(&guard, &target, Access::Write), None);
}

#[test]
fn sibling_prefix_root_does_not_match() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("work");
    let evil = base.path().join("work-evil");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&evil).unwrap();
    let guard = PathGuard::new(vec![root], Vec::new(), false).unwrap();
    let target = evil.join("file.txt");
    fs::write(&target, b"x").unwrap();
    assert_eq!(
        reason(&guard, &target, Access::Read),
        Some(Reason::OutsideRoots)
    );
}

#[test]
fn any_of_several_roots_authorizes() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let guard = PathGuard::new(
        vec![a.path().to_path_buf(), b.path().to_path_buf()],
        Vec::new(),
        false,
    )
    .unwrap();
    for dir in [a.path(), b.path()] {
        let f = dir.join("f.txt");
        fs::write(&f, b"x").unwrap();
        assert_eq!(reason(&guard, &f, Access::Read), None);
    }
    let bad = outside.path().join("f.txt");
    fs::write(&bad, b"x").unwrap();
    assert!(reason(&guard, &bad, Access::Read).is_some());
}

#[test]
fn check_resolved_applies_containment_and_hard_deny_without_spelling_rules() {
    let (guard, root, outside) = fixture();
    let out = outside.path().join("x.txt");
    fs::write(&out, b"x").unwrap();
    assert_eq!(
        guard.check_resolved(&out, Access::Read).unwrap_err().reason,
        Reason::OutsideRoots
    );
    let key = root.path().join(".ssh").join("id_ed25519");
    assert_eq!(
        guard.check_resolved(&key, Access::Read).unwrap_err().reason,
        Reason::HardDenied
    );
}

// ---------------------------------------------------------------------------
// Call-level
// ---------------------------------------------------------------------------

#[test]
fn shell_is_never_declared() {
    for name in [
        "run_terminal_cmd",
        "bash",
        "exec_command",
        "write_stdin",
        "GrokBuild:run_terminal_cmd",
    ] {
        assert!(declared_path_fields(name).is_none(), "{name}");
    }
}

#[test]
fn undeclared_tool_is_not_servable() {
    let (guard, _r, _o) = fixture();
    assert_eq!(
        guard
            .check_call(
                "run_terminal_cmd",
                ToolKind::Execute,
                &json!({"command": "ls"})
            )
            .unwrap_err()
            .reason,
        Reason::UndeclaredTool
    );
}

#[test]
fn read_only_server_refuses_mutating_kind() {
    let root = tempfile::tempdir().unwrap();
    let guard = PathGuard::new(vec![root.path().to_path_buf()], Vec::new(), true).unwrap();
    assert_eq!(
        guard
            .check_call(
                "search_replace",
                ToolKind::Edit,
                &json!({"file_path": s(&root.path().join("f.txt"))})
            )
            .unwrap_err()
            .reason,
        Reason::ReadOnlyServer
    );
}

#[test]
fn sweep_catches_path_in_an_undeclared_field() {
    let (guard, _root, outside) = fixture();
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, b"x").unwrap();
    assert_eq!(
        guard
            .check_call(
                "read_file",
                ToolKind::Read,
                &json!({"undeclared_new_arg": s(&secret)})
            )
            .unwrap_err()
            .reason,
        Reason::OutsideRoots
    );
}

#[test]
fn sweep_does_not_deny_free_text_properties() {
    let (guard, root, _o) = fixture();
    assert!(
        guard
            .check_call(
                "grep",
                ToolKind::Search,
                &json!({
                    "path": s(root.path()),
                    "pattern": "fn main() -> Result<(), Box<dyn Error>>",
                    "-A": 3
                })
            )
            .is_ok()
    );
}

#[test]
fn audit_grep_accepts_an_ordinary_glob() {
    let (guard, root, _o) = fixture();
    assert!(
        guard
            .check_call(
                "grep",
                ToolKind::Search,
                &json!({"path": s(root.path()), "pattern": "x", "glob": "*.rs"})
            )
            .is_ok()
    );
}

#[test]
fn audit_grep_refuses_globs_that_could_leave_the_root() {
    let (guard, root, _o) = fixture();
    for glob in ["../*", "/etc/*", "C:\\Windows\\*", "~/.ssh/*", "a/../../b"] {
        assert_eq!(
            guard
                .check_call(
                    "grep",
                    ToolKind::Search,
                    &json!({"path": s(root.path()), "pattern": "x", "glob": glob})
                )
                .unwrap_err()
                .reason,
            Reason::Inadmissible,
            "{glob}"
        );
    }
}

#[test]
fn grep_glob_must_be_a_string() {
    let (guard, root, _o) = fixture();
    assert_eq!(
        guard
            .check_call(
                "grep",
                ToolKind::Search,
                &json!({"path": s(root.path()), "pattern": "x", "glob": 5})
            )
            .unwrap_err()
            .reason,
        Reason::MalformedArgument
    );
}

#[test]
fn sweep_depth_is_bounded() {
    let (guard, _r, _o) = fixture();
    let mut v = json!("leaf");
    for _ in 0..64 {
        v = json!({ "n": v });
    }
    assert_eq!(
        guard
            .check_call("read_file", ToolKind::Read, &v)
            .unwrap_err()
            .reason,
        Reason::MalformedArgument
    );
}

#[test]
fn malformed_declared_argument_is_refused() {
    let (guard, _r, _o) = fixture();
    assert_eq!(
        guard
            .check_call("read_file", ToolKind::Read, &json!({"target_file": 42}))
            .unwrap_err()
            .reason,
        Reason::MalformedArgument
    );
}

#[test]
fn denials_are_indistinguishable_on_the_wire() {
    let (guard, root, outside) = fixture();
    let existing = outside.path().join("exists.txt");
    fs::write(&existing, b"x").unwrap();
    let missing = outside.path().join("does-not-exist.txt");
    let secret = root.path().join(".grok").join("auth.json");

    let msgs: Vec<String> = [&existing, &missing, &secret]
        .iter()
        .map(|p| {
            guard
                .check_path(&s(p), Access::Read)
                .unwrap_err()
                .to_string()
        })
        .collect();

    assert_eq!(msgs[0], msgs[1]);
    assert_eq!(msgs[1], msgs[2]);
    for m in &msgs {
        assert!(!m.contains(".grok"), "{m}");
        assert!(!m.contains("exists.txt"), "{m}");
    }
}

// ---------------------------------------------------------------------------
// Schema validation
// ---------------------------------------------------------------------------

#[test]
fn schema_with_only_declared_and_known_free_properties_validates() {
    let schema = json!({"properties": {"target_file": {}, "offset": {}, "limit": {}, "pages": {}}});
    assert!(validate_tool_schema("read_file", &schema).is_ok());
}

#[test]
fn schema_with_an_unaccounted_property_refuses_to_serve() {
    let schema = json!({"properties": {"target_file": {}, "backup_destination": {}}});
    assert_eq!(
        validate_tool_schema("read_file", &schema)
            .unwrap_err()
            .reason,
        Reason::UndeclaredProperty
    );
}

#[test]
fn real_tool_schemas_validate_against_the_declaration_table() {
    let cases: &[(&str, &[&str])] = &[
        (
            "read_file",
            &["format", "limit", "offset", "pages", "target_file"],
        ),
        ("list_dir", &["target_directory"]),
        (
            "grep",
            &[
                "-A",
                "-B",
                "-C",
                "-i",
                "glob",
                "head_limit",
                "multiline",
                "path",
                "pattern",
                "type",
            ],
        ),
        (
            "search_replace",
            &[
                "file_path",
                "old_string",
                "new_string",
                "replace_all",
                "skip_read_before_edit",
            ],
        ),
    ];
    for (tool, props) in cases {
        let mut map = serde_json::Map::new();
        for p in *props {
            map.insert((*p).to_string(), json!({}));
        }
        assert!(
            validate_tool_schema(tool, &json!({ "properties": map })).is_ok(),
            "{tool}"
        );
    }
}

#[test]
fn undeclared_tool_fails_schema_validation_too() {
    assert_eq!(
        validate_tool_schema("run_terminal_cmd", &json!({"properties": {"command": {}}}))
            .unwrap_err()
            .reason,
        Reason::UndeclaredTool
    );
}

// ---------------------------------------------------------------------------
// Roots, git metadata by content, default walks, folder-trust markers
// ---------------------------------------------------------------------------

#[test]
fn audit_over_broad_roots_are_refused() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("home").join("me");
    let project = home.join("project");
    fs::create_dir_all(&project).unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    let homes = vec![home.clone()];

    assert!(
        is_over_broad_root(&home, &homes),
        "the home directory itself"
    );
    assert!(
        is_over_broad_root(&base.path().join("home"), &homes),
        "a directory that contains the home directory"
    );
    let filesystem_root = base.path().ancestors().last().unwrap();
    assert!(
        is_over_broad_root(filesystem_root, &[]),
        "{filesystem_root:?}"
    );
    assert!(
        !is_over_broad_root(&project, &homes),
        "a project inside home"
    );
    assert!(
        !is_over_broad_root(unrelated.path(), &homes),
        "an unrelated directory"
    );
    #[cfg(unix)]
    assert!(
        is_over_broad_root(Path::new("/mnt/c"), &[]),
        "a Windows drive mounted under WSL"
    );
}

#[test]
fn audit_a_home_under_another_spelling_or_another_account_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("me");
    let other = base.path().join("other");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&other).unwrap();
    // Every account's home is a candidate, not only the current one.
    assert!(is_over_broad_root(&other, &[home.clone(), other.clone()]));
    // File identity, not spelling: a link to the home directory is the home.
    let alias = base.path().join("alias");
    if !symlinks_or_skip(make_dir_symlink(&home, &alias), "directory") {
        return;
    }
    assert!(is_over_broad_root(&alias, &[home]));
}

#[test]
fn candidate_homes_include_the_current_home() {
    if let Some(home) = dirs::home_dir() {
        assert!(candidate_homes().contains(&home));
    }
}

#[test]
fn folders_beside_a_home_are_candidate_homes_unless_it_is_top_level() {
    let base = tempfile::tempdir().unwrap();
    let users = base.path().join("users");
    let (alice, bob) = (users.join("alice"), users.join("bob"));
    fs::create_dir_all(&alice).unwrap();
    fs::create_dir_all(&bob).unwrap();
    fs::write(users.join("notes.txt"), "a file, not an account").unwrap();
    let mut beside = crate::guard::folders_beside_home(&alice);
    beside.sort();
    assert_eq!(beside, vec![alice, bob]);

    // A container's `/root`: its neighbours are system and project folders.
    let top_level = if cfg!(windows) {
        Path::new(r"C:\root")
    } else {
        Path::new("/root")
    };
    assert!(crate::guard::folders_beside_home(top_level).is_empty());
}

#[test]
fn a_home_reached_under_another_path_is_over_broad() {
    let base = tempfile::tempdir().unwrap();
    let users = base.path().join("Users");
    let alice = users.join("alice");
    fs::create_dir_all(&alice).unwrap();
    // A second path to the volume that holds the homes, the way
    // /System/Volumes/Data does on macOS: `volume/Users` is the same folder.
    let volume = base.path().join("volume");
    fs::create_dir_all(&volume).unwrap();
    if !symlinks_or_skip(make_dir_symlink(&users, &volume.join("Users")), "directory") {
        return;
    }
    assert!(is_over_broad_root(&volume, std::slice::from_ref(&alice)));
    let project = base.path().join("project");
    fs::create_dir_all(&project).unwrap();
    assert!(!is_over_broad_root(&project, &[alice]));
}

#[test]
fn windows_mounts_count_by_the_folder_they_expose() {
    use crate::guard::WindowsMount::{Drive, Profile, Profiles};
    let mountinfo = r"36 25 0:31 / /mnt/c rw,noatime - 9p C:\134 rw,dirsync,aname=drvfs;path=C:\;uid=1000
37 25 0:32 / /c rw,noatime - 9p D:\134 rw,aname=drvfs;path=D:\
38 25 0:33 / /mnt/wsl rw,relatime - tmpfs none rw
39 25 0:34 / /mnt/my\040drive rw - drvfs E: rw
40 25 8:32 / / rw,relatime - ext4 /dev/sdd rw
41 25 0:35 / /data rw - virtiofs F: rw
42 25 0:36 / /home/me/code rw,noatime - 9p C:\134Users\134me\134code rw,aname=drvfs;path=C:\Users\me\code
43 36 0:31 /Users/me/src/app /workspaces/app rw - 9p C:\134 rw,aname=drvfs;path=C:\
44 36 0:31 /Users /srv/profiles rw - 9p C:\134 rw,aname=drvfs;path=C:\
45 25 0:37 / /home/me/win rw - drvfs C:\134Users\134me rw
48 25 0:40 / /home/me/work rw - 9p C:\134Users\134John\040Smith\134code rw,aname=drvfs;path=C:\Users\John Smith\code;uid=1000
49 25 0:41 / /mnt/profile rw - 9p C:\134Users\1341234567 rw,aname=drvfs;path=C:\Users\1234567;uid=1000";
    let mut bytes = mountinfo.as_bytes().to_vec();
    // A line that is not UTF-8 is skipped; the lines after it still count.
    bytes.extend_from_slice(
        b"\n46 25 0:38 / /mnt/\xff rw - drvfs G: rw\n47 25 0:39 / /g rw - drvfs G: rw",
    );
    assert_eq!(
        crate::guard::parse_windows_mounts(&bytes),
        vec![
            (PathBuf::from("/mnt/c"), Drive),
            (PathBuf::from("/c"), Drive),
            (PathBuf::from("/mnt/my drive"), Drive),
            (PathBuf::from("/data"), Drive),
            // A folder mounted from a drive, directly or through a bind mount,
            // is an ordinary directory.
            (PathBuf::from("/srv/profiles"), Profiles),
            (PathBuf::from("/home/me/win"), Profile),
            // A folder name holding a space, and one made of digits, read as written.
            (PathBuf::from("/mnt/profile"), Profile),
            (PathBuf::from("/g"), Drive),
        ]
    );
}

#[test]
fn mounts_are_parsed_with_their_device_and_folder() {
    let mountinfo = b"20 1 8:1 / / rw - ext4 /dev/sda1 rw
21 20 8:17 /users /home rw - ext4 /dev/sdb1 rw
22 20 0:41 / /mnt/nfs\\040share rw - nfs4 server:/export rw";
    let entry =
        |device: &str, root: &str, mount_point: &str, fstype: &str| crate::guard::MountEntry {
            device: device.to_string(),
            root: PathBuf::from(root),
            mount_point: PathBuf::from(mount_point),
            fstype: fstype.to_string(),
        };
    assert_eq!(
        crate::guard::parse_mounts(mountinfo),
        vec![
            entry("8:1", "/", "/", "ext4"),
            entry("8:17", "/users", "/home", "ext4"),
            entry("0:41", "/export", "/mnt/nfs share", "nfs4"),
        ]
    );
}

#[test]
fn a_home_is_found_wherever_its_filesystem_is_mounted_again() {
    use crate::guard::{home_aliases, parse_mounts};
    // The folder holding the homes, bound over /home, and its whole disk
    // mounted again at /srv/storage.
    let storage = parse_mounts(
        b"20 1 8:1 / / rw - ext4 /dev/sda1 rw
21 20 8:17 /users /home rw - ext4 /dev/sdb1 rw
22 20 8:17 / /srv/storage rw - ext4 /dev/sdb1 rw",
    );
    assert_eq!(
        home_aliases(Path::new("/home/alice"), &storage),
        vec![PathBuf::from("/srv/storage/users/alice")]
    );
    // One home bound somewhere else under another name, as an SFTP chroot does.
    let chroot = parse_mounts(
        b"20 1 8:1 / / rw - ext4 /dev/sda1 rw
23 20 8:1 /home/alice /srv/sftp/alice/home rw - ext4 /dev/sda1 rw",
    );
    assert_eq!(
        home_aliases(Path::new("/home/alice"), &chroot),
        vec![PathBuf::from("/srv/sftp/alice/home")]
    );
    assert!(home_aliases(Path::new("/home/carol"), &chroot).is_empty());
    // A btrfs subvolume mounted at /var/home, and the whole filesystem at
    // /mnt/pool.
    let btrfs = parse_mounts(
        b"20 1 8:1 / / rw - ext4 /dev/sda1 rw
24 20 0:31 /@home /var/home rw - btrfs /dev/sdc1 rw
25 20 0:31 / /mnt/pool rw - btrfs /dev/sdc1 rw",
    );
    assert_eq!(
        home_aliases(Path::new("/var/home/bob"), &btrfs),
        vec![PathBuf::from("/mnt/pool/@home/bob")]
    );
}

#[test]
fn a_hooks_path_set_in_an_included_git_config_is_write_refused() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    put_repository(r, "[include]\n\tpath = ../.gitconfig\n");
    // Included files are read too, their includes resolve against their own
    // folder, and a cycle ends.
    put(
        r,
        ".gitconfig",
        "[core]\n\thooksPath = scripts/githooks\n[include]\n\tpath = conf/more.cfg\n",
    );
    put(
        r,
        "conf/more.cfg",
        "[core]\n\thooksPath = ci/hooks\n[include]\n\tpath = ../.gitconfig\n",
    );
    let guard = PathGuard::new(vec![r.to_path_buf()], Vec::new(), false).unwrap();
    for rel in [
        "scripts/githooks/pre-commit",
        "ci/hooks/pre-push",
        ".gitconfig",
        "conf/more.cfg",
    ] {
        assert_eq!(
            reason(&guard, &r.join(rel), Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    assert_eq!(
        reason(&guard, &r.join("scripts/build.sh"), Access::Write),
        None
    );
}

#[test]
fn plugin_and_skill_paths_declared_anywhere_turbo_reads_them_are_write_refused() {
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("project");
    let root = project.join("app");
    fs::create_dir_all(&root).unwrap();
    // Above the root, in a folder under it, and in the Grok home.
    // A relative entry counts from the folder holding `.grok` and from the root,
    // either of which Turbo may have been started in.
    put(
        &project,
        ".grok/config.toml",
        "[plugins]\npaths = [\"app/above-plugin\", \"from-root-plugin\"]\n",
    );
    put(
        &root,
        "sub/.grok/config.toml",
        "[plugins]\npaths = [\"nested-plugin\"]\n[skills]\npaths = [\"nested-skills\"]\n",
    );
    let grok_home = base.path().join("grok-home");
    put(&root, "global-skills/SKILL.md", "x");
    put(
        &grok_home,
        "config.toml",
        &format!(
            "[plugins]\npaths = [{:?}]\n[skills]\npaths = [{:?}]\n",
            root.join("global-plugin"),
            root.join("global-skills").join("SKILL.md")
        ),
    );
    // The managed configuration declares locations too.
    put(
        &grok_home,
        "managed_config.toml",
        &format!("[plugins]\npaths = [{:?}]\n", root.join("managed-plugin")),
    );
    let guard =
        PathGuard::with_grok_homes(vec![root.clone()], vec![grok_home], Vec::new(), false).unwrap();
    for rel in [
        "from-root-plugin/hooks.sh",
        "managed-plugin/run.sh",
        "above-plugin/hooks.sh",
        "sub/nested-plugin/run.sh",
        "sub/nested-skills/s/SKILL.md",
        "global-plugin/scripts/on-start.sh",
        // A skills entry naming a file makes its whole folder a skills folder.
        "global-skills/other.md",
    ] {
        assert_eq!(
            reason(&guard, &root.join(rel), Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    assert_eq!(
        reason(&guard, &root.join("src/main.rs"), Access::Write),
        None
    );
}

#[test]
fn the_argument_sweep_is_bounded() {
    let (guard, root, _o) = fixture();
    let target = s(&root.path().join("f.txt"));
    assert!(
        guard
            .check_call("read_file", ToolKind::Read, &json!({"target_file": target}))
            .is_ok()
    );
    let many: Vec<String> = (0..=crate::guard::MAX_ARGUMENT_STRINGS)
        .map(|i| s(&root.path().join(format!("f{i}.txt"))))
        .collect();
    let err = guard
        .check_call(
            "read_file",
            ToolKind::Read,
            &json!({"target_file": target, "extra": many}),
        )
        .unwrap_err();
    assert_eq!(err.reason, Reason::MalformedArgument);
}

#[test]
fn the_unicode_fallback_scan_is_bounded() {
    let (guard, root, _o) = fixture();
    let dir = root.path().join("big");
    fs::create_dir_all(&dir).unwrap();
    for i in 0..=crate::guard::MAX_SIBLING_SCAN {
        fs::File::create(dir.join(format!("f{i}"))).unwrap();
    }
    assert_eq!(
        reason(&guard, &dir.join("no such file"), Access::Read),
        Some(Reason::Unresolvable)
    );
    // A missing name without a space never scans the directory.
    assert_eq!(reason(&guard, &dir.join("absent"), Access::Read), None);
}

#[test]
fn audit_the_real_home_directory_cannot_be_a_root() {
    let home = dirs::home_dir().expect("a home directory");
    assert_eq!(
        PathGuard::new(vec![home], Vec::new(), true)
            .unwrap_err()
            .reason,
        Reason::OverBroadRoot
    );
}

#[test]
fn audit_bare_repositories_are_git_metadata_whatever_their_name() {
    let (guard, root, _o) = fixture();
    for name in [".bare", "project.git", "plain-name"] {
        let repo = root.path().join(name);
        fs::create_dir_all(repo.join("objects")).unwrap();
        fs::create_dir_all(repo.join("refs")).unwrap();
        fs::write(repo.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(repo.join("config"), "[core]\n").unwrap();

        let hook = repo.join("hooks").join("post-checkout");
        assert_eq!(
            reason(&guard, &hook, Access::Write),
            Some(Reason::HardDenied),
            "{name}: plant a hook"
        );
        assert_eq!(
            reason(&guard, &hook, Access::Read),
            Some(Reason::HardDenied),
            "{name}: read a hook"
        );
        assert_eq!(
            reason(&guard, &repo.join("config"), Access::Read),
            Some(Reason::HardDenied),
            "{name}: read the config"
        );
        let worktree_config = repo.join("worktrees").join("w").join("config.worktree");
        assert_eq!(
            reason(&guard, &worktree_config, Access::Read),
            Some(Reason::HardDenied),
            "{name}: read a worktree config"
        );
        assert_eq!(
            reason(&guard, &repo, Access::Walk),
            Some(Reason::HardDenied),
            "{name}: walk it"
        );
        assert_eq!(
            reason(&guard, &repo.join("HEAD"), Access::Read),
            None,
            "{name}: ordinary reads stay allowed"
        );
    }
}

#[test]
fn a_checkout_named_like_a_repository_is_not_git_metadata() {
    let (guard, root, _o) = fixture();
    let checkout = root.path().join("tool.git");
    fs::create_dir_all(checkout.join(".git")).unwrap();
    fs::create_dir_all(checkout.join("src")).unwrap();
    assert_eq!(
        reason(&guard, &checkout.join("src").join("main.rs"), Access::Write),
        None
    );
    assert_eq!(reason(&guard, &checkout, Access::Walk), None);
}

#[test]
fn audit_a_grep_with_no_named_path_is_checked_against_the_first_root() {
    // A relocated Grok home with no `.grok` component inside the root: grep with
    // no path would walk straight into it.
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("cfg");
    fs::create_dir_all(&home).unwrap();
    let guard = PathGuard::with_grok_homes(
        vec![root.path().to_path_buf()],
        vec![home],
        Vec::new(),
        true,
    )
    .unwrap();
    for args in [
        json!({"pattern": "token"}),
        json!({"pattern": "token", "path": null}),
    ] {
        assert_eq!(
            guard
                .check_call("grep", ToolKind::Search, &args)
                .unwrap_err()
                .reason,
            Reason::HardDenied,
            "{args}"
        );
    }
    let (plain, _root, _o) = fixture();
    assert!(
        plain
            .check_call("grep", ToolKind::Search, &json!({"pattern": "x"}))
            .is_ok()
    );
}

#[test]
fn audit_every_folder_trust_marker_kind_is_covered_and_write_denied() {
    // Folder trust records each kind of repository code or configuration it
    // gates on with `hit!("kind")`. Every such kind is something Turbo runs in
    // an already-trusted folder with no prompt, so the edit tier must refuse to
    // create it. A new kind there fails this test until it has a sample here.
    let source = include_str!("../../xai-grok-workspace/src/folder_trust.rs");
    // One sample per kind is not one per detection site: a new site that reuses
    // an existing kind must still be reviewed against the write rules.
    const HIT_SITES: usize = 17;
    let sites = source.matches("hit!(").count();
    assert_eq!(
        sites, HIT_SITES,
        "folder_trust.rs now has {sites} hit!(...) sites: review FOLDER_TRUST_MARKER_SAMPLES \
         and the edit-tier write rules for the new site, then update HIT_SITES"
    );
    assert_eq!(
        source.matches("hit!(\"").count(),
        sites,
        "a hit! site no longer passes a literal kind, so this test cannot see it"
    );
    let kinds: std::collections::BTreeSet<&str> = source
        .match_indices("hit!(\"")
        .map(|(at, open)| {
            let rest = &source[at + open.len()..];
            &rest[..rest.find('"').expect("closing quote")]
        })
        .collect();
    for kind in &kinds {
        assert!(
            FOLDER_TRUST_MARKER_SAMPLES.iter().any(|(k, _)| k == kind),
            "folder trust gates on {kind:?}, but FOLDER_TRUST_MARKER_SAMPLES has no sample for it"
        );
    }
    let (guard, root, _o) = fixture();
    for (kind, paths) in FOLDER_TRUST_MARKER_SAMPLES {
        for rel in *paths {
            assert_eq!(
                reason(&guard, &root.path().join(rel), Access::Write),
                Some(Reason::HardDenied),
                "{kind}: {rel}"
            );
        }
    }
}

#[test]
fn audit_symlink_probes_never_inspect_a_bare_drive_or_the_root() {
    let (_g, root, _o) = fixture();
    let target = root.path().join("a").join("b.txt");
    let probes = symlink_probe_paths(&target);
    assert!(!probes.is_empty());
    for probe in &probes {
        assert!(
            probe.file_name().is_some(),
            "probed a prefix or root: {probe:?}"
        );
    }
    assert_eq!(probes.last(), Some(&target));
}

#[test]
fn a_home_whose_real_path_lies_under_the_root_is_over_broad() {
    let base = tempfile::tempdir().unwrap();
    let real = base.path().join("real");
    fs::create_dir_all(real.join("sub").join("alice")).unwrap();
    // The home is known only by a path through a link.
    let alias = base.path().join("alias");
    if !symlinks_or_skip(make_dir_symlink(&real.join("sub"), &alias), "directory") {
        return;
    }
    let home = alias.join("alice");
    assert!(is_over_broad_root(&real, std::slice::from_ref(&home)));
    let elsewhere = base.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    assert!(!is_over_broad_root(&elsewhere, &[home]));
}

#[test]
fn a_crowded_folder_of_homes_counts_as_one_home() {
    let base = tempfile::tempdir().unwrap();
    let users = base.path().join("users");
    for i in 0..=crate::guard::MAX_ACCOUNT_FOLDERS {
        fs::create_dir_all(users.join(format!("u{i}"))).unwrap();
    }
    assert_eq!(
        crate::guard::folders_beside_home(&users.join("u0")),
        vec![users]
    );
}

#[test]
fn declared_paths_expand_variables_and_keep_relative_entries_relative() {
    let base = tempfile::tempdir().unwrap();
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let value = std::env::var_os(variable).expect("the home variable is set");
    let config = base.path().join("config.toml");
    fs::write(
        &config,
        format!("[plugins]\npaths = [\"${{{variable}}}/dev/plugin\", \"tools/plugin\"]\n"),
    )
    .unwrap();
    let declared = crate::guard::grok_config_paths(&config, None);
    let expected = lexical_normalize(&PathBuf::from(value).join("dev").join("plugin"));
    assert!(
        declared.paths.contains(&expected),
        "{expected:?} is not in {:?}",
        declared.paths
    );
    // A relative entry counts from wherever Turbo runs, and so do the names
    // after a variable.
    for names in [["tools", "plugin"], ["dev", "plugin"]] {
        assert!(
            declared
                .relative
                .iter()
                .any(|entry| entry.anchor.is_none() && entry.up == 0 && entry.names == names),
            "{names:?} is not in {:?}",
            declared.relative
        );
    }
}

#[test]
fn git_homes_include_the_home_variables_git_for_windows_uses() {
    let (profile, home, drive, path) = if cfg!(windows) {
        (r"C:\Users\me", r"D:\home", "C:", r"\Users\me")
    } else {
        ("/home/me", "/srv/home", "/", "home/me")
    };
    assert_eq!(
        crate::guard::tilde_homes_from(
            Some(profile.into()),
            Some(home.into()),
            Some(drive.into()),
            Some(path.into())
        ),
        vec![PathBuf::from(profile), PathBuf::from(home)]
    );
    // A relative HOME names no home.
    assert_eq!(
        crate::guard::tilde_homes_from(Some(profile.into()), Some("home".into()), None, None),
        vec![PathBuf::from(profile)]
    );
}

#[test]
fn a_root_no_client_path_can_name_is_refused_when_the_guard_is_built() {
    let base = tempfile::tempdir().unwrap();
    let quoted = base.path().join("Dan's Projects");
    fs::create_dir_all(&quoted).unwrap();
    let error = PathGuard::with_grok_homes(vec![quoted], Vec::new(), Vec::new(), true).unwrap_err();
    assert_eq!(error.reason, Reason::Inadmissible);
    // The tools print a share's or an over-long path's verbatim form, which no
    // client path can start with.
    assert!(crate::guard::printable_root(Path::new(r"\\?\UNC\nas\team\proj")).is_err());
    #[cfg(windows)]
    assert!(crate::guard::printable_root(Path::new(r"\\?\C:\long\proj")).is_err());
    // Reached through a link with a usable name, the folder is still refused: the
    // tools print its real path.
    let link = base.path().join("projects-link");
    if symlinks_or_skip(
        make_dir_symlink(&base.path().join("Dan's Projects"), &link),
        "directory",
    ) {
        let error =
            PathGuard::with_grok_homes(vec![link], Vec::new(), Vec::new(), true).unwrap_err();
        assert_eq!(error.reason, Reason::Inadmissible);
    }
    // No JSON string can name a file under a folder whose name is not Unicode.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let odd = base.path().join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
        fs::create_dir_all(&odd).unwrap();
        let error =
            PathGuard::with_grok_homes(vec![odd], Vec::new(), Vec::new(), true).unwrap_err();
        assert_eq!(error.reason, Reason::Inadmissible);
    }
    #[cfg(unix)]
    {
        // Windows cannot create a folder named like a device; Linux can.
        let device = base.path().join("thesis").join("aux");
        fs::create_dir_all(&device).unwrap();
        let error =
            PathGuard::with_grok_homes(vec![device], Vec::new(), Vec::new(), true).unwrap_err();
        assert_eq!(error.reason, Reason::Inadmissible);
    }
}

#[cfg(unix)]
#[test]
fn configuration_files_that_are_devices_are_not_read() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    fs::create_dir_all(r.join("vendor/lib/.claude")).unwrap();
    std::os::unix::fs::symlink("/dev/zero", r.join("vendor/lib/.claude/settings.json")).unwrap();
    fs::create_dir_all(r.join(".git")).unwrap();
    std::os::unix::fs::symlink("/dev/zero", r.join(".git/config")).unwrap();
    // Reading either whole would never end.
    let started = std::time::Instant::now();
    assert!(PathGuard::new(vec![r.to_path_buf()], Vec::new(), false).is_ok());
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

#[test]
fn a_relative_entry_in_a_nested_configuration_counts_only_from_its_own_folder() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    // A plugin under development declares itself; a session started in its
    // folder loads it. A session at the root never reads this file.
    put(
        r,
        "packages/docs-plugin/.grok/config.toml",
        "[plugins]\npaths = [\".\"]\n[skills]\npaths = [\"scripts\"]\n",
    );
    let guard =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), false).unwrap();
    assert_eq!(
        reason(
            &guard,
            &r.join("packages/docs-plugin/hooks.sh"),
            Access::Write
        ),
        Some(Reason::HardDenied)
    );
    assert_eq!(reason(&guard, &r.join("src/main.rs"), Access::Write), None);
    assert_eq!(
        reason(&guard, &r.join("scripts/tool.sh"), Access::Write),
        None
    );
}

#[test]
fn a_crowded_folder_of_accounts_counts_as_one_home() {
    let accounts: Vec<PathBuf> = (0..=crate::guard::MAX_ACCOUNT_FOLDERS)
        .map(|i| PathBuf::from(format!("/nfs/home/u{i}")))
        .collect();
    let mut crowded = Vec::new();
    assert_eq!(
        crate::guard::cap_account_homes(accounts, |_| true, &mut crowded),
        vec![PathBuf::from("/nfs/home")]
    );
    assert_eq!(crowded, vec![PathBuf::from("/nfs/home")]);
    // A smaller folder keeps only the homes that are there.
    let base = tempfile::tempdir().unwrap();
    let alice = base.path().join("alice");
    fs::create_dir_all(&alice).unwrap();
    assert_eq!(
        crate::guard::cap_account_homes(
            vec![alice.clone(), base.path().join("bob")],
            Path::is_dir,
            &mut Vec::new()
        ),
        vec![alice]
    );
}

#[test]
fn plugin_and_skill_paths_set_by_a_patch_are_write_refused() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("app");
    fs::create_dir_all(&root).unwrap();
    let grok_home = base.path().join("grok-home");
    // Turbo merges version and campaign patches into the configuration.
    put(
        &grok_home,
        "managed_config.toml",
        &format!(
            "[[version_overrides]]\nminimum_version = \"0.1.0\"\n[version_overrides.plugins]\npaths = [{:?}]\n\n[[campaigns]]\n[campaigns.skills]\npaths = [{:?}]\n",
            root.join("override-plugin"),
            root.join("campaign-skills")
        ),
    );
    let guard =
        PathGuard::with_grok_homes(vec![root.clone()], vec![grok_home], Vec::new(), false).unwrap();
    for rel in ["override-plugin/run.sh", "campaign-skills/s/SKILL.md"] {
        assert_eq!(
            reason(&guard, &root.join(rel), Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
}

#[test]
fn marketplaces_claude_knows_about_are_found() {
    let base = tempfile::tempdir().unwrap();
    let plugins = base.path().join(".claude").join("plugins");
    let checkout = base.path().join("marketplaces").join("team");
    put(
        &plugins,
        "known_marketplaces.json",
        &json!({
            "team": {"installLocation": checkout.to_string_lossy(), "source": {"source": "git"}},
            "broken": {"source": {}}
        })
        .to_string(),
    );
    assert_eq!(
        crate::guard::known_marketplace_paths(&plugins).paths,
        vec![lexical_normalize(&checkout)]
    );
}

#[cfg(unix)]
#[test]
fn files_a_linked_envrc_sources_are_write_refused() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    put(r, ".envrc.shared", "source_env scripts/dev-env.sh\n");
    put(r, "scripts/dev-env.sh", "export A=1\n");
    // direnv loads a linked .envrc like a regular one.
    std::os::unix::fs::symlink(".envrc.shared", r.join(".envrc")).unwrap();
    let guard =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), false).unwrap();
    assert_eq!(
        reason(&guard, &r.join("scripts/dev-env.sh"), Access::Write),
        Some(Reason::HardDenied)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn the_macos_data_volume_and_the_folders_above_it_cannot_be_roots() {
    let homes = candidate_homes();
    assert!(
        homes.contains(&PathBuf::from("/System/Volumes/Data")),
        "{homes:?}"
    );
    assert!(is_over_broad_root(Path::new("/System/Volumes"), &homes));
}

#[test]
fn a_link_named_like_a_refused_file_is_refused_by_that_name_and_where_it_leads() {
    let base = tempfile::tempdir().unwrap();
    let r = base.path();
    put(r, "tools/envrc", "export A=1\n");
    put(r, "ai/claude/settings.json", "{}\n");
    put(r, "shared/grok/config.toml", "[mcp_servers]\n");
    put(r, "config/dev.env", "TOKEN=1\n");
    fs::create_dir_all(r.join("pkg")).unwrap();
    if !symlinks_or_skip(
        make_file_symlink(&Path::new("tools").join("envrc"), &r.join(".envrc")),
        "file",
    ) {
        return;
    }
    assert!(make_dir_symlink(
        &Path::new("ai").join("claude"),
        &r.join(".claude")
    ));
    assert!(make_dir_symlink(
        &Path::new("..").join("shared").join("grok"),
        &r.join("pkg").join(".grok")
    ));
    assert!(make_file_symlink(
        &Path::new("config").join("dev.env"),
        &r.join(".env")
    ));
    let guard =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), false).unwrap();
    for (rel, access) in [
        (".envrc", Access::Write),
        ("tools/envrc", Access::Write),
        ("ai/claude/settings.json", Access::Write),
        ("shared/grok/config.toml", Access::Read),
        ("config/dev.env", Access::Read),
    ] {
        assert_eq!(
            reason(&guard, &r.join(rel), access),
            Some(Reason::HardDenied),
            "{rel} {access:?}"
        );
    }
    // Where a name refused only for writes leads is still readable.
    assert_eq!(reason(&guard, &r.join("tools/envrc"), Access::Read), None);
    // A read-only server refuses what a refused name leads to as well.
    let readonly =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), true).unwrap();
    assert_eq!(
        reason(&readonly, &r.join("shared/grok/config.toml"), Access::Read),
        Some(Reason::HardDenied)
    );
}

#[test]
fn a_link_in_a_git_hooks_folder_leaves_what_it_leads_to_write_refused() {
    let base = tempfile::tempdir().unwrap();
    let r = base.path();
    put(r, ".git/HEAD", "ref: refs/heads/main\n");
    fs::create_dir_all(r.join(".git/objects")).unwrap();
    fs::create_dir_all(r.join(".git/refs")).unwrap();
    fs::create_dir_all(r.join(".git/hooks")).unwrap();
    put(r, "scripts/pre-commit", "#!/bin/sh\n");
    let target = Path::new("..")
        .join("..")
        .join("scripts")
        .join("pre-commit");
    if !symlinks_or_skip(
        make_file_symlink(&target, &r.join(".git").join("hooks").join("pre-commit")),
        "file",
    ) {
        return;
    }
    let guard =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), false).unwrap();
    assert_eq!(
        reason(&guard, &r.join("scripts/pre-commit"), Access::Write),
        Some(Reason::HardDenied)
    );
    assert_eq!(
        reason(&guard, &r.join("scripts/other.sh"), Access::Write),
        None
    );
}

#[test]
fn a_relative_plugin_or_skill_entry_counts_wherever_turbo_could_run() {
    let base = tempfile::tempdir().unwrap();
    let r = base.path().join("repo");
    let grok_home = base.path().join("home-config");
    put(
        &r,
        ".grok/config.toml",
        "[skills]\npaths = [\"docs/skills\"]\n",
    );
    put(
        &grok_home,
        "config.toml",
        "[plugins]\npaths = [\"~/dev/grok-plugin\", \"${PROJECT_TOOLS}/agent-plugins\"]\n",
    );
    put(
        &grok_home,
        "requirements.toml",
        "[skills]\npaths = [\".ai/team\"]\nserver_skill_dirs = [\"../team-skills\"]\n",
    );
    let guard =
        PathGuard::with_grok_homes(vec![r.clone()], vec![grok_home], Vec::new(), false).unwrap();
    for rel in [
        "docs/skills/x/SKILL.md",
        "packages/web/docs/skills/x/SKILL.md",
        "~/dev/grok-plugin/skills/x/SKILL.md",
        "sub/agent-plugins/p/plugin.toml",
        "apps/api/.ai/team/x/SKILL.md",
        "team-skills/x/SKILL.md",
    ] {
        assert_eq!(
            reason(&guard, &r.join(rel), Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    for rel in ["docs/skills.md", "src/skills/x.rs", "packages/docs/x.md"] {
        assert_eq!(reason(&guard, &r.join(rel), Access::Write), None, "{rel}");
    }
}

#[test]
fn a_relative_entry_in_a_grok_home_inside_a_root_counts_from_the_root() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let home = r.join(".turbo-home");
    put(
        &home,
        "config.toml",
        "[skills]\npaths = [\"agent-skills\"]\n",
    );
    let guard =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], vec![home], Vec::new(), false).unwrap();
    assert_eq!(
        reason(&guard, &r.join("agent-skills/x/SKILL.md"), Access::Write),
        Some(Reason::HardDenied)
    );
}

#[test]
fn a_relative_declaration_covers_its_names_from_its_base_down() {
    use crate::guard::{RelativeDeclaration, lowercase_names};
    let base = if cfg!(windows) {
        PathBuf::from(r"C:\r\pkg")
    } else {
        PathBuf::from("/r/pkg")
    };
    let parent = base.parent().unwrap().to_path_buf();
    let declared = RelativeDeclaration::new(Some(&base), 1, vec!["shared".into(), "skills".into()]);
    let covers = |path: &Path| declared.covers(path, &lowercase_names(path));
    assert!(covers(
        &parent
            .join("shared")
            .join("skills")
            .join("s")
            .join("SKILL.md")
    ));
    assert!(covers(&base.join("sub").join("shared").join("skills")));
    assert!(covers(&parent.join("Shared").join("SKILLS")));
    assert!(!covers(&parent.join("other").join("x")));
    let elsewhere = if cfg!(windows) {
        PathBuf::from(r"C:\elsewhere\shared\skills")
    } else {
        PathBuf::from("/elsewhere/shared/skills")
    };
    assert!(!covers(&elsewhere));
}

#[test]
fn envrc_loads_are_read_past_shell_keywords() {
    use crate::guard::EnvrcLoad;
    let loads = crate::guard::envrc_loads(
        "export A=1\nsource_env_if_exists .envrc.private; dotenv \"conf/.env.dev\"\n\
         . ./helpers.sh && use nix\n\
         if [ -f scripts/dev-env.sh ]; then source scripts/dev-env.sh; fi\n\
         { source grouped.sh; }\n( . sub.sh )\n! source negated.sh\n\
         source_up_if_exists dev-env.sh\nuse flake .#dev\nuse_nix shell.nix\nuse devenv\n",
    );
    assert_eq!(
        loads,
        vec![
            EnvrcLoad::FileOrEnvrc(".envrc.private".into()),
            EnvrcLoad::File("conf/.env.dev".into()),
            EnvrcLoad::File("./helpers.sh".into()),
            EnvrcLoad::Nix(None),
            EnvrcLoad::File("scripts/dev-env.sh".into()),
            EnvrcLoad::File("grouped.sh".into()),
            EnvrcLoad::File("sub.sh".into()),
            EnvrcLoad::File("negated.sh".into()),
            EnvrcLoad::Up("dev-env.sh".into()),
            EnvrcLoad::Flake(Some(".#dev".into())),
            EnvrcLoad::Nix(Some("shell.nix".into())),
            EnvrcLoad::Devenv,
        ]
    );
}

#[test]
fn files_an_envrc_loads_in_every_form_are_write_refused() {
    let base = tempfile::tempdir().unwrap();
    let r = base.path().join("app");
    fs::create_dir_all(&r).unwrap();
    // A byte that is not UTF-8 hides nothing: direnv reads the file as bytes.
    fs::write(
        r.join(".envrc"),
        b"# r\xe9glages\nif [ -f scripts/dev-env.sh ]; then source scripts/dev-env.sh; fi\nuse flake\n",
    )
    .unwrap();
    put(
        &r,
        "services/api/.envrc",
        "source_up_if_exists dev-env.sh\nsource_env ..\n",
    );
    // direnv loads the nearest .envrc above a folder, which can load files in it.
    fs::write(
        base.path().join(".envrc"),
        "source_env_if_exists app/web/env.sh\n",
    )
    .unwrap();
    let guard = PathGuard::with_grok_homes(vec![r.clone()], Vec::new(), Vec::new(), false).unwrap();
    for rel in [
        "scripts/dev-env.sh",
        "flake.nix",
        "flake.lock",
        "services/dev-env.sh",
        "dev-env.sh",
        "web/env.sh",
    ] {
        assert_eq!(
            reason(&guard, &r.join(rel), Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    // `source_env ..` loads that folder's `.envrc`, not every file in the folder.
    assert_eq!(
        reason(&guard, &r.join("services/notes.md"), Access::Write),
        None
    );
}

#[test]
fn passwd_is_read_as_bytes_one_line_at_a_time() {
    let passwd = b"root:x:0:0:root:/root:/bin/bash\n\
daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\n\
jose:x:1000:1000:Jos\xe9:/home/jose:/bin/bash\n\
nobody:x:65534:65534:nobody:/:/usr/sbin/nologin\n\
ana:x:1001:1001::/home/ana:/bin/zsh\n";
    assert_eq!(
        crate::guard::passwd_homes(passwd),
        vec![
            PathBuf::from("/root"),
            PathBuf::from("/home/jose"),
            PathBuf::from("/home/ana")
        ]
    );
}

#[test]
fn a_folder_directly_inside_a_crowded_folder_of_homes_cannot_be_a_root() {
    let base = tempfile::tempdir().unwrap();
    let users = base.path().join("users");
    let bob = users.join("bob");
    fs::create_dir_all(bob.join("project")).unwrap();
    let crowded = [users.clone()];
    assert!(crate::guard::is_in_a_crowded_folder(&bob, &crowded));
    assert!(!crate::guard::is_in_a_crowded_folder(
        &bob.join("project"),
        &crowded
    ));
    let mut found = Vec::new();
    let accounts: Vec<PathBuf> = (0..=crate::guard::MAX_ACCOUNT_FOLDERS)
        .map(|i| users.join(format!("u{i}")))
        .collect();
    assert_eq!(
        crate::guard::cap_account_homes(accounts, |_| true, &mut found),
        vec![users.clone()]
    );
    assert_eq!(found, vec![users]);
}

#[test]
fn automount_triggers_nothing_is_mounted_on_are_found_and_left_alone() {
    use crate::guard::{is_over_broad_root_with, parse_mounts, unmounted_automounts};
    let automounts = unmounted_automounts(&parse_mounts(
        b"20 1 8:1 / / rw - ext4 /dev/sda1 rw
30 20 0:50 / /home rw - autofs auto.home rw,fd=7
40 30 0:60 / /home/dan rw - nfs4 nas:/vol/home/dan rw
31 20 0:51 / /mnt/pool rw - autofs systemd-1 rw,fd=8
32 31 0:31 / /mnt/pool rw - btrfs /dev/sdc1 rw",
    ));
    // A direct map with something mounted on it is not a trigger any more; an
    // indirect one, whose keys mount below it, still is.
    assert_eq!(automounts.points, vec![PathBuf::from("/home")]);
    assert_eq!(automounts.mounted, vec![PathBuf::from("/home/dan")]);
    // A key that is mounted can be looked at; one that is not, cannot.
    assert!(automounts.could_mount(Path::new("/home/alice")));
    assert!(!automounts.could_mount(Path::new("/home/dan")));
    assert!(!automounts.could_mount(Path::new("/home/dan/proj")));
    assert!(!automounts.could_mount(Path::new("/home")));

    // A home below a trigger is judged by its path, never by looking at it. The
    // root holds a link to the folder holding the homes, so looking would find
    // the home under the root and call the root over-broad.
    let base = tempfile::tempdir().unwrap();
    let point = base.path().join("auto");
    let home = point.join("alice");
    let root = base.path().join("r");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&root).unwrap();
    if !symlinks_or_skip(
        make_dir_symlink(&Path::new("..").join("auto"), &root.join("auto")),
        "directory",
    ) {
        return;
    }
    let homes = [home.clone()];
    let trigger = crate::guard::Automounts {
        points: vec![point.clone()],
        mounted: Vec::new(),
    };
    assert!(
        crate::guard::is_over_broad_root(&root, &homes),
        "looking below the trigger finds the home under the root"
    );
    assert!(
        !is_over_broad_root_with(&root, &homes, &trigger),
        "a home below a trigger must be judged by its path alone"
    );
    // Once the key is mounted, the same home is looked at again.
    let mounted = crate::guard::Automounts {
        points: vec![point.clone()],
        mounted: vec![home.clone()],
    };
    assert!(is_over_broad_root_with(&root, &homes, &mounted));
    // The point itself, and a home reached through a link to it, count too.
    assert!(is_over_broad_root_with(
        &point,
        std::slice::from_ref(&home),
        &trigger
    ));
    let spelled = base.path().join("homes").join("alice");
    if make_dir_symlink(Path::new("auto"), &base.path().join("homes")) {
        assert!(!trigger.could_mount(&base.path().join("homes")));
        assert!(
            trigger.could_mount(&spelled),
            "a home spelled through a link to the trigger is below it too"
        );
    }
}

// ---------------------------------------------------------------------------
// Round 8: what a link leads to, git repositories, declared locations, and the
// homes a mount can show.
// ---------------------------------------------------------------------------

#[test]
fn hooks_of_the_repository_holding_a_root_and_of_ones_inside_it_are_write_refused() {
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("client");
    // The root is a folder inside a repository whose hooks folder is outside
    // it, and git still runs those hooks for every commit in the repository.
    put_repository(
        &repo,
        "[core]\n\thooksPath = scripts/git-hooks # set by bootstrap\n",
    );
    let root = repo.join("scripts");
    fs::create_dir_all(&root).unwrap();
    // A repository inside the root has hooks of its own.
    put_repository(&root.join("api"), "[core]\n\thooksPath = ci/git-hooks\n");
    let guard =
        PathGuard::with_grok_homes(vec![root.clone()], Vec::new(), Vec::new(), false).unwrap();
    for path in [
        repo.join("scripts").join("git-hooks").join("pre-commit"),
        root.join("api")
            .join("ci")
            .join("git-hooks")
            .join("pre-commit"),
    ] {
        assert_eq!(
            reason(&guard, &path, Access::Write),
            Some(Reason::HardDenied),
            "{path:?}"
        );
    }
    assert_eq!(
        reason(&guard, &root.join("api/src/main.rs"), Access::Write),
        None
    );
}

#[test]
fn a_link_named_like_the_first_half_of_a_rule_is_refused_where_it_leads() {
    let base = tempfile::tempdir().unwrap();
    let r = base.path();
    put(r, "k8s-auth/config", "token: SENTINEL\n");
    put(r, "aws-profile/credentials", "[default]\n");
    put(r, "hooks-dir/hooks.json", "{}\n");
    if !symlinks_or_skip(
        make_dir_symlink(Path::new("k8s-auth"), &r.join(".kube")),
        "directory",
    ) {
        return;
    }
    assert!(make_dir_symlink(Path::new("aws-profile"), &r.join(".aws")));
    assert!(make_dir_symlink(Path::new("hooks-dir"), &r.join("hooks")));
    let guard =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), true).unwrap();
    for (rel, access) in [
        ("k8s-auth/config", Access::Read),
        ("aws-profile/credentials", Access::Read),
        ("k8s-auth", Access::Walk),
        ("aws-profile", Access::Walk),
    ] {
        assert_eq!(
            reason(&guard, &r.join(rel), access),
            Some(Reason::HardDenied),
            "{rel} {access:?}"
        );
    }
    // Only what the rule names is refused, not the rest of the folder.
    assert_eq!(
        reason(&guard, &r.join("k8s-auth/notes.md"), Access::Read),
        None
    );
    let edit =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), false).unwrap();
    assert_eq!(
        reason(&edit, &r.join("hooks-dir/hooks.json"), Access::Write),
        Some(Reason::HardDenied)
    );
}

#[test]
fn a_declared_folder_that_is_a_link_leaves_what_it_leads_to_write_refused() {
    let base = tempfile::tempdir().unwrap();
    let r = base.path();
    put(
        r,
        ".grok/config.toml",
        "[plugins]\npaths = [\"tools/my-plugin\"]\n[skills]\npaths = [\"docs/skills\"]\n",
    );
    fs::create_dir_all(r.join("vendor/my-plugin")).unwrap();
    fs::create_dir_all(r.join("handbook/skills")).unwrap();
    fs::create_dir_all(r.join("tools")).unwrap();
    if !symlinks_or_skip(
        make_dir_symlink(
            &Path::new("..").join("vendor").join("my-plugin"),
            &r.join("tools").join("my-plugin"),
        ),
        "directory",
    ) {
        return;
    }
    // A link on a name above the declared folder counts too.
    assert!(make_dir_symlink(Path::new("handbook"), &r.join("docs")));
    let guard =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), false).unwrap();
    for rel in [
        "vendor/my-plugin/scripts/run.sh",
        "tools/my-plugin/scripts/run.sh",
        "handbook/skills/release/SKILL.md",
        "docs/skills/release/SKILL.md",
    ] {
        assert_eq!(
            reason(&guard, &r.join(rel), Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    assert_eq!(
        reason(&guard, &r.join("vendor/other/main.rs"), Access::Write),
        None
    );
}

#[test]
fn a_refused_name_link_that_leads_nowhere_yet_refuses_what_it_will_lead_to() {
    let base = tempfile::tempdir().unwrap();
    let r = base.path();
    // A repository can commit the link and create its target later.
    if !symlinks_or_skip(
        make_dir_symlink(&Path::new("ai").join("claude"), &r.join(".claude")),
        "directory",
    ) {
        return;
    }
    let guard =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), false).unwrap();
    assert_eq!(
        reason(&guard, &r.join("ai/claude/settings.json"), Access::Write),
        Some(Reason::HardDenied)
    );
    assert_eq!(reason(&guard, &r.join("ai/notes.md"), Access::Write), None);
}

#[test]
fn a_link_below_a_folder_a_link_leads_to_is_followed_too() {
    let base = tempfile::tempdir().unwrap();
    let r = base.path();
    fs::create_dir_all(r.join(".claude")).unwrap();
    fs::create_dir_all(r.join("skills")).unwrap();
    fs::create_dir_all(r.join("vendor/deploy-skill")).unwrap();
    if !symlinks_or_skip(
        make_dir_symlink(
            &Path::new("..").join("skills"),
            &r.join(".claude").join("skills"),
        ),
        "directory",
    ) {
        return;
    }
    assert!(make_dir_symlink(
        &Path::new("..").join("vendor").join("deploy-skill"),
        &r.join("skills").join("deploy")
    ));
    let guard =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), false).unwrap();
    assert_eq!(
        reason(
            &guard,
            &r.join("vendor/deploy-skill/SKILL.md"),
            Access::Write
        ),
        Some(Reason::HardDenied)
    );
    assert_eq!(
        reason(&guard, &r.join("vendor/other/SKILL.md"), Access::Write),
        None
    );
}

#[test]
fn a_link_from_the_grok_home_into_a_root_is_refused_where_it_leads() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("app");
    let grok_home = base.path().join("grok-home");
    fs::create_dir_all(root.join("team-skills")).unwrap();
    fs::create_dir_all(grok_home.join("skills")).unwrap();
    // Turbo loads every skill folder in the Grok home, link or not.
    if !symlinks_or_skip(
        make_dir_symlink(
            &root.join("team-skills"),
            &grok_home.join("skills").join("team"),
        ),
        "directory",
    ) {
        return;
    }
    let guard =
        PathGuard::with_grok_homes(vec![root.clone()], vec![grok_home], Vec::new(), true).unwrap();
    assert_eq!(
        reason(&guard, &root.join("team-skills/SKILL.md"), Access::Read),
        Some(Reason::HardDenied)
    );
    assert_eq!(
        reason(&guard, &root.join("src/main.rs"), Access::Read),
        None
    );
}

#[test]
fn plugins_the_install_registry_records_are_write_refused() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("app");
    fs::create_dir_all(&root).unwrap();
    let grok_home = base.path().join("grok-home");
    // A local install keeps a snapshot of the whole source folder and re-copies
    // it at every session spawn, so both the source and the snapshot run.
    let registry = json!({
        "version": 1,
        "repos": {
            "deploy-tools-a1b2c3d4": {
                "kind": {
                    "type": "Local",
                    "source_path": root.join("tools").join("deploy-tools"),
                    "subdir": null
                },
                "path": root.join("snapshots").join("deploy-tools"),
                "installed_at": "",
                "updated_at": "",
                "plugins": {}
            }
        }
    });
    put(
        &grok_home,
        "installed-plugins/registry.json",
        &registry.to_string(),
    );
    let guard =
        PathGuard::with_grok_homes(vec![root.clone()], vec![grok_home], Vec::new(), false).unwrap();
    for rel in [
        "tools/deploy-tools/skills/x/SKILL.md",
        "tools/deploy-tools/scripts/pre.sh",
        "snapshots/deploy-tools/scripts/run.sh",
    ] {
        assert_eq!(
            reason(&guard, &root.join(rel), Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    assert_eq!(
        reason(&guard, &root.join("src/main.rs"), Access::Write),
        None
    );
}

#[test]
fn campaign_overrides_declare_their_paths_in_the_form_turbo_reads() {
    let base = tempfile::tempdir().unwrap();
    let skills = base.path().join("campaign-skills");
    // An entry is a campaign id and a patch flattened beside it.
    let json = json!([{
        "id": "team",
        "skills": {"paths": [skills.to_string_lossy()]},
        "plugins": {"paths": ["team-plugin"]}
    }])
    .to_string();
    let declared = crate::guard::campaign_override_paths_from(&json);
    assert!(
        declared.paths.contains(&lexical_normalize(&skills)),
        "{declared:?}"
    );
    assert!(
        declared
            .relative
            .iter()
            .any(|entry| entry.names == ["team-plugin"]),
        "{declared:?}"
    );
}

#[test]
fn an_entry_whose_variable_expands_to_nothing_declares_nothing() {
    // A variable that is set but empty names no folder, and Turbo loads nothing
    // from it. A location of no names would be every location.
    let mut declared = crate::guard::Declared::default();
    crate::guard::declare_expanded("$TEAM_SKILLS", "", None, &mut declared);
    assert!(
        declared.paths.is_empty() && declared.relative.is_empty(),
        "{declared:?}"
    );
    let absolute = if cfg!(windows) {
        r"C:\team\skills"
    } else {
        "/team/skills"
    };
    crate::guard::declare_expanded("$TEAM_SKILLS/skills", absolute, None, &mut declared);
    assert_eq!(declared.paths, vec![PathBuf::from(absolute)]);
}

#[test]
fn plugins_claude_records_as_installed_are_found() {
    let base = tempfile::tempdir().unwrap();
    let plugins = base.path().join(".claude").join("plugins");
    let installed = base.path().join("claude-plugins").join("p");
    put(
        &plugins,
        "installed_plugins.json",
        &json!({
            "version": 2,
            "plugins": {
                "p@local": [{"scope": "user", "installPath": installed.to_string_lossy()}]
            }
        })
        .to_string(),
    );
    assert_eq!(
        crate::guard::installed_plugin_paths(&plugins).paths,
        vec![lexical_normalize(&installed)]
    );
}

#[test]
fn more_declared_locations_than_the_guard_keeps_refuse_the_edit_tier() {
    let base = tempfile::tempdir().unwrap();
    let r = base.path();
    let entries: Vec<String> = (0..=crate::guard::MAX_DECLARED_LOCATIONS)
        .map(|i| format!("\"p{i}\""))
        .collect();
    put(
        r,
        ".grok/config.toml",
        &format!("[plugins]\npaths = [{}]\n", entries.join(", ")),
    );
    let error = PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), false)
        .unwrap_err();
    assert_eq!(error.reason, Reason::TooManyDeclarations);
    // The read-only tier reads no declarations at all.
    assert!(
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), true).is_ok()
    );
    // One location declared many times is one location.
    put(
        r,
        ".grok/config.toml",
        &format!("[plugins]\npaths = [{}]\n", vec!["\"p\""; 8000].join(", ")),
    );
    assert!(
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), false).is_ok()
    );
}

#[test]
fn envrc_arguments_are_read_the_way_a_shell_splits_them() {
    use crate::guard::EnvrcLoad;
    let loads = crate::guard::envrc_loads(
        "source \"scripts/my env.sh\"\nsource \\\n  scripts/dev.sh\n# source commented.sh\n\
         source 'single quoted.sh' # trailing\n(source grouped.sh)\ndotenv conf/.env#tag\n",
    );
    assert_eq!(
        loads,
        vec![
            EnvrcLoad::File("scripts/my env.sh".into()),
            EnvrcLoad::File("scripts/dev.sh".into()),
            EnvrcLoad::File("single quoted.sh".into()),
            EnvrcLoad::File("grouped.sh".into()),
            EnvrcLoad::File("conf/.env#tag".into()),
        ]
    );
}

#[test]
fn a_file_an_envrc_sources_by_a_quoted_name_is_write_refused() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    put(r, ".envrc", "source \"scripts/my env.sh\"\n");
    put(r, "scripts/my env.sh", "export A=1\n");
    let guard =
        PathGuard::with_grok_homes(vec![r.to_path_buf()], Vec::new(), Vec::new(), false).unwrap();
    assert_eq!(
        reason(&guard, &r.join("scripts/my env.sh"), Access::Write),
        Some(Reason::HardDenied)
    );
    assert_eq!(
        reason(&guard, &r.join("scripts/my.sh"), Access::Write),
        None
    );
}

#[test]
fn git_config_values_end_at_a_comment_and_a_key_can_follow_its_header() {
    use crate::guard::testing::parse_git_config;
    let entries = parse_git_config(
        "[core] hooksPath = scripts/hooks # set by bootstrap\n\
         [include] path = .gitconfig-team ; shared\n\
         [core]\n\thooksPath = \"a \\\n b\"\n\
         [core] attributesFile = a\\tb\\bc\n",
    );
    assert_eq!(
        entries,
        vec![
            (
                "core".to_string(),
                "hookspath".to_string(),
                "scripts/hooks".to_string()
            ),
            (
                "include".to_string(),
                "path".to_string(),
                ".gitconfig-team".to_string()
            ),
            (
                "core".to_string(),
                "hookspath".to_string(),
                "a  b".to_string()
            ),
            // Git's escapes: a tab, and a backspace character rather than an
            // instruction to drop the character before it.
            (
                "core".to_string(),
                "attributesfile".to_string(),
                "a\tb\u{8}c".to_string()
            ),
        ]
    );
    // A `]` inside a quoted subsection does not close the header.
    assert_eq!(
        parse_git_config("[includeIf \"gitdir:~/work/[ab]/\"] path = inc.cfg\n"),
        vec![(
            "includeif".to_string(),
            "path".to_string(),
            "inc.cfg".to_string()
        )]
    );
}

#[test]
fn every_pair_rule_name_is_refused_with_the_name_that_completes_it() {
    for (first, seconds) in crate::guard::PAIR_RULE_NAMES {
        for second in *seconds {
            let path = PathBuf::from("/r").join(first).join(second);
            assert!(
                [Access::Read, Access::Walk, Access::Write]
                    .iter()
                    .any(|access| {
                        crate::guard::testing::name_rule_reason(&path, &path, *access).is_some()
                    }),
                "{path:?} matches no rule, so probing it through a link proves nothing"
            );
        }
    }
}

#[test]
fn the_compare_form_matches_the_one_canonicalizing_produces() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = xai_grok_tools::types::resources::canonicalize_for_permission(dir.path());
    assert_eq!(
        crate::guard::fold_for_compare(&canonical.display),
        canonical.compare
    );
}

#[test]
fn a_bind_mount_of_one_home_inside_a_crowded_folder_is_that_home() {
    use crate::guard::{crowded_folder_homes, parse_mounts};
    // An SFTP server chroots each account, and there are too many accounts in
    // /home to list one by one.
    let mounts = parse_mounts(
        b"20 1 8:1 / / rw - ext4 /dev/sda1 rw
50 20 8:1 /home/bob /srv/sftp/bob/home rw - ext4 /dev/sda1 rw
51 20 8:1 /home/bob/data /srv/data rw - ext4 /dev/sda1 rw",
    );
    assert_eq!(
        crowded_folder_homes(Path::new("/home"), &mounts),
        vec![PathBuf::from("/srv/sftp/bob/home")]
    );
}

#[test]
fn nfs_mounts_are_placed_by_the_folder_their_source_names() {
    use crate::guard::{home_aliases, parse_mounts};
    // NFS prints "/" as every mount's root, so without the source a project
    // export would look like a second path to the folder holding the homes.
    let mounts = parse_mounts(
        b"20 1 8:1 / / rw - ext4 /dev/sda1 rw
30 20 0:60 / /home rw - nfs4 nas:/vol/home rw
31 20 0:60 / /mnt/app rw - nfs4 nas:/vol/projects/app rw
32 20 0:60 / /mnt/nas rw - nfs4 nas:/vol rw
33 20 0:60 / /mnt/six rw - nfs4 [fe80::1]:/vol rw",
    );
    let aliases = home_aliases(Path::new("/home/dan"), &mounts);
    // A bracketed IPv6 source names its export folder like any other.
    assert!(
        aliases.contains(&PathBuf::from("/mnt/six/home/dan")),
        "{aliases:?}"
    );
    assert!(
        aliases.contains(&PathBuf::from("/mnt/nas/home/dan")),
        "{aliases:?}"
    );
    assert!(
        !aliases.iter().any(|alias| alias.starts_with("/mnt/app")),
        "{aliases:?}"
    );
}

#[test]
fn audit_a_search_argument_ripgrep_could_not_be_given_is_refused() {
    let (guard, root, _o) = fixture();
    let long = "x".repeat(9000);
    for args in [
        json!({"path": s(root.path()), "pattern": "a\u{0}b"}),
        json!({"path": s(root.path()), "pattern": long}),
        json!({"path": s(root.path()), "pattern": "x", "type": "rust\u{0}"}),
    ] {
        assert_eq!(
            guard
                .check_call("grep", ToolKind::Search, &args)
                .unwrap_err()
                .reason,
            Reason::MalformedArgument,
            "{args}"
        );
    }
    assert!(
        guard
            .check_call(
                "grep",
                ToolKind::Search,
                &json!({"path": s(root.path()), "pattern": "x".repeat(4096), "type": "rust"})
            )
            .is_ok()
    );
}

// ---------------------------------------------------------------------------
// Round 9: what the verification audit found, each proved by a test that fails
// if its fix is reverted.
// ---------------------------------------------------------------------------

#[test]
fn a_dangling_link_from_a_loader_folder_refuses_the_target_it_names() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("home");
    let root = base.path().join("app");
    fs::create_dir_all(home.join(".claude").join("skills")).unwrap();
    fs::create_dir_all(&root).unwrap();
    // The target does not exist yet: a link left behind when a folder was
    // renamed, or committed before the folder was created.
    if !symlinks_or_skip(
        make_dir_symlink(
            &root.join("team-skills"),
            &home.join(".claude").join("skills").join("team"),
        ),
        "directory",
    ) {
        return;
    }
    let guard = crate::guard::testing::with_test_homes(vec![home], || {
        PathGuard::with_grok_homes(vec![root.clone()], Vec::new(), Vec::new(), false)
    })
    .unwrap();
    assert_eq!(
        reason(&guard, &root.join("team-skills/SKILL.md"), Access::Write),
        Some(Reason::HardDenied),
        "a client could create the folder the link already names"
    );
    assert_eq!(
        reason(&guard, &root.join("src/main.rs"), Access::Write),
        None
    );
}

#[test]
fn a_chain_of_links_out_of_a_loader_folder_is_followed_back_into_a_root() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("home");
    let root = base.path().join("app");
    let shared = base.path().join("shared-skills");
    fs::create_dir_all(home.join(".claude").join("skills")).unwrap();
    fs::create_dir_all(root.join("skills")).unwrap();
    fs::create_dir_all(&shared).unwrap();
    // The first link leaves every root; the second comes back into one.
    if !symlinks_or_skip(
        make_dir_symlink(&shared, &home.join(".claude").join("skills").join("shared")),
        "directory",
    ) {
        return;
    }
    assert!(make_dir_symlink(&root.join("skills"), &shared.join("app")));
    let guard = crate::guard::testing::with_test_homes(vec![home], || {
        PathGuard::with_grok_homes(vec![root.clone()], Vec::new(), Vec::new(), false)
    })
    .unwrap();
    assert_eq!(
        reason(&guard, &root.join("skills/deploy/SKILL.md"), Access::Write),
        Some(Reason::HardDenied)
    );
    assert_eq!(
        reason(&guard, &root.join("src/main.rs"), Access::Write),
        None
    );
}

#[test]
fn a_global_git_config_inside_a_root_is_write_refused() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("srv");
    let home = root.join("config-home");
    fs::create_dir_all(&home).unwrap();
    put(&home, ".gitconfig", "[core]\n\thooksPath = githooks\n");
    let guard = crate::guard::testing::with_test_homes(vec![home.clone()], || {
        PathGuard::with_grok_homes(vec![root.clone()], Vec::new(), Vec::new(), false)
    })
    .unwrap();
    // Git reads it for every repository, so a client could add a hooksPath.
    assert_eq!(
        reason(&guard, &home.join(".gitconfig"), Access::Write),
        Some(Reason::HardDenied)
    );
    assert_eq!(
        reason(&guard, &root.join("src/main.rs"), Access::Write),
        None
    );
}

fn put_repository(top: &Path, config: &str) {
    put(top, ".git/config", config);
    put(top, ".git/HEAD", "ref: refs/heads/main\n");
    fs::create_dir_all(top.join(".git").join("objects")).unwrap();
    fs::create_dir_all(top.join(".git").join("refs")).unwrap();
}

#[test]
fn a_stray_git_folder_does_not_hide_the_repository_that_holds_a_root() {
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("proj");
    // Git runs these hooks for a commit made anywhere in the repository.
    put_repository(&repo, "[core]\n\thooksPath = web/hooks\n");
    let root = repo.join("web");
    // A stray `.git`: an aborted clone, or a bare `mkdir`. Git walks past it.
    fs::create_dir_all(root.join(".git")).unwrap();
    let guard =
        PathGuard::with_grok_homes(vec![root.clone()], Vec::new(), Vec::new(), false).unwrap();
    assert_eq!(
        reason(&guard, &root.join("hooks/pre-commit"), Access::Write),
        Some(Reason::HardDenied)
    );
    assert_eq!(
        reason(&guard, &root.join("src/main.rs"), Access::Write),
        None
    );
}

#[test]
fn a_hooks_path_that_names_the_worktree_refuses_nothing() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("app");
    // An operator turning hooks off, which git reads as the worktree itself.
    put_repository(&root, "[core]\n\thooksPath =\n");
    let guard =
        PathGuard::with_grok_homes(vec![root.clone()], Vec::new(), Vec::new(), false).unwrap();
    assert_eq!(
        reason(&guard, &root.join("src/main.rs"), Access::Write),
        None
    );
    assert_eq!(reason(&guard, &root.join("notes.md"), Access::Write), None);
}

#[test]
fn an_install_dir_a_patch_moves_or_names_relatively_is_still_refused() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("app");
    fs::create_dir_all(&root).unwrap();
    let grok_home = base.path().join("grok-home");
    let moved = root.join("tools").join("installed");
    // A campaign patch moves the tree Turbo installs plugins into.
    put(
        &grok_home,
        "config.toml",
        &format!(
            "[[campaigns]]\nid = \"team\"\n[campaigns.plugins]\ninstall_dir = {:?}\n",
            moved
        ),
    );
    put(
        &moved,
        "registry.json",
        &json!({
            "version": 1,
            "repos": {
                "local-a1b2c3d4": {
                    "kind": {"type": "Local", "source_path": root.join("src-plugin"), "subdir": null},
                    "path": moved.join("local"),
                    "installed_at": "",
                    "updated_at": "",
                    "plugins": {}
                }
            }
        })
        .to_string(),
    );
    let guard =
        PathGuard::with_grok_homes(vec![root.clone()], vec![grok_home], Vec::new(), false).unwrap();
    for rel in [
        "tools/installed/local/run.sh",
        "src-plugin/scripts/on-start.sh",
    ] {
        assert_eq!(
            reason(&guard, &root.join(rel), Access::Write),
            Some(Reason::HardDenied),
            "{rel}"
        );
    }
    assert_eq!(
        reason(&guard, &root.join("src/main.rs"), Access::Write),
        None
    );
}

#[test]
fn a_relative_install_dir_counts_wherever_turbo_runs() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("app");
    fs::create_dir_all(&root).unwrap();
    let grok_home = base.path().join("grok-home");
    put(
        &grok_home,
        "config.toml",
        "[plugins]\ninstall_dir = \"installed-plugins\"\n",
    );
    let guard =
        PathGuard::with_grok_homes(vec![root.clone()], vec![grok_home], Vec::new(), false).unwrap();
    assert_eq!(
        reason(
            &guard,
            &root.join("installed-plugins/p/plugin.json"),
            Access::Write
        ),
        Some(Reason::HardDenied)
    );
    assert_eq!(
        reason(&guard, &root.join("src/main.rs"), Access::Write),
        None
    );
}

#[test]
fn an_nfs_bind_mount_keeps_the_subtree_it_shows() {
    use crate::guard::{home_aliases, parse_mounts};
    // The project is bind-mounted out of an NFS home before serving.
    let mounts = parse_mounts(
        b"20 1 8:1 / / rw - ext4 /dev/sda1 rw
30 20 0:60 / /home rw - nfs4 nas:/vol/home rw
31 20 0:60 /dan/projects/app /srv/app rw - nfs4 nas:/vol/home rw",
    );
    let bind = mounts
        .iter()
        .find(|mount| mount.mount_point == Path::new("/srv/app"))
        .expect("the bind mount is parsed");
    assert_eq!(bind.root, PathBuf::from("/vol/home/dan/projects/app"));
    // So no home is invented inside it, and the root stays servable.
    let aliases = home_aliases(Path::new("/home/dan"), &mounts);
    assert!(
        !aliases.iter().any(|alias| alias.starts_with("/srv/app")),
        "{aliases:?}"
    );
}

#[test]
fn a_grep_glob_too_long_for_a_command_line_is_refused() {
    let (guard, root, _o) = fixture();
    let long = "a".repeat(9000);
    assert_eq!(
        guard
            .check_call(
                "grep",
                ToolKind::Search,
                &json!({"path": s(root.path()), "pattern": "x", "glob": long})
            )
            .unwrap_err()
            .reason,
        Reason::MalformedArgument
    );
    assert!(
        guard
            .check_call(
                "grep",
                ToolKind::Search,
                &json!({"path": s(root.path()), "pattern": "x", "glob": "*.rs"})
            )
            .is_ok()
    );
}
