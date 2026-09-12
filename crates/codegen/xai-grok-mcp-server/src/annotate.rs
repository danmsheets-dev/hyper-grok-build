//! `ToolKind` -> MCP tool annotations.
//!
//! OpenAI's app-submission guidelines require honest annotations, and a client
//! uses them to decide what needs confirmation. Both hints are **derived** from
//! `ToolKind` rather than hand-maintained, so a tool added to the served surface
//! cannot ship mislabelled.

use rmcp::model::ToolAnnotations;
use xai_grok_tools::types::tool::ToolKind;

/// Annotations for one tool kind.
///
/// `read_only_hint` follows `ToolKind::is_read_only()`, which asks whether the
/// tool modifies local state. `open_world_hint` is a separate axis: `WebFetch`
/// and `WebSearch` are read-only *and* open-world, so they need both flags set
/// rather than one standing in for the other.
pub fn for_kind(kind: ToolKind) -> ToolAnnotations {
    let read_only = kind.is_read_only();
    let open_world = matches!(
        kind,
        ToolKind::WebFetch | ToolKind::WebSearch | ToolKind::DeployApp | ToolKind::Meeting
    );
    // `ToolAnnotations` is #[non_exhaustive]; build it through the API rather
    // than a struct literal so an added hint does not break this crate.
    ToolAnnotations::from_raw(
        None,
        Some(read_only),
        // Meaningful only when read_only_hint is false, per the MCP schema.
        Some(!read_only),
        None,
        Some(open_world),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_kinds_are_annotated_read_only() {
        let a = for_kind(ToolKind::Read);
        assert_eq!(a.read_only_hint, Some(true));
        assert_eq!(a.destructive_hint, Some(false));
    }

    #[test]
    fn mutating_kinds_are_annotated_destructive() {
        for kind in [ToolKind::Edit, ToolKind::Delete, ToolKind::Execute] {
            let a = for_kind(kind);
            assert_eq!(a.read_only_hint, Some(false), "{kind:?}");
            assert_eq!(a.destructive_hint, Some(true), "{kind:?}");
        }
    }

    #[test]
    fn network_kinds_are_open_world_even_when_read_only() {
        // The two axes are independent; a read-only tool can still reach out.
        let a = for_kind(ToolKind::WebFetch);
        assert_eq!(a.read_only_hint, Some(true));
        assert_eq!(a.open_world_hint, Some(true));
    }

    #[test]
    fn local_kinds_are_closed_world() {
        assert_eq!(for_kind(ToolKind::Read).open_world_hint, Some(false));
        assert_eq!(for_kind(ToolKind::Search).open_world_hint, Some(false));
    }
}
