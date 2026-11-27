# Why — STD-08 Economy

The reason for each rule in [STD-08](STD-08.md), tied to the correction or incident behind it. This file adds no rules.

The standard exists because of the owner's direction in session f09993e5 (2026-11-15): design with an economical mindset over thoroughness and verbosity, minimal especially in token use, and write a standard that prevents agents from producing verbose, cluttered comment output. The comment in question was an Orbit resident checkpoint: 273 words of JSON whose notes restated open PRs, still-open items and a "FOR DANIEL" list that the previous checkpoints had already carried, and which every later cycle reread.

## Design

- **R1 (state readers and size first).** The resident checkpoint format shipped without anyone stating its repeated size. Its brief carried the last five checkpoints in full plus every older checkpoint's notes (up to 2000 characters each), so an 8-hour mandate at 20-minute cycles rereads tens of thousands of characters by its last cycle (ORB-25099). The arithmetic takes one line; nobody wrote it down.
- **R2 (smaller form, detail reachable).** The same checkpoint inlined its schema, digest, authority snapshot and ledger keys "for completeness". None of it helps the next reader act, and all of it was already recoverable from the action ledger and run records (ORB-25100). Economy is not deletion: the raw record stays one click away.

## Context agents read

- **R3 (fixed upper size).** ORB-25099: the resident brief's `older_checkpoint_notes` had no bound. Orbit's notice brief already shows the alternative: a fixed item count and a per-item character cap.
- **R4 (each fact once).** Orbit's prompt audits repeatedly found instructions restating protocols that installed references already document, for example ORB-19506 (the implementer prompt repeated the post-deploy protocol). The weekly prompt audit (ORB-22868) exists to make agent instructions "clearer and shorter".
- **R5 (compact default).** Orbit's task tools return full records unless asked for `fields`, and agents habitually read whole tasks to check one field. A projection that is opt-in for less is the wrong way round.
- **R12.** The owner, in a typed scribe line (scribe standard-1, session 5eee6743), directed that an agent finding stale, incomplete, unclear or suboptimal instructions, wordings or delivery in skills, agent files and docs resolve it on the encounter when appropriate, and otherwise file a fix task or friction, never letting it slide. Guidance that agents reread (STD-08 R3, R4) decays when each reader routes around a defect and leaves it for the next; the reader who hits it is the one who knows it is wrong. Fix-or-file keeps the cost with the finder while the context is fresh, and the file-or-friction branch keeps agents from editing what they have no authority over (the canon goes through standards.gap, RB-22).

## Records agents write

- **R6 (delta only).** The resident checkpoint retold standing state every cycle (open PRs, blocked tasks, items for the owner), so each record grew and most of it repeated the last one (ORB-25099).
- **R7 (bookkeeping off the page).** The owner found the raw checkpoint comment too verbose and hard to read (ORB-25100).
- **R8 (one channel per ask).** The resident's "FOR DANIEL" paragraph repeated the same asks in each checkpoint, while decision requests and notices existed for them; the owner asked for asks to go through notices (ORB-25095).
- **R9 (outcome first).** The owner runs agent sessions in a concise output style, leading with the result and giving no closing recap, because narration and recaps are what the owner skips.

## Errors, logs and audit

- **R10 (one-line errors, repeats collapsed).** The owner, on the Orbit run list, where every failed `resident_cycle_pipeline` run showed "execution failed: v2 job dispatch: worktree integrity violation `worktree_mismatch`:" followed by the whole diagnostic as inline JSON, and the same failure listed once per run; the owner asked that such cases also be prevented (session f09993e5). The cause fitted in one line: the resident cycle passed no repo root (ORB-25050). The error type interpolates its serialized diagnostic into the message (`WorktreeIntegrity` in `orbit-engine`'s dispatcher), so every surface shows it raw.
- **R11 (complete stored records).** The owner, right after v1, said errors, logs and anything audit should be thorough (session f09993e5). Errors, logs and audit records are evidence: context cannot be added after the fact, and the worktree-mismatch failure above was diagnosable only because its full diagnostic was kept. Without this rule, v1's economy rules could be read as licence to log less. The split is storage versus view: record everything in structured fields, show one line and read filtered slices.

The kit's audit line is identifiers-only by STD-05 §R13's intent, and R11's "nothing is trimmed" was read as conflicting with it. R11 already allowed a digest of the inputs; the sentence says which standard decides what is stored in clear (rust-cli-template sweep at 7539c08) (ORB-28599).
