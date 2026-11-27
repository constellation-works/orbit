---
type: design
summary: "Spec: Compact Viewports"
tags: ["user-interface"]
last_validated: 2026-11-27
---

# Spec: Compact Viewports

> **Approved. Phases 1 and 2 built.** The operator approved this design and its mocks on 2026-11-27
> (STD-07 R25) [ORB-29230]. [2_design.md](../2_design.md) describes the dashboard as it is.
> [Phasing](#phasing) names the task that builds each part.

Below 1316px the dashboard gives each region the width it needs or takes the region off the
screen; no region is squeezed below its measured minimum. On a phone, one header row and one
bottom bar are the only fixed chrome, and the board or the open task gets the rest.

## Why This Exists

The operator used the dashboard on a phone for the first time on 2026-11-26, and at a
mid-sized window, and found both hard to use [ORB-29230]. Measurements of the Tasks view on
the fixture dashboard (`busy-workspace`, agent-main `4cb6c2d341`):

**Phone, 375×812.** Four rows of chrome sit above the status pills: brand and Notify, search
and New task, host and workspace, the navigation strip. The pills wrap onto two rows.

| Region | Height |
|---|---|
| Top bar (two rows) | 103px |
| Host, workspace and navigation strip | 95px |
| Padding and status pills (two rows) | 87px |
| Board | 474px (58%) |
| Log line | 34px |

An open task starts 198px down the screen. Row text sits 37px in from each screen edge (20px
of page padding, then the card's border and 16px of row padding). A row is 93px tall.

**Medium, task open.** The rail keeps 200px and the task pane never goes below 320px, so the
list absorbs the whole loss:

| Viewport | List | Task pane |
|---|---|---|
| 1400px | 661px | 487px |
| 1316px | 612px | 452px |
| 1200px | 545px | 403px |
| 1024px | 443px | 329px |
| 820px | 248px | 320px |
| 768px | 196px | 320px |

At 768px a title shows eight characters and the crew chip overlaps the row's action. At
329px and below the pane's property list wraps one word per line.

## Measured minimums

Every threshold in this spec is a sum of these widths. None is chosen by eye (STD-07 P4).

| Name | Width | Source |
|---|---|---|
| List, one-line row | 620px | The `tasks-panel` container query in `css/tasks.css` |
| List, two-line row | 380px | The list share today's pane clamp reserves; verified at 443px |
| Task pane | 440px | Where the pane's own narrow mode begins (`task-pane` container query in `css/task-detail.css`); broken at 329px |
| Rail, labelled | 200px | `.shell` grid in `css/shell.css` |
| Rail, icons | 56px | A 40px target with 8px either side |
| Page padding | 40px | 20px either side of `main` |
| Gap between list and pane | 16px | `main` grid gap |

The pane's true floor lies between 330px and 440px. The build measures it and replaces 440px
with the measured figure; the sums below follow.

## Width tiers

| Tier | Viewport | Rail | Scope pickers | Tasks regions |
|---|---|---|---|---|
| Wide | 1316px and up | Labelled, 200px | In the rail (until phase 4) | List with dock or task pane (unchanged) |
| Medium | 761 to 1315px | Icons, 56px | In the header | List with task pane when both fit, otherwise one column |
| Small | 760px and below | None; bottom bar | In the header | One column |

- **1316px** is the existing dock breakpoint (`TASKS_ONE_COLUMN_QUERY`). It is also the sum
  for a labelled rail, a one-line list and a pane: 200 + 40 + 620 + 16 + 440.
- **760px** is the existing phone breakpoint (`PHONE_SHEET_QUERY`).
- **List with pane** needs 836px of content width: 380 + 16 + 440. With the icon rail that
  is a 932px viewport. The rule reads the content width of the Tasks view, not the
  viewport, so it stays right if the rail or the padding changes.

The stylesheet and the script MUST decide "one column" from the same source. Today the
stylesheet uses media queries and `task-pane.js` repeats them as `matchMedia` strings; a
container width has no `matchMedia` form, so the build picks one mechanism for both.

## Shell

### Header

One row at every width (STD-07 R39). Left to right: brand, host, workspace, host warning,
provider usage, spacer, search, New task, Notify.

- **Scope pickers.** At medium and small the host and workspace selects sit in the header,
  directly after the brand. They are the same two native selects the rail holds today, with
  the same options (R37). Each is as wide as its longest option until the row runs out
  (R42), then ends in an ellipsis and keeps the full name in its title. At wide they stay in
  the rail until phase 4 moves them. The Serving badge shows at medium and is dropped at small, where the host
  select's accessible name carries it.
- **Fit.** When the row stops fitting at full size, members shrink in this order, each at
  the width where the row measured from its content no longer fits (R39): Notify loses its
  label; provider usage goes; New task becomes its plus icon; the brand becomes its mark;
  search becomes an icon button.
- **Small.** All five steps apply and Notify is not shown. Background-tab notifications are
  a desktop use; the phone gets no replacement. The row is 48px: mark, host, workspace,
  search, New task. Icon buttons are 40px wide and 44px tall. New task shows on Tasks only,
  as today.
- **Search at small.** The icon opens the search field over the header row, with an `esc`
  button at its right (R44). It does not push the page (R5). Results use today's popover.

### Navigation at medium: icon rail

The rail shows one 40px icon button per destination, in today's order and groups, with a
hairline between groups. The active destination keeps today's selected background.

- An attention count (failed runs, failing checks) shows as a dot on the icon, in the
  attention colour. A volume count is not shown. Both stay in the button's accessible name.
- The Drain entry keeps its live state as its icon's tint.
- A destination's views (Health, Analytics, Automation, Settings) open in a popover beside
  the icon on click, listing the same view names as the labelled rail.
- The rail toggle expands the rail to its labelled form **over** the content, not beside
  it, so opening it moves nothing (R5). Esc or a click outside closes it. The host
  readings, refresh time and UTC/Local control are in the expanded rail.
- The elevated-reading warning stays in the header, as today.

The icon rail is also what a collapsed rail looks like at wide. It replaces today's 45px
strip, which holds only the toggle and no navigation. A collapsed rail at wide moves the
scope pickers to the header, by the rule above: the header carries scope whenever the rail
shows no labels.

### Navigation at small: bottom bar

A bar at the foot of the viewport with five equal slots: **Tasks**, **Drain**, **Jobs**,
**Health**, **More**. It replaces the navigation strip under the header, which scrolls
sideways today (R39), and the log line.

- Each slot is an icon over a label, at least 44px tall, in one grid (R3). The bar is 56px
  plus the bottom safe-area inset.
- Tasks, Jobs and Drain carry the counts and live state the rail shows today. Health
  carries its attention count.
- **More** opens a sheet from the bottom edge listing the remaining destinations, each with
  its views beneath it. Health's views are listed there too. The slot is marked while the
  current destination is one from the sheet.
- The log line is not shown at small. The log is the Log tab of the Drain view.
- The floating message area of the Tasks view sits above the bar, never under it.

Tasks and Drain are the two views a phone switches between most, which is why navigation
keeps a permanent row at small and why that row is at the thumb's end of the screen.

## Tasks view

### Status pills

One row at every width (R38, R39). When the full-size row stops fitting, measured from its
content, every status pill drops its label and keeps its dot and count. **More** keeps its
label. A pill's dot uses the status colour that marks the same status on the board's group
headers (R11), and the pill's full name stays in its accessible name and its title (R32).

At 375px with 8px of page padding the row has 359px. Five pills at 50px, four 6px gaps and
More at 64px take 338px. The full-size row needs about 610px, so every phone gets the short
form.

### Board padding

At small the page padding is 8px on all four sides and a row's side padding is 12px, so row
text sits 20px from each screen edge in place of 37px. A title gains 34px. Medium and wide
keep today's padding.

### Rows at small

Two lines in place of three, about 60px in place of 93px:

| Line | Left | Right |
|---|---|---|
| 1 | Title, then any badge that qualifies it | |
| 2 | ID, wait or run, risk, crew, in the muted metadata style (R33) | The row's action: Approve, View run or the PR link |

The action column keeps one width across rows and groups (R3). The crew is text at small;
changing a crew is done in the open task, whose head already holds the crew select. Rows at
medium and wide keep today's layouts.

### Open task

| Layout | Where the task shows | Stays visible |
|---|---|---|
| Wide | Side pane (unchanged) | Header, rail, list |
| Medium, content 836px or wider | Side pane | Header, icon rail, list |
| Medium, content under 836px | Over the list, filling the Tasks view | Header, icon rail |
| Small | The whole viewport | Nothing else |

- **Side pane floors.** The pane is never narrower than the task-pane minimum and the list
  never narrower than the two-line minimum. The pane's width clamp becomes
  `clamp(<pane minimum>, <remembered share>, 100% - <list minimum> - gap)`. The resizer
  moves between the same floors.
- **Over the list.** The pane's own head and footer are unchanged: the ID, **Open full
  page** and **esc** at the top right (R7, R44), the actions stuck to the bottom edge
  (R24). At small the footer sits above the bottom safe-area inset.
- **Previous and next.** While the list is covered, the pane's head shows previous and next
  buttons beside **esc**. They step through the list in its current order, as the J and K
  keys do. They are hidden while the list is visible.
- **Back.** Opening a task while the list would be covered adds a history entry, so the
  browser's Back closes the task and returns to the list at its scroll position.
- **Resizing.** When the window narrows past the point where both fit, a task that was open
  beside the list is hidden and the list stays (R43). Its row stays marked, and widening
  shows the pane again. A task opened while the list was covered stays shown across
  resizes.
- **The pane's content** is out of scope here, except that its property list MUST NOT wrap
  one word per line at the pane minimum. The build verifies that at the measured floor.

### Dock

Unchanged: beside the list at wide, a route of its own (`#tasks?dock=…`) below. At medium
the rail's Drain icon opens it. At small the bottom bar's Drain slot opens it.

## Options not taken

- **Dropping the rail and the side pane together at about 1400px.** At 1400px the list has
  661px and the pane 487px, both above their minimums. One threshold for both removes the
  side-by-side view from 13 and 14 inch laptops, where it works. The rail's labels are the
  cheapest thing on the screen, so they go first and buy 144px.
- **Accordion rows: the task expands under its row.** It would be a third way to show a
  task, after the pane and the full page. Inside a scrolling list the task's actions cannot
  stick to the bottom edge (R24), and an open task pushes every later row off the screen.
- **A status pill row that scrolls sideways.** It keeps the labels, at the cost of hiding
  pills past the edge. R39 rules out a row that scrolls sideways, so it would need a
  recorded deviation.
- **One combined scope control** in place of two selects. It is a new control with its own
  look (R37) and adds a tap to every switch.

## Phasing

Each phase ships on its own, in this order: they change the same stylesheets and markup.

| Phase | Builds | Task |
|---|---|---|
| 1. Small shell (built) | The 48px header, the bottom bar and More sheet, 8px board padding, and the one-row status pills at every width | [ORB-29233] |
| 2. Small Tasks view (built) | Two-line rows, the full-viewport task with previous and next, the history entry | [ORB-29234] |
| 3. Medium | The icon rail (and as the collapsed rail at wide), scope pickers in the header, the header's fit order, the side pane floors and the one-column task over the list | [ORB-29235] |
| 4. Scope at wide | The host and workspace selects move to the header at wide too, so they are in one place at every width | Not filed; after phases 1 to 3 have been in use |

Every implementation task carries the `user-interface` and `ui` tags and attaches
screenshots at the view manifest's widths: 375px (small), 768px (medium, one column), 1000px
(medium, list with pane) and 1440px (wide, unchanged).

## Mocks

Drawn with the dashboard's tokens and a mock-only stylesheet
([`compact.css`](../references/mocks/compact.css)); none of that stylesheet is product CSS.

| Mock | Shows |
|---|---|
| [3.1](../references/mocks/3-1-phone.html) | Small: Tasks with the header, short pills, two-line rows and bottom bar; an open task; the More sheet |
| [3.2](../references/mocks/3-2-medium.html) | Medium: the icon rail with the list and task pane at 1000px, and one column at 768px |

## Concerns and Honest Limitations

- **Dot-and-count pills lean on colour.** The board's group headers repeat each colour with
  its name, and the accessible name is complete, but a new operator has to learn five dots.
- **Views are two taps away at small.** Switching between Analytics views, or to any
  destination outside the bar, goes through More.
- **Search is unreachable while a task fills a phone screen.** Close the task first.
- **Crew cannot be changed from a phone row.** It takes opening the task.
- **The dock stays a route below 1316px.** With the icon rail it would fit beside the list
  from about 1170px. This spec does not move that breakpoint.
- **Only the shell and Tasks are designed.** Other destinations get the new header, rail
  and bar, and keep their own content layouts and breakpoints.
- **The task-pane minimum is provisional** until the build measures it.

## Decided at Approval

The operator settled these with the design on 2026-11-27 [ORB-29230].

1. **Status pills at small are dots with counts.** A row that scrolls sideways was the
   alternative and would have needed a deviation from STD-07 R39.
2. **The close button stays "esc" on a phone** (R44). The browser's Back also closes the
   task. A different label on touch would be an amendment to the standard.
3. **Rows at small are two lines with the crew as text.** The crew is changed in the open
   task.
4. **Scope pickers move to the header at wide too,** as phase 4.

## Task References

- [ORB-29230] recorded the phone and mid-width findings, wrote this spec and carried its approval.
- [ORB-29233] builds phase 1, the small shell.
- [ORB-29234] builds phase 2, the small Tasks view.
- [ORB-29235] builds phase 3, the medium tier.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
