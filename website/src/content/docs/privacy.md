---
title: Privacy Policy
description: "What Orbit, its MCP server, its agent plugins, and orbit-cli.com do with your data."
tableOfContents:
  minHeadingLevel: 2
  maxHeadingLevel: 2
---

**Effective date: 24 September 2026**

This policy covers the Orbit project maintained by
[constellation-works](https://github.com/constellation-works):

- the `orbit` command-line tool, installed from GitHub Releases, `install.sh`,
  or the `@orbit-tools/cli` npm package;
- the Orbit MCP server (`orbit mcp serve` and `orbit mcp listen`);
- the Orbit plugins for Claude Code, Codex, and Cursor; and
- this website, `orbit-cli.com`.

## Summary

Orbit runs on your machine. We do not operate a service that receives your data.
The CLI, MCP server, and plugins send no telemetry, analytics, usage statistics, or
crash reports to us or to anyone else. Your data leaves your machine only through
tools you configure and commands you run. Those tools include your chosen AI coding
agent, your Git host, and npm, and each handles data under its own terms.

## Data Orbit stores on your machine

Orbit keeps its state in local files and local SQLite databases:

- **Global root** (`~/.orbit`, or `ORBIT_REGISTRY_ROOT` if set): configuration,
  the workspace registry, task records, the audit log, run history, logs, and
  saved MCP destinations.
- **Repository root** (`.orbit/` in each workspace): workspace configuration,
  resources, runtime state, the local search index, and agent worktrees.

This data includes task titles, descriptions, and comments. It also includes audit
events, agent invocation records such as token counts and cost estimates, and the
output of agent runs. It stays on your disk and is never uploaded to us. Before
logs, audit records, and captured agent error output are written, Orbit redacts
values that look like credentials.

Orbit does not store API keys for AI providers. Agent processes start with an empty
environment. Orbit then adds only the variables your configuration allows.

## When data leaves your machine

Every outbound connection Orbit makes happens because you configured a tool or ran
a command. The destinations are listed below.

### AI coding agents you choose

Orbit runs agent work by launching the command-line tool of the provider you
configure. The supported tools are Claude Code, Codex, Gemini CLI, Antigravity,
Grok, GitHub Copilot, Cursor Agent, Pi, OpenCode, and Ollama. Orbit passes each
agent your task's context, such as the task description, prompts, and files in the
workspace. The agent may send that content to its provider, and the provider's own
terms and privacy policy apply. Orbit itself does not contact any AI provider's
API.

### Your Git host

Delivery workflows run `git push` and `git fetch` against the remotes of your
repository. They also use the GitHub CLI (`gh`) to open, update, and merge pull
requests and to read code-scanning and Dependabot alerts. These requests go to the
Git host you configured, under that host's terms. The scheduled routines that ship
with Orbit are all disabled until you enable them.

### Task publication (optional)

`orbit task publication publish` pushes a snapshot of your task records to a
separate Git repository that you bind to the workspace. It runs only when you run
it. Orbit refuses remote URLs that contain credentials. Attachments are never
published unless you pass `--attachments include`, and even then files such as
`.env`, `*.pem`, `*.key`, and `credentials.json` are rejected.

### Installing and updating Orbit

- **npm.** Installing `@orbit-tools/cli`, including when a plugin runs it with
  `npx`, downloads the package from the npm registry. The package's install script
  then downloads the matching Orbit binary, its checksum, and its signature from
  GitHub Releases, and verifies them. Its requests send only a fixed
  `@orbit-tools/cli installer` User-Agent. `ORBIT_SKIP_DOWNLOAD=1` or
  `ORBIT_BINARY` skips that download.
- **`install.sh`** downloads release files from GitHub Releases, or from
  `ORBIT_INSTALL_BASE_URL` if you set it.
- **`orbit update`** asks the GitHub Releases API for the latest version and
  downloads it. It sends an `orbit-cli/<version>` User-Agent. It runs only when
  you invoke it; Orbit never checks for updates in the background.
  `ORBIT_UPDATE_RELEASE_DIR` points it at a local mirror instead.

GitHub and npm receive the usual request metadata, such as your IP address, under
their own terms.

### Other commands you run

- `orbit plugin add` with an `https://` or `git+` source downloads that source
  with `curl` or `git`.
- `orbit web connect`, remote MCP destinations, and `~/.orbit/mcp-destinations.toml`
  entries connect over SSH to hosts you name.

## Local servers

- `orbit mcp serve`, the MCP server that the plugins start, talks to your agent
  client over standard input and output. It opens no network port.
- `orbit mcp listen` binds `127.0.0.1:7879` by default and refuses any other
  interface unless you pass `--allow-non-loopback`.
- `orbit web serve` (the dashboard) binds `127.0.0.1:7878` and refuses addresses
  that are not loopback. Its content security policy lets pages load scripts,
  styles, and other resources only from the dashboard itself.

## Agent plugins

The Claude Code, Codex, and Cursor plugins contain skills (Markdown instructions),
an MCP configuration, and, for Claude Code, a session-start hook.

- The MCP configuration starts `npx -y @orbit-tools/cli@<version> mcp serve`
  locally. The npm download is covered in [Installing and updating Orbit](#installing-and-updating-orbit).
- The session-start hook reads your project directory and working directory and
  checks whether an `.orbit/` workspace exists there. It makes no network calls.
  If no workspace is found, it shows a fixed message.

The MCP server gives your agent access to your local Orbit tasks and tools. What
the agent then does with that content is governed by the agent's provider, as
described in [AI coding agents you choose](#ai-coding-agents-you-choose).

## This website

`orbit-cli.com` is a static site hosted on Cloudflare Pages.

- It has no analytics, tracking pixels, advertising, or third-party scripts, and
  it loads no fonts or other assets from other domains.
- The site's code sets no cookies. It stores one value, your light or dark theme
  choice (`starlight-theme`), in your browser's local storage. That value never
  leaves your browser.
- Site search runs in your browser against an index served from this site. Your
  search queries are not sent anywhere.
- Cloudflare, as the host, processes the standard data of each request, such as
  IP address, user agent, and requested URL, to serve and protect the site, under
  [Cloudflare's privacy policy](https://www.cloudflare.com/privacypolicy/).
  Cloudflare may set strictly necessary security cookies.
- Links to GitHub and other sites are governed by those sites' policies.

## Children

Orbit is a developer tool and is not directed at children.

## Changes to this policy

When this policy changes, we update the effective date at the top of this page.
Every revision is recorded in the page's history in the
[Orbit repository](https://github.com/constellation-works/orbit/commits/main/website/src/content/docs/privacy.md).

## Contact

Questions about privacy go to the Orbit maintainers:

- Private reports and sensitive questions:
  [open a private advisory](https://github.com/constellation-works/orbit/security/advisories/new)
  (the contact listed in [`security.txt`](/.well-known/security.txt)).
- General questions: [GitHub issues](https://github.com/constellation-works/orbit/issues).
