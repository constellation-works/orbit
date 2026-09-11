//! Borderless list rendering: a header row followed by exactly one line per record.
//!
//! No box glyphs, two-space gutters, and cells truncated to width rather than
//! wrapped, so `grep`/`cut`/`awk` see whole records. The contract is
//! `docs/design/terminal-interface/specs/table-rendering.md` (ADR-0307).
//!
//! [`Table::add_row`] is the only way to add a row and it caps every row at one
//! line; `comfy_table`'s unbounded row constructor is not reachable from outside
//! this module.
//!
//! Width and styling both come from the `OutputSink` the renderer passes in
//! (ADR-0306): a zero-width sink truncates nothing, and a sink that disallows
//! color renders the same bytes a file redirect would.

use comfy_table::{
    Attribute, Cell, CellAlignment, ColumnConstraint, ContentArrangement, Row, Table as Grid,
    Width, presets,
};

use crate::output::sink::{OutputMode, OutputSink};

/// Spaces between two rendered columns.
const GUTTER: usize = 2;
/// Narrowest a flexible column may be squeezed before it is dropped instead.
const FLEXIBLE_FLOOR: usize = 8;
/// Marks a value that did not fit its column.
const ELLIPSIS: &str = "…";

/// Whether a column may be squeezed when the result set is wider than the sink.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sizing {
    /// IDs, statuses, timestamps: rendered whole or not at all.
    Fixed,
    /// Prose and names: shrink first, down to [`FLEXIBLE_FLOOR`].
    Flexible,
}

/// Which end of an overlong value is sacrificed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Overflow {
    /// Prose: the beginning identifies the value.
    Tail,
    /// Paths, refs, hashes: both ends identify the value.
    Middle,
}

/// One column of a list view: its header plus the policy for width, alignment,
/// and overflow that the renderer applies to every cell beneath it.
pub struct Column {
    header: String,
    align: CellAlignment,
    sizing: Sizing,
    overflow: Overflow,
    keep_when_uniform: bool,
}

impl Column {
    /// A name or prose column: left aligned, shrinks under width pressure, and
    /// loses its tail when it overflows.
    ///
    /// Headers are written uppercase by the caller, so that a unit suffix
    /// (`DURATION (ms)`) keeps the casing its unit is defined with.
    pub fn new(header: &str) -> Self {
        Self {
            header: header.to_string(),
            align: CellAlignment::Left,
            sizing: Sizing::Flexible,
            overflow: Overflow::Tail,
            keep_when_uniform: false,
        }
    }

    /// An identifier, status, or timestamp: never squeezed, so the value a
    /// caller would copy out of the list is always whole.
    #[must_use]
    pub fn fixed(mut self) -> Self {
        self.sizing = Sizing::Fixed;
        self
    }

    /// A count or duration: right aligned so magnitudes compare vertically, and
    /// never squeezed. Name the unit in the header (`DURATION (ms)`) rather than
    /// repeating it in every cell.
    #[must_use]
    pub fn number(mut self) -> Self {
        self.align = CellAlignment::Right;
        self.sizing = Sizing::Fixed;
        self
    }

    /// A path, ref, or hash, whose tail identifies it: overflow eats the middle.
    #[must_use]
    pub fn path(mut self) -> Self {
        self.overflow = Overflow::Middle;
        self
    }

    /// Keep this column even when every row carries the same value. Pass the
    /// filter's presence: a user who asked for `--status done` expects `STATUS`
    /// to stay on screen even though it now carries no information.
    #[must_use]
    pub fn filtered(mut self, filtered: bool) -> Self {
        self.keep_when_uniform = filtered;
        self
    }
}

/// A list view under construction. Rows are buffered so that column widths and
/// uniform-column suppression can be computed from the whole result set.
pub struct Table {
    columns: Vec<Column>,
    rows: Vec<Vec<Cell>>,
    empty_message: String,
    suppress_uniform: bool,
    trailing_notices: Vec<String>,
}

/// Build a table whose columns are all plain left-aligned text. Commands with
/// numeric, duration, identifier, or path columns should describe them with
/// [`Column`] and [`Table::new`] instead.
pub fn build_table(headers: &[&str]) -> Table {
    Table::new(headers.iter().map(|header| Column::new(header)).collect())
}

impl Table {
    /// Build a table from explicit column policies.
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            empty_message: "no results".to_string(),
            suppress_uniform: true,
            trailing_notices: Vec::new(),
        }
    }

    /// The line printed to stderr — leaving stdout empty — when there are no
    /// records. Name what was searched, and why it may be empty where that is
    /// cheap to say.
    #[must_use]
    pub fn empty_message(mut self, message: impl Into<String>) -> Self {
        self.empty_message = message.into();
        self
    }

    /// Render every column even when its value repeats. For fixed-shape views
    /// (a status readout, a parameter schema) rather than result sets, where a
    /// missing column reads as a missing field.
    #[must_use]
    pub fn keep_all_columns(mut self) -> Self {
        self.suppress_uniform = false;
        self
    }

    /// Add a trailing notice — a truncation count, say — for the renderer to
    /// print to stderr. Not printed by [`Table::emit`] itself: `output::render`
    /// reads it via [`Table::trailing_notices`] before mode dispatch, so it
    /// reaches a `json`/`ndjson` caller too, not only the human table view
    /// (ORB-12203).
    #[must_use]
    pub fn trailing_notice(mut self, notice: impl Into<String>) -> Self {
        self.trailing_notices.push(notice.into());
        self
    }

    /// Notices for the renderer to print to stderr in every mode, ahead of
    /// this table's own render (which may add further, rendering-specific
    /// notices such as dropped columns).
    pub(crate) fn trailing_notices(&self) -> &[String] {
        &self.trailing_notices
    }

    /// Add one record. The row occupies exactly one line however long its cells
    /// are; there is no unbounded variant.
    pub fn add_row<T: Into<Cell>>(&mut self, cells: Vec<T>) {
        self.rows.push(cells.into_iter().map(Into::into).collect());
    }

    /// Write the list to stdout in the sink's mode, or the empty-state line to
    /// stderr when there are no records. Notices about dropped columns go to
    /// stderr so that they never land in a consumer's record stream. This
    /// table's own trailing notices (a truncation count, say) are not printed
    /// here — `output::render` prints those in every mode, human or not, so
    /// this only handles rendering-specific notices discovered at this width.
    ///
    /// Called only by `output::render`; a command hands its table back inside a
    /// payload rather than emitting one itself.
    pub(crate) fn emit(&self, sink: &OutputSink) {
        if self.rows.is_empty() {
            eprintln!("{}", self.empty_message);
            return;
        }
        if sink.mode() == OutputMode::Plain {
            println!("{}", self.render_plain(sink));
            return;
        }
        let rendered = self.render_at(
            sink.truncate_width(),
            sink.color_allowed(),
            sink.suppress_uniform_columns(),
        );
        for notice in &rendered.notices {
            eprintln!("{notice}");
        }
        println!("{}", rendered.body);
    }

    /// The plain form: the same visible columns and the same cell values as
    /// `table`, with the header suppressed, no borders or ANSI, no truncation,
    /// and a single tab between fields — what `cut -f` expects (spec §2).
    ///
    /// Truncation is disabled by construction rather than by passing a width:
    /// a plain sink has no width, and silently shortening a value on its way
    /// into a pipe is the failure this form exists to avoid.
    pub(crate) fn render_plain(&self, sink: &OutputSink) -> String {
        let visible = self.visible_columns(sink.suppress_uniform_columns());
        self.rows
            .iter()
            .map(|row| {
                visible
                    .iter()
                    .map(|index| cell_at(row, *index).content())
                    .collect::<Vec<_>>()
                    .join("\t")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Render at an explicit width. `sink_width` of `None` means the sink has no
    /// width and nothing is truncated; `styled` carries whether the sink accepts
    /// ANSI styling, and `suppress_uniform` whether a column whose value repeats
    /// may be dropped.
    ///
    /// All three come from the sink in [`Table::emit`]. Tests pass them directly
    /// so geometry and styling are pinned rather than inherited from whatever
    /// terminal ran `cargo test`.
    pub(crate) fn render_at(
        &self,
        sink_width: Option<usize>,
        styled: bool,
        suppress_uniform: bool,
    ) -> Rendered {
        let visible = self.visible_columns(suppress_uniform);
        let natural = self.natural_widths(&visible);
        let (layout, dropped) = self.resolve_widths(&visible, &natural, sink_width);

        let mut grid = Grid::new();
        grid.load_preset(presets::NOTHING);
        grid.set_content_arrangement(ContentArrangement::Disabled);
        grid.set_truncation_indicator(ELLIPSIS);
        // Told, never asked. Left to itself `comfy_table` probes stdout and
        // ignores `NO_COLOR`, which is exactly the disagreement with `colored`
        // that let a redirect capture escape sequences [ADR-0308].
        if styled {
            grid.enforce_styling();
        } else {
            grid.force_no_tty();
        }
        grid.set_header(layout.iter().map(|(index, _)| {
            Cell::new(&self.columns[*index].header).add_attribute(Attribute::Dim)
        }));
        for row in &self.rows {
            let cells = layout
                .iter()
                .map(|(index, width)| self.cell(row, *index, *width))
                .collect::<Vec<_>>();
            let mut single_line = Row::from(cells);
            single_line.max_height(1);
            grid.add_row(single_line);
        }

        for (position, (index, width)) in layout.iter().enumerate() {
            let gutter = if position + 1 == layout.len() {
                0
            } else {
                GUTTER
            };
            let Some(column) = grid.column_mut(position) else {
                continue;
            };
            column.set_padding((0, clamp_u16(gutter)));
            column.set_cell_alignment(self.columns[*index].align);
            column.set_constraint(ColumnConstraint::Absolute(Width::Fixed(clamp_u16(
                width + gutter,
            ))));
        }

        let body = grid
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let notices = if dropped.is_empty() {
            Vec::new()
        } else {
            vec![format!(
                "columns hidden to fit the terminal: {}",
                dropped.join(", ")
            )]
        };
        Rendered { body, notices }
    }

    /// Columns that survive uniform-value suppression, in order.
    ///
    /// `sink_allows` is the sink's answer (`--format table` asked for the full
    /// shape); `self.suppress_uniform` is the table's own (a fixed-shape view
    /// is not a result set). Either one is enough to keep every column.
    fn visible_columns(&self, sink_allows: bool) -> Vec<usize> {
        let all = (0..self.columns.len()).collect::<Vec<_>>();
        // A single record is no evidence that a column is uninformative.
        if !sink_allows || !self.suppress_uniform || self.rows.len() < 2 {
            return all;
        }
        let kept = all
            .iter()
            .copied()
            .filter(|index| self.columns[*index].keep_when_uniform || !self.is_uniform(*index))
            .collect::<Vec<_>>();
        // A result set whose every column repeats still has to render as rows.
        if kept.is_empty() { all } else { kept }
    }

    fn is_uniform(&self, index: usize) -> bool {
        let mut values = self
            .rows
            .iter()
            .map(|row| row.get(index).map(Cell::content).unwrap_or_default());
        let Some(first) = values.next() else {
            return false;
        };
        values.all(|value| value == first)
    }

    /// Maximum display width of each visible column's header and cells, measured
    /// in grapheme clusters by the same code that lays the grid out.
    fn natural_widths(&self, visible: &[usize]) -> Vec<usize> {
        let mut probe = Grid::new();
        probe.load_preset(presets::NOTHING);
        probe.set_content_arrangement(ContentArrangement::Disabled);
        probe.set_header(
            visible
                .iter()
                .map(|index| Cell::new(&self.columns[*index].header)),
        );
        for row in &self.rows {
            probe.add_row(
                visible
                    .iter()
                    .map(|index| cell_at(row, *index))
                    .collect::<Vec<_>>(),
            );
        }
        probe
            .column_max_content_widths()
            .into_iter()
            .map(usize::from)
            .collect()
    }

    /// Fit the visible columns into `sink_width`: shrink flexible columns widest
    /// first down to the floor, then drop them from the right. Fixed columns
    /// never move, so the same result set at two widths differs only in
    /// truncation and column presence.
    fn resolve_widths(
        &self,
        visible: &[usize],
        natural: &[usize],
        sink_width: Option<usize>,
    ) -> (Vec<(usize, usize)>, Vec<String>) {
        let mut layout = visible
            .iter()
            .copied()
            .zip(natural.iter().copied())
            .collect::<Vec<_>>();
        let mut dropped = Vec::new();
        let Some(limit) = sink_width else {
            return (layout, dropped);
        };

        while total_width(&layout) > limit {
            let widest = layout
                .iter_mut()
                .filter(|(index, width)| {
                    self.columns[*index].sizing == Sizing::Flexible && *width > FLEXIBLE_FLOOR
                })
                .max_by_key(|(_, width)| *width);
            let Some(column) = widest else {
                break;
            };
            column.1 -= 1;
        }

        while total_width(&layout) > limit {
            let Some(position) = layout
                .iter()
                .rposition(|(index, _)| self.columns[*index].sizing == Sizing::Flexible)
            else {
                break;
            };
            let (index, _) = layout.remove(position);
            dropped.push(self.columns[index].header.clone());
        }

        (layout, dropped)
    }

    /// The cell as the grid should receive it. Tail truncation is left to
    /// `comfy-table`, which is width-accurate and preserves the cell's styling;
    /// middle truncation has to be applied here because the library has no such
    /// mode.
    fn cell(&self, row: &[Cell], index: usize, width: usize) -> Cell {
        let cell = cell_at(row, index);
        if self.columns[index].overflow != Overflow::Middle {
            return cell;
        }
        let content = cell.content();
        if content.chars().count() <= width {
            return cell;
        }
        Cell::new(truncate_middle(&content, width))
    }
}

/// A rendered list plus the notices that belong on stderr.
pub(crate) struct Rendered {
    pub(crate) body: String,
    pub(crate) notices: Vec<String>,
}

fn cell_at(row: &[Cell], index: usize) -> Cell {
    row.get(index).cloned().unwrap_or_else(|| Cell::new(""))
}

fn total_width(layout: &[(usize, usize)]) -> usize {
    layout.iter().map(|(_, width)| width).sum::<usize>() + GUTTER * layout.len().saturating_sub(1)
}

fn clamp_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// Keep the head and the identifying tail: `crates/orbit-cli/…/table.rs`.
///
/// Width is counted in `char`s rather than display columns because the values
/// routed here — paths, refs, hashes — are ASCII. A wide-character value would
/// be measured short, and the row's `max_height(1)` cap is what still
/// guarantees it renders as one line.
fn truncate_middle(value: &str, width: usize) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() <= width {
        return value.to_string();
    }
    if width <= 1 {
        return ELLIPSIS.to_string();
    }
    let kept = width - 1;
    let head = kept.div_ceil(2);
    let tail = kept - head;
    let mut truncated = chars[..head].iter().collect::<String>();
    truncated.push_str(ELLIPSIS);
    truncated.extend(&chars[chars.len() - tail..]);
    truncated
}
