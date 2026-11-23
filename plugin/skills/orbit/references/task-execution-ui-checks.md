# Executing a task: UI checks and screenshots

## Checking a UI in a browser

When the host has a verified headless browser kit, your environment carries
`PLAYWRIGHT_BROWSERS_PATH`, `PLAYWRIGHT_MODULE` and `ORBIT_BROWSER_KIT_ENV`.
Check a changed page by serving it on loopback and scripting headless
Chromium. In the same shell, source the kit first, then import `chromium`
from the module:

```bash
. "$ORBIT_BROWSER_KIT_ENV" && node --input-type=module -e '
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE);
// launch, goto http://127.0.0.1:<port>/, assert, screenshot to .orbit/tmp/
'
```

Save screenshots under `.orbit/tmp/` and open them to look at the result.
Inside a worker, Chromium's own sandbox cannot start, and that failure is not a
blocker: launch Playwright's default (sandbox off) with `--no-zygote
--disable-gpu --disable-dev-shm-usage`, and wait for rendered content rather
than a static shell element. The worker launch recipe is in the orbit-setup
browser-checks reference, section "Launching Chromium in a worker".

Don't install Playwright, download a browser or run `sudo` yourself. When the
variables are absent, the host has no verified kit: record the browser check
as unavailable, naming `orbit config browser-kit install` as the host fix. A missing
`lib*.so`, a Skia/FontConfig crash or `Executable doesn't exist` is a host kit
defect, not a defect in the code under test.

## Screenshots of a UI change

A diff that changes something a user sees (dashboard, web pages, docs site,
desktop app UI) follows the workspace guide's screenshot evidence rule for
task artifacts. This review evidence follows the diff, not the `ui` tag.

Publishing screenshots in the PR body is optional: if
the task is tagged `ui` and none are attached, Orbit writes `No
screenshots attached.` and does not draft or refuse the PR for that reason.
`.orbit/tmp/` is cleared with the worktree, so a screenshot left only there is
lost evidence.

- Capture every theme the view supports (light and dark where both exist),
  plus a 375px phone width when the layout is responsive.
- Add a before shot from the base revision when it is practical and it shows
  the change.
- Write each under `.orbit/tmp/` (`$ORBIT_SCRATCH_DIR`) and give it a stable
  stored path such as `screenshots/<view>-<theme>-<width>[-before|-after].<ext>`.
  A file with no suffix is an After shot, and the Before shot stays optional.
  Owner mode
  attaches screenshots with `orbit.task.artifact` action `put`.
  - A claimed implementer
    returns each screenshot in `task_artifacts`, setting `path` to its stable
    stored path and `source_path` to its file under `.orbit/tmp/`, for
    deterministic attachment. Use this task-artifact channel for diff and
    no-diff candidates, including `no-diff-expected` tasks; keep
    `no_diff_artifacts` for a clean-tree report and its declared validation logs
    only.
- Keep each under the 1 MiB per-artifact limit: prefer a viewport capture to a
  full-page one, or JPEG or WebP when a PNG is too large.
- List the artifact paths in the execution summary.
- Orbit publishes these artifacts into the PR's `## Screenshots` table when
  the task is tagged `ui`. `pr_open` commits attached shots to the
  task's permanent evidence ref `refs/orbit/evidence/<task-id>` and renders
  the Before/After table in the PR body; an empty section is optional. Do not
  push your own evidence branch:
  Orbit replaces the hand-pushed evidence branch used for
  constellation-works/orbit#6155.

When the host has no browser kit or the view cannot render, say so in the
handoff instead of claiming visual verification.
