//! `rmcp::ServerHandler` over the served toolset.
//!
//! # Content bridging
//!
//! Turbo's `xai_tool_runtime::ContentBlock` (3 variants) and
//! `rmcp::model::ContentBlock` (5 variants, `#[non_exhaustive]`) are
//! structurally incompatible and no conversion exists in-tree. The bridge here
//! is deliberately narrow: the served tools are all text-producing, and
//! `ToolRunResult::prompt_text` is the model-facing rendering the rest of Turbo
//! already uses. Adding an image-producing tool to the served surface means
//! extending this, not widening it by accident.
//!
//! # Refusals are tool errors, not protocol errors
//!
//! A boundary refusal comes back as `CallToolResult { is_error: true }` carrying
//! the guard's opaque message. That is deliberate: a protocol error would look
//! like a broken server and invite a retry loop, whereas a tool error is
//! something the model can read and adapt to. The message never says *why* it
//! was refused — see the denial-opacity rule in `guard.rs`.

use std::sync::Arc;

use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};
use serde_json::Value;

use crate::annotate;
use crate::toolset::{CallFailure, ServedToolset};

#[derive(Clone)]
pub struct TurboMcpHandler {
    toolset: Arc<ServedToolset>,
}

impl TurboMcpHandler {
    pub fn new(toolset: Arc<ServedToolset>) -> Self {
        Self { toolset }
    }

    /// The advertised tool list. Shared by `list_tools` and the tests so both
    /// exercise the same construction.
    pub fn advertised_tools(&self) -> Vec<Tool> {
        self.toolset
            .list()
            .iter()
            .map(|t| {
                let schema = match &t.schema {
                    Value::Object(map) => Arc::new(map.clone()),
                    // A tool whose schema is not an object takes no arguments;
                    // advertise an empty object rather than dropping the tool.
                    _ => Arc::new(serde_json::Map::new()),
                };
                // `Tool` is #[non_exhaustive]; go through the constructor.
                Tool::new(t.name.clone(), t.description.clone(), schema)
                    .with_annotations(annotate::for_kind(t.kind))
            })
            .collect()
    }

    /// Dispatch used by both `call_tool` and the tests.
    pub async fn dispatch(&self, name: &str, args: Value) -> CallToolResult {
        match self.toolset.call(name, args).await {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            // Tool-level errors, not protocol errors: rmcp's own guidance is
            // that a protocol error is rendered opaquely and the caller never
            // sees the message, which would leave the model with no signal at
            // all about why its request did not run.
            Err(CallFailure::Refused(denial)) => {
                CallToolResult::error(vec![ContentBlock::text(denial.to_string())])
            }
            Err(CallFailure::Failed(msg)) => CallToolResult::error(vec![ContentBlock::text(msg)]),
        }
    }
}

impl ServerHandler for TurboMcpHandler {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        let mut impl_info = Implementation::from_build_env();
        impl_info.name = "Turbo Build".to_string();
        impl_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = impl_info;
        // Naming the roots is the difference between a first session that
        // works and a string of identical opaque refusals: every path must be
        // absolute and start with one of them.
        let roots: Vec<String> = self
            .toolset
            .printable_roots()
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect();
        info.instructions = Some(format!(
            "Turbo Build's file tools, bounded to the operator's approved roots. \
             Paths must be absolute and must start with one of these roots: {}. \
             Requests outside them are refused.",
            roots.join(", ")
        ));
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.advertised_tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        Ok(self.dispatch(&request.name, args).await)
    }
}
