---
title: Privacy Policy
description: "What Orbit, its MCP server, its agent plugins, and orbit-cli.com do with your data."
tableOfContents:
  minHeadingLevel: 2
  maxHeadingLevel: 2
---

**Effective date: 4 October 2026**

This policy covers the Orbit project maintained by
[constellation-works](https://github.com/constellation-works):

- the `orbit` command-line tool, installed from GitHub Releases, `install.sh`,
  or the `@orbit-tools/cli` npm package;
- the Orbit MCP server (`orbit mcp serve` and `orbit mcp listen`);
- the Orbit plugins for Claude Code, Codex, and Cursor; and
- this website, `orbit-cli.com`.

## Summary

Orbit runs on your machine. We run no service that receives your data. The CLI,
MCP server, and plugins send no telemetry, analytics, usage statistics, or crash
reports to us or to anyone else.

Your data leaves your machine only through tools you configure and commands you
run, such as your AI coding agent, your Git host, and npm. Each handles that data
under its own terms.

## Data Orbit stores on your machine

Orbit keeps its state in local files and local SQLite databases:

- **Global root** (`~/.orbit`, or `ORBIT_REGISTRY_ROOT` if set): configuration,
  the workspace registry, task records, the audit log, run history, logs, and
  saved MCP destinations.
- **Repository root** (`.orbit/` in each workspace): workspace configuration,
  resources, runtime state, the local search index, and agent worktrees.

That state includes task titles, descriptions, and comments; audit events; agent
invocation records, such as token counts and cost estimates; and the output of
agent runs. It stays on your disk and is never uploaded to us. Orbit redacts
values that look like credentials before it writes logs, audit records, and
captured agent error output.

Orbit does not store API keys for AI providers. Agent processes start with an
empty environment, and Orbit adds only the variables your configuration allows.

## When data leaves your machine

Orbit makes an outbound connection only because you configured a tool or ran a
command. These are the destinations.

### AI coding agents you choose

Orbit runs agent work by launching the CLI of the provider you configure: Claude
Code, Codex, Gemini CLI, Antigravity, Grok, GitHub Copilot, Cursor Agent, Pi, or
OpenCode. It passes the agent your task's context, such as the task
description, prompts, and workspace files. The agent may send that content to
its provider, under the provider's own terms and privacy policy. Orbit itself
does not contact any AI provider's API.

### Your Git host

Delivery workflows run `git push` and `git fetch` against your repository's
remotes. They use the GitHub CLI (`gh`) to open, update, and merge pull requests
and to read code-scanning and Dependabot alerts. These requests go to the Git
host you configured, under that host's terms. The scheduled routines that ship
with Orbit are all disabled until you enable them.

### Task publication (optional)

`orbit task publication publish` pushes a snapshot of your task records to a
separate Git repository that you bind to the workspace. It runs only when you
run it. Orbit refuses remote URLs that contain credentials. Attachments are
published only if you pass `--attachments include`, and even then files such as
`.env`, `*.pem`, `*.key`, and `credentials.json` are rejected.

### Installing and updating Orbit

- **npm.** Installing `@orbit-tools/cli`, including when a plugin runs it with
  `npx`, downloads the package from the npm registry. Its install script then
  downloads the matching Orbit binary, checksum, and signature from GitHub
  Releases and verifies them. These requests send only a fixed
  `@orbit-tools/cli installer` User-Agent. `ORBIT_SKIP_DOWNLOAD=1` or
  `ORBIT_BINARY` skips the download.
- **`install.sh`** downloads release files from GitHub Releases, or from
  `ORBIT_INSTALL_BASE_URL` if you set it.
- **`orbit update`** asks the GitHub Releases API for the latest version and
  downloads it, sending an `orbit-cli/<version>` User-Agent. It runs only when
  you invoke it; Orbit never checks for updates in the background.
  `ORBIT_UPDATE_RELEASE_DIR` points it at a local mirror instead.

GitHub and npm receive the usual request metadata, such as your IP address,
under their own terms.

### Other commands you run

- `orbit plugin add` with an `https://` or `git+` source downloads that source
  with `curl` or `git`.
- `orbit web connect`, remote MCP destinations, and the hosts you register
  with `orbit host add` connect over SSH to hosts you name. `orbit host add`,
  `orbit host list`, `orbit host show` and `orbit doctor` open an SSH session
  to each registered host to read its identity, version and workspaces.

## Local servers

- `orbit mcp serve`, the MCP server the plugins start, talks to your agent
  client over standard input and output. It opens no network port.
- `orbit mcp listen` binds `127.0.0.1:7879` by default and refuses any other
  interface unless you pass `--allow-non-loopback`.
- `orbit web serve` (the dashboard) binds `127.0.0.1:7878` and refuses
  addresses that are not loopback. Its content security policy lets pages load
  scripts, styles, and other resources only from the dashboard itself.

## Agent plugins

The Claude Code, Codex, and Cursor plugins contain skills (Markdown
instructions) and an MCP configuration. The Claude Code plugin also has a
session-start hook and a mod that shows your Orbit tasks in the session.

- The MCP configuration starts `npx -y @orbit-tools/cli@<version> mcp serve`
  locally. [Installing and updating Orbit](#installing-and-updating-orbit)
  covers the npm download.
- The session-start hook checks whether your project or working directory is
  inside an `.orbit/` workspace. If it is not, the hook shows a fixed message.
  It makes no network calls.
- The mod reads and changes tasks by running `orbit` on your machine. When the
  checkout is a replica or is not registered on this machine, it runs `orbit`
  over SSH instead: on the host you set in the plugin's `ownerHost` option, or
  on a host registered with `orbit host add`.

The MCP server gives your agent access to your local Orbit tasks and tools. What
the agent then does with that content is governed by its provider; see
[AI coding agents you choose](#ai-coding-agents-you-choose).

## This website

`orbit-cli.com` is a static site hosted on Cloudflare Pages.

- It has no analytics, tracking pixels, advertising, or third-party scripts, and
  loads no fonts or other assets from other domains.
- The site's code sets no cookies. Dark mode is the default and writes nothing
  to browser storage. If you use the theme toggle, your light or dark choice is
  stored in local storage as `orbit-theme-choice` and never leaves your
  browser. An earlier version stored a `starlight-theme` value automatically;
  the site now ignores it.
- Site search runs in your browser against an index served from this site. Your
  search queries are not sent anywhere.
- Cloudflare, as the host, processes the standard data of each request, such as
  IP address, user agent, and requested URL, to serve and protect the site,
  under [Cloudflare's privacy policy](https://www.cloudflare.com/privacypolicy/).
  Cloudflare may set strictly necessary security cookies.
- Links to GitHub and other sites are governed by those sites' policies.

## Children

Orbit is a developer tool and is not directed at children.

## Changes to this policy

When this policy changes, we update the effective date at the top of this page.
The page's history in the
[Orbit repository](https://github.com/constellation-works/orbit/commits/main/website/src/content/docs/privacy.md)
records every revision.

## Contact

Send privacy questions to the Orbit maintainers:

- Private reports and sensitive questions:
  [open a private advisory](https://github.com/constellation-works/orbit/security/advisories/new)
  (the contact listed in [`security.txt`](/.well-known/security.txt)).
- General questions: [GitHub issues](https://github.com/constellation-works/orbit/issues).
