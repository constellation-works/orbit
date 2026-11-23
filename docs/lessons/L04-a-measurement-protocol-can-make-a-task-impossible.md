---
type: context
summary: Price a task's measurement protocol before dispatch, and give the implementer a way out when the harness, not the change, is what fails.
created_date: 2026-11-23
last_validated: 2026-11-23
---

# L04. A measurement protocol can make a task impossible

On 2026-11-23, sol@high spent four hours on ORB-26338 (jrun-20261123-0600-c6),
a task to speed up `orbit doctor` by bounding the standing-drain scan. The code
change was done and passing its checks early. Every remaining hour went into a
measurement that could not succeed, until the run was cancelled.

The task was the fourth attempt at the same goal: ORB-26301 and ORB-26308 were
held for having no reproducible fixture, ORB-26323 for a parity mask that named
no doctor field, and ORB-26338 replaced them. Each replacement answered the
previous failure by tightening the protocol: a frozen snapshot of the live
store, three arms (base, candidate, control) restored before every round, exact
flags, and a back-to-back message comparison on one restored root. Each step
was reasonable on its own. Nobody priced them together.

The 34G fixture took about an hour to restore (3,734 s and 3,396 s), and the
paired run took another 3,614 s. The snapshot had been taken just after a
commit landed, so its copied `origin/agent-main` was one commit ahead of the
after-landing consumer's observed commit. Doctor's `review` row compares that
gap with the current wall clock and fails after 120 minutes. The fixture aged
past that during the first restore, every arm exited 1, and `paired.py`
correctly refused to sample. No timing was ever taken (friction F2026-11-448).

The agent diagnosed this correctly and wrote it up, then started another
hour-long restore for a same-root diagnostic that could only hit the same wall.
The task text said to follow the protocol exactly and allowed a rerun only for
an inconclusive verdict. It gave no way out for a harness that cannot sample,
so the agent kept executing. The run was cancelled, the acceptance criteria
were relaxed to "any measured improvement", and the change merged as
#10844 once its required CI passed.

**Lesson**: A measurement protocol is part of the task and has a cost; price it
before dispatch: restores times rounds, against every clock-sensitive check in
the fixture. Give the implementer an exit: when the harness refuses to sample
for a reason the change does not cause, stop after one attempt and hand back
with the candidate. And treat a third replacement task for the same goal as a
signal that the method needs redesigning, not another rewrite of the text.
