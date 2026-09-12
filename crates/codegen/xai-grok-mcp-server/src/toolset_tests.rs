//! Tests for the served toolset: that the boundary is really in the dispatch
//! path, that the filesystem layer re-checks resolved paths and reports what it
//! stops, that grep never returns a refused file, and that resource bounds,
//! shutdown and private state hold.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use xai_grok_tools::computer::local::LocalFs;
use xai_grok_tools::computer::types::AsyncFileSystem;
use xai_grok_tools::types::tool::ToolKind;

use crate::guard::{PathGuard, REFUSAL_TEXT, Reason};
use crate::read_confined_fs::{
    MAX_LINE_WINDOW_BYTES, MAX_WHOLE_FILE_BYTES, ReadConfinedFs, SyncGate, TestHooks,
};
use crate::toolset::{
    CALL_TIMEOUT, CallFailure, MAX_CONCURRENT_CALLS, ServeEvent, ServedToolset, ToolsetOptions,
};

fn symlinks_or_skip(created: bool, what: &str) -> bool {
    if created {
        return true;
    }
    if std::env::var("TURBO_ALLOW_SYMLINK_SKIP").as_deref() == Ok("1") {
        eprintln!("SKIPPED ({what} symlink): creation unavailable and TURBO_ALLOW_SYMLINK_SKIP=1");
        return false;
    }
    panic!(
        "cannot create a {what} symlink on this host; this test proves a security property. \
         Enable symlink creation or set TURBO_ALLOW_SYMLINK_SKIP=1 to skip it explicitly."
    );
}

#[cfg(unix)]
fn make_file_symlink(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}
#[cfg(windows)]
fn make_file_symlink(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_file(target, link).is_ok()
}

#[cfg(unix)]
fn make_dir_symlink(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}
#[cfg(windows)]
fn make_dir_symlink(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_dir(target, link).is_ok()
}

fn put(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

async fn readonly(root: &Path) -> ServedToolset {
    ServedToolset::new(vec![root.to_path_buf()], true)
        .await
        .expect("toolset builds")
}

async fn editable(root: &Path) -> ServedToolset {
    ServedToolset::new(vec![root.to_path_buf()], false)
        .await
        .expect("toolset builds")
}

/// A toolset whose filesystem operations wait while the returned gate holds
/// `false`. Built with the gate open, so construction never waits on it.
async fn gated_with(
    root: &Path,
    read_only: bool,
    mut options: ToolsetOptions,
) -> (ServedToolset, tokio::sync::watch::Sender<bool>) {
    let (gate, receiver) = tokio::sync::watch::channel(true);
    options.hooks.gate = Some(receiver);
    let ts = ServedToolset::with_options(vec![root.to_path_buf()], read_only, options)
        .await
        .expect("toolset builds");
    (ts, gate)
}

async fn gated(
    root: &Path,
    read_only: bool,
    call_timeout: Duration,
) -> (ServedToolset, tokio::sync::watch::Sender<bool>) {
    gated_with(
        root,
        read_only,
        ToolsetOptions {
            call_timeout,
            ..ToolsetOptions::default()
        },
    )
    .await
}

fn record_refusals(ts: &mut ServedToolset) -> Arc<Mutex<Vec<Reason>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    ts.set_observer(Arc::new(move |event: ServeEvent<'_>| {
        if let ServeEvent::Refused { reason, .. } = event {
            sink.lock().unwrap().push(reason);
        }
    }));
    seen
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_ripgrep_ran(out: &str) {
    // A served call reports a failed spawn in its own words, which the
    // unserved wording would never match.
    assert!(
        !out.contains("failed to spawn ripgrep") && !out.contains("could not be started"),
        "ripgrep is unavailable, so this test proves nothing:\n{out}"
    );
}

#[tokio::test]
async fn served_toolset_lists_only_declared_tools() {
    let root = tempfile::tempdir().unwrap();
    let ts = readonly(root.path()).await;
    let names: Vec<&str> = ts.list().iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"read_file"), "{names:?}");
    assert!(names.contains(&"list_dir"), "{names:?}");
    assert!(names.contains(&"grep"), "{names:?}");
    assert!(
        !names
            .iter()
            .any(|n| n.contains("run_terminal_cmd") || n.contains("bash") || n.contains("exec")),
        "{names:?}"
    );
}

#[tokio::test]
async fn every_served_tool_has_the_expected_kind_schema_and_description() {
    let root = tempfile::tempdir().unwrap();
    let ts = editable(root.path()).await;
    for t in ts.list() {
        assert!(
            matches!(
                (t.name.as_str(), t.kind),
                ("read_file", ToolKind::Read)
                    | ("list_dir", ToolKind::List)
                    | ("grep", ToolKind::Search)
                    | ("search_replace", ToolKind::Edit)
            ),
            "unexpected (name, kind) for {}",
            t.name
        );
        assert!(
            t.schema
                .get("properties")
                .and_then(Value::as_object)
                .is_some(),
            "{} must advertise an object `properties` map",
            t.name
        );
        assert!(
            !t.description.trim().is_empty(),
            "{} has no description",
            t.name
        );
    }
}

#[tokio::test]
async fn call_outside_root_is_refused_before_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("id_rsa_copy");
    fs::write(&secret, b"PRIVATE KEY").unwrap();
    let ts = readonly(root.path()).await;
    let err = ts
        .call(
            "read_file",
            json!({ "target_file": secret.to_string_lossy() }),
        )
        .await
        .expect_err("outside-root read must be refused");
    assert!(matches!(err, CallFailure::Refused(_)), "{err:?}");
}

#[tokio::test]
async fn call_to_an_unlisted_tool_never_reaches_the_bridge() {
    let root = tempfile::tempdir().unwrap();
    let ts = readonly(root.path()).await;
    let err = ts
        .call("run_terminal_cmd", json!({ "command": "whoami" }))
        .await
        .expect_err("shell must be refused");
    assert!(matches!(err, CallFailure::Refused(_)), "{err:?}");
}

#[tokio::test]
async fn read_inside_root_succeeds_end_to_end() {
    let root = tempfile::tempdir().unwrap();
    let f = root.path().join("hello.txt");
    fs::write(&f, b"turbo mcp serve").unwrap();
    let ts = readonly(root.path()).await;
    let out = ts
        .call("read_file", json!({"target_file": f.to_string_lossy()}))
        .await
        .expect("in-root read must succeed");
    assert!(out.contains("turbo mcp serve"), "{out}");
}

#[tokio::test]
async fn empty_roots_refuses_to_build() {
    match ServedToolset::new(Vec::new(), true).await {
        Ok(_) => panic!("empty roots must never build a servable toolset"),
        Err(e) => assert!(e.to_string().contains("invalid roots"), "{e}"),
    }
}

#[tokio::test]
async fn audit_the_home_directory_refuses_to_build() {
    let home = dirs::home_dir().expect("a home directory");
    match ServedToolset::new(vec![home], true).await {
        Ok(_) => panic!("the home directory must not be approvable as a root"),
        Err(e) => assert!(e.to_string().contains("home directory"), "{e}"),
    }
}

/// The drift guard: every property the live bridge advertises must be accounted
/// for, or the tool would be silently dropped from the served surface.
#[tokio::test]
async fn every_advertised_property_is_accounted_for() {
    use xai_grok_tools::bridge::ToolBridge;
    use xai_grok_tools::computer::local::LocalTerminalBackend;
    use xai_grok_tools::computer::types::TerminalBackend;
    use xai_grok_tools::implementations::grok_build;
    use xai_grok_tools::notification::ToolNotificationHandle;
    use xai_grok_tools::registry::types::{SessionContext, ToolServerConfig};

    let root = tempfile::tempdir().unwrap();
    let sf = tempfile::tempdir().unwrap();
    let fs_impl: Arc<dyn AsyncFileSystem> = Arc::new(LocalFs);
    let backend: Arc<dyn TerminalBackend> = Arc::new(LocalTerminalBackend::new());
    let ctx = SessionContext {
        backend,
        fs: fs_impl,
        cwd: root.path().to_path_buf(),
        session_folder: sf.path().to_path_buf(),
        session_env: Arc::new(std::collections::HashMap::new()),
        notification_handle: ToolNotificationHandle::noop(),
        owner_session_id: None,
        subagent: None,
        parent_scheduler_handle: None,
        skills: vec![],
        state_path: sf.path().join("s.json"),
        memory_backend: None,
        web_search_config: Default::default(),
        web_fetch_config: Default::default(),
        lsp: None,
        image_gen_config: Default::default(),
        video_gen_config: Default::default(),
        app_builder_deployer_config: Default::default(),
        api_key_provider: None,
        auth_provider: None,
        attribution_callback: None,
        system_reminder_tag: xai_grok_tools::reminders::DEFAULT_REMINDER_TAG,
    };
    let cfg = ToolServerConfig {
        tools: vec![
            (&grok_build::ReadFileTool).into(),
            (&grok_build::ListDirTool).into(),
            (&grok_build::GrepTool).into(),
            (&grok_build::SearchReplaceTool).into(),
        ],
        behavior_preset: None,
    };
    let b = ToolBridge::finalize_builder(ToolBridge::get_builder(), cfg, ctx)
        .await
        .unwrap();

    let mut unaccounted = Vec::new();
    for d in b.tool_definitions().await {
        let name = d.function.name.clone();
        if let Err(e) = crate::guard::validate_tool_schema(&name, &d.function.parameters) {
            let props: Vec<String> = d
                .function
                .parameters
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            unaccounted.push(format!("{name} ({:?}) advertises {props:?}", e.reason));
        }
    }
    assert!(
        unaccounted.is_empty(),
        "declaration table is stale:\n  {}",
        unaccounted.join("\n  ")
    );
}

#[tokio::test]
async fn readonly_tier_does_not_serve_search_replace() {
    let root = tempfile::tempdir().unwrap();
    let ts = readonly(root.path()).await;
    assert!(!ts.list().iter().any(|t| t.name == "search_replace"));
}

#[tokio::test]
async fn edit_tier_serves_search_replace() {
    let root = tempfile::tempdir().unwrap();
    let ts = editable(root.path()).await;
    assert!(ts.list().iter().any(|t| t.name == "search_replace"));
}

#[tokio::test]
async fn edit_tier_creates_a_file_inside_root() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("new.txt");
    let ts = editable(root.path()).await;
    let res = ts
        .call(
            "search_replace",
            json!({"file_path": target.to_string_lossy(), "old_string": "", "new_string": "hello"}),
        )
        .await;
    assert!(res.is_ok(), "in-root create must succeed: {res:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
}

#[tokio::test]
async fn edit_tier_refuses_a_create_outside_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("pwn.bat");
    let ts = editable(root.path()).await;
    let err = ts
        .call(
            "search_replace",
            json!({"file_path": target.to_string_lossy(), "old_string": "", "new_string": "x"}),
        )
        .await
        .expect_err("outside-root create must be refused");
    assert!(matches!(err, CallFailure::Refused(_)), "{err:?}");
    assert!(!target.exists());
}

#[tokio::test]
async fn edit_tier_refuses_writing_git_config_inside_root() {
    let root = tempfile::tempdir().unwrap();
    let cfg = root.path().join(".git").join("config");
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    fs::write(&cfg, "[core]\n").unwrap();
    let ts = editable(root.path()).await;
    let err = ts
        .call(
            "search_replace",
            json!({"file_path": cfg.to_string_lossy(), "old_string": "[core]", "new_string": "[core]\n\thooksPath = /tmp"}),
        )
        .await
        .expect_err(".git/config must be refused");
    // Refused by the boundary, not a tool failure to match `old_string`.
    assert!(matches!(err, CallFailure::Refused(_)), "{err:?}");
    assert_eq!(fs::read_to_string(&cfg).unwrap(), "[core]\n");
}

#[tokio::test]
async fn audit_edit_tier_refuses_planting_a_project_hook_config() {
    // A hook config Turbo runs on its next launch in an already-trusted folder.
    let root = tempfile::tempdir().unwrap();
    let ts = editable(root.path()).await;
    let r = ts.roots()[0].clone();
    for rel in [
        ".claude/settings.json",
        ".mcp.json",
        ".cursor/hooks.json",
        "lefthook.toml",
    ] {
        let target = r.join(rel);
        let out = ts
            .call(
                "search_replace",
                json!({"file_path": target.to_string_lossy(), "old_string": "", "new_string": "{}"}),
            )
            .await;
        assert!(
            matches!(&out, Err(CallFailure::Refused(_))),
            "{rel}: {out:?}"
        );
        assert!(!target.exists(), "{rel} was created");
    }
}

#[tokio::test]
async fn audit_trailing_nbsp_cannot_read_through_an_in_root_symlink() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, "TOP-SECRET\n").unwrap();
    let link = root.path().join("link");
    if !symlinks_or_skip(make_file_symlink(&secret, &link), "file") {
        return;
    }
    let ts = readonly(root.path()).await;
    let r = ts.roots()[0].clone();

    let real = ts
        .call(
            "read_file",
            json!({"target_file": r.join("link").to_string_lossy()}),
        )
        .await;
    assert!(matches!(real, Err(CallFailure::Refused(_))), "{real:?}");

    let spelled = format!("{}\u{00A0}", r.join("link").display());
    let out = ts.call("read_file", json!({"target_file": spelled})).await;
    assert!(
        matches!(out, Err(CallFailure::Refused(_))),
        "escaped the root: {out:?}"
    );
}

#[tokio::test]
async fn audit_unicode_filename_fallback_cannot_swap_in_an_out_of_root_symlink() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, "TOP-SECRET\n").unwrap();
    let sibling = root.path().join("a\u{00A0}b.txt");
    if !symlinks_or_skip(make_file_symlink(&secret, &sibling), "file") {
        return;
    }
    let ts = readonly(root.path()).await;
    let requested = ts.roots()[0].join("a b.txt");
    let out = ts
        .call(
            "read_file",
            json!({"target_file": requested.to_string_lossy()}),
        )
        .await;
    assert!(
        matches!(out, Err(CallFailure::Refused(_))),
        "escaped the root: {out:?}"
    );
    if let Ok(text) = &out {
        assert!(!text.contains("TOP-SECRET"));
    }
}

#[tokio::test]
async fn read_confined_fs_refuses_an_out_of_root_resolved_read() {
    // The filesystem layer on its own, independent of the pre-dispatch guard.
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let inside = root.path().join("in.txt");
    let secret = outside.path().join("out.txt");
    fs::write(&inside, "inside").unwrap();
    fs::write(&secret, "outside").unwrap();
    let guard = PathGuard::new(vec![root.path().to_path_buf()], Vec::new(), true).unwrap();
    let rfs = ReadConfinedFs::new(Arc::new(LocalFs), guard);

    assert_eq!(rfs.read_file(&inside).await.unwrap(), b"inside");
    let err = rfs.read_file(&secret).await.unwrap_err().to_string();
    assert!(err.contains(REFUSAL_TEXT), "{err}");
    assert!(rfs.read_file_lines(&secret, 1, 10).await.is_err());
    assert!(rfs.read_file_prefix(&secret, 8).await.is_err());
}

#[tokio::test]
async fn the_bounded_line_reader_matches_the_local_one() {
    let root = tempfile::tempdir().unwrap();
    let f = root.path().join("lines.txt");
    fs::write(&f, "one\ntwo\nthree\nfour\nfive").unwrap();
    let guard = PathGuard::new(vec![root.path().to_path_buf()], Vec::new(), true).unwrap();
    let rfs = ReadConfinedFs::new(Arc::new(LocalFs), guard);
    let local = LocalFs;
    for (start, limit) in [(1, 2), (2, 2), (4, 10), (0, 1), (6, 3), (1, 0), (5, 1)] {
        assert_eq!(
            rfs.read_file_lines(&f, start, limit).await.unwrap(),
            local.read_file_lines(&f, start, limit).await.unwrap(),
            "start {start}, limit {limit}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_a_filesystem_layer_refusal_is_reported_as_a_refusal() {
    // The pre-dispatch guard sees an ordinary file. Once the call has reached
    // the filesystem layer the file becomes a link out of the root, so only that
    // layer can refuse it, and read_file rewrites the refusal as its own
    // "Permission denied" text. The client must still get the refusal.
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, "TOP-SECRET\n").unwrap();
    if !symlinks_or_skip(
        make_file_symlink(&secret, &outside.path().join("probe")),
        "file",
    ) {
        return;
    }
    let entered = Arc::new(AtomicUsize::new(0));
    let (mut ts, gate) = gated_with(
        root.path(),
        true,
        ToolsetOptions {
            hooks: TestHooks {
                gate_entered: Some(entered.clone()),
                ..TestHooks::default()
            },
            ..ToolsetOptions::default()
        },
    )
    .await;
    let seen = record_refusals(&mut ts);
    let ts = Arc::new(ts);
    let target = ts.roots()[0].join("swap.txt");
    fs::write(&target, "harmless\n").unwrap();
    let before = entered.load(Ordering::SeqCst);
    gate.send_replace(false);

    let call = {
        let ts = ts.clone();
        let args = json!({"target_file": target.to_string_lossy()});
        tokio::spawn(async move { ts.call("read_file", args).await })
    };
    wait_until("the call to reach the filesystem layer", || {
        entered.load(Ordering::SeqCst) > before
    })
    .await;
    fs::remove_file(&target).unwrap();
    assert!(make_file_symlink(&secret, &target), "swap in the link");
    gate.send_replace(true);

    let out = call.await.unwrap();
    assert!(
        matches!(&out, Err(CallFailure::Refused(d)) if d.reason == Reason::OutsideRoots),
        "{out:?}"
    );
    assert_eq!(seen.lock().unwrap().as_slice(), &[Reason::OutsideRoots]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_a_filesystem_layer_refusal_stops_an_edit() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let probe = tempfile::tempdir().unwrap();
    if !symlinks_or_skip(
        make_dir_symlink(outside.path(), &probe.path().join("l")),
        "directory",
    ) {
        return;
    }
    let entered = Arc::new(AtomicUsize::new(0));
    let (mut ts, gate) = gated_with(
        root.path(),
        false,
        ToolsetOptions {
            hooks: TestHooks {
                gate_entered: Some(entered.clone()),
                ..TestHooks::default()
            },
            ..ToolsetOptions::default()
        },
    )
    .await;
    let seen = record_refusals(&mut ts);
    let ts = Arc::new(ts);
    let sub = ts.roots()[0].join("sub");
    fs::create_dir(&sub).unwrap();
    let target = sub.join("planted.txt");
    let before = entered.load(Ordering::SeqCst);
    gate.send_replace(false);

    let call = {
        let ts = ts.clone();
        let args = json!({"file_path": target.to_string_lossy(), "old_string": "", "new_string": "PLANTED"});
        tokio::spawn(async move { ts.call("search_replace", args).await })
    };
    // search_replace resolves its target before its first filesystem call, so
    // swapping only once that call is waiting keeps this test deterministic.
    wait_until("the edit to reach the filesystem layer", || {
        entered.load(Ordering::SeqCst) > before
    })
    .await;
    fs::remove_dir(&sub).unwrap();
    assert!(make_dir_symlink(outside.path(), &sub), "swap in the link");
    gate.send_replace(true);

    let out = call.await.unwrap();
    assert!(matches!(&out, Err(CallFailure::Refused(_))), "{out:?}");
    assert!(
        !outside.path().join("planted.txt").exists(),
        "the edit escaped the root"
    );
    assert!(!seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn audit_an_edit_never_overwrites_a_file_it_was_not_allowed_to_read() {
    // search_replace takes an unreadable file for an absent one and would
    // recreate it from scratch.
    let root = tempfile::tempdir().unwrap();
    let ts = editable(root.path()).await;
    let r = ts.roots()[0].clone();
    let deck = r.join("deck.pptx");
    fs::write(&deck, "ORIGINAL").unwrap();
    let big = r.join("big.log");
    fs::File::create(&big)
        .unwrap()
        .set_len(MAX_WHOLE_FILE_BYTES + 1)
        .unwrap();

    for (target, expected) in [(&deck, "document formats"), (&big, "too large")] {
        let out = ts
            .call(
                "search_replace",
                json!({
                    "file_path": target.to_string_lossy(),
                    "old_string": "",
                    "new_string": "CLOBBERED"
                }),
            )
            .await;
        assert!(
            matches!(&out, Err(CallFailure::Failed(m)) if m.contains(expected)),
            "{}: {out:?}",
            target.display()
        );
    }
    assert_eq!(fs::read_to_string(&deck).unwrap(), "ORIGINAL");
    assert_eq!(fs::metadata(&big).unwrap().len(), MAX_WHOLE_FILE_BYTES + 1);
}

#[tokio::test]
async fn audit_a_completed_edit_is_reported_as_completed() {
    // The new content starts with a PDF signature. search_replace re-reads the
    // file after writing it to hash the result, and that re-read is refused as
    // a document; the edit itself landed and must be reported as such.
    let root = tempfile::tempdir().unwrap();
    let ts = editable(root.path()).await;
    let notes = ts.roots()[0].join("notes.txt");
    let out = ts
        .call(
            "search_replace",
            json!({"file_path": notes.to_string_lossy(), "old_string": "", "new_string": "%PDF-1.4 not really"}),
        )
        .await;
    assert!(out.is_ok(), "{out:?}");
    assert_eq!(fs::read_to_string(&notes).unwrap(), "%PDF-1.4 not really");
}

#[tokio::test]
async fn audit_a_newline_free_file_is_not_buffered_whole() {
    let root = tempfile::tempdir().unwrap();
    let ts = readonly(root.path()).await;
    let dump = ts.roots()[0].join("dump.txt");
    fs::write(&dump, vec![b'a'; MAX_LINE_WINDOW_BYTES + 1]).unwrap();
    let out = ts
        .call("read_file", json!({"target_file": dump.to_string_lossy()}))
        .await;
    assert!(
        matches!(&out, Err(CallFailure::Failed(m)) if m.contains("too large")),
        "{out:?}"
    );
}

#[tokio::test]
async fn audit_grep_never_returns_hard_denied_files_in_any_letter_case() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    put(r, "ok.txt", "SENTINEL_OK\n");
    put(r, "visible/.hidden_ok.txt", "SENTINEL_HIDDEN_OK\n");
    // Each sentinel sits in its own directory so that differently-cased names
    // do not collide on a case-insensitive filesystem. Policy-named files stay
    // below the root: the workspace policy loader parses `<cwd>/grok.toml`.
    let denied = [
        "g1/.git/config",
        "g2/.GIT/config",
        "g3/.git/hooks/pre-commit",
        "m1/mirror.git/config",
        "m2/Mirror.GIT/CONFIG",
        "m3/mirror.git/hooks/post-update",
        "m4/mirror.git/modules/sub/config",
        "b1/.bare/config",
        "b2/.BARE/hooks/pre-commit",
        "k1/.git-credentials",
        "k2/.Git-Credentials",
        "d1/.grok/notes.txt",
        "d2/.GROK/notes.txt",
        "nested/grok.toml",
        "p1/GROK.TOML",
        "p2/policy.toml",
        "p3/Policy.Toml",
        "s1/.ssh/id_rsa",
        "s2/.SSH/id_rsa",
        "s3/.gnupg/secring",
        "s4/.GnuPG/secring",
        "a1/.aws/credentials",
        "a2/.AWS/Credentials",
        "c1/.docker/config.json",
        "c2/.Docker/Config.JSON",
        "c3/.npmrc",
        "c4/.kube/config",
        "c5/prod.tfstate",
        "c6/ID_RSA",
        "n1/.netrc",
        "n2/.NETRC",
        "n3/_netrc",
        "n4/_NETRC",
        "e1/.env",
        "e2/.ENV",
        "e3/.env.local",
        "e4/.Env.Production",
    ];
    for rel in denied {
        put(r, rel, "SENTINEL_DENIED\n");
    }
    // Whitelist every hidden name past ripgrep's hidden filter, so only the
    // deny globs and the result filter can keep the sentinels out.
    put(r, ".ignore", "!.*\n");

    let ts = readonly(r).await;
    let out = ts
        .call(
            "grep",
            json!({"pattern": "SENTINEL_", "path": ts.roots()[0].to_string_lossy()}),
        )
        .await
        .expect("grep over the root must run");
    assert_ripgrep_ran(&out);
    assert!(out.contains("SENTINEL_OK"), "{out}");
    assert!(
        out.contains("SENTINEL_HIDDEN_OK"),
        "hidden files were not searched, so the hidden filter and not the deny rules \
         would be what kept the sentinels out:\n{out}"
    );
    assert!(
        !out.contains("SENTINEL_DENIED"),
        "grep returned a hard-denied file:\n{out}"
    );
}

#[tokio::test]
async fn audit_grep_never_returns_git_metadata_found_by_content() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    put(r, "src/ok.txt", "SENTINEL_OK\n");
    // A bare repository with an ordinary name: no name pattern matches it, so
    // only the per-result check against the guard can drop its files.
    fs::create_dir_all(r.join("backups/project/objects")).unwrap();
    fs::create_dir_all(r.join("backups/project/refs")).unwrap();
    put(r, "backups/project/HEAD", "ref: refs/heads/main\n");
    put(
        r,
        "backups/project/config",
        "[remote] url = SENTINEL_DENIED_CONFIG\n",
    );
    put(
        r,
        "backups/project/hooks/pre-receive",
        "SENTINEL_DENIED_HOOK\n",
    );
    put(
        r,
        "backups/project/worktrees/w/config.worktree",
        "SENTINEL_DENIED_WORKTREE\n",
    );
    // The hooks of a module nested inside a named bare repository.
    put(
        r,
        "mirror.git/modules/lib/hooks/post-checkout",
        "SENTINEL_DENIED_NESTED\n",
    );

    let ts = readonly(r).await;
    let out = ts
        .call(
            "grep",
            json!({"pattern": "SENTINEL_", "path": ts.roots()[0].to_string_lossy()}),
        )
        .await
        .expect("grep over the root must run");
    assert_ripgrep_ran(&out);
    assert!(out.contains("SENTINEL_OK"), "{out}");
    assert!(!out.contains("SENTINEL_DENIED"), "{out}");
}

#[tokio::test]
async fn audit_grep_output_does_not_reveal_whether_a_refused_file_matched() {
    // A refused file that ripgrep still searches: the reply for a pattern found
    // only there must match the reply for a pattern found nowhere, or a client
    // could recover the file one character at a time.
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    fs::create_dir_all(r.join("backups/project/objects")).unwrap();
    fs::create_dir_all(r.join("backups/project/refs")).unwrap();
    put(r, "backups/project/HEAD", "ref: refs/heads/main\n");
    put(
        r,
        "backups/project/config",
        "[remote] url = https://oauth2:TOKEN_7Qx@host/x.git\n",
    );
    put(r, "src/ok.txt", "nothing to see\n");
    let ts = readonly(r).await;
    // The served schema offers no output mode; the grep module's own tests
    // cover the rule in every mode.
    let search = |pattern: &str| json!({"pattern": pattern, "glob": "config"});
    let present = ts.call("grep", search("TOKEN_7")).await.expect("grep runs");
    let absent = ts.call("grep", search("TOKEN_8")).await.expect("grep runs");
    assert_ripgrep_ran(&present);
    assert_eq!(
        present, absent,
        "the reply shows whether a refused file matched"
    );

    // With a kept match as well, refused blocks must leave no trace either.
    // ripgrep separates files with a blank line, so a dropped last block could
    // leave one behind; with many refused files, the kept one is rarely last.
    for i in 0..12 {
        let repo = r.join("backups").join(format!("p{i}"));
        fs::create_dir_all(repo.join("objects")).unwrap();
        fs::create_dir_all(repo.join("refs")).unwrap();
        fs::write(repo.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            repo.join("config"),
            "[remote] url = https://oauth2:TOKEN_7Qx@host/x.git\n",
        )
        .unwrap();
    }
    put(r, "src/config", "needle_1\n");
    let present = ts
        .call("grep", search("needle_1|TOKEN_7"))
        .await
        .expect("grep runs");
    let absent = ts
        .call("grep", search("needle_1|TOKEN_8"))
        .await
        .expect("grep runs");
    assert!(present.contains("needle_1"), "{present}");
    assert_eq!(
        present, absent,
        "the reply shows whether a refused file matched beside a kept one"
    );
}

#[tokio::test]
async fn audit_grep_cannot_be_pointed_into_a_credential_folder_by_another_spelling() {
    let root = tempfile::tempdir().unwrap();
    put(root.path(), "a1/.aws/credentials", "SENTINEL_DENIED\n");
    let ts = readonly(root.path()).await;
    let aws = ts.roots()[0].join("a1").join(".aws");
    for spelled in [
        format!("{}/.", aws.display()),
        format!("{}//", aws.display()),
        aws.display().to_string(),
    ] {
        let out = ts
            .call("grep", json!({"pattern": "SENTINEL_", "path": spelled}))
            .await;
        assert!(
            matches!(&out, Err(CallFailure::Refused(_))),
            "{spelled}: {out:?}"
        );
    }
    let link = ts.roots()[0].join("innocent");
    if symlinks_or_skip(make_dir_symlink(&aws, &link), "directory") {
        let out = ts
            .call(
                "grep",
                json!({"pattern": "SENTINEL_", "path": link.to_string_lossy()}),
            )
            .await;
        assert!(matches!(&out, Err(CallFailure::Refused(_))), "{out:?}");
    }
}

#[tokio::test]
async fn audit_grep_without_a_path_searches_the_first_root() {
    let root = tempfile::tempdir().unwrap();
    // Built at run time, so this source file can never be the match.
    let needle = format!("NEEDLE_{}", u64::from(std::process::id()) * 7 + 13);
    fs::write(root.path().join("x.txt"), format!("{needle}\n")).unwrap();
    let ts = readonly(root.path()).await;
    let out = ts
        .call("grep", json!({"pattern": needle}))
        .await
        .expect("grep with no path runs");
    assert_ripgrep_ran(&out);
    assert!(out.contains(&needle), "{out}");
    assert!(out.contains("x.txt"), "{out}");
    assert!(
        !out.contains("toolset_tests"),
        "grep walked the process directory:\n{out}"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn audit_grep_and_list_dir_do_not_walk_through_a_junction_out_of_the_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "SENTINEL_OUTSIDE\n").unwrap();
    fs::write(root.path().join("ok.txt"), "SENTINEL_INSIDE\n").unwrap();
    let junction = root.path().join("jn");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(outside.path())
        .status()
        .expect("cmd runs");
    assert!(made.success(), "mklink /J failed");

    let ts = readonly(root.path()).await;
    let r = ts.roots()[0].to_string_lossy().into_owned();
    let found = ts
        .call("grep", json!({"pattern": "SENTINEL_", "path": r}))
        .await
        .expect("grep runs");
    assert_ripgrep_ran(&found);
    assert!(found.contains("SENTINEL_INSIDE"), "{found}");
    assert!(!found.contains("SENTINEL_OUTSIDE"), "{found}");
    let listing = ts
        .call("list_dir", json!({"target_directory": r}))
        .await
        .expect("list_dir runs");
    assert!(!listing.contains("secret.txt"), "{listing}");
}

#[tokio::test]
async fn audit_documents_are_not_served() {
    let root = tempfile::tempdir().unwrap();
    let disguised = root.path().join("notes.txt");
    fs::write(&disguised, "%PDF-1.4\n%crafted\n").unwrap();
    let slides = root.path().join("deck.pptx");
    fs::write(&slides, "not really a deck").unwrap();
    let ts = readonly(root.path()).await;
    for target in [&disguised, &slides] {
        let err = ts
            .call(
                "read_file",
                json!({"target_file": target.to_string_lossy()}),
            )
            .await
            .expect_err("documents must not reach the parsers");
        assert!(
            matches!(&err, CallFailure::Failed(m) if m.contains("document formats")),
            "{err:?}"
        );
    }
}

#[tokio::test]
async fn audit_a_document_behind_the_unicode_filename_fallback_is_not_served() {
    let root = tempfile::tempdir().unwrap();
    let ts = readonly(root.path()).await;
    let r = ts.roots()[0].clone();
    fs::write(r.join("report\u{00A0}final.txt"), "%PDF-1.4\n%crafted\n").unwrap();
    let requested = r.join("report final.txt");
    let out = ts
        .call(
            "read_file",
            json!({"target_file": requested.to_string_lossy()}),
        )
        .await;
    assert!(
        matches!(&out, Err(CallFailure::Failed(m)) if m.contains("document formats")),
        "{out:?}"
    );
}

#[tokio::test]
async fn audit_a_document_behind_a_symlink_is_not_served() {
    let root = tempfile::tempdir().unwrap();
    let ts = readonly(root.path()).await;
    let r = ts.roots()[0].clone();
    let real = r.join("real.pdf");
    fs::write(&real, "no signature, only the extension").unwrap();
    let link = r.join("notes.txt");
    if !symlinks_or_skip(make_file_symlink(&real, &link), "file") {
        return;
    }
    let out = ts
        .call("read_file", json!({"target_file": link.to_string_lossy()}))
        .await;
    assert!(
        matches!(&out, Err(CallFailure::Failed(m)) if m.contains("document formats")),
        "{out:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn audit_a_fifo_inside_a_root_is_never_opened() {
    let root = tempfile::tempdir().unwrap();
    let ts = readonly(root.path()).await;
    let fifo = ts.roots()[0].join("pipe");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo runs");
    assert!(made.success(), "mkfifo failed");
    let call = ts.call("read_file", json!({"target_file": fifo.to_string_lossy()}));
    let out = match tokio::time::timeout(Duration::from_secs(30), call).await {
        Ok(out) => out,
        Err(_) => {
            // Unblock the reader stuck in open(2), so the runtime can shut down
            // and this test fails instead of hanging the job.
            let _writer = fs::OpenOptions::new().write(true).open(&fifo);
            panic!("reading a FIFO blocked");
        }
    };
    assert!(
        matches!(&out, Err(CallFailure::Failed(m)) if m.contains("regular files")),
        "{out:?}"
    );
}

#[tokio::test]
async fn audit_session_folder_is_private_keeps_no_file_content_and_is_removed_on_drop() {
    let root = tempfile::tempdir().unwrap();
    let editor = editable(root.path()).await;
    let reader = readonly(root.path()).await;
    let (pe, pr) = (
        editor.session_dir().to_path_buf(),
        reader.session_dir().to_path_buf(),
    );
    assert_ne!(pe, pr, "each server needs its own session folder");
    assert!(
        pe.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("turbo-mcp-serve-")
    );
    let registered = crate::private_dir::registered_session_dirs();
    assert!(
        registered.contains(&pe) && registered.contains(&pr),
        "live session folders are registered for panic cleanup: {registered:?}"
    );

    // A served edit keeps no copy of the file's previous content: a client has
    // no way to undo with one, and a copy could outlive the server.
    let target = editor.roots()[0].join("edited.txt");
    fs::write(&target, "before-SENTINEL\n").unwrap();
    let out = editor
        .call(
            "search_replace",
            json!({
                "file_path": target.to_string_lossy(),
                "old_string": "before-SENTINEL",
                "new_string": "after"
            }),
        )
        .await;
    assert!(out.is_ok(), "{out:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "after\n");
    let copies: Vec<PathBuf> = files_under(&pe)
        .into_iter()
        .filter(|file| fs::read(file).is_ok_and(|bytes| bytes.windows(8).any(|w| w == b"SENTINEL")))
        .collect();
    assert!(copies.is_empty(), "the edit kept a copy: {copies:?}");
    assert!(!pe.join("receipts").exists());

    // Whatever is created inside still gets the folder's owner-only access.
    let probe = pe.join("probe.txt");
    fs::write(&probe, "x").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&pe).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "session folder must be owner-only");
    }
    #[cfg(windows)]
    {
        use crate::private_dir::win::describe_dacl;
        let dacl = describe_dacl(&pe).expect("read the session folder DACL");
        assert!(
            dacl.protected,
            "the DACL must not inherit from the parent: {dacl:?}"
        );
        assert!(!dacl.entries.is_empty(), "{dacl:?}");
        for entry in &dacl.entries {
            assert!(entry.allows_current_user, "{dacl:?}");
            // OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
            assert_eq!(entry.flags & 0x03, 0x03, "{dacl:?}");
            // DELETE | FILE_DELETE_CHILD: removing the folder needs both.
            assert_eq!(entry.mask & 0x0001_0040, 0x0001_0040, "{dacl:?}");
        }
        let inherited = describe_dacl(&probe).expect("read the probe file's DACL");
        assert!(
            !inherited.entries.is_empty()
                && inherited
                    .entries
                    .iter()
                    .all(|entry| entry.allows_current_user),
            "{inherited:?}"
        );
    }

    drop(editor);
    drop(reader);
    assert!(!pe.exists(), "session folder must be removed on drop");
    assert!(!pr.exists(), "session folder must be removed on drop");
    let registered = crate::private_dir::registered_session_dirs();
    assert!(!registered.contains(&pe) && !registered.contains(&pr));
}

#[tokio::test]
async fn calls_after_shutdown_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let f = root.path().join("f.txt");
    fs::write(&f, "x").unwrap();
    let ts = readonly(root.path()).await;
    ts.shutdown().await;
    let err = ts
        .call("read_file", json!({"target_file": f.to_string_lossy()}))
        .await
        .expect_err("no new calls after shutdown");
    assert!(
        matches!(&err, CallFailure::Failed(m) if m.contains("shutting down")),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_shutdown_stops_calls_still_running_after_the_drain_window() {
    let root = tempfile::tempdir().unwrap();
    let (ts, gate) = gated_with(
        root.path(),
        true,
        ToolsetOptions {
            shutdown_drain: Duration::from_millis(200),
            ..ToolsetOptions::default()
        },
    )
    .await;
    let ts = Arc::new(ts);
    let f = ts.roots()[0].join("f.txt");
    fs::write(&f, "x").unwrap();
    gate.send_replace(false);

    let call = {
        let ts = ts.clone();
        let args = json!({"target_file": f.to_string_lossy()});
        tokio::spawn(async move { ts.call("read_file", args).await })
    };
    wait_until("the call to start", || ts.in_flight() == 1).await;
    tokio::time::timeout(Duration::from_secs(20), ts.shutdown())
        .await
        .expect("shutdown finishes");
    assert_eq!(
        ts.in_flight(),
        0,
        "a call was still running after shutdown returned"
    );
    let out = call.await.unwrap();
    assert!(
        matches!(&out, Err(CallFailure::Failed(m)) if m.contains("shutting down")),
        "{out:?}"
    );
}

#[tokio::test]
async fn refusals_reach_the_operator_observer() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("s.txt");
    fs::write(&secret, "x").unwrap();
    let mut ts = readonly(root.path()).await;
    let seen = record_refusals(&mut ts);
    let _ = ts
        .call(
            "read_file",
            json!({"target_file": secret.to_string_lossy()}),
        )
        .await;
    assert_eq!(seen.lock().unwrap().as_slice(), &[Reason::OutsideRoots]);
}

#[tokio::test]
async fn served_output_carries_no_system_reminder() {
    let root = tempfile::tempdir().unwrap();
    let f = root.path().join("f.txt");
    fs::write(&f, "plain\n").unwrap();
    let ts = readonly(root.path()).await;
    let out = ts
        .call("read_file", json!({"target_file": f.to_string_lossy()}))
        .await
        .unwrap();
    assert!(!out.contains("<system-reminder>"), "{out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_calls_beyond_the_concurrency_limit_are_refused_as_busy() {
    let root = tempfile::tempdir().unwrap();
    let (ts, gate) = gated(root.path(), true, CALL_TIMEOUT).await;
    let ts = Arc::new(ts);
    let f = ts.roots()[0].join("f.txt");
    fs::write(&f, "x").unwrap();
    let args = json!({"target_file": f.to_string_lossy()});
    gate.send_replace(false);

    let calls: Vec<_> = (0..MAX_CONCURRENT_CALLS)
        .map(|_| {
            let ts = ts.clone();
            let args = args.clone();
            tokio::spawn(async move { ts.call("read_file", args).await })
        })
        .collect();
    wait_until("every permit to be taken", || {
        ts.in_flight() == MAX_CONCURRENT_CALLS
    })
    .await;

    let busy = ts.call("read_file", args.clone()).await;
    assert!(
        matches!(&busy, Err(CallFailure::Failed(m)) if m.contains("busy")),
        "{busy:?}"
    );

    gate.send_replace(true);
    for call in calls {
        let out = call.await.unwrap();
        assert!(out.is_ok(), "{out:?}");
    }
    assert_eq!(ts.in_flight(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_a_timed_out_call_keeps_its_permit_until_the_tool_finishes() {
    let root = tempfile::tempdir().unwrap();
    let (ts, gate) = gated(root.path(), true, Duration::from_millis(300)).await;
    let f = ts.roots()[0].join("f.txt");
    fs::write(&f, "x").unwrap();
    gate.send_replace(false);

    let out = ts
        .call("read_file", json!({"target_file": f.to_string_lossy()}))
        .await;
    assert!(
        matches!(&out, Err(CallFailure::Failed(m)) if m.contains("timed out")),
        "{out:?}"
    );
    // The client has its answer; the work is still running and still counted.
    assert_eq!(ts.in_flight(), 1);

    gate.send_replace(true);
    wait_until("the abandoned call to finish", || ts.in_flight() == 0).await;
}

#[test]
fn audit_a_call_stuck_in_synchronous_work_still_times_out_on_schedule() {
    // list_dir walks synchronously. Here every filesystem operation blocks its
    // thread without yielding, on a current-thread runtime: if calls ran on the
    // runtime's own thread, the timeout could never fire. The test thread waits
    // with its own deadline, so a regression fails instead of hanging.
    let gate = Arc::new(SyncGate::default());
    gate.set(true);
    let worker_gate = gate.clone();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds");
        let result = runtime.block_on(async move {
            let root = tempfile::tempdir().unwrap();
            let options = ToolsetOptions {
                call_timeout: Duration::from_millis(300),
                hooks: TestHooks {
                    sync_gate: Some(worker_gate.clone()),
                    ..TestHooks::default()
                },
                ..ToolsetOptions::default()
            };
            let ts = ServedToolset::with_options(vec![root.path().to_path_buf()], true, options)
                .await
                .expect("toolset builds");
            let f = ts.roots()[0].join("f.txt");
            fs::write(&f, "x").unwrap();
            worker_gate.set(false);
            let outcome = ts
                .call("read_file", json!({"target_file": f.to_string_lossy()}))
                .await;
            (outcome, ts.in_flight())
        });
        let _ = done.send(result);
        // Dropping the runtime waits for the stuck call; the test releases it.
    });
    let received = finished.recv_timeout(Duration::from_secs(20));
    gate.set(true);
    let (outcome, still_running) =
        received.expect("the call must time out while its tool blocks a thread");
    assert!(
        matches!(&outcome, Err(CallFailure::Failed(m)) if m.contains("timed out")),
        "{outcome:?}"
    );
    assert_eq!(still_running, 1, "the stuck call keeps its permit");
}

#[tokio::test]
async fn audit_an_argument_the_tool_does_not_advertise_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let ts = editable(root.path()).await;
    let target = ts.roots()[0].join("a.txt");
    fs::write(&target, "before\n").unwrap();
    // `confirm` would satisfy a workspace policy that requires confirmation.
    let out = ts
        .call(
            "search_replace",
            json!({
                "file_path": target.to_string_lossy(),
                "old_string": "before",
                "new_string": "after",
                "confirm": true
            }),
        )
        .await;
    assert!(
        matches!(&out, Err(CallFailure::Failed(m)) if m.contains("unknown argument") && m.contains("confirm")),
        "{out:?}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "before\n");
}

#[tokio::test]
async fn audit_a_workspace_policy_refusal_does_not_quote_the_policy_file() {
    let root = tempfile::tempdir().unwrap();
    // Not valid TOML: the parser's error quotes the offending line.
    put(
        root.path(),
        "grok.toml",
        "[policy]\napi_key = sk-live-SENTINEL\n",
    );
    let ts = readonly(root.path()).await;
    let f = ts.roots()[0].join("README.md");
    fs::write(&f, "hello\n").unwrap();
    let out = ts
        .call("read_file", json!({"target_file": f.to_string_lossy()}))
        .await;
    match out {
        Err(CallFailure::Refused(denial)) => {
            assert!(!denial.to_string().contains("SENTINEL"), "{denial}")
        }
        other => panic!("expected the opaque refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn audit_replace_all_cannot_build_a_file_larger_than_the_cap() {
    let root = tempfile::tempdir().unwrap();
    let ts = editable(root.path()).await;
    let target = ts.roots()[0].join("many.txt");
    fs::write(&target, "a".repeat(100_000)).unwrap();
    // 100,000 replacements of 400 bytes each: a 40 MB result from a small request.
    let out = ts
        .call(
            "search_replace",
            json!({
                "file_path": target.to_string_lossy(),
                "old_string": "a",
                "new_string": "X".repeat(400),
                "replace_all": true
            }),
        )
        .await;
    let text = match &out {
        Ok(text) | Err(CallFailure::Failed(text)) => text.clone(),
        other => panic!("{other:?}"),
    };
    assert!(text.contains("larger than"), "{text}");
    assert_eq!(fs::metadata(&target).unwrap().len(), 100_000);
}

#[tokio::test]
async fn audit_a_readonly_call_does_not_read_large_text_whole() {
    // read_file reads skill markdown whole; in a read-only call text read whole
    // gets no more room than a line window.
    let root = tempfile::tempdir().unwrap();
    let ts = readonly(root.path()).await;
    let skill = ts.roots()[0]
        .join("docs")
        .join("skills")
        .join("reference.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::write(&skill, "line\n".repeat(MAX_LINE_WINDOW_BYTES / 5 + 1)).unwrap();
    let out = ts
        .call("read_file", json!({"target_file": skill.to_string_lossy()}))
        .await;
    assert!(
        matches!(&out, Err(CallFailure::Failed(m)) if m.contains("too large")),
        "{out:?}"
    );

    // Text that merely starts like an image container is still text: the cap
    // decides the way read_file does.
    let disguised = skill.with_file_name("disguised.md");
    let mut body = b"zzzzftypheic".to_vec();
    body.extend_from_slice("line\n".repeat(MAX_LINE_WINDOW_BYTES / 5 + 1).as_bytes());
    fs::write(&disguised, body).unwrap();
    let out = ts
        .call(
            "read_file",
            json!({"target_file": disguised.to_string_lossy()}),
        )
        .await;
    assert!(
        matches!(&out, Err(CallFailure::Failed(m)) if m.contains("too large")),
        "{out:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_a_timed_out_edit_says_it_may_still_apply() {
    let root = tempfile::tempdir().unwrap();
    let (ts, gate) = gated(root.path(), false, Duration::from_millis(300)).await;
    let target = ts.roots()[0].join("slow.txt");
    gate.send_replace(false);
    let out = ts
        .call(
            "search_replace",
            json!({"file_path": target.to_string_lossy(), "old_string": "", "new_string": "hello"}),
        )
        .await;
    assert!(
        matches!(&out, Err(CallFailure::Failed(m)) if m.contains("may still be applied")),
        "{out:?}"
    );
    gate.send_replace(true);
    wait_until("the edit to finish", || ts.in_flight() == 0).await;
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
}

#[cfg(windows)]
#[tokio::test]
async fn audit_a_root_the_tools_would_print_in_verbatim_form_is_refused() {
    // Past MAX_PATH the canonical spelling keeps `\\?\`, which is how the tools
    // would print every path under the root, and no client path can start with
    // it.
    let base = tempfile::tempdir().unwrap();
    let mut long = base.path().to_path_buf();
    for i in 0..6 {
        long.push(format!("{i}-{}", "x".repeat(48)));
    }
    fs::create_dir_all(&long).unwrap();
    match ServedToolset::new(vec![long], true).await {
        Ok(_) => panic!("a root the tools print in verbatim form must not be served"),
        Err(e) => assert!(e.to_string().contains("no client path could name"), "{e}"),
    }
}

#[test]
fn audit_the_argument_check_runs_under_the_call_timeout_off_the_runtime_thread() {
    // The argument check touches the disk. Here it blocks its thread, on a
    // current-thread runtime: if it ran on the runtime's own thread, the timeout
    // could never fire. The test thread waits with its own deadline, so a
    // regression fails instead of hanging.
    let gate = Arc::new(SyncGate::default());
    let worker_gate = gate.clone();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds");
        let result = runtime.block_on(async move {
            let root = tempfile::tempdir().unwrap();
            let options = ToolsetOptions {
                call_timeout: Duration::from_millis(300),
                hooks: TestHooks {
                    check_gate: Some(worker_gate),
                    ..TestHooks::default()
                },
                ..ToolsetOptions::default()
            };
            let ts = ServedToolset::with_options(vec![root.path().to_path_buf()], true, options)
                .await
                .expect("toolset builds");
            let f = ts.roots()[0].join("f.txt");
            fs::write(&f, "x").unwrap();
            let outcome = ts
                .call("read_file", json!({"target_file": f.to_string_lossy()}))
                .await;
            (outcome, ts.in_flight())
        });
        let _ = done.send(result);
        // Dropping the runtime waits for the stuck check; the test releases it.
    });
    let received = finished.recv_timeout(Duration::from_secs(20));
    gate.set(true);
    let (outcome, still_running) =
        received.expect("the call must time out while its argument check blocks a thread");
    assert!(
        matches!(&outcome, Err(CallFailure::Failed(m)) if m.contains("timed out")),
        "{outcome:?}"
    );
    assert_eq!(still_running, 1, "the stuck check keeps the call's permit");
}

#[test]
fn audit_checking_the_roots_at_startup_leaves_the_runtime_free() {
    // Building the guard reads the disk: home folders, mounts and configuration
    // files. On a current-thread runtime a timer must still fire while it runs,
    // which is what lets a stop signal end startup.
    let gate = Arc::new(SyncGate::default());
    let worker_gate = gate.clone();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds");
        let timer_won = runtime.block_on(async move {
            let root = tempfile::tempdir().unwrap();
            let options = ToolsetOptions {
                hooks: TestHooks {
                    build_gate: Some(worker_gate),
                    ..TestHooks::default()
                },
                ..ToolsetOptions::default()
            };
            let build = ServedToolset::with_options(vec![root.path().to_path_buf()], true, options);
            tokio::select! {
                _ = build => false,
                () = tokio::time::sleep(Duration::from_millis(200)) => true,
            }
        });
        let _ = done.send(timer_won);
        // Dropping the runtime waits for the held check; the test releases it.
    });
    let received = finished.recv_timeout(Duration::from_secs(20));
    gate.set(true);
    assert!(
        received.expect("building the guard held the runtime thread"),
        "the toolset finished building while its gate was closed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_shutdown_lets_an_edit_whose_write_has_begun_finish() {
    // Once a write has begun, the file may already have changed: shutdown must
    // not stop that call and tell the client nothing happened.
    let root = tempfile::tempdir().unwrap();
    let (write_gate, receiver) = tokio::sync::watch::channel(false);
    let writes_begun = Arc::new(AtomicUsize::new(0));
    let ts = ServedToolset::with_options(
        vec![root.path().to_path_buf()],
        false,
        ToolsetOptions {
            shutdown_drain: Duration::from_millis(200),
            hooks: TestHooks {
                write_gate: Some(receiver),
                writes_begun: Some(writes_begun.clone()),
                ..TestHooks::default()
            },
            ..ToolsetOptions::default()
        },
    )
    .await
    .expect("toolset builds");
    let ts = Arc::new(ts);
    let target = ts.roots()[0].join("created.txt");
    let call = {
        let ts = ts.clone();
        let args = json!({
            "file_path": target.to_string_lossy(),
            "old_string": "",
            "new_string": "hello"
        });
        tokio::spawn(async move { ts.call("search_replace", args).await })
    };
    wait_until("the write to begin", || {
        writes_begun.load(Ordering::SeqCst) == 1
    })
    .await;
    let shutdown = {
        let ts = ts.clone();
        tokio::spawn(async move { ts.shutdown().await })
    };
    wait_until("shutdown to cancel running calls", || ts.calls_cancelled()).await;
    write_gate.send_replace(true);
    tokio::time::timeout(Duration::from_secs(20), shutdown)
        .await
        .expect("shutdown finishes")
        .unwrap();
    let out = call.await.unwrap();
    assert!(out.is_ok(), "{out:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
}

#[tokio::test]
async fn audit_an_edit_the_workspace_policy_refuses_is_the_opaque_refusal() {
    let root = tempfile::tempdir().unwrap();
    put(root.path(), "grok.toml", "[policy]\nmax_diff_lines = 200\n");
    let mut ts = editable(root.path()).await;
    let seen = record_refusals(&mut ts);
    let target = ts.roots()[0].join("lines.txt");
    let original = "x\n".repeat(500);
    fs::write(&target, &original).unwrap();
    // The arguments add one line, which passes the dispatcher's own check; the
    // edit adds 500, which only the tool itself counts.
    let out = ts
        .call(
            "search_replace",
            json!({
                "file_path": target.to_string_lossy(),
                "old_string": "x",
                "new_string": "x\ny",
                "replace_all": true
            }),
        )
        .await;
    match &out {
        Err(CallFailure::Refused(denial)) => {
            assert!(!denial.to_string().contains("200"), "{denial}")
        }
        other => panic!("expected the opaque refusal, got {other:?}"),
    }
    assert_eq!(seen.lock().unwrap().as_slice(), &[Reason::WorkspacePolicy]);
    assert_eq!(fs::read_to_string(&target).unwrap(), original);
}

#[tokio::test]
async fn audit_a_deny_paths_rule_matched_through_a_link_is_the_opaque_refusal() {
    let root = tempfile::tempdir().unwrap();
    put(
        root.path(),
        "grok.toml",
        "[policy]\ndeny_paths = [\"secret-notes.md\"]\n",
    );
    let mut ts = editable(root.path()).await;
    let seen = record_refusals(&mut ts);
    let r = ts.roots()[0].clone();
    let notes = r.join("releases").join("secret-notes.md");
    put(&r, "releases/secret-notes.md", "draft\n");
    fs::create_dir_all(r.join("docs")).unwrap();
    // The name the client uses matches no rule; the file it resolves to does.
    let link = r.join("docs").join("current.md");
    if !symlinks_or_skip(make_file_symlink(&notes, &link), "file") {
        return;
    }
    let out = ts
        .call(
            "search_replace",
            json!({
                "file_path": link.to_string_lossy(),
                "old_string": "draft",
                "new_string": "final"
            }),
        )
        .await;
    assert!(matches!(&out, Err(CallFailure::Refused(_))), "{out:?}");
    assert_eq!(seen.lock().unwrap().as_slice(), &[Reason::WorkspacePolicy]);
    assert_eq!(fs::read_to_string(&notes).unwrap(), "draft\n");
}

#[tokio::test]
async fn audit_a_root_no_client_path_can_name_refuses_to_build() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("Dan's Projects");
    fs::create_dir_all(&root).unwrap();
    let error = ServedToolset::new(vec![root], true)
        .await
        .err()
        .expect("the root is refused at startup");
    assert!(
        error.to_string().contains("no client path could name"),
        "{error:#}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_shutdown_waits_past_its_bounds_for_an_edit_still_writing() {
    // A write stopped part-way would leave its file cut short, so shutdown waits
    // for an edit that has begun writing, even past the drain and cancel grace.
    let root = tempfile::tempdir().unwrap();
    let (write_gate, receiver) = tokio::sync::watch::channel(false);
    let writes_begun = Arc::new(AtomicUsize::new(0));
    let ts = ServedToolset::with_options(
        vec![root.path().to_path_buf()],
        false,
        ToolsetOptions {
            shutdown_drain: Duration::from_millis(100),
            cancel_grace: Duration::from_millis(100),
            hooks: TestHooks {
                write_gate: Some(receiver),
                writes_begun: Some(writes_begun.clone()),
                ..TestHooks::default()
            },
            ..ToolsetOptions::default()
        },
    )
    .await
    .expect("toolset builds");
    let ts = Arc::new(ts);
    let target = ts.roots()[0].join("slow.txt");
    let call = {
        let ts = ts.clone();
        let args = json!({
            "file_path": target.to_string_lossy(),
            "old_string": "",
            "new_string": "hello"
        });
        tokio::spawn(async move { ts.call("search_replace", args).await })
    };
    wait_until("the write to begin", || {
        writes_begun.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(ts.calls_writing(), 1);
    let shutdown = {
        let ts = ts.clone();
        tokio::spawn(async move { ts.shutdown().await })
    };
    wait_until("shutdown to cancel running calls", || ts.calls_cancelled()).await;
    // Well past the drain and the cancel grace, shutdown is still waiting.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        !shutdown.is_finished(),
        "shutdown gave up on an edit that was still writing"
    );
    write_gate.send_replace(true);
    tokio::time::timeout(Duration::from_secs(20), shutdown)
        .await
        .expect("shutdown finishes once the write does")
        .unwrap();
    let out = call.await.unwrap();
    assert!(out.is_ok(), "{out:?}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
    assert_eq!(ts.calls_writing(), 0);
}

#[tokio::test]
async fn audit_a_served_call_never_reads_skill_files_the_guard_did_not_check() {
    // Turbo's skill discovery walks up from each file a tool touched and reads
    // the skill files it finds: above a second root, where the walk passes the
    // root, and inside `.grok` folders the guard always refuses.
    let base = tempfile::tempdir().unwrap();
    let b = base.path();
    put(
        b,
        ".agents/skills/above/SKILL.md",
        "---\nname: above\n---\n",
    );
    put(
        b,
        "first/src/.grok/skills/refused/SKILL.md",
        "---\nname: refused\n---\n",
    );
    put(b, "first/src/main.rs", "fn main() {}\n");
    put(b, "second/notes.md", "hello\n");
    let ts = ServedToolset::new(vec![b.join("first"), b.join("second")], true)
        .await
        .expect("toolset builds");
    let roots = ts.roots();
    for file in [
        roots[0].join("src").join("main.rs"),
        roots[1].join("notes.md"),
    ] {
        let out = ts
            .call("read_file", json!({"target_file": file.to_string_lossy()}))
            .await;
        assert!(out.is_ok(), "{out:?}");
    }
    let known = ts.known_skill_names().await;
    assert!(
        known.is_empty(),
        "a served call read skill files: {known:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn audit_grep_judges_a_folder_name_holding_a_newline_whole() {
    // A bare mirror whose folder name holds a newline followed by text shaped
    // like a numbered line. Read line by line, its config's path would split
    // into a harmless heading and a numbered line.
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    let mirror = r.join("backups").join("mirror\n1:old");
    fs::create_dir_all(mirror.join("objects")).unwrap();
    fs::create_dir_all(mirror.join("refs")).unwrap();
    fs::write(mirror.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(
        mirror.join("config"),
        "[remote] url = https://oauth2:TOKEN_9Zq@host/x.git\n",
    )
    .unwrap();
    put(r, "src/notes.txt", "url = public\n");
    let ts = readonly(r).await;
    let out = ts
        .call("grep", json!({"pattern": "url"}))
        .await
        .expect("grep runs");
    assert_ripgrep_ran(&out);
    assert!(out.contains("public"), "{out}");
    assert!(!out.contains("TOKEN_9Zq"), "{out}");
}

#[tokio::test]
async fn audit_a_served_toolset_starts_no_scheduler() {
    // Turbo's scheduler reloads `.grok/schedules.json` in the first root and
    // records every due fire there, even with nothing to run the job.
    let root = tempfile::tempdir().unwrap();
    let job = json!({
        "version": 1,
        "tasks": [{
            "id": "due-job",
            "intervalSecs": 60,
            "prompt": "check the build",
            "recurring": true,
            "durable": true,
            "foreground": true,
            "standing": true,
            "createdAt": "2026-01-01T00:00:00Z",
            "lastFiredAt": "2026-01-01T00:00:00Z",
            "expiresAt": null
        }],
        "cancelled": []
    })
    .to_string();
    put(root.path(), ".grok/schedules.json", &job);
    let ts = readonly(root.path()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        fs::read_to_string(root.path().join(".grok").join("schedules.json")).unwrap(),
        job
    );
    drop(ts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_no_write_begins_after_shutdown_has_cancelled_the_call() {
    // A call held in a synchronous filesystem step cannot see the cancellation.
    // Once past it, it must not begin a write that shutdown did not wait for.
    let root = tempfile::tempdir().unwrap();
    let gate = Arc::new(SyncGate::default());
    let ts = ServedToolset::with_options(
        vec![root.path().to_path_buf()],
        false,
        ToolsetOptions {
            shutdown_drain: Duration::from_millis(100),
            cancel_grace: Duration::from_millis(100),
            hooks: TestHooks {
                pre_write_gate: Some(gate.clone()),
                ..TestHooks::default()
            },
            ..ToolsetOptions::default()
        },
    )
    .await
    .expect("toolset builds");
    let ts = Arc::new(ts);
    let target = ts.roots()[0].join("kept.txt");
    fs::write(&target, "before\n").unwrap();
    let call = {
        let ts = ts.clone();
        let args = json!({
            "file_path": target.to_string_lossy(),
            "old_string": "before",
            "new_string": "after"
        });
        tokio::spawn(async move { ts.call("search_replace", args).await })
    };
    let reached = tokio::time::timeout(Duration::from_secs(20), async {
        while gate.waiting() != 1 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let stopped = match reached {
        Ok(()) => tokio::time::timeout(Duration::from_secs(20), ts.shutdown())
            .await
            .is_ok(),
        Err(_) => false,
    };
    // Released on every path, so a failure here fails the test instead of
    // leaving a thread parked on the gate, which would hang the runtime's drop.
    gate.set(true);
    assert!(reached.is_ok(), "the edit never reached its write");
    assert!(
        stopped,
        "shutdown waited for an edit that had not begun writing"
    );
    let out = tokio::time::timeout(Duration::from_secs(20), call)
        .await
        .expect("the call ends once released")
        .unwrap();
    assert!(
        matches!(&out, Err(CallFailure::Failed(m)) if m.contains("stopped before it changed any file")),
        "{out:?}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "before\n");
    assert_eq!(ts.calls_writing(), 0);
}

#[tokio::test]
async fn audit_what_a_link_named_grok_leads_to_is_never_read_or_searched() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    put(
        r,
        "shared/grok/config.toml",
        "[mcp_servers]\ntoken = \"GROK_LINK_SECRET_7\"\n",
    );
    fs::create_dir_all(r.join("pkg")).unwrap();
    let target = Path::new("..").join("shared").join("grok");
    if !symlinks_or_skip(
        make_dir_symlink(&target, &r.join("pkg").join(".grok")),
        "directory",
    ) {
        return;
    }
    let ts = readonly(r).await;
    let config = ts.roots()[0]
        .join("shared")
        .join("grok")
        .join("config.toml");
    let out = ts
        .call(
            "read_file",
            json!({"target_file": config.to_string_lossy()}),
        )
        .await;
    assert!(matches!(out, Err(CallFailure::Refused(_))), "{out:?}");
    let out = ts
        .call("grep", json!({"pattern": "GROK_LINK_SECRET_7"}))
        .await
        .expect("grep runs");
    assert_ripgrep_ran(&out);
    assert!(!out.contains("GROK_LINK_SECRET_7"), "{out}");
}

#[tokio::test]
async fn audit_grep_never_shows_what_ripgrep_says_about_files_outside_the_root() {
    // ripgrep reads ignore files in the folders above the root. A line it could
    // not parse there made it exit 2, and the reply was its message, quoting the
    // line, in place of the matches.
    let base = tempfile::tempdir().unwrap();
    fs::write(base.path().join(".ignore"), "OUTSIDE_ROOT_LINE_[\n").unwrap();
    let root = base.path().join("app");
    put(&root, "notes.txt", "needle here\n");
    let ts = readonly(&root).await;
    let out = ts
        .call("grep", json!({"pattern": "needle"}))
        .await
        .expect("grep runs");
    assert_ripgrep_ran(&out);
    assert!(out.contains("needle here"), "{out}");
    assert!(!out.contains("OUTSIDE_ROOT_LINE"), "{out}");
}

#[tokio::test]
async fn audit_grep_reports_a_pattern_ripgrep_refuses_for_a_newline() {
    // Without multiline, ripgrep refuses a pattern that can match a newline and
    // says how to fix it. Hiding that answered "No matches found" for a search
    // that never ran.
    let root = tempfile::tempdir().unwrap();
    put(root.path(), "src/main.rs", "fn main() {\n}\n");
    let ts = readonly(root.path()).await;
    let out = ts
        .call("grep", json!({"pattern": "fn main\\(\\) \\{\\n"}))
        .await
        .expect("grep runs");
    assert_ripgrep_ran(&out);
    assert!(out.contains("multiline"), "{out}");
    assert!(!out.contains("No matches found"), "{out}");
}

#[tokio::test]
async fn audit_a_search_pattern_ripgrep_cannot_take_is_refused() {
    // A NUL, or a pattern too long for a command line, would fail to start
    // ripgrep, and the failure named where it is installed.
    let root = tempfile::tempdir().unwrap();
    let ts = readonly(root.path()).await;
    for pattern in ["a\u{0}b".to_string(), "x".repeat(20_000)] {
        let out = ts.call("grep", json!({ "pattern": pattern })).await;
        assert!(matches!(out, Err(CallFailure::Refused(_))), "{out:?}");
    }
}
