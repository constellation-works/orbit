# Read or answer a decision request on held work

## Decision requests on held work

Some held work waits for a human judgment rather than a retry. Orbit then
records a **decision request** on the task. The request names its gate
(`block_triage`, `pilot_admission`, `proposed_approval` or `review_approval`),
the allowed answers with each one's consequence, the binding it was raised
against, and an expiry. It is a task-history entry, so it stays with the task.

A request reads in one of four effective states:

- `open`: waiting for an answer.
- `answered`: an allowed answer was applied. Terminal.
- `superseded`: the hold changed before an answer arrived, such as a new block
  episode, a newer pilot assessment, a newer request on the same task, a task
  that moved, or an answer given on another surface. Terminal.
- `expired`: `expires_at` passed while the request was open. Terminal.

Supersession and expiry are derived when a request is read, so a request can
read `superseded` before any sweeper has run.

### Find requests

```bash
orbit decision list                          # open requests in the active workspace
orbit decision list --gate pilot_admission   # one gate only
orbit decision list --task <task-id> --all   # one task, including closed requests
orbit decision show <request-id>             # allowed answers, binding, expiry, resolution
```

The task lists its open requests in `open_decisions`:

```bash
orbit tool run orbit.task.show --input '{"id":"<task-id>","fields":["status","open_decisions"]}'
```

An agent session reads the same requests through the registered read-only tools
`orbit.decision` action `list` (`gate`, `task_id`, `include_closed`) and
`orbit.decision` action `show` (`id`).

The dashboard's **Why stuck** card links each blocked or held task to the run or
finding that stopped it, with one-click requeue, settle-merged and
pilot-admission actions. Use the card to find what stopped a task, and answer
its request as described below.

### Answer requests (operator only)

An operator answers a request from the CLI or the dashboard:

```bash
orbit decision answer <request-id> <answer-id> --note "<evidence>"
```

`<answer-id>` must be one of the request's allowed answers, as
`orbit decision show` lists them. The answer is applied as the human operator.
For a pilot-admission verdict the note is the verdict's evidence, so give one.
A superseded or expired request is refused and nothing changes.

### Agents read, never answer

An agent session may read a request: seeing it never answers it. It may not
answer one. `orbit.decision.answer` is registered inactive, so only the operator
CLI and the dashboard answer. It may not submit one either: `orbit.decision.submit`
is governed and never callable from an agent session. Submissions come from a
registered decision channel's plugin, which presents a per-request token;
`orbit decision submit` is the CLI form of that operation.

When a request waits on an answer, report its id, gate and allowed answers to
the operator and leave the answer to them. Do not route around the request with
a task update.

### Verify an answer applied

1. `orbit decision show <request-id>` reads `answered`, and its `resolution`
   names the chosen answer, the answering human, the note and when it was applied.
2. `orbit decision list --task <task-id>` no longer lists the request as open,
   and `open_decisions` no longer names it.
3. The task history holds a `decision_answered` entry for the request, beside the
   entry the answer itself produced (a pilot verdict, a triage application or an
   approval):

   ```bash
   orbit tool run orbit.task.show --input '{"id":"<task-id>","fields":["history"]}'
   ```

4. Check the outcome the answer asked for in the task's own state, such as its
   status.

If the command errored or the request still reads `open`, do not assume the
answer was applied. Read the request again before answering it a second time.
