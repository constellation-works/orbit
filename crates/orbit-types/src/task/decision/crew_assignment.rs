//! The crew change a `crew_assigned` or `crew_redrawn` history event records,
//! as [`TaskDecision::CrewAssignment`].
//!
//! The event's note stays the human account of the change. Readers act on
//! [`RecordedCrewAssignment::from_entry`], which reads the decision and
//! decodes the note only for an entry written before the decision existed.

use serde::{Deserialize, Serialize};

use super::TaskDecision;
use crate::task::{CrewSource, TaskHistoryEntry};

/// History event recording a crew assignment: at creation, by an operator,
/// by a provider-limit reassignment or by an admission write-back.
pub const CREW_ASSIGNED_EVENT: &str = "crew_assigned";
/// History event recording a crew redraw: a re-rate or reclassification, or
/// a usage-pressure redraw at admission.
pub const CREW_REDRAWN_EVENT: &str = "crew_redrawn";

/// The crew and provenance on one side of a recorded crew change. `None` is
/// no crew, or no recorded provenance.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrewAssignmentState {
    /// The task's crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// Why the task has that crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CrewSource>,
}

impl CrewAssignmentState {
    /// The crew `crew` with provenance `source`.
    pub fn new(crew: Option<String>, source: Option<CrewSource>) -> Self {
        Self { crew, source }
    }
}

/// A field of a [`RecordedCrewAssignment`] that an entry written before the
/// typed decision did not record.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CrewAssignmentField {
    /// The crew before the change.
    BeforeCrew,
    /// The provenance before the change.
    BeforeSource,
    /// The crew after the change.
    AfterCrew,
    /// The provenance after the change.
    AfterSource,
}

/// The crew and provenance a task had before and after one recorded crew
/// change. Every writer records all four fields under the task write lock
/// that changes the crew; only a legacy entry decoded from its note lists
/// fields it could not recover in `unknown`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordedCrewAssignment {
    /// The crew and provenance the change replaced.
    #[serde(default)]
    pub before: CrewAssignmentState,
    /// The crew and provenance the change left.
    #[serde(default)]
    pub after: CrewAssignmentState,
    /// Fields a legacy note did not record; empty on every typed decision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<CrewAssignmentField>,
}

impl RecordedCrewAssignment {
    /// A change from `before` to `after`, with every field known.
    pub fn new(before: CrewAssignmentState, after: CrewAssignmentState) -> Self {
        Self {
            before,
            after,
            unknown: Vec::new(),
        }
    }

    /// Whether `field` was recorded.
    pub fn is_known(&self, field: CrewAssignmentField) -> bool {
        !self.unknown.contains(&field)
    }

    /// The crew before the change, when recorded and set.
    pub fn known_before_crew(&self) -> Option<&str> {
        self.is_known(CrewAssignmentField::BeforeCrew)
            .then_some(self.before.crew.as_deref())
            .flatten()
    }

    /// The provenance before the change, when recorded and set.
    pub fn known_before_source(&self) -> Option<&CrewSource> {
        self.is_known(CrewAssignmentField::BeforeSource)
            .then_some(self.before.source.as_ref())
            .flatten()
    }

    /// The crew the change left, when recorded and set.
    pub fn known_after_crew(&self) -> Option<&str> {
        self.is_known(CrewAssignmentField::AfterCrew)
            .then_some(self.after.crew.as_deref())
            .flatten()
    }

    /// The provenance the change left, when recorded and set.
    pub fn known_after_source(&self) -> Option<&CrewSource> {
        self.is_known(CrewAssignmentField::AfterSource)
            .then_some(self.after.source.as_ref())
            .flatten()
    }

    /// The crew change a `crew_assigned` or `crew_redrawn` entry records:
    /// its typed decision, else, for an entry written before the decision
    /// existed, what its note can tell. `None` for any other event.
    pub fn from_entry(entry: &TaskHistoryEntry) -> Option<Self> {
        if let Some(TaskDecision::CrewAssignment(recorded)) = &entry.decision {
            return Some(recorded.clone());
        }
        matches!(
            entry.event.as_str(),
            CREW_ASSIGNED_EVENT | CREW_REDRAWN_EVENT
        )
        .then(|| legacy_crew_assignment(entry.note.as_deref()))
    }
}

/// The one reader of crew-assignment notes written before the typed
/// decision. It keeps the attribution those notes always had:
///
/// - the crew after the change is the note's last backticked name, and a
///   placeholder such as `(none)` is no crew — even where a note names
///   another crew after the assigned one;
/// - the provenance after the change is recovered only from the shapes
///   ``assigned crew `<crew>` from <source>``,
///   ``… to `<crew>` via explicit name`` or ``… via pool draw (<source>)``
///   and ``crew redrawn from <source> (`<from>`) to <source> (`<to>`) …``,
///   and only when the crew those shapes name is that last crew;
/// - the crew and provenance before the change are recovered only from the
///   redraw shape.
///
/// Everything else is unknown.
fn legacy_crew_assignment(note: Option<&str>) -> RecordedCrewAssignment {
    use CrewAssignmentField::{AfterCrew, AfterSource, BeforeCrew, BeforeSource};
    let mut recorded = RecordedCrewAssignment {
        unknown: vec![BeforeCrew, BeforeSource, AfterCrew, AfterSource],
        ..Default::default()
    };
    let Some(note) = note else {
        return recorded;
    };
    let last = note.split('`').skip(1).step_by(2).last();
    let after_crew = last
        .filter(|last| !last.is_empty() && !last.starts_with(['(', '<']))
        .map(str::to_string);
    let mut after_source = None;
    let mut before = None;
    if let Some((crew, source)) = note
        .strip_prefix("assigned crew `")
        .and_then(|rest| rest.split_once("` from "))
    {
        after_source = Some((crew, Some(CrewSource::parse(source))));
    } else if let Some((head, via)) = note.split_once("` via ")
        && let Some((_, crew)) = head.rsplit_once(" to `")
    {
        let source = if via == "explicit name" {
            Some(CrewSource::Explicit)
        } else {
            via.strip_prefix("pool draw (")
                .and_then(|source| source.strip_suffix(')'))
                .map(CrewSource::parse)
        };
        // A `via` the note does not model records no provenance.
        if source.is_some() {
            after_source = Some((crew, source));
        }
    } else if let Some((from_source, from, to_source, to)) = legacy_redraw(note) {
        before = Some((from, from_source));
        after_source = Some((to, Some(CrewSource::parse(to_source))));
    }
    let mut known = vec![AfterCrew];
    if let Some((from, from_source)) = before {
        known.extend([BeforeCrew, BeforeSource]);
        recorded.before = CrewAssignmentState::new(
            (!from.is_empty()).then(|| from.to_string()),
            Some(CrewSource::parse(from_source)),
        );
    }
    if let Some((crew, source)) = after_source
        && last == Some(crew)
    {
        known.push(AfterSource);
        recorded.after.source = source;
    }
    recorded.after.crew = after_crew;
    recorded.unknown.retain(|field| !known.contains(field));
    recorded
}

/// `(from source, from, to source, to)` of a legacy redraw note:
/// ``crew redrawn from <source> (`<from>`) to <source> (`<to>`) …``.
fn legacy_redraw(note: &str) -> Option<(&str, &str, &str, &str)> {
    let rest = note.strip_prefix("crew redrawn from ")?;
    let (from_source, rest) = rest.split_once(" (`")?;
    let (from, rest) = rest.split_once("`) to ")?;
    let (to_source, rest) = rest.split_once(" (`")?;
    let (to, _) = rest.split_once("`)")?;
    Some((from_source, from, to_source, to))
}
