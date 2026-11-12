# Why — STD-07 UI and UX

Why each rule in [STD-07](STD-07.md) exists, tied to the corrections behind it. Task ids
are Orbit tasks on dk-server-2. Session ids are the first 8 characters of a Claude Code
transcript, and quotes are the owner's words. Read a rule's entry when its intent is unclear
or before deviating. The rules themselves are in `STD-07.md`; this file explains them and
adds none.

The rules come from two sweeps on 2026-11-09 (ORB-23557):
- about 95 UI fix tasks across ws_orbit, ws_orbit-office and ws_constellation-site;
- about 110 corrections across 331 interactive sessions, 2026-09-25 to 2026-11-09.

A rule is here only when its correction recurred.

## Layout and alignment

- **R1 (sibling spacing).** Raised in 9 separate sessions: the most repeated correction.
  "paddings are inconsistent for "setup implement..." compared to the other" (cdd16d4b);
  "there is an extra padding after "Approve" and "PR #5878"" (6b0bb806); "too much
  unnecessary space in the left column" (c43e4f47). Tasks: ORB-15181, ORB-17520. The
  symmetric-padding clause was added during review of v1 (2026-11-09, session 31048ad4). The
  task panel's title block had a tight top and a deep bottom under the title: "inconsistent
  top/bottom padding".
- **R2 (shared edges).** Raised in 7 sessions and 7 tasks: ORB-16937, ORB-15188, ORB-19331,
  ORB-21636, ORB-14442, ORB-19590, ORB-22614 ("50:50 width."). "it should be the same width
  as the "auto-drain" column. not more, no less." (d5e3df3c); "can we make the height
  between the two columns consistent?" (ad7e58ba).
- **R3 (one column grid).** ORB-15180, ORB-17764, ORB-17900, ORB-17956, ORB-13839, ORB-16199:
  crew chips and row actions drifted between rows and groups.
- **R4 (stay inside the container).** ORB-19685, ORB-19908, ORB-12652, ORB-17294, ORB-15633,
  ORB-23172, plus sessions 49fa738d and 770f88b4 (overlapping and cut-off text).
- **R5 (no layout shift).** "whenever text appear it shifts the whole panel up and down - bad
  UX" (dbad1c64); "right side column panel should not drop down along with task board. but
  anyways, let's make "create task" section a popup window" (ORB-20011). Also ORB-20022,
  ORB-22628 and ORB-23530.
- **R6 (peer controls).** ORB-23271 (a stepper like its sibling) and ORB-12889 (oversized
  selects).
- **R7 (Close and Back top right).** ""Back" should be on the upper right corner"
  (ORB-22754); "share with the team toggle can be next to Close button, as just Share"
  (ORB-21656); also ORB-17511.
- **R27 (no orphan tiles).** Added during review of v1 (2026-11-09, session 31048ad4). A
  stats row of six tiles wrapped "Hard" onto a row of its own: "I would rather remove one
  than keeping one left out like that." This fits R2's shared edges and R16's "only what is
  acted on": a lone tile breaks the row's symmetry, and the choice that fixes it is usually
  to drop a tile, not to squeeze one in.
- **R28 (stacked blocks share the column width).** Added during review of v1 (2026-11-09,
  session 31048ad4). On constellation-site's Reviewers board, the provisional notice and the
  score explanation stopped at about half the page while the table ran the full width: "also
  things like this, where right column is empty". This is the vertical counterpart of R2's
  shared edges. A reading-width cap on prose is a good reason to cap a line's length, but it
  is no reason to leave half the page blank: the space goes to a related block, or the prose
  goes behind an info icon. The same correction on prose against tables: "can you make the
  width consistent between the paragraphs and tables?" (f2c1c565, 2026-09-27), repeated on
  the crew-rankings note on 2026-11-09 ("inconsistent widths"), where the table also clipped
  its last column (R4).

- **R30 (one look per kind of element).** Added during review of v1 (2026-11-09, session
  31048ad4). The Orbit dashboard has a task lifecycle strip (Proposed → Done) and a run stage
  strip (Setup → Re-review). Their bars differ in height and weight, their top and bottom
  margins differ, and "Implement · 2m" wraps to two lines: "different "height/boldness" and
  top/bottom margin". Earlier: ORB-19326 and ORB-19336 (strips and severities styled per
  view), and ORB-19357 (completed is green on every strip). A component copied per view
  drifts apart, while a shared one can't. On the same strip a terminal step, "Awaiting
  approval", wrapped into a narrow column four lines tall, while its neighbours were
  truncated: "no multi-line like that. keep them all single line, with "Awaiting..." with
  hover over".

- **R31 (no needless nesting).** Added during review of v1 (2026-11-09, session 31048ad4).
  In the Orbit task panel, the "Execution summary" disclosure is a card, and its Markdown body
  is drawn in a second bordered card inside it: "no double nested containers that doesn't
  draw any meaningful distinction." Every border costs padding on both sides and adds a
  level the eye has to resolve. Orbit's `ui.nested-card` rule already flags it.
- **R33 (metadata on its own line).** "meta-data, e.g. "sol, sol@xhigh, sol@high", can be
  placed as a new row" (7995d040, ORB-24113): the Agents rank table ran each model's crews
  inline after its name, so long names and crew lists competed for one line.

## Typography

- **R8 (one type scale).** In PR #8842 the dashboard's "No running residents." differed from
  its neighbour in both width and font size: "not just different width but also different
  font size". "no need to bold font for percentage and core texts." (75ceb1a9). Also
  ORB-14678 ("a bit off"), ORB-19326, ORB-19336 and ORB-17951. The rule that monospace is for
  data only is Orbit's theme spec, which held up under these corrections.
- **R9 (label/value lists).** The same-row half was added during review of v1 (2026-11-09,
  session 31048ad4). On constellation-site's analytics pages, "Snapshot" sat on its own row
  above an indented "Latest snapshot …" line: "Snapshot Latest in different row".
  ORB-20526 and ORB-20536. The owner asked for both to become
  validator rules: "let's add this to the ui ux rule - so that it will be caught by the new
  visual validator job".
- **R35 (chart text on the type scale).** "the texts inside this chart are way bigger than
  the rest of the texts around it. should be similar sizes" (7995d040, ORB-24114). The
  frontier chart drew a fixed 760-unit `viewBox` and stretched it to the panel, so its text
  grew with the panel.

## Colour and theme

- **R10 (sections and idle elements visible).** Raised in 6 sessions. "when idle, should have
  more contrast against the background color. its currently invisible" (ca51ec07); "use like a
  card/container with clear boundary or contrast between the detail section and the rest"
  (dbad1c64); "let's use different background color for meta data section for better contrast."
  (31048ad4, ORB-23534). Also ORB-22757 and ORB-17763.
- **R11 (one meaning per colour).** "white texts in incidents page and friction Details panel
  are too bright, unlike other pages" (6f2baa40). Tasks: ORB-19357 (completed is green on
  every strip), ORB-17120, and ORB-16076 (pilot warnings as a yellow tag on the task).
- **R12 (declared default theme).** The default changed per surface without being written
  down. "make light mode the default, not the dark mode" (ORB-13354, September dashboard);
  the website then moved to dark (410a5432); the Orbit dashboard's current theme spec is dark.
  The rule makes each product state its choice once, instead of the standard picking one.

## Words

- **R13 (minimal words).** The most repeated theme across tasks and sessions (8 sessions,
  6 tasks), and the owner's stated standing preference. "these long sentence summary line or
  helper texts should be minimal in the UI" (04aa5878); "too many long sentences - it feels
  chaotic. perhaps use hover message or something." (dd5029a6); "let's delete the yellow
  warning texts and move to hover-over texts on symbol" (ORB-22789); "Cut taglines, helper
  text and secondary labels if not necessary." (ORB-21727). Also ORB-22319, ORB-22326,
  ORB-22331 and ORB-22329.
- **R14 (one-line status, "show full", untruncated errors).** ""why stuck" is too verbose. can
  we truncate long texts with clickable "show full.."" (895a3cbc); "acceptance criteria also
  needs "show full AC" button, and default should be collapsed." (adc7f22c). Tasks:
  ORB-22321, ORB-18093, ORB-17505. The error exception comes from ORB-12978, ORB-14829 and
  ORB-19685, where truncation hid the cause an operator needed.
- **R15 (short labels without repeated context).** "instead of "on mac-mini-1" just use "View
  run" - the host info is already present" (6662e60f).
- **R16 (show each thing once, only what is acted on).** 18 tasks and 9 sessions. "we don't
  need "537 denied" and "1 job" - just noise." (ORB-20662); "what is the point of displaying
  both. seems redundant" (dbad1c64); "let's also hide Leases from nav when its disabled"
  (1b5897bc); "what do you think about removing this page. seems bit redundant" (ORB-21641).
  Also ORB-19252, ORB-19253, ORB-22598–22601, ORB-22627, ORB-19161, ORB-17276, ORB-20509,
  ORB-20502, ORB-22935, ORB-22755, ORB-22756 and ORB-19697.
- **R32 (tooltips that add something).** "we don't need a hover-over help just to
  additionally display "done" - the green color make that obvious ... We just need those for
  those that gets truncated" and "excess space in the hover-over container" (7995d040,
  ORB-24124). Every progress-strip step had a tooltip, and each bubble was a fixed 280px
  wide.

## Data display

- **R17 (answer first, collapse detail).** "long tables or texts, either paginate or have
  "show full"" (cdd16d4b); "these long tables should be a drop-down or something."
  (ORB-23343). The short-list half comes from "this pagination in routine is annoying. can you
  bring back no-pagination" (ORB-23384). The "no tabs for small sets" clause was added
  during review of v1 (2026-11-09, session 31048ad4). The dashboard's Notices panel opened
  on an empty "Open 0" tab, with the four recently closed notices one click away: "there will
  be a smaller number of "Open" notices and "recently closed", so in these cases, using tabs
  should not be preferred." The exemption for a list that owns its region was added at the same review: a task's Context files tab lists all 33 selectors, and "the rule doesn't apply to things like this. because it owns the rest of the bottom panels". Collapsing there would only add a click, because nothing else competes for the space. Also ORB-20525, ORB-22329, ORB-23480, ORB-23520
  and ORB-19610.
- **R18 (visual over raw).** "can we get some visuals like CPU and memory for backlog
  forecasts?" (ORB-20609); "instead of raw data, we should just display chart like we have in
  the dashboard" (31048ad4). Also ORB-23361, ORB-17931 and ORB-21605 (raw Markdown).
- **R19 (honest charts).** "this chart doesn't make much sense. sol is second in rank, yet the
  chart suggest below median." (52618a92, ORB-18538); "either we should group them by
  direction of better or align all to same direction" (309ba5e1); "we need something like
  rolling sum" (ORB-20003). Also ORB-23415 and ORB-23451.

- **R29 (summary with its detail).** Added during review of v1 (2026-11-09, session
  31048ad4). On the Orbit dashboard's tool-call view, "Tool call failure rate · window 24h"
  was a full-width panel holding one line ("0.6% · 231 failed / 36626 tool calls") above the
  per-tool table it summarises: "does not belong there alone like that." A lone figure in its
  own panel costs a whole band of height, splits the answer from its evidence, and reads as a
  separate topic.
- **R34 (chart labels by name).** "too cluttered ... details like "40.1 · $0.24 · 37 tasks"
  can be hover-over" (7995d040, ORB-24120). Two-line captions on close points printed over
  each other and over the `fleet = 100` line.
- **R36 (tied groups in ranked order).** "for tied ranks, it should be ordered by the score"
  (7995d040, ORB-24118). The rank board ordered each tie band cheapest first, so scores
  inside a band jumped around. Cost-first was deliberate (a band means the scores can't be
  told apart), and the owner chose score order anyway, with cost still shown on each row.

## State and feedback

- **R20 (state on the item).** Raised in 6 sessions. "just color the buttons or just add a
  colored dot next to task_id" (1df063d1); "there should be a visual indicator if there is a
  repair in-progress. perhaps "repairing" pill" (1b5897bc); "when the drain is going on, there
  should be some visual indicators" (ORB-13634). Also ORB-20653 and ORB-20709.
- **R21 (transient feedback).** "there is a background alert message that pops up briefly in
  that corner. can you get rid of it?" (ORB-22628). Also ORB-16517 and ORB-19300.
- **R22 (empty, completed and refused states).** "when a task's status is changed, the task
  needs to be automatically collapsed." (ORB-13281). The owner asked for this twice, a
  fortnight apart (b3120a66, 3f89aead), after the first fix didn't stick. Also ORB-15216
  (empty state), ORB-16065 (clearable search) and ORB-23332 (confirm-and-force recovery).

## Responsiveness and interaction

- **R23 (use the width).** "let's make the section in the screenshot into a second column,
  when screen width is sufficiently large" (0c8aa357); "can you fix these tables? the window
  has enough width" (de43d4bd). The narrow half comes from ORB-20453, ORB-22756 (nav redundant
  beside the auto-drain panel), ORB-14442, ORB-23530 and ORB-23172.
- **R24 (reachable actions).** ""Comment" button should be sticky to the bottom, just like
  buttons in "overview"" (75ceb1a9); "when I scroll down, the setting detail panel disappears.
  it should scroll down to be visible." (ad7e58ba).
- **R37 (behaviour change keeps the look).** "revert this drop down css back to its original
  before the change" (7995d040, ORB-24111). ORB-23882 made the rail workspace select
  multi-select by replacing the native control with a custom popup; ORB-24027 restored
  single-select but kept the new chrome and coloured options, which nobody had approved.

## Process

- **R25 (approved mock first).** Nine redesigns went through a design-canvas mock the owner
  approved before the build: ORB-21727, ORB-21345, ORB-20525, ORB-22329, ORB-23480,
  ORB-23520, ORB-17763, ORB-17933 and ORB-17934. The ones that skipped the mock are the ones
  that came back.
- **R26 (corrections become rules).** The owner has asked four times for his UI corrections
  to become automated checks: cdd16d4b, 1b5897bc, 0d19f735, 04aa5878, and ORB-20526,
  ORB-20536, ORB-21637, ORB-20865. A correction that exists only in chat recurs, while a
  task is searchable, and the owner prefers to search tasks over transcripts. That is why
  every correction is filed as a task, and why this standard grows by searching UI fix tasks.
