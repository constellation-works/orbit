This task changes something a user sees, so its delivery owes screenshots of
each changed view as task artifacts (the orbit skill's task-execution
reference, "Screenshots of a UI change"):

- every theme the view supports, a 375px phone width when the layout is
  responsive, and a before shot from the base revision when it is practical
  and shows the change;
- each captured under `.orbit/tmp/` and attached with
  `orbit.task.artifact.put` at a stable path such as
  `screenshots/<view>-<theme>-<width>.png`, under the 1 MiB artifact limit;
- the artifact paths listed in the execution summary.

When the host has no browser kit or the view cannot render, say so in the
handoff instead of claiming visual verification.
