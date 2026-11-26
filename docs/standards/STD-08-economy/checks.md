# Machine checks — STD-08 Economy

The gate that enforces each rule in [STD-08](STD-08.md), or `review-only`. Ship a change with the gate its rule names, or say it is review-only.

| Rule | Gate |
|---|---|
| R1 | review-only: the task or design doc names the readers and gives the steady-state and repeated size. |
| R2 | review-only. |
| R3 | A test that builds the context after many synthetic repetitions (for example 50 cycles) and asserts its size stays under a fixed budget. Orbit: none for the resident brief; it once had a prompt token-budget test (ORB-12753, ORB-12864). |
| R4 | review-only; the weekly prompt audit looks for restated protocols. |
| R5 | review-only. |
| R6 | A length cap on the record's free-text fields, enforced where the record is accepted. Orbit: resident checkpoint `notes` at 2000 characters (`comments.rs`), to be lowered by ORB-25099. |
| R7 | review-only: render the record as its readers see it. |
| R8 | review-only. |
| R9 | review-only. |
| R10 | review-only: trigger the failure and read it where operators see it. A candidate test that fails when an error's `Display` contains a serialized object (`{"`) or exceeds one line. |
| R11 | review-only: a change that shortens a message must keep the full diagnostic in a field, and a change that drops a logged or audited field needs a stated reason. |
| R12 | review-only: a reviewer who finds a defect in guidance a change touched or cited asks whether the change fixed it or filed a task or friction. |
