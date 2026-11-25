# Changelog — STD-07 UI and UX

Version history of [STD-07](STD-07.md). Adopting repos pin the `version:` in its
frontmatter as `STD-07@<version>`; a version changes only when a rule is added,
removed or changed in meaning.

- **v1 (2026-11-09):** initial publication, R1–R31. Distilled from the owner's UI corrections
  in Orbit UI fix tasks and interactive sessions, 2026-09-20 to 2026-11-09 (ORB-23557).
- **v2 (2026-11-09):** R17 adds that small sets are not split into tabs: a handful of items in
  two states stay in one list, with the active group first (the dashboard Notices panel),
  and the row cap exempts a list that is the whole content of its own tab or panel.
- **v3 (2026-11-11):** adds R32 (a tooltip only where it adds something, sized to its
  text), R33 (secondary metadata on its own line under the label), R34 (chart marks labelled
  by name, figures in a tooltip, no overlapping labels), R35 (chart text uses the page's type
  scale and never scales with the chart), R36 (rows in a tied group ordered by the ranked
  figure) and R37 (a behaviour change keeps the control's look). From the owner's dashboard
  corrections in session 7995d040 (ORB-24111, ORB-24113, ORB-24114, ORB-24118, ORB-24120,
  ORB-24124).
- **v4 (2026-11-14):** adds R38 (a row of controls uses the full width available before it
  wraps, and a bar that belongs to the whole view spans the view). From the owner's Tasks
  status-chip correction in session f09993e5 (ORB-24686, F2026-11-288).
- **v5 (2026-11-14):** R38 no longer says a view-wide bar spans the view. A row of controls
  extends into the empty space beside it only where it would otherwise wrap inside its
  column; where it fits its column it stays there. From the owner's correction of the
  ORB-24686 fix (ORB-24777, F2026-11-294).
- **v6 (2026-11-14):** adds R39 (a panel's header row and a row of summary figures stay on
  one line: members shrink first, then the least important are removed). From the owner's
  drain-card correction in session f09993e5 (ORB-24812, F2026-11-296).
- **v7 (2026-11-15):** R39 keeps members at full size wherever the row fits and starts
  shrinking only at the width where the full-size row stops fitting, never at a fixed
  breakpoint that also catches fitting widths. It also covers tab strips. From the owner's
  Tasks-dock tab correction in session f09993e5 (ORB-25032, F2026-11-302).
- **v8 (2026-11-15):** adds R40 (an inline list's separator sits directly after the
  preceding item or its icon and is followed by a clear gap, the same everywhere; a small
  icon sits close to its item, and an enlarged tap target adds no visible space). From the
  owner's task-detail relation-row corrections in session f09993e5 (ORB-25033,
  F2026-11-303).
- **v9 (2026-11-15):** adds R41 (every button carries a short hover help saying what it
  does, including consequences the label does not show, and why it is disabled), and R32
  names a button's effect as detail a tooltip may add. From the owner's direction in
  session f09993e5 (F2026-11-304).
- **v10 (2026-11-15):** adds R42 (a control is as wide as its content: a select fits its
  longest option, capped by the space its row leaves, and shrinks no further than the part
  that tells options apart). From the owner's drain-target select question in session
  f09993e5 (ORB-25074, F2026-11-310).
- **v11 (2026-11-15):** adds R43 (when side-by-side regions collapse into one column, the
  page's primary region stays shown; a secondary region replaces it only when the operator
  navigated to it at that width, and a selection inside it does not count). From the
  owner's Tasks/Drain collapse correction in session f09993e5 (ORB-25093, F2026-11-311).
- **v12 (2026-11-17):** R9 adds that a label is vertically centred against its value, including a multi-line value (the owner's Fleet host-dialog correction, ORB-25399).
  R16 adds that a detail view, dialog or drill-down does not repeat what the view that opened it already shows (the owner's Fleet host-dialog correction, ORB-25399).
- **v13 (2026-11-17):** R41 reverses button hover help: a button shows no hover or focus bubble or native tooltip, its effect goes in its accessible description, and a consequence or disabled reason goes behind an info icon or in its confirmation (the owner's Fleet correction, ORB-25398).
  R32's closing sentence now says buttons carry no tooltip, following R41 (ORB-25398).
- **v14 (2026-11-17):** adds R44 (a close button on a surface Esc closes is labelled "esc", not ×, "Close" or "Back to list"; its accessible name still says what it closes) (the owner's direction after ORB-25406, ORB-25389).
- **v15 (2026-11-21):** adds `## Principles`, P1–P8, above the rules: the reasons the rules
  share, for judging a case no rule covers yet. Each principle is distilled from corrections
  the owner made more than once, and lists the rules it explains. No rule changes. From the
  owner's request to "build up our own philosophy" after the Astryx and Linear research
  (ORB-26273).
- **v16 (2026-11-22):** adds R45 (a text colour follows the text's role: the strongest colour for titles, naming labels and scanned values, the step below it for prose; ids, code and links inside prose keep their own colour) (the owner's correction on a dashboard notice card, 2026-11-22).
- **Editorial (2026-11-23, version unchanged):** the frontmatter drops `status`, which nothing read (ORB-27289).
- **v17 (2026-11-25):** Prefer unambiguous abbreviations and scaled large values in screens (scribe standard-7).
- **v18 (2026-11-25):** clarifies R6 (peer buttons MUST match in rendered height, strictest when adjacent or sharing a function, with width uniformity a softer goal subject to content; Water Economy 26/40/26/40px -> 26px example, scribe standard-6).
  clarifies R32 (tooltips may reveal the full text of a shortened label as well as a truncated one, aligning the closing button note with R41's narrow full-name exception, scribe standard-6).
  clarifies R39 (constrained control rows preserve full labels where they fit, shorten labels by content before reducing readable type to hold single-line peer heights, and reveal ambiguous shortened names under R41, scribe standard-6).
  clarifies R41 (adds a narrow full-name-only hover, keyboard-focus and touch exception when label shortening introduces ambiguity, keeping general button help tooltips forbidden, scribe standard-6).
  clarifies R42 (peer button widths aim for comparability subject to content without breaking R6 equal height, shorten labels by content before reducing readable type, and reveal ambiguous shortened names under R41, scribe standard-6).
- **v19 (2026-11-25):** adds R47 (a container's height comes from its content: the last item of a stacked list does not stretch to fill its parent; a region that fills its parent is exempt). From the owner's correction on the Orbit dashboard's last comment card, scribe standard-3.
- **v20 (2026-11-25):** R48 added: everything shown under a selected tab belongs to that tab; a panel that stays the same across tabs moves above the strip or into the one tab it serves (Analytics > Metrics) (scribe standard-4, ORB-28134)
