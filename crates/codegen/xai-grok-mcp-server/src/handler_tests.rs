//! Handler-level tests: the shape an external client actually observes.

use std::fs;
use std::sync::Arc;

use serde_json::json;

use crate::guard::REFUSAL_TEXT;
use crate::handler::TurboMcpHandler;
use crate::toolset::ServedToolset;

async fn handler(root: &std::path::Path, read_only: bool) -> TurboMcpHandler {
    let ts = ServedToolset::new(vec![root.to_path_buf()], read_only)
        .await
        .expect("toolset builds");
    TurboMcpHandler::new(Arc::new(ts))
}

#[tokio::test]
async fn every_advertised_tool_carries_annotations_a_description_and_properties() {
    let root = tempfile::tempdir().unwrap();
    let h = handler(root.path(), false).await;
    let tools = h.advertised_tools();
    assert!(!tools.is_empty());
    for t in &tools {
        assert!(t.annotations.is_some(), "{} has no annotations", t.name);
        assert!(
            t.description
                .as_deref()
                .is_some_and(|d| !d.trim().is_empty()),
            "{} has an empty description",
            t.name
        );
        assert!(
            t.input_schema
                .get("properties")
                .and_then(|p| p.as_object())
                .is_some(),
            "{} has no object `properties`",
            t.name
        );
    }
}

#[tokio::test]
async fn read_only_server_advertises_only_read_only_tools() {
    let root = tempfile::tempdir().unwrap();
    let h = handler(root.path(), true).await;
    for t in h.advertised_tools() {
        let a = t.annotations.expect("annotations");
        assert_eq!(a.read_only_hint, Some(true), "{}", t.name);
    }
}

#[tokio::test]
async fn edit_server_marks_search_replace_destructive() {
    let root = tempfile::tempdir().unwrap();
    let h = handler(root.path(), false).await;
    let sr = h
        .advertised_tools()
        .into_iter()
        .find(|t| t.name == "search_replace")
        .expect("edit tier serves search_replace");
    let a = sr.annotations.expect("annotations");
    assert_eq!(a.read_only_hint, Some(false));
    assert_eq!(a.destructive_hint, Some(true));
}

#[tokio::test]
async fn successful_call_is_not_an_error() {
    let root = tempfile::tempdir().unwrap();
    let f = root.path().join("hello.txt");
    fs::write(&f, b"turbo mcp serve").unwrap();
    let h = handler(root.path(), true).await;
    let res = h
        .dispatch("read_file", json!({"target_file": f.to_string_lossy()}))
        .await;
    assert_eq!(res.is_error, Some(false));
    assert!(format!("{:?}", res.content).contains("turbo mcp serve"));
}

#[tokio::test]
async fn refused_call_is_a_tool_error_carrying_the_opaque_refusal() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("id_rsa_copy");
    fs::write(&secret, b"KEY").unwrap();
    let h = handler(root.path(), true).await;
    let res = h
        .dispatch(
            "read_file",
            json!({"target_file": secret.to_string_lossy()}),
        )
        .await;
    assert_eq!(res.is_error, Some(true));
    assert!(format!("{:?}", res.content).contains(REFUSAL_TEXT));
}

#[tokio::test]
async fn refusal_text_leaks_nothing_about_the_filesystem() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let existing = outside.path().join("exists.txt");
    fs::write(&existing, b"x").unwrap();
    let missing = outside.path().join("missing.txt");
    let h = handler(root.path(), true).await;

    let a = h
        .dispatch(
            "read_file",
            json!({"target_file": existing.to_string_lossy()}),
        )
        .await;
    let b = h
        .dispatch(
            "read_file",
            json!({"target_file": missing.to_string_lossy()}),
        )
        .await;

    let text = |r: &rmcp::model::CallToolResult| format!("{:?}", r.content);
    assert_eq!(text(&a), text(&b));
    assert!(!text(&a).contains("exists.txt"), "{}", text(&a));
}

#[tokio::test]
async fn unlisted_tool_is_refused_at_the_handler() {
    let root = tempfile::tempdir().unwrap();
    let h = handler(root.path(), true).await;
    let res = h
        .dispatch("run_terminal_cmd", json!({"command": "whoami"}))
        .await;
    assert_eq!(res.is_error, Some(true));
    assert!(format!("{:?}", res.content).contains(REFUSAL_TEXT));
}

#[tokio::test]
async fn server_info_declares_tools_capability() {
    use rmcp::ServerHandler;
    let root = tempfile::tempdir().unwrap();
    let h = handler(root.path(), true).await;
    let info = h.get_info();
    assert!(info.capabilities.tools.is_some());
    assert!(info.instructions.is_some());
}
