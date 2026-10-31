When the actual diff changes something a user sees, follow the workspace
guide's screenshot evidence rule and attach screenshots of each changed view
as task artifacts. This evidence follows the diff, not the `ui` tag itself.
Screenshots in the PR body are optional: if `pr.screenshot_section` applies
and none are attached, Orbit writes `No screenshots attached.` and does not
draft or refuse the PR for that reason:

- every theme the view supports, a 375px phone width when the layout is
  responsive, and a before shot from the base revision when it is practical
  and shows the change;
- each captured under `.orbit/tmp/` and attached with
  `orbit.task.artifact.put` at a stable path such as
  `screenshots/<view>-<theme>-<width>.png`, under the 1 MiB artifact limit;
- the artifact paths listed in the execution summary.

When the host has no browser kit or the view cannot render, say so in the
handoff instead of claiming visual verification.

For a task that also carries `verify-mock-ui`, the mocks under `mocks/`
are the visual specification and the `verify-mock-ui` instructions govern
the mocked views.
