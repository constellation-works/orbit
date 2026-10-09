//! Semantic roles, and the two renderings of a role-tagged value.
//!
//! A call site tags a value with what it *means* — either directly
//! ([`Role::Ok`] for a healthy-workspace line) or by naming the vocabulary it
//! came from ([`role_for`]) — and [`cell`] or [`text`] turns that into ANSI.
//! No call site names a color, and none asks whether color is permitted:
//! emission is gated once, by the sink, via
//! [`OutputSink::apply_color_policy`](crate::output::sink::OutputSink::apply_color_policy)
//! for the `colored` paths here and by `output::table` for the `comfy_table`
//! ones. See `docs/design/terminal-interface/specs/color-and-styling.md`
//! (ADR-0308).

use colored::Colorize;
use comfy_table::{Attribute, Cell, Color as TableColor};

/// The closed set of semantic roles a domain value can carry.
/// See `docs/design/terminal-interface/specs/color-and-styling.md` (ADR-0308).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Ok,
    Warn,
    Error,
    Active,
    Muted,
    Neutral,
}

/// The domain vocabularies this crate colors. Distinguishes values that read
/// the same across domains (e.g. `"active"` as a job state vs. `"archived"`
/// as a task status) so each gets the role its own vocabulary intends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Domain {
    TaskStatus,
    Priority,
    TaskType,
    JobState,
    DoctorStatus,
    AuditStatus,
}

/// The single value-to-role mapping covering every domain vocabulary in the
/// crate. Unmapped values resolve to `Role::Neutral` — never a panic, never
/// an arbitrary color. Adding a status is a one-line edit here.
pub fn role_for(domain: Domain, value: &str) -> Role {
    use Domain::*;
    use Role::*;
    match (domain, value) {
        (TaskStatus, "proposed") => Warn,
        (TaskStatus, "in-progress") => Active,
        (TaskStatus, "done") => Ok,
        (TaskStatus, "rejected" | "blocked") => Error,
        (TaskStatus, "archived") => Muted,
        // "review" and "backlog" have no dedicated role; see color.rs's
        // module-level completion notes for the resulting collapses.
        (Priority, "high") => Error,
        (Priority, "medium") => Warn,
        (Priority, "low") => Muted,

        (JobState, "success" | "active") => Ok,
        (JobState, "failed" | "error" | "disabled") => Error,
        (JobState, "running") => Active,
        (JobState, "pending") => Warn,

        (DoctorStatus, "ok") => Ok,
        (DoctorStatus, "warning") => Warn,
        (DoctorStatus, "ERROR" | "error") => Error,
        (DoctorStatus, "info") => Muted,

        (AuditStatus, "success") => Ok,
        (AuditStatus, "failure") => Error,
        // Denied is a policy outcome, not a fault: it needs attention without
        // reading as a broken command.
        (AuditStatus, "denied") => Warn,

        // TaskType carries no role today; every value renders neutral.
        _ => Neutral,
    }
}

impl Role {
    pub fn table_color(self) -> Option<TableColor> {
        match self {
            Role::Ok => Some(TableColor::Green),
            Role::Warn => Some(TableColor::Yellow),
            Role::Error => Some(TableColor::Red),
            Role::Active => Some(TableColor::Cyan),
            Role::Muted | Role::Neutral => None,
        }
    }

    pub fn line_color(self) -> Option<colored::Color> {
        match self {
            Role::Ok => Some(colored::Color::Green),
            Role::Warn => Some(colored::Color::Yellow),
            Role::Error => Some(colored::Color::Red),
            Role::Active => Some(colored::Color::Cyan),
            Role::Muted | Role::Neutral => None,
        }
    }

    pub fn is_dim(self) -> bool {
        matches!(self, Role::Muted)
    }
}

/// How a call site tags a value: with a [`Role`] outright when it knows the
/// meaning, or with the [`Domain`] the value came from when the mapping table
/// should decide.
///
/// Both spellings exist because both cases are real. `"Workspace healthy."` is
/// a sentence whose role is `Ok` and which belongs to no vocabulary; a task's
/// `status` belongs to `TaskStatus` and must get whatever role that table says,
/// so that adding a status stays a one-line edit in [`role_for`].
pub trait Tag {
    /// The role this tag assigns to `value`.
    fn role_of(self, value: &str) -> Role;
}

impl Tag for Role {
    fn role_of(self, _value: &str) -> Role {
        self
    }
}

impl Tag for Domain {
    fn role_of(self, value: &str) -> Role {
        role_for(self, value)
    }
}

/// A role-tagged value as a table cell.
///
/// The role's color is attached unconditionally; whether `comfy_table` emits it
/// is the sink's call, applied per render in `output::table`.
pub fn cell(value: &str, tag: impl Tag) -> Cell {
    cell_for(value, tag.role_of(value))
}

/// A role-tagged value as a styled line, for the `println!` paths that are not
/// tables. Emits nothing when the sink disallowed color.
pub fn text(value: &str, tag: impl Tag) -> String {
    string_for(value, tag.role_of(value))
}

fn cell_for(value: &str, role: Role) -> Cell {
    let cell = Cell::new(value);
    let cell = match role.table_color() {
        Some(color) => cell.fg(color),
        None => cell,
    };
    if role.is_dim() {
        cell.add_attribute(Attribute::Dim)
    } else {
        cell
    }
}

fn string_for(value: &str, role: Role) -> String {
    let styled = match role.line_color() {
        Some(color) => value.color(color),
        None => value.normal(),
    };
    let styled = if role.is_dim() {
        styled.dimmed()
    } else {
        styled
    };
    styled.to_string()
}

/// Structure, not severity: a field label or the primary identifier column
/// (spec §3). Never used to mean "worse" — that is what [`Role::Error`] is for.
pub fn bold(text: &str) -> String {
    text.bold().to_string()
}

/// De-emphasis for a value the reader may skip. Prefer
/// `text(value, Role::Muted)` when the dimness is carrying a *meaning*; this is
/// for incidental chrome such as a bracketed timestamp.
pub fn dimmed(text: &str) -> String {
    text.dimmed().to_string()
}
