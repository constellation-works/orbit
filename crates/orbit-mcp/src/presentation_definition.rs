//! Definitions for adapter-owned read-only presentation entrypoints.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresentationKind {
    Open,
    Inspect,
}

pub(crate) struct PresentationDefinition {
    pub name: &'static str,
    pub alias: &'static str,
    pub title: &'static str,
    pub entrypoint: &'static str,
    pub kind: PresentationKind,
}

pub(crate) const PRESENTATION_DEFINITIONS: &[PresentationDefinition] = &[
    PresentationDefinition {
        name: "orbit_ui_open",
        alias: "orbit.ui.open",
        title: "Open Orbit control center",
        entrypoint: "global",
        kind: PresentationKind::Open,
    },
    PresentationDefinition {
        name: "orbit_ui_inspect",
        alias: "orbit.ui.inspect",
        title: "Inspect Orbit task or run",
        entrypoint: "thread",
        kind: PresentationKind::Inspect,
    },
];

pub(crate) fn presentation_definition(name: &str) -> Option<&'static PresentationDefinition> {
    PRESENTATION_DEFINITIONS
        .iter()
        .find(|definition| name == definition.name || name == definition.alias)
}
