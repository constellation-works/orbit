# Orbit mod for Claude Code

The mod links task and run IDs in assistant replies to the Orbit dashboard.
It rewrites the displayed Markdown and leaves the stored transcript intact.
IDs in code, URLs and existing links stay as written.

At session start it reads the local registry with
`orbit host list --no-probe --json` and the dashboard origin with
`orbit config get machine.web.public_url --scope global --json`. The two reads
have a two-second timeout each. The registry includes local and remote prefixes
stored on this machine. If the read fails, run links still work. Open a new
session to pick up registry or address changes.

Task links use `/t/<id>` and the dashboard resolves the prefix. Run links use
`/r/<id>` and open a run on the machine serving the dashboard. The default
origin is `http://localhost:7878`. Set `machine.web.public_url` to the actual
origin when using a remote dashboard or another port.

## Verification

Run the pure Markdown cases with Node 22.18 or later from the repository root.

```bash
node plugin/hooks/mod/tests/linkify.mjs
claude plugin validate plugin
```

`make ci-guards` runs the Node cases without Claude Code.

### Manual Claude Code QA

1. Load the base plugin and the candidate in separate sessions in the same
   initialized test workspace. For the terminal, use
   `claude --plugin-dir /path/to/checkout/plugin`. On each available Claude
   surface, ask for a reply with a task ID from the local registry, one with a
   registered remote prefix, `STD-12`, and a known run ID. Ask for the same IDs
   in a fenced command and one already linked in Markdown. Use a known task
   and run for click-through.
2. Check that the candidate draws prose IDs as links with the original labels.
   Open the local and remote task links and confirm the dashboard selects the
   right task and host. Open the run link and confirm it selects the run on
   the serving machine. The unknown prefix, code and pre-existing link should
   be unchanged. Capture the same transcript and prompt area before and after
   in each supported theme. Check that the band, status entry and slash
   commands are gone.
3. Start fresh sessions with an unset and a configured
   `machine.web.public_url`. Check for the local and configured origins,
   respectively. With a test CLI that fails the host list but serves the
   configured origin, check that run links still use that origin and task IDs
   stay plain. Record the commands at startup and leave the session idle past
   the former three-minute poll. Expect one host-list read and one config read,
   with no later mod commands.
4. Open a fresh session outside an initialized workspace. Check that Claude
   still receives the session-start workspace setup guidance.
