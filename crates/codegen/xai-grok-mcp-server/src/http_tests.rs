//! Transport tests. `StreamableHttpService` authenticates nothing and limits
//! nothing on its own, so these assert that our layers are really in front of it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::http::{
    MAX_BODY_BYTES, MAX_CONCURRENT_REQUESTS, MAX_JSON_VALUES, ServeHandle, ServeOptions,
    json_value_count_exceeds, limit_body, serve, serve_with_options, shutdown_gracefully,
};
use crate::read_confined_fs::TestHooks;
use crate::toolset::{CallFailure, ServeEvent, ServedToolset, ToolsetOptions};

async fn server() -> (ServeHandle, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let ts = ServedToolset::new(vec![root.path().to_path_buf()], true)
        .await
        .expect("toolset builds");
    let (handle, _join) = serve(Arc::new(ts), None).await.expect("server binds");
    (handle, root)
}

/// A read-only toolset whose filesystem operations wait while the gate holds
/// `false`, with a file to read.
async fn gated_toolset(
    content: &str,
) -> (
    Arc<ServedToolset>,
    tokio::sync::watch::Sender<bool>,
    tempfile::TempDir,
    PathBuf,
) {
    let root = tempfile::tempdir().unwrap();
    let (gate, receiver) = tokio::sync::watch::channel(true);
    let ts = ServedToolset::with_options(
        vec![root.path().to_path_buf()],
        true,
        ToolsetOptions {
            hooks: TestHooks {
                gate: Some(receiver),
                ..TestHooks::default()
            },
            ..ToolsetOptions::default()
        },
    )
    .await
    .expect("toolset builds");
    let file = ts.roots()[0].join("slow.txt");
    std::fs::write(&file, content).unwrap();
    (Arc::new(ts), gate, root, file)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

fn list_body() -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}).to_string()
}

fn read_call_body(file: &Path) -> String {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "read_file", "arguments": {"target_file": file.to_string_lossy()}}
    })
    .to_string()
}

async fn post_list(h: &ServeHandle, token: &str) -> reqwest::Response {
    client()
        .post(&h.url)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(list_body())
        .send()
        .await
        .expect("request completes")
}

/// The random path segment of a server URL: `http://host:port/<segment>/mcp`.
fn path_segment(url: &str) -> String {
    url.splitn(4, '/')
        .nth(3)
        .and_then(|path| path.split('/').next())
        .expect("URL has a path")
        .to_string()
}

/// The head of a raw HTTP request to the server's MCP path.
fn raw_request_head(h: &ServeHandle, token: Option<&str>, content_length: usize) -> String {
    let path = format!("/{}", h.url.splitn(4, '/').nth(3).unwrap());
    let authorization = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{authorization}\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: {content_length}\r\n\r\n",
        port = h.port
    )
}

/// What the server sent within `within`: `None` if nothing yet, an empty string
/// if it closed the connection.
async fn read_head(stream: &mut TcpStream, within: Duration) -> Option<String> {
    let mut buf = vec![0u8; 512];
    match tokio::time::timeout(within, stream.read(&mut buf)).await {
        Ok(Ok(n)) => Some(String::from_utf8_lossy(&buf[..n]).into_owned()),
        Ok(Err(_)) => Some(String::new()),
        Err(_) => None,
    }
}

async fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The value of `read` once it has stopped changing.
async fn settled(mut read: impl FnMut() -> usize) -> usize {
    let mut last = read();
    let mut unchanged = 0;
    for _ in 0..400 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        let now = read();
        if now == last {
            unchanged += 1;
            if unchanged >= 8 {
                break;
            }
        } else {
            unchanged = 0;
            last = now;
        }
    }
    last
}

#[tokio::test]
async fn binds_loopback_only() {
    let (h, _root) = server().await;
    assert_eq!(
        h.addr.ip(),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "bound to {}",
        h.addr
    );
    assert_eq!(h.port, h.addr.port());
    assert!(
        h.url.starts_with(&format!("http://{}/", h.addr)),
        "{}",
        h.url
    );
    h.shutdown();
}

#[tokio::test]
async fn dropping_the_handle_stops_the_server() {
    let root = tempfile::tempdir().unwrap();
    let ts = ServedToolset::new(vec![root.path().to_path_buf()], true)
        .await
        .unwrap();
    let (h, join) = serve(Arc::new(ts), None).await.unwrap();
    let addr = h.addr;
    drop(h);
    tokio::time::timeout(Duration::from_secs(10), join)
        .await
        .expect("the accept loop exits")
        .unwrap();
    assert!(
        TcpStream::connect(addr).await.is_err(),
        "{addr} still accepts connections"
    );
}

#[tokio::test]
async fn missing_bearer_is_unauthorized() {
    let (h, _root) = server().await;
    let res = client()
        .post(&h.url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(list_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);
    h.shutdown();
}

#[tokio::test]
async fn wrong_bearer_is_unauthorized() {
    let (h, _root) = server().await;
    let res = post_list(&h, "not-the-token").await;
    assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);
    h.shutdown();
}

#[tokio::test]
async fn audit_same_length_wrong_token_is_unauthorized() {
    // A wrong token of the right length reaches the content comparison, not just
    // the length check, and a valid MCP request means a bypass would show as 200.
    let (h, _root) = server().await;
    for position in [0, h.token.len() - 1] {
        let mut wrong = h.token.clone().into_bytes();
        wrong[position] = if wrong[position] == b'a' { b'b' } else { b'a' };
        let wrong = String::from_utf8(wrong).unwrap();
        let res = post_list(&h, &wrong).await;
        assert_eq!(
            res.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "byte {position} differs"
        );
    }
    h.shutdown();
}

#[tokio::test]
async fn correct_bearer_serves_a_valid_request() {
    let (h, _root) = server().await;
    let res = post_list(&h, &h.token).await;
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    assert!(res.text().await.unwrap().contains("read_file"));
    h.shutdown();
}

#[tokio::test]
async fn audit_the_bearer_scheme_name_is_case_insensitive() {
    let (h, _root) = server().await;
    let res = client()
        .post(&h.url)
        .header("authorization", format!("bearer  {}", h.token))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(list_body())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    h.shutdown();
}

#[tokio::test]
async fn unknown_path_is_not_served() {
    let (h, _root) = server().await;
    let bad = format!("http://{}/wrong-segment/mcp", h.addr);
    let res = client()
        .post(&bad)
        .header("authorization", format!("Bearer {}", h.token))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::NOT_FOUND);
    h.shutdown();
}

#[tokio::test]
async fn token_and_path_segment_are_distinct_and_random() {
    let (a, _r1) = server().await;
    let (b, _r2) = server().await;
    assert_ne!(a.token, b.token);
    // Compare the path segments alone: the whole URLs differ by port anyway.
    assert_ne!(path_segment(&a.url), path_segment(&b.url));
    let segment = path_segment(&a.url);
    assert_eq!(segment.len(), 32, "{}", a.url);
    assert!(segment.chars().all(|c| c.is_ascii_hexdigit()), "{}", a.url);
    assert!(a.token.len() >= 64);
    a.shutdown();
    b.shutdown();
}

#[tokio::test]
async fn audit_oversized_body_is_rejected_before_buffering() {
    // DefaultBodyLimit never reached rmcp's raw body read. Send only the headers
    // of an oversized request: a real limit answers without waiting for a body.
    let (h, _root) = server().await;
    let mut stream = TcpStream::connect(h.addr).await.unwrap();
    stream
        .write_all(raw_request_head(&h, Some(&h.token), MAX_BODY_BYTES + 1).as_bytes())
        .await
        .unwrap();
    let head = read_head(&mut stream, Duration::from_secs(10))
        .await
        .expect("the server must answer without waiting for the body");
    assert!(head.starts_with("HTTP/1.1 413"), "{head}");
    h.shutdown();
}

#[tokio::test]
async fn audit_an_unauthenticated_request_is_refused_before_its_body_is_read() {
    // A declared body that never arrives: if the body were read first, the
    // answer would wait for it.
    let (h, _root) = server().await;
    let mut stream = TcpStream::connect(h.addr).await.unwrap();
    stream
        .write_all(raw_request_head(&h, None, MAX_BODY_BYTES).as_bytes())
        .await
        .unwrap();
    let head = read_head(&mut stream, Duration::from_secs(5))
        .await
        .expect("an unauthenticated request is answered at once");
    assert!(head.starts_with("HTTP/1.1 401"), "{head}");
    h.shutdown();
}

#[tokio::test]
async fn audit_a_streamed_body_over_the_limit_is_refused_without_content_length() {
    use axum::body::{Body, Bytes};
    use tower::ServiceExt as _;

    let app = axum::Router::new()
        .route(
            "/",
            axum::routing::post(|request: axum::extract::Request| async move {
                let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                    .await
                    .unwrap();
                body.len().to_string()
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            ServeOptions::default(),
            limit_body,
        ));

    let chunk = Bytes::from(vec![b'x'; 1024 * 1024]);
    let streamed = |chunks: usize| {
        let chunk = chunk.clone();
        let stream = futures_util::stream::iter(
            (0..chunks).map(move |_| Ok::<_, std::io::Error>(chunk.clone())),
        );
        axum::http::Request::post("/")
            .body(Body::from_stream(stream))
            .unwrap()
    };
    let at_limit = MAX_BODY_BYTES / chunk.len();
    assert_eq!(at_limit * chunk.len(), MAX_BODY_BYTES);

    let res = app.clone().oneshot(streamed(at_limit + 1)).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::PAYLOAD_TOO_LARGE);

    let res = app.oneshot(streamed(at_limit)).await.unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], MAX_BODY_BYTES.to_string().as_bytes());
}

#[tokio::test]
async fn audit_a_body_with_too_many_json_values_is_refused() {
    use axum::body::Body;
    use tower::ServiceExt as _;

    let app = axum::Router::new()
        .route("/", axum::routing::post(|| async { "parsed" }))
        .layer(axum::middleware::from_fn_with_state(
            ServeOptions::default(),
            limit_body,
        ));
    // Small in bytes, huge as a parsed tree.
    let mut many = String::from("[");
    many.push_str(&"0,".repeat(MAX_JSON_VALUES + 1));
    many.push_str("0]");
    assert!(many.len() < MAX_BODY_BYTES);
    let res = app
        .clone()
        .oneshot(
            axum::http::Request::post("/")
                .body(Body::from(many))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::PAYLOAD_TOO_LARGE);

    let ordinary = read_call_body(Path::new("/a/b.txt"));
    let res = app
        .oneshot(
            axum::http::Request::post("/")
                .body(Body::from(ordinary))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
}

#[test]
fn json_values_inside_strings_are_not_counted() {
    assert!(!json_value_count_exceeds(br#"{"a":"[[[,,,:::]]]\"[["}"#, 2));
    assert!(json_value_count_exceeds(br#"{"a":1,"b":2}"#, 2));
}

#[tokio::test]
async fn audit_a_body_that_never_finishes_is_timed_out() {
    let root = tempfile::tempdir().unwrap();
    let ts = ServedToolset::new(vec![root.path().to_path_buf()], true)
        .await
        .unwrap();
    let (h, _join) = serve_with_options(
        Arc::new(ts),
        None,
        ServeOptions {
            body_read_timeout: Duration::from_millis(300),
            ..ServeOptions::default()
        },
    )
    .await
    .unwrap();
    let mut stream = TcpStream::connect(h.addr).await.unwrap();
    stream
        .write_all(raw_request_head(&h, Some(&h.token), 100).as_bytes())
        .await
        .unwrap();
    stream.write_all(b"{\"jsonrpc\"").await.unwrap();
    let head = read_head(&mut stream, Duration::from_secs(5))
        .await
        .expect("a stalled body is answered");
    assert!(head.starts_with("HTTP/1.1 408"), "{head}");
    h.shutdown();
}

#[tokio::test]
async fn audit_a_connection_that_never_finishes_its_headers_is_closed() {
    let root = tempfile::tempdir().unwrap();
    let ts = ServedToolset::new(vec![root.path().to_path_buf()], true)
        .await
        .unwrap();
    let (h, _join) = serve_with_options(
        Arc::new(ts),
        None,
        ServeOptions {
            header_read_timeout: Duration::from_millis(300),
            ..ServeOptions::default()
        },
    )
    .await
    .unwrap();
    let mut stream = TcpStream::connect(h.addr).await.unwrap();
    stream.write_all(b"POST / HTT").await.unwrap();
    let head = read_head(&mut stream, Duration::from_secs(5))
        .await
        .expect("the server closes a connection whose headers never finish");
    assert!(
        head.is_empty() || head.starts_with("HTTP/1.1 408"),
        "{head}"
    );
    wait_for("the connection to be released", || {
        h.open_connections() == 0
    })
    .await;
    h.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_request_slots_bound_concurrency_and_are_taken_only_after_authentication() {
    let (h, _root) = server().await;
    // Authenticated requests whose bodies never finish each hold a slot.
    let mut held = Vec::new();
    for _ in 0..MAX_CONCURRENT_REQUESTS {
        let mut stream = TcpStream::connect(h.addr).await.unwrap();
        stream
            .write_all(raw_request_head(&h, Some(&h.token), 100).as_bytes())
            .await
            .unwrap();
        stream.write_all(b"{\"jsonrpc\"").await.unwrap();
        held.push(stream);
    }
    wait_for("every request slot to be taken", || {
        h.free_request_slots() == 0
    })
    .await;

    // Another authenticated request waits for a slot.
    let mut waiting = TcpStream::connect(h.addr).await.unwrap();
    waiting
        .write_all(raw_request_head(&h, Some(&h.token), 2).as_bytes())
        .await
        .unwrap();
    waiting.write_all(b"{}").await.unwrap();
    assert!(
        read_head(&mut waiting, Duration::from_millis(500))
            .await
            .is_none(),
        "a request was served while every slot was held"
    );

    // An unauthenticated request is still answered at once: it never takes a slot.
    let mut stranger = TcpStream::connect(h.addr).await.unwrap();
    stranger
        .write_all(raw_request_head(&h, None, 2).as_bytes())
        .await
        .unwrap();
    stranger.write_all(b"{}").await.unwrap();
    let head = read_head(&mut stranger, Duration::from_secs(5))
        .await
        .expect("a prompt answer");
    assert!(head.starts_with("HTTP/1.1 401"), "{head}");

    drop(held);
    let head = read_head(&mut waiting, Duration::from_secs(10))
        .await
        .expect("the waiting request is served once slots free up");
    assert!(head.starts_with("HTTP/1.1 "), "{head}");
    h.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_a_client_disconnect_mid_call_strands_no_task() {
    let (ts, gate, _root, file) = gated_toolset("eventually\n").await;
    let (h, _join) = serve(ts.clone(), None).await.expect("server binds");
    let call = read_call_body(&file);
    let post = |client: &reqwest::Client| {
        client
            .post(&h.url)
            .header("authorization", format!("Bearer {}", h.token))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(call.clone())
            .send()
    };

    // Warm up, so whatever the bridge starts lazily is part of the baseline.
    let warm = client();
    let res = post(&warm).await.expect("warm-up call completes");
    assert!(res.status().is_success(), "{}", res.status());
    let _ = res.text().await;
    drop(warm);

    let metrics = tokio::runtime::Handle::current().metrics();
    let baseline = settled(|| metrics.num_alive_tasks()).await;

    gate.send_replace(false);
    let impatient = client();
    let pending = tokio::spawn(post(&impatient));
    wait_for("the call to reach the tool", || ts.in_flight() == 1).await;
    // The client goes away mid-call.
    pending.abort();
    let _ = pending.await;
    drop(impatient);
    // Only reopen the gate once the server has dropped the request: otherwise
    // the call could finish first and the test would pass without the fix.
    wait_for("the server to notice the disconnect", || {
        h.open_connections() == 0
    })
    .await;

    gate.send_replace(true);
    wait_for("the abandoned call to finish", || ts.in_flight() == 0).await;
    wait_for("every request task to exit", || {
        metrics.num_alive_tasks() <= baseline
    })
    .await;
    h.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_a_disconnected_request_keeps_its_slot_until_its_call_ends() {
    let (ts, gate, _root, file) = gated_toolset("eventually\n").await;
    let (h, _join) = serve(ts.clone(), None).await.expect("server binds");
    let body = read_call_body(&file);
    gate.send_replace(false);

    let mut stream = TcpStream::connect(h.addr).await.unwrap();
    stream
        .write_all(raw_request_head(&h, Some(&h.token), body.len()).as_bytes())
        .await
        .unwrap();
    stream.write_all(body.as_bytes()).await.unwrap();
    wait_for("the call to reach the tool", || ts.in_flight() == 1).await;
    assert_eq!(h.free_request_slots(), MAX_CONCURRENT_REQUESTS - 1);

    drop(stream);
    wait_for("the server to notice the disconnect", || {
        h.open_connections() == 0
    })
    .await;
    assert_eq!(
        h.free_request_slots(),
        MAX_CONCURRENT_REQUESTS - 1,
        "the slot was freed while rmcp still held the request"
    );

    gate.send_replace(true);
    wait_for("the call to finish", || ts.in_flight() == 0).await;
    wait_for("the slot to come back", || {
        h.free_request_slots() == MAX_CONCURRENT_REQUESTS
    })
    .await;
    h.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_graceful_shutdown_lets_a_running_call_deliver_its_result() {
    let (ts, gate, _root, file) = gated_toolset("delivered after shutdown began\n").await;
    let (h, join) = serve(ts.clone(), None).await.expect("server binds");
    gate.send_replace(false);

    let pending = {
        let url = h.url.clone();
        let token = h.token.clone();
        let body = read_call_body(&file);
        tokio::spawn(async move {
            client()
                .post(&url)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .body(body)
                .send()
                .await?
                .text()
                .await
        })
    };
    wait_for("the call to reach the tool", || ts.in_flight() == 1).await;

    let shutdown = {
        let ts = ts.clone();
        tokio::spawn(async move { shutdown_gracefully(&h, &ts, join).await })
    };
    wait_for("shutdown to begin", || ts.is_shutting_down()).await;
    // New calls are refused from the moment shutdown begins.
    let refused = ts
        .call(
            "read_file",
            serde_json::json!({"target_file": file.to_string_lossy()}),
        )
        .await;
    assert!(
        matches!(&refused, Err(CallFailure::Failed(m)) if m.contains("shutting down")),
        "{refused:?}"
    );

    gate.send_replace(true);
    let text = tokio::time::timeout(Duration::from_secs(30), pending)
        .await
        .expect("the running call returns")
        .unwrap()
        .expect("a response, not a dropped connection");
    assert!(text.contains("delivered after shutdown began"), "{text}");
    tokio::time::timeout(Duration::from_secs(30), shutdown)
        .await
        .expect("shutdown finishes")
        .unwrap();
}

#[tokio::test]
async fn audit_cross_origin_preflight_gets_no_cors_grant() {
    let (h, _root) = server().await;
    let res = client()
        .request(reqwest::Method::OPTIONS, &h.url)
        .header("origin", "https://evil.example")
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "authorization,content-type",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(res.headers().get("access-control-allow-origin").is_none());
    h.shutdown();
}

#[tokio::test]
async fn audit_rejected_credentials_reach_the_operator_observer() {
    let root = tempfile::tempdir().unwrap();
    let mut ts = ServedToolset::new(vec![root.path().to_path_buf()], true)
        .await
        .unwrap();
    let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    ts.set_observer(Arc::new(move |event: ServeEvent<'_>| {
        sink.lock().unwrap().push(match event {
            ServeEvent::Unauthorized => "unauthorized",
            ServeEvent::Refused { .. } => "refused",
        });
    }));
    let (h, _join) = serve(Arc::new(ts), None).await.unwrap();
    let _ = post_list(&h, "wrong").await;
    assert!(events.lock().unwrap().contains(&"unauthorized"));
    h.shutdown();
}

// ---------------------------------------------------------------------------
// Wire level: real JSON-RPC over HTTP, exactly as an MCP client sends it
// ---------------------------------------------------------------------------

async fn rpc(
    h: &ServeHandle,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let body = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let res = client()
        .post(&h.url)
        .header("authorization", format!("Bearer {}", h.token))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(body.to_string())
        .send()
        .await
        .expect("request completes");
    let status = res.status();
    let text = res.text().await.expect("body");
    assert!(status.is_success(), "{method} -> {status}: {text}");
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{method} returned non-JSON ({e}): {text}"))
}

#[tokio::test]
async fn wire_initialize_identifies_turbo_and_declares_tools() {
    let (h, _root) = server().await;
    let v = rpc(
        &h,
        1,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "wire-test", "version": "0"}
        }),
    )
    .await;
    assert_eq!(v["result"]["serverInfo"]["name"], "Turbo Build", "{v}");
    assert!(v["result"]["capabilities"]["tools"].is_object(), "{v}");
    h.shutdown();
}

#[tokio::test]
async fn wire_tools_list_is_annotated_and_shell_free() {
    let (h, _root) = server().await;
    let v = rpc(&h, 2, "tools/list", serde_json::json!({})).await;
    let tools = v["result"]["tools"].as_array().expect("tools array");
    assert!(!tools.is_empty(), "{v}");
    for t in tools {
        let name = t["name"].as_str().unwrap_or_default();
        assert!(!name.contains("run_terminal_cmd"), "shell served: {name}");
        assert_eq!(t["annotations"]["readOnlyHint"], true, "{name}: {t}");
        assert!(t["inputSchema"].is_object(), "{name}: {t}");
    }
    assert!(tools.iter().any(|t| t["name"] == "read_file"), "{v}");
    h.shutdown();
}

#[tokio::test]
async fn wire_tools_call_reads_a_file_inside_the_root() {
    let (h, root) = server().await;
    let f = root.path().join("hello.txt");
    std::fs::write(&f, "over the wire").unwrap();
    let v = rpc(
        &h,
        3,
        "tools/call",
        serde_json::json!({"name": "read_file", "arguments": {"target_file": f.to_string_lossy()}}),
    )
    .await;
    assert_eq!(v["result"]["isError"], false, "{v}");
    assert!(v.to_string().contains("over the wire"), "{v}");
    h.shutdown();
}

#[tokio::test]
async fn wire_tools_call_outside_the_root_is_refused_opaquely() {
    let (h, _root) = server().await;
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("id_rsa_wire_test");
    std::fs::write(&secret, "PRIVATE").unwrap();
    let v = rpc(
        &h,
        4,
        "tools/call",
        serde_json::json!({"name": "read_file", "arguments": {"target_file": secret.to_string_lossy()}}),
    )
    .await;
    assert_eq!(v["result"]["isError"], true, "{v}");
    let text = v.to_string();
    // The refusal is the guard's, not some unrelated tool failure.
    assert!(text.contains(crate::guard::REFUSAL_TEXT), "{text}");
    assert!(!text.contains("PRIVATE"), "{text}");
    assert!(!text.contains("id_rsa_wire_test"), "{text}");
    h.shutdown();
}

#[tokio::test]
async fn wire_tools_call_to_a_shell_is_refused() {
    let (h, _root) = server().await;
    let v = rpc(
        &h,
        5,
        "tools/call",
        serde_json::json!({"name": "run_terminal_cmd", "arguments": {"command": "whoami"}}),
    )
    .await;
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert!(v.to_string().contains(crate::guard::REFUSAL_TEXT), "{v}");
    h.shutdown();
}

// ---------------------------------------------------------------------------
// OAuth 2.1 (Task 11). These five routes answer before any credential exists,
// so each case here is a refusal the server must make, not a happy path.
// ---------------------------------------------------------------------------

/// A client that does not chase redirects: the consent step answers `302` to a
/// URL that is not this server, and following it would leave the test host.
fn oauth_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn origin(h: &ServeHandle) -> String {
    format!("http://{}", h.addr)
}

/// A PKCE pair the server will accept: the challenge is what S256 makes of the
/// verifier, computed the same way the server checks it.
fn pkce() -> (String, String) {
    let verifier = "turbo-mcp-verifier-0123456789-0123456789-abc".to_string();
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    let challenge = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        digest.as_ref(),
    );
    (verifier, challenge)
}

const REDIRECT: &str = "https://chat.example/callback";

/// Register a client the way a connector does, and return its id.
async fn register(h: &ServeHandle, redirect: &str) -> reqwest::Response {
    oauth_client()
        .post(format!("{}/oauth/register", origin(h)))
        .json(&serde_json::json!({
            "redirect_uris": [redirect],
            "client_name": "Test connector",
        }))
        .send()
        .await
        .expect("request completes")
}

async fn registered_client(h: &ServeHandle) -> String {
    let res = register(h, REDIRECT).await;
    assert_eq!(res.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = res.json().await.expect("registration is JSON");
    assert!(
        body.get("client_secret").is_none(),
        "a public client is issued no secret: {body}"
    );
    body["client_id"].as_str().expect("a client id").to_string()
}

/// The `state` every test drives the flow with. It deliberately holds the
/// characters that break an unescaped redirect: `+` and `=` are what base64
/// padding looks like, `&` and `#` would end the parameter, and `code=` after
/// them would be read as a second authorization code.
const HOSTILE_STATE: &str = "a+b/c=d&code=injected#frag";

fn authorize_url(h: &ServeHandle, client_id: &str, challenge: &str, method: &str) -> String {
    authorize_url_with_state(h, client_id, challenge, method, HOSTILE_STATE)
}

/// The same, with a `state` of the caller's choosing. Appending a second
/// `state=` to the URL above would not do: the query is deserialized into one
/// struct, so a duplicate key is refused as a malformed request before any of
/// this server's own rules are reached — which looks exactly like the refusal a
/// test of those rules is trying to observe.
fn authorize_url_with_state(
    h: &ServeHandle,
    client_id: &str,
    challenge: &str,
    method: &str,
    state: &str,
) -> String {
    format!(
        "{}/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri={}\
         &code_challenge={challenge}&code_challenge_method={method}&state={}",
        origin(h),
        urlencode(REDIRECT),
        urlencode(state)
    )
}

/// Fetch the consent page, which is what a browser does before the operator
/// types anything. The consent window starts here, so a test that posts an
/// approval without this is approving a window that never opened.
async fn open_consent(url: &str) {
    let res = oauth_client()
        .get(url)
        .send()
        .await
        .expect("request completes");
    // Asserted rather than ignored: if the page stops rendering, every approval
    // after it fails as "expired", which points at the wrong layer entirely.
    assert_eq!(
        res.status(),
        reqwest::StatusCode::OK,
        "the consent page is served before the operator can approve"
    );
}

/// Percent-encode every character a query parameter gives a meaning to, so the
/// value reaches the server as one parameter.
fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The raw `Location` an approved consent answers with.
fn location_of(res: &reqwest::Response) -> String {
    res.headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .expect("a redirect back to the client")
        .to_string()
}

/// The `code` an approved consent hands back through the redirect.
fn code_from_location(res: &reqwest::Response) -> String {
    let location = res
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .expect("a redirect back to the client");
    location
        .split(['?', '&'])
        .find_map(|part| part.strip_prefix("code="))
        .expect("the redirect carries a code")
        .to_string()
}

/// Register, approve with the console code, and redeem: the whole flow a
/// connector performs, returning the access and refresh tokens.
async fn authorized_tokens(h: &ServeHandle) -> (String, String, String) {
    let client_id = registered_client(h).await;
    let (verifier, challenge) = pkce();
    let url = authorize_url(h, &client_id, &challenge, "S256");
    open_consent(&url).await;
    let res = oauth_client()
        .post(url)
        .form(&[("consent_code", h.oauth.consent_code())])
        .send()
        .await
        .expect("request completes");
    assert_eq!(res.status(), reqwest::StatusCode::FOUND, "consent approves");
    let code = code_from_location(&res);
    let res = oauth_client()
        .post(format!("{}/oauth/token", origin(h)))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", REDIRECT),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .expect("request completes");
    assert_eq!(
        res.status(),
        reqwest::StatusCode::OK,
        "the code is redeemed"
    );
    let body: serde_json::Value = res.json().await.expect("a token response");
    (
        client_id,
        body["access_token"]
            .as_str()
            .expect("an access token")
            .to_string(),
        body["refresh_token"]
            .as_str()
            .expect("a refresh token")
            .to_string(),
    )
}

#[tokio::test]
async fn oauth_metadata_names_this_server_and_its_authorization_server() {
    let (h, _root) = server().await;
    let res = oauth_client()
        .get(h.metadata_url())
        .send()
        .await
        .expect("request completes");
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = res.json().await.expect("metadata is JSON");
    assert_eq!(body["resource"], h.url, "the resource is the MCP URL");
    assert_eq!(body["authorization_servers"][0], origin(&h));
    assert_eq!(body["bearer_methods_supported"][0], "header");
}

#[tokio::test]
async fn an_unauthenticated_call_is_told_where_the_metadata_is() {
    let (h, _root) = server().await;
    let res = post_list(&h, "").await;
    assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);
    let challenge = res
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .expect("a challenge");
    assert!(
        challenge.contains(&format!("resource_metadata=\"{}\"", h.metadata_url())),
        "{challenge}"
    );
}

#[tokio::test]
async fn the_challenge_follows_the_public_url_once_the_tunnel_reports_one() {
    let (h, _root) = server().await;
    // What a tunnel does at startup: the server keeps listening on loopback,
    // but the URL clients reach it at is somewhere else entirely.
    h.oauth
        .set_resource("https://example-tunnel.test/abc123/mcp".to_string());

    let res = post_list(&h, "").await;
    assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);
    let challenge = res
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .expect("a challenge")
        .to_string();

    // The whole point of the challenge is to be followable by the client that
    // received it. A remote client resolves 127.0.0.1 to itself, so a loopback
    // pointer here is the same as no pointer at all.
    assert!(
        challenge.contains(
            "resource_metadata=\"https://example-tunnel.test\
             /.well-known/oauth-protected-resource/abc123/mcp\""
        ),
        "the challenge must name the URL the client actually used: {challenge}"
    );
    assert!(
        !challenge.contains("127.0.0.1"),
        "a client that is not on this host cannot follow loopback: {challenge}"
    );
}

#[tokio::test]
async fn the_authorization_server_offers_only_what_oauth_21_requires() {
    let (h, _root) = server().await;
    let res = oauth_client()
        .get(format!(
            "{}/.well-known/oauth-authorization-server",
            origin(&h)
        ))
        .send()
        .await
        .expect("request completes");
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = res.json().await.expect("metadata is JSON");
    assert_eq!(body["code_challenge_methods_supported"][0], "S256");
    assert_eq!(
        body["code_challenge_methods_supported"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "plain must not be offered"
    );
    assert_eq!(body["token_endpoint_auth_methods_supported"][0], "none");
}

#[tokio::test]
async fn registration_refuses_a_redirect_that_is_not_https_or_loopback() {
    let (h, _root) = server().await;
    for redirect in ["http://evil.example/callback", "ftp://x/y", ""] {
        let res = register(&h, redirect).await;
        assert_eq!(
            res.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{redirect} must not be registrable"
        );
    }
    // A native client comes back to loopback, which OAuth 2.1 allows.
    let res = register(&h, "http://127.0.0.1:7777/callback").await;
    assert_eq!(res.status(), reqwest::StatusCode::CREATED);
}

#[tokio::test]
async fn authorization_refuses_plain_pkce_an_unknown_client_and_a_wrong_redirect() {
    let (h, _root) = server().await;
    let client_id = registered_client(&h).await;
    let (_verifier, challenge) = pkce();

    let plain = oauth_client()
        .get(authorize_url(&h, &client_id, &challenge, "plain"))
        .send()
        .await
        .expect("request completes");
    assert_eq!(plain.status(), reqwest::StatusCode::BAD_REQUEST);

    let unknown = oauth_client()
        .get(authorize_url(&h, "0000", &challenge, "S256"))
        .send()
        .await
        .expect("request completes");
    assert_eq!(unknown.status(), reqwest::StatusCode::UNAUTHORIZED);

    let elsewhere = oauth_client()
        .get(format!(
            "{}/oauth/authorize?response_type=code&client_id={client_id}\
             &redirect_uri=https%3A%2F%2Fattacker.example%2Fcb&code_challenge={challenge}\
             &code_challenge_method=S256",
            origin(&h)
        ))
        .send()
        .await
        .expect("request completes");
    assert_eq!(
        elsewhere.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "a redirect that was never registered is an open redirect"
    );
}

#[tokio::test]
async fn consent_without_the_console_code_issues_nothing() {
    let (h, _root) = server().await;
    let client_id = registered_client(&h).await;
    let (_verifier, challenge) = pkce();
    let url = authorize_url(&h, &client_id, &challenge, "S256");
    for presented in ["", "AAAAAAAA", "aaaaaaaa"] {
        // Opened each time, so what refuses below is the wrong code and not a
        // closed window: a test that cannot tell those apart proves neither.
        open_consent(&url).await;
        let res = oauth_client()
            .post(&url)
            .form(&[("consent_code", presented)])
            .send()
            .await
            .expect("request completes");
        assert_eq!(
            res.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "reaching the endpoint must not be enough to mint a token"
        );
    }
}

#[tokio::test]
async fn the_state_comes_back_whole_and_cannot_add_parameters_of_its_own() {
    let (h, _root) = server().await;
    let client_id = registered_client(&h).await;
    let (_verifier, challenge) = pkce();
    let url = authorize_url(&h, &client_id, &challenge, "S256");
    open_consent(&url).await;
    let approved = oauth_client()
        .post(url)
        .form(&[("consent_code", h.oauth.consent_code())])
        .send()
        .await
        .expect("request completes");
    assert_eq!(approved.status(), reqwest::StatusCode::FOUND);
    let location = location_of(&approved);

    // The client compares what it gets back against what it sent, so anything
    // but a byte-for-byte match is a failure for it.
    let carried = location
        .split(['?', '&'])
        .find_map(|part| part.strip_prefix("state="))
        .expect("the redirect carries the state");
    assert_eq!(
        percent_decode(carried),
        HOSTILE_STATE,
        "the state must survive the redirect unchanged: {location}"
    );

    // An unescaped state would have ended the parameter and written a second
    // `code=` the client might prefer over the real one.
    let codes = location
        .split(['?', '&'])
        .filter(|part| part.starts_with("code="))
        .count();
    assert_eq!(codes, 1, "exactly one authorization code: {location}");
    // The injected text may still appear, since letters need no escaping. What
    // matters is that the characters around it were escaped, so it stays part
    // of the state's value instead of becoming a parameter beside it.
    assert!(
        location.contains("%26code%3Dinjected"),
        "the separators inside the state must be escaped: {location}"
    );
    // `#` would have made everything after it a fragment the client never sees.
    assert!(
        !location.contains('#'),
        "the state must not open a fragment: {location}"
    );
}

#[tokio::test]
async fn an_overlong_state_is_refused_rather_than_shortened() {
    let (h, _root) = server().await;
    let client_id = registered_client(&h).await;
    let (_verifier, challenge) = pkce();
    let huge = "s".repeat(4096);
    let url = authorize_url_with_state(&h, &client_id, &challenge, "S256", &huge);
    // The page renders for this request too: the length is judged where the
    // state would be echoed into a header, which is the approval, not the page.
    open_consent(&url).await;
    let res = oauth_client()
        .post(url)
        .form(&[("consent_code", h.oauth.consent_code())])
        .send()
        .await
        .expect("request completes");
    assert_eq!(
        res.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "a state too long to echo is refused, not truncated into one the client will reject"
    );
}

/// Undo [`urlencode`], so a test can compare against the value it sent.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).expect("ascii hex");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).expect("the state was UTF-8")
}

#[tokio::test]
async fn a_code_is_useless_without_its_verifier_and_can_be_spent_once() {
    let (h, _root) = server().await;
    let client_id = registered_client(&h).await;
    let (verifier, challenge) = pkce();
    let url = authorize_url(&h, &client_id, &challenge, "S256");
    open_consent(&url).await;
    let approved = oauth_client()
        .post(url)
        .form(&[("consent_code", h.oauth.consent_code())])
        .send()
        .await
        .expect("request completes");
    let code = code_from_location(&approved);
    let base = origin(&h);

    // The wrong verifier cannot redeem it, and the attempt spends the code.
    let wrong = redeem_code(
        &base,
        &client_id,
        &code,
        "another-verifier-0123456789-0123456789-abcd",
    )
    .await;
    assert_eq!(wrong.status(), reqwest::StatusCode::BAD_REQUEST);
    let replay = redeem_code(&base, &client_id, &code, &verifier).await;
    assert_eq!(
        replay.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "a code survives no failed attempt, so it cannot be guessed at"
    );
}

#[tokio::test]
async fn a_token_this_server_issued_opens_the_mcp_route() {
    let (h, _root) = server().await;
    let (_client_id, access, _refresh) = authorized_tokens(&h).await;
    let res = post_list(&h, &access).await;
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    // And the operator's own token still works: OAuth is additive.
    let token = h.token.clone();
    assert_eq!(
        post_list(&h, &token).await.status(),
        reqwest::StatusCode::OK
    );
}

/// Redeem an authorization code. A free function, not a closure: each test
/// calls it more than once, which a closure capturing the handle cannot do.
async fn redeem_code(
    origin: &str,
    client_id: &str,
    code: &str,
    verifier: &str,
) -> reqwest::Response {
    oauth_client()
        .post(format!("{origin}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .expect("request completes")
}

async fn refresh_token(origin: &str, client_id: &str, refresh: &str) -> reqwest::Response {
    oauth_client()
        .post(format!("{origin}/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh),
        ])
        .send()
        .await
        .expect("request completes")
}

#[tokio::test]
async fn a_refresh_token_rotates_and_a_replay_revokes_the_grant() {
    let (h, _root) = server().await;
    let (client_id, _access, refresh) = authorized_tokens(&h).await;
    let base = origin(&h);

    // Only the client the grant was issued to may refresh it.
    let stranger = registered_client(&h).await;
    let other = refresh_token(&base, &stranger, &refresh).await;
    assert_eq!(other.status(), reqwest::StatusCode::BAD_REQUEST);

    // A refresh rotates: the token that comes back is not the one sent.
    let res = refresh_token(&base, &client_id, &refresh).await;
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = res.json().await.expect("a token response");
    let rotated_access = body["access_token"].as_str().expect("an access token");
    let rotated_refresh = body["refresh_token"].as_str().expect("a refresh token");
    assert_ne!(rotated_refresh, refresh, "a refresh token must rotate");
    assert_eq!(
        post_list(&h, rotated_access).await.status(),
        reqwest::StatusCode::OK,
        "the rotated access token works"
    );

    // Presenting the spent one is theft, so the grant it became is revoked.
    let replay = refresh_token(&base, &client_id, &refresh).await;
    assert_eq!(replay.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        post_list(&h, rotated_access).await.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a replayed refresh token revokes what it had become"
    );
}

#[tokio::test]
async fn a_token_issued_for_another_url_is_not_accepted_here() {
    let (h, _root) = server().await;
    let (_client_id, access, _refresh) = authorized_tokens(&h).await;
    assert_eq!(
        post_list(&h, &access).await.status(),
        reqwest::StatusCode::OK
    );
    // The tunnel comes up and the server is reached at another URL. A token
    // issued for the old one is not a token for this resource.
    h.oauth
        .set_resource("https://public.example/abcdef/mcp".to_string());
    assert_eq!(
        post_list(&h, &access).await.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    // Refusing the old token only shows the store was emptied. Mint a token at
    // the new resource and show it works: that separates "the rebind cleared
    // everything" from "nothing issued here is ever accepted again", which the
    // assertion above cannot tell apart on its own.
    let (_client_id, fresh, _refresh) = authorized_tokens(&h).await;
    assert_eq!(
        post_list(&h, &fresh).await.status(),
        reqwest::StatusCode::OK,
        "a token minted for the current resource is accepted"
    );
}

/// The audience comparison itself, which the wire test above cannot reach:
/// `set_resource` clears every grant, so no stale-resource token survives long
/// enough for `accepts` to judge it. Without this, deleting the `same_resource`
/// check entirely would leave the whole suite green.
#[test]
fn a_grant_minted_for_another_resource_is_refused_by_the_audience_check() {
    let state = crate::oauth::OauthState::new("http://127.0.0.1:9/abc/mcp".to_string());
    let issued = state.testing_issue_grant("https://somewhere-else.example/xyz/mcp");
    assert!(
        !state.accepts(&issued),
        "a token carrying another resource must be refused on audience alone"
    );

    let mine = state.testing_issue_grant("http://127.0.0.1:9/abc/mcp");
    assert!(
        state.accepts(&mine),
        "a token carrying this resource is accepted, so the refusal above is \
         the audience check and not a store that refuses everything"
    );
}

#[tokio::test]
async fn an_approval_before_the_consent_page_was_opened_issues_nothing() {
    let (h, _root) = server().await;
    let client_id = registered_client(&h).await;
    let (_verifier, challenge) = pkce();
    let url = authorize_url(&h, &client_id, &challenge, "S256");

    // No GET first, so nothing ever asked the operator to approve.
    let res = oauth_client()
        .post(&url)
        .form(&[("consent_code", h.oauth.consent_code())])
        .send()
        .await
        .expect("request completes");
    assert_eq!(
        res.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the right code is not enough on a window that never opened"
    );

    // The same code, once the page has been served.
    open_consent(&url).await;
    let res = oauth_client()
        .post(&url)
        .form(&[("consent_code", h.oauth.consent_code())])
        .send()
        .await
        .expect("request completes");
    assert_eq!(
        res.status(),
        reqwest::StatusCode::FOUND,
        "opening the page is what starts the window"
    );
}

#[tokio::test]
async fn the_consent_window_closes_but_opening_the_page_again_re_arms_it() {
    let (h, _root) = server().await;
    let client_id = registered_client(&h).await;
    let (_verifier, challenge) = pkce();
    let url = authorize_url(&h, &client_id, &challenge, "S256");

    open_consent(&url).await;
    h.oauth
        .testing_age_consent(std::time::Duration::from_secs(6 * 60));
    let res = oauth_client()
        .post(&url)
        .form(&[("consent_code", h.oauth.consent_code())])
        .send()
        .await
        .expect("request completes");
    assert_eq!(
        res.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "an approval that came too late is refused"
    );

    // The operator loads the page again, which is the whole recovery: before
    // this, a window measured from process start could never be reopened and
    // the only way back was a restart that re-rolled every credential.
    open_consent(&url).await;
    let res = oauth_client()
        .post(&url)
        .form(&[("consent_code", h.oauth.consent_code())])
        .send()
        .await
        .expect("request completes");
    assert_eq!(
        res.status(),
        reqwest::StatusCode::FOUND,
        "a closed window can be reopened without restarting the server"
    );
}

#[tokio::test]
async fn a_burst_of_registrations_cannot_lock_out_the_next_client() {
    let (h, _root) = server().await;
    // Comfortably past the cap. An unauthenticated party filling the table
    // must not be able to stop the operator's own connector from registering.
    for attempt in 0..40 {
        let res = register(&h, REDIRECT).await;
        assert_eq!(
            res.status(),
            reqwest::StatusCode::CREATED,
            "registration {attempt} was refused"
        );
    }
}

#[tokio::test]
async fn a_redirect_hiding_a_remote_host_behind_userinfo_is_refused() {
    let (h, _root) = server().await;
    // Every real URL parser reads the host of these as the name after `@`,
    // while scanning for the first `:` or `/` reads them as loopback.
    for uri in [
        "http://127.0.0.1:80@evil.example/cb",
        "http://localhost@evil.example/cb",
        "http://127.0.0.1@evil.example:8080/cb",
    ] {
        let res = register(&h, uri).await;
        assert_eq!(
            res.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "a code must never be sent in the clear to {uri}"
        );
    }
}

#[tokio::test]
async fn an_access_token_stops_working_after_the_hour_it_advertises() {
    let (h, _root) = server().await;
    let (_client_id, access, _refresh) = authorized_tokens(&h).await;
    assert_eq!(
        post_list(&h, &access).await.status(),
        reqwest::StatusCode::OK
    );

    // Only the access timestamp moves, so the grant survives to be judged:
    // `expires_in` promised an hour, and honouring it is the point.
    h.oauth
        .testing_age_grants(std::time::Duration::from_secs(61 * 60));
    assert_eq!(
        post_list(&h, &access).await.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a token past its advertised life is not accepted"
    );
}

/// The head of a raw request to any path, carrying no credential: the OAuth
/// endpoints answer before one exists, which is exactly why their cost has to
/// be bounded. [`raw_request_head`] cannot be used, since it builds the MCP
/// path out of the handle's URL and attaches a token.
fn raw_head_for(h: &ServeHandle, path: &str, content_length: usize) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\nContent-Length: {content_length}\r\n\r\n",
        port = h.port
    )
}

#[tokio::test]
async fn a_stalled_body_on_an_unauthenticated_oauth_route_is_timed_out() {
    let root = tempfile::tempdir().unwrap();
    let ts = ServedToolset::new(vec![root.path().to_path_buf()], true)
        .await
        .unwrap();
    let (h, _join) = serve_with_options(
        Arc::new(ts),
        None,
        ServeOptions {
            body_read_timeout: Duration::from_millis(300),
            ..ServeOptions::default()
        },
    )
    .await
    .unwrap();

    // A complete head promising a hundred bytes, then almost none of them. The
    // OAuth routes are merged after the timeout layer is applied to the MCP
    // router, so without a layer of their own this connection is held for the
    // life of the process — and sixty-four of them take the whole server down,
    // the bearer path with it, since they share the connection budget.
    let mut stream = TcpStream::connect(h.addr).await.unwrap();
    stream
        .write_all(raw_head_for(&h, "/oauth/register", 100).as_bytes())
        .await
        .unwrap();
    stream.write_all(b"{\"redirect").await.unwrap();

    let head = read_head(&mut stream, Duration::from_secs(5))
        .await
        .expect("a stalled body on an OAuth route must be answered, not held");
    assert!(head.starts_with("HTTP/1.1 408"), "{head}");
    h.shutdown();
}

#[tokio::test]
async fn an_oauth_refusal_reaches_the_operator_observer() {
    let root = tempfile::tempdir().unwrap();
    let mut ts = ServedToolset::new(vec![root.path().to_path_buf()], true)
        .await
        .unwrap();
    let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    ts.set_observer(Arc::new(move |event: ServeEvent<'_>| {
        sink.lock().unwrap().push(match event {
            ServeEvent::Unauthorized => "unauthorized",
            ServeEvent::Refused { .. } => "refused",
        });
    }));
    let (h, _join) = serve(Arc::new(ts), None).await.unwrap();

    // An unregistered client id, refused before any credential exists. These
    // endpoints are reachable by anyone who can reach the port, so a scan of
    // them that left no trace would be invisible to the only person watching.
    let challenge = "a".repeat(43);
    let res = oauth_client()
        .get(authorize_url(
            &h,
            "not-a-registered-client",
            &challenge,
            "S256",
        ))
        .send()
        .await
        .expect("request completes");
    assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(
        events.lock().unwrap().contains(&"unauthorized"),
        "an OAuth refusal must reach the operator, like a bad bearer token does"
    );
    h.shutdown();
}

#[tokio::test]
async fn an_oversized_body_on_an_unauthenticated_route_is_refused() {
    let (h, _root) = server().await;
    let big = "a".repeat(crate::oauth::MAX_OAUTH_BODY_BYTES + 1024);
    let res = oauth_client()
        .post(format!("{}/oauth/register", origin(&h)))
        .header("content-type", "application/json")
        .body(format!("{{\"client_name\": \"{big}\"}}"))
        .send()
        .await
        .expect("request completes");
    assert_eq!(res.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
}
