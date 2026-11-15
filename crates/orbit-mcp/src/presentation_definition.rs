//! The adapter-owned read-only presentation tool and its actions.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresentationKind {
    Open,
    Inspect,
}

/// One action of the presentation tool and the host entrypoint it serves.
pub(crate) struct PresentationAction {
    pub action: &'static str,
    pub entrypoint: &'static str,
    pub kind: PresentationKind,
}

/// The presentation tool's advertised and canonical names.
pub(crate) const PRESENTATION_TOOL_NAME: &str = "orbit_ui";
pub(crate) const PRESENTATION_TOOL_ALIAS: &str = "orbit.ui";
pub(crate) const PRESENTATION_TOOL_TITLE: &str = "Orbit control center";

pub(crate) const PRESENTATION_ACTIONS: &[PresentationAction] = &[
    PresentationAction {
        action: "open",
        entrypoint: "global",
        kind: PresentationKind::Open,
    },
    PresentationAction {
        action: "inspect",
        entrypoint: "thread",
        kind: PresentationKind::Inspect,
    },
];

pub(crate) fn is_presentation_tool(name: &str) -> bool {
    name == PRESENTATION_TOOL_NAME || name == PRESENTATION_TOOL_ALIAS
}

pub(crate) fn presentation_action(action: &str) -> Option<&'static PresentationAction> {
    PRESENTATION_ACTIONS
        .iter()
        .find(|definition| definition.action == action)
}
