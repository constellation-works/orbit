# macOS: the Claude worker token and the clock env file

Use this on a Mac that runs unattended Claude activities (implementers, final
recovery, the task pilot, reviewers) or has the sweep clock installed.

## Why a worker token

Without `CLAUDE_CODE_OAUTH_TOKEN` (or `ANTHROPIC_API_KEY`) the `claude` CLI
falls back to the Claude Desktop app's shared OAuth login. The Desktop revokes
that login whenever it refreshes, so a run that started on it fails part-way
with `claude provider authentication failure (HTTP 401): OAuth token revoked`
and the task goes `blocked`. Unattended runs use a dedicated worker token
instead. Orbit enforces this: on macOS a Claude activity whose provider
environment holds neither variable is refused at invocation, before `claude`
starts, with an error naming the variable.

## Set it up

1. Create the token once: `claude setup-token`.
2. Export it for login shells in `~/.zprofile`:
   `export CLAUDE_CODE_OAUTH_TOKEN=<token>`. Start drains from a login shell
   (`zsh -l -c 'orbit run auto …'`); the non-login shell an agent tool opens
   does not read `~/.zprofile`.
3. Admit it to agents. Every workspace uses the global
   `[execution.env] pass` unless its own `config.toml` sets `pass`, which
   *replaces* the global list. Add `CLAUDE_CODE_OAUTH_TOKEN` to the list each
   workspace actually resolves (`orbit config show`); a name missing there is
   stripped even when the process holds it.
4. Give the clock the same token, below.

## The clock env file

launchd starts `orbit clock tick` with no login environment, so runs the clock
starts (`ship-sweep-*`, auto-tasks) never see `~/.zprofile`. The unit file
holds no secret. Instead the tick reads `~/.orbit/clock.env`:

```bash
orbit routine init --install-clock   # also creates ~/.orbit/clock.env (0600, comments only) if absent
$EDITOR ~/.orbit/clock.env           # CLAUDE_CODE_OAUTH_TOKEN=<token>
```

- One `NAME=value` per line; `#` comments, a leading `export `, and one pair of
  quotes around the value are accepted. Nothing is expanded.
- Only names in the effective `execution.env.pass` of the workspaces the tick
  evaluates are loaded; everything else in the file is ignored. `ORBIT_*`
  names are never loaded.
- A value already set in the tick's own environment wins over the file.
- The tick refuses a file that is a symlink, not owned by the user, or readable
  by group or others (`chmod 600 ~/.orbit/clock.env`). It reports a
  `clock.env` load error and still runs; Claude activities then fail with the
  missing-token error above.
- `orbit clock tick --json` lists the names it loaded as `clock_env_loaded`
  (never values). A `--dry-run` loads nothing.

Linux uses the same file and the same loader, so the systemd unit is unchanged.
A systemd user service has no login environment either, so put there any
pass-listed credential the timer-started runs need.

## Verify

`orbit doctor` has a `claude-worker-token` row. On macOS, when a routed crew
uses Claude, it warns if the workspace's effective `pass` omits the token or
the installed clock has no token in `clock.env` that the same `pass` admits (a
`clock.env` holding only a name the workspace's `pass` omits is a mismatch, and
the row names it). `env-pass` separately reports
pass-listed names the current shell does not hold.
