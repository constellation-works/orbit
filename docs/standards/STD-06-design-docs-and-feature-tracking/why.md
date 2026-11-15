# Why — STD-06 Design docs and feature tracking

Why each rule in [STD-06](STD-06.md) exists, tied to the incident behind it where there
was one. Read the entry for a rule when its intent is unclear or before deviating. The
rules themselves are in `STD-06.md`; this file explains them and adds none.

The Orbit evidence below cites tasks (`ORB-`) and Orbit's own design conventions,
`codebases/orbit/docs/design/CONVENTIONS.md` (here "CONVENTIONS").

**R1–R3.** Orbit grew 43 feature folders across four agent families. Where they followed one
layout, any agent could find the design, the open questions and the decisions without asking;
where they did not, the same knowledge sat in READMEs, roadmaps and task artifacts that
nobody read twice. A stable slug matters because it is the join key to the tracker (R12):
renaming a folder silently orphans every task tagged with the old name.

**R4–R6.** On 2026-10-25 Daniel said he had trouble keeping track of all the features and
where they stood. Orbit's design docs had one state field, `status: Draft/Accepted`, and it
recorded doc approval. 29 of 43 were `Draft`, and that told a reader nothing about delivery:
auto-concurrency was marked `Draft` while its entry summary said "partly implemented", and
path-leases opened with "nothing is implemented" while its own §2 listed implemented parts.
Delivery state lived in prose banners that had to be read one by one and drifted
independently. A separate `stage` field with five fixed values, plus one `next` line, made the
state queryable and the next step visible.

**R7.** "Approved" and "built" are different moments, and the board is only useful if it moves
with the work. Tying the change to the change that moves the feature (approving the first
implementation task, landing the last slice) keeps the update in a diff someone reviews,
instead of in a periodic clean-up nobody owns. Design approval is excluded because a design
can be accepted and then wait months for capacity; calling that `building` overstates
progress.

**R8.** Two sources of the same fact drift. The Orbit backfill (constellation-works/orbit#6242)
found human-decision-channel's banner still saying "proposed" after its core had landed.

**R9.** A hand-maintained board goes stale within a week, which is the failure the board is
meant to fix. Generating it from the docs and checking it in CI (STD-04 §R11) means the board
can only be wrong if a doc is, and a missing stage stops the change that adds the folder.

**R10–R11.** Orbit once ran a separate decision store with ids and a lifecycle. Measured over
210 records, 39% were cited from code, 55% only from other docs and 24% from nowhere; the store
cost ten CLI subcommands and an MCP write path whose supersede half-worked. It was retired by
ORB-10726, which moved the bodies into each feature's `4_decisions.md`. The two doors are the
admission test that came out of that review (CONVENTIONS §4a, §4b): the code-anchored door
keeps entries that explain surprising code, and the second door keeps standing preferences,
which a code-citation-only test had wrongly discarded. The `Cost:` line is the cheapest test of
whether a decision was real.

**R12.** The board says where a feature stands; only the tracker says what is in flight. Without
a shared key the two cannot be joined, and an operator answering "what's left on path-leases"
has to search task titles. Tagging at filing is the only moment the person who knows the
feature is touching the task; an implementer adding tags afterwards guesses. Some Orbit features
already did this (`auto-concurrency`, `operations-as-data`, `path-leases`), which is what made
their progress answerable.

**R13.** Counts copied into a doc are wrong by the next merge. Reading them from the tracker by
tag keeps one source of truth. Task References at the foot of each doc keep cited ids
resolvable without linking into a tracker that may move (CONVENTIONS §8, §9).

**R14.** A retired design left in place reads as live and gets cited as authority. Archiving or
deleting it keeps the board honest, while `parked` keeps genuinely paused work visible.
