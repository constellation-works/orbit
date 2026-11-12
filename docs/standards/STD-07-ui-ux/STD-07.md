---
id: STD-07-ui-ux
title: UI and UX — consistent spacing, shared edges, one type scale, minimal words, show each thing once, honest charts, stable layouts, visible state
summary: Normative rules for every operator-facing screen (dashboards, web apps, sites, panels): sibling spacing and shared edges, layouts that never shift, one type scale, contrast and one meaning per colour, minimal words with helper text behind an info icon, tooltips only where they add something, no redundant or inert elements, answer-first data with honest charts, visible live state, responsive use of width, and turning each owner correction into a rule; follow it in any project that renders a UI.
status: draft
tags: [standard, ui, ux, design]
version: 3
created: 2026-11-09
updated: 2026-11-11
last_validated: 2026-11-11
related: [STD-01-cli-surface, STD-04-testing-and-verification]
---

# UI and UX

These rules cover how an operator-facing screen is laid out, worded and kept calm. They
are distilled from the corrections the owner made repeatedly between September and November
2026, across the Orbit dashboard and website, orbit-office and constellation-site. Each rule
names a correction that came back at least once after it was first fixed. The aim is that a
reviewer can catch it before the owner sees it.

The house reference is a tool that says one thing per region and nothing it doesn't need
to: "simple purpose, simple design" (ORB-21727). When a rule and a design token disagree,
the project's design tokens give the values, and these rules give the relationships those
values must keep.

This standard is a directory. This file holds the binding text. Beside it,
[`why.md`](why.md) gives the corrections behind each rule, [`checks.md`](checks.md) the
gate that enforces it, [`exemplars.md`](exemplars.md) worked examples in Orbit, and
[`CHANGELOG.md`](CHANGELOG.md) what changed in each version.

## Rules

### Layout and alignment

- **R1.** Sibling elements of the same kind MUST share padding and gaps: rows, cards,
  buttons, headings and the space after them. No element carries padding beyond what its
  content needs, and removing or adding one sibling MUST NOT leave an odd gap. A container's
  padding is symmetric: top equals bottom, and left equals right.
- **R2.** Panels placed side by side MUST share their top and bottom edges and inner insets.
  A stated split (such as 50:50, or "the same width as" a named panel) is exact. No panel
  leaves a ragged end or an empty gutter beside its neighbour.
- **R3.** Rows in a repeated list or grid MUST share one column grid, so that chips, cells,
  badges and actions line up across rows and across groups.
- **R4.** Content MUST stay inside its container: text wraps instead of overflowing, no
  element draws over another, and the page never scrolls sideways at any supported width
  down to 375px.
- **R5.** The layout MUST NOT shift when content appears, changes or disappears. That covers
  text that loads in, a status that changes, and a transient message. Neighbouring regions
  never move or reflow, and creating or editing an item opens in a modal or popover, not by
  pushing the page down.
- **R6.** Peer controls MUST be the same size and the same kind of input. A number beside a
  stepper is a stepper too, and selects are no taller than the buttons beside them.
- **R7.** Close, Back and panel-level toggles MUST sit at the top right of their panel,
  beside the related header actions.
- **R27.** A row of tiles, cards or stats MUST NOT leave an orphan: a lone item wrapped onto
  a row of its own at any supported width. Choose a count that fills every row, use a grid
  whose columns divide the count evenly at each breakpoint, or drop the weakest item. It is
  better to remove one than to leave one out on its own.
- **R28.** Blocks stacked in one column MUST share that column's width. A notice, a
  paragraph or a header block narrower than the table or chart below it MUST NOT leave an
  empty right-hand column. Widen it to the column, put a related block beside it (such as the
  legend or a summary), or, for helper prose, move it behind an info icon (R13).

- **R30.** One kind of element MUST look the same wherever it appears. This covers a
  progress or stage strip, a pill, a table, a stat tile and a panel header: each shares
  height, weight, label size, margins and current-step treatment across views. Build it as
  one shared component, never restyle it per view. Its labels never wrap. A label longer than
  its slot is truncated with an ellipsis and shows the full text on hover and focus. A
  duration goes beside the label or in that hover text.

- **R31.** A container MUST NOT sit inside another container unless the inner one marks a
  real distinction: a different kind of content, a selectable item, or a state. A
  disclosure's body, a section's prose or a lone block gets no second border or background
  inside its card.

- **R33.** Secondary metadata about an item MUST go on its own line under the item's
  primary label, aligned with that label, in a muted style. Aliases, crews, IDs and counts
  do not run inline after the label. A badge that qualifies the label itself (such as
  "single tier") stays beside it. An item with no metadata has no empty second line.

### Typography

- **R8.** A view MUST use one type scale: elements of the same kind share size, weight and
  family. Weight shows hierarchy only, and figures and labels are not bolded for emphasis.
  Monospace is used only for IDs, metrics, timestamps, code and logs; prose, hints and
  messages are sans-serif.
- **R9.** A label MUST sit on the same row as its value, not on a row of its own above an
  indented value. In label/value lists, a label MUST NOT wrap one word per line, and a value
  MUST NOT draw over its label.
- **R35.** Text inside a chart MUST use the page's type scale. Labels, ticks and axis titles
  render at the same sizes as the equivalent text around the chart at every width. A chart
  never scales its text with its own width (for example an SVG with a fixed `viewBox`
  stretched to fill its panel). The plot reflows instead.

### Colour and theme

- **R10.** Adjacent sections and idle elements MUST be told apart by a visible boundary or a
  background step. An idle or empty control is never invisible against its background.
  Text meets 4.5:1 contrast, except disabled text.
- **R11.** A colour MUST mean one thing everywhere. A status has one colour on every surface,
  text colours match across pages, and colours come from shared tokens, never chosen per
  view.
- **R12.** Each product MUST declare its default theme (light or dark) in its design docs and
  apply it consistently across its views. A default that is applied MUST NOT be stored as if
  the user had chosen it.

### Words

- **R13.** Screens MUST use minimal words. There are no taglines, summary sentences or helper
  paragraphs in the working view. Explanation that is needed goes behind an info icon that
  works on hover, keyboard focus and tap.
- **R14.** A status line MUST fit on one line. Longer text, such as a reason, description or
  acceptance criteria, is collapsed by default behind a "show full" control. The exception
  is an error the operator must act on: it is never truncated.
- **R15.** A label MUST be short and MUST NOT repeat context the screen already shows. For
  example, it says "View run", not "View run on host-1" next to the host's name.
- **R16.** A screen MUST show each piece of information once, and only what the operator
  acts on. Duplicate controls, nav entries for panels already on screen, redundant pages,
  noise badges and filler values ("no estimate", "+0/-0") are removed. Disabled features
  are hidden, not shown greyed out in nav.
- **R32.** A hover tooltip MUST add something the screen does not already show: the full
  text of a truncated label, a definition, or detail deliberately moved off the screen. It
  never restates a visible label or a state that a colour, icon or position already shows
  (for example "Implement · done" on a green step). The full text stays in the accessible
  name either way. A tooltip's bubble fits its text up to a maximum width, with no empty
  area beside a short text.

### Data display

- **R17.** A view MUST put the answer first and collapse the detail. A table longer than
  about seven rows collapses or offers "show all", while a short list is never paginated.
  The cap is for a list that shares its region with other content. A list that is the whole
  content of its own tab or panel shows in full. A
  page that keeps growing is split into tabs. Small sets are not split into tabs: a handful
  of items in two states (such as open and recently closed) stay in one list, with the
  active group first and the rest under a heading or a collapsed disclosure. That way an
  empty tab never hides the items that exist.
- **R18.** Where a shape or trend matters, a view SHOULD show a chart, bar or sparkline
  instead of raw numbers or JSON. Markdown is always rendered, never shown raw.
- **R19.** A chart MUST agree with the data it summarises. It agrees with any ranking shown
  beside it, puts every axis in the same "better" direction (or groups axes by direction),
  and uses a cumulative or rolling view for spiky counts.
- **R29.** A summary figure MUST sit with the detail it summarises, in that panel's header
  or as its first line. It never gets a full-width panel of its own. A panel holding a single
  number and its caption is folded into its neighbour.
- **R34.** A chart MUST label each mark with its name only. Its figures (score, cost, count)
  go in a tooltip on hover and keyboard focus, and in the chart's accessible description.
  Labels never overlap each other, a mark or a reference line. A label with no room gets a
  leader line or is shown in the tooltip only.
- **R36.** Rows that share a rank, band or group MUST be ordered by the figure the view
  ranks on, so that the column reads in order. Another key (such as cost, then name) only
  breaks exact ties. Ordering inside a group by a value the view does not rank on reads as
  a mistake.

### State and feedback

- **R20.** Live state MUST be shown on the item itself as a small pill, dot or tint, not as a
  new column or a sentence. Running, draining, repairing and warnings are visible at a glance.
- **R21.** Transient feedback MUST expire on its own and be dismissible. There is no flashing
  loading text, and no background toast the operator can't act on.
- **R22.** An empty state MUST say why it is empty. A completed action resets its UI: an item
  collapses when its status changes, and a search can be cleared. A refused action offers
  its recovery path.

### Responsiveness and interaction

- **R23.** A view MUST use the width it has. Wide screens get a second column, and tables
  don't scroll sideways when the window is wide enough. Narrow widths drop secondary content
  and any nav that is redundant at that width.
- **R24.** Primary actions MUST stay reachable. A panel's action buttons stick to its bottom
  edge, and a detail panel stays in view while the list beside it scrolls.
- **R37.** Changing how a control behaves MUST NOT change how it looks. A native control
  (such as a `<select>`) is not replaced by a custom widget with different chrome, and its
  options gain no new colours or decoration, unless the owner approved the new look as a
  mock (R25).

### Process

- **R25.** A new view or a redesign MUST start from a mock the owner approves before it is
  built.
- **R26.** Every UI correction the owner makes MUST be recorded as a UI fix task that quotes
  it. When it reveals a gap in this standard, a `standards` friction MUST also be filed (see
  the README's canon-gap convention). Where a machine check can catch the defect, the fix
  adds or extends that check with the fix.

## Applies to

`ui`: any project that renders a screen for people (web dashboard, site, desktop or TUI
panel). Terminal command output stays under STD-01.

## Deviations

An adopting repo that departs from a rule records a decision in its own design docs, citing
`STD-07@<version> §Rn` and the reason. It never edits its vendored copy of this standard.
