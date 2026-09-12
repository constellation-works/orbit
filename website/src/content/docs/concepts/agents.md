---
title: Agents
description: "How Orbit invokes coding agents through provider CLIs, which executors ship today, and how to write a crew for one."
sidebar:
  order: 6
head:
  - tag: style
    content: |
      .orbit-setup-explorer {
        --ose-line: var(--sl-color-hairline-light);
        max-width: 100%;
        margin: 1.5rem 0 2rem;
        padding: 1.1rem;
        border: 1px solid var(--ose-line);
        border-radius: 10px;
        background: var(--sl-color-bg);
      }
      .orbit-setup-explorer * { min-width: 0; }
      .ose-field { margin: 0 0 1.1rem; padding: 0; border: 0; }
      .ose-field > legend {
        margin-bottom: 0.5rem;
        padding: 0;
        font-size: var(--sl-text-sm);
        font-weight: 600;
        color: var(--sl-color-white);
      }
      .ose-lede {
        margin: 0 0 1rem;
        font-size: var(--sl-text-sm);
        line-height: 1.6;
        color: var(--sl-color-gray-3);
      }
      .ose-choices { display: flex; flex-wrap: wrap; gap: 0.4rem; }
      .ose-choice { position: relative; display: inline-flex; }
      .ose-choice input {
        position: absolute;
        width: 1px;
        height: 1px;
        margin: 0;
        opacity: 0;
        pointer-events: none;
      }
      .ose-choice label {
        display: inline-flex;
        align-items: baseline;
        gap: 0.4rem;
        padding: 0.35rem 0.75rem;
        border: 1px solid var(--ose-line);
        border-radius: 999px;
        background: var(--sl-color-bg-inline-code);
        color: var(--sl-color-gray-2);
        font-family: var(--sl-font-mono);
        font-size: var(--sl-text-xs);
        line-height: 1.5;
        cursor: pointer;
      }
      .ose-choice label:hover { border-color: var(--sl-color-gray-4); }
      .ose-choice input:checked + label {
        border-color: var(--sl-color-text-accent);
        background: var(--sl-color-accent-low);
        color: var(--sl-color-white);
      }
      .ose-choice input:focus-visible + label {
        outline: 2px solid var(--sl-color-text-accent);
        outline-offset: 2px;
      }
      .ose-choice-note {
        font-family: var(--sl-font);
        font-size: 0.7rem;
        letter-spacing: 0.04em;
        text-transform: uppercase;
        color: var(--sl-color-gray-3);
      }
      .ose-panel {
        display: none;
        padding-top: 1.1rem;
        border-top: 1px solid var(--ose-line);
      }
      .orbit-setup-explorer:has(#ose-p-claude:checked) #ose-panel-claude,
      .orbit-setup-explorer:has(#ose-p-codex:checked) #ose-panel-codex,
      .orbit-setup-explorer:has(#ose-p-antigravity:checked) #ose-panel-antigravity,
      .orbit-setup-explorer:has(#ose-p-gemini:checked) #ose-panel-gemini,
      .orbit-setup-explorer:has(#ose-p-grok:checked) #ose-panel-grok,
      .orbit-setup-explorer:has(#ose-p-copilot:checked) #ose-panel-copilot,
      .orbit-setup-explorer:has(#ose-p-cursor:checked) #ose-panel-cursor,
      .orbit-setup-explorer:has(#ose-p-pi:checked) #ose-panel-pi,
      .orbit-setup-explorer:has(#ose-p-opencode:checked) #ose-panel-opencode { display: block; }
      .ose-facts {
        display: grid;
        grid-template-columns: max-content minmax(0, 1fr);
        gap: 0.35rem 1.1rem;
        margin: 0 0 1.1rem;
        font-size: var(--sl-text-sm);
      }
      .ose-facts dt { color: var(--sl-color-gray-3); }
      .ose-facts dd {
        margin: 0;
        color: var(--sl-color-white);
        overflow-wrap: anywhere;
      }
      .ose-facts code, .ose-note code, .ose-lede code {
        padding: 0.1rem 0.3rem;
        border-radius: 4px;
        background: var(--sl-color-bg-inline-code);
        font-family: var(--sl-font-mono);
        font-size: 0.9em;
      }
      .ose-code {
        margin: 0;
        padding: 0.85rem 1rem;
        overflow-x: auto;
        border: 1px solid var(--ose-line);
        border-radius: 8px;
        background: var(--sl-color-bg-inline-code);
        color: var(--sl-color-white);
        font-family: var(--sl-font-mono);
        font-size: var(--sl-text-xs);
        line-height: 1.6;
      }
      .ose-code code {
        padding: 0;
        border: 0;
        background: none;
        font: inherit;
        color: inherit;
      }
      .ose-eff { display: none; }
      .ose-panel:has(.ose-eff-radio[value="low"]:checked) .ose-eff[data-effort="low"],
      .ose-panel:has(.ose-eff-radio[value="medium"]:checked) .ose-eff[data-effort="medium"],
      .ose-panel:has(.ose-eff-radio[value="high"]:checked) .ose-eff[data-effort="high"],
      .ose-panel:has(.ose-eff-radio[value="xhigh"]:checked) .ose-eff[data-effort="xhigh"],
      .ose-panel:has(.ose-eff-radio[value="max"]:checked) .ose-eff[data-effort="max"] { display: inline; }
      .ose-actions {
        display: none;
        flex-wrap: wrap;
        align-items: center;
        gap: 0.75rem;
        margin-top: 0.7rem;
      }
      .ose-js .ose-actions { display: flex; }
      .ose-copy {
        padding: 0.35rem 0.85rem;
        border: 1px solid var(--ose-line);
        border-radius: 6px;
        background: var(--sl-color-bg);
        color: var(--sl-color-gray-2);
        font-family: var(--sl-font);
        font-size: var(--sl-text-xs);
        cursor: pointer;
      }
      .ose-copy:hover { border-color: var(--sl-color-text-accent); color: var(--sl-color-white); }
      .ose-copy:focus-visible { outline: 2px solid var(--sl-color-text-accent); outline-offset: 2px; }
      .ose-status { font-size: var(--sl-text-xs); color: var(--sl-color-gray-3); }
      .ose-status[data-state="error"] { color: var(--sl-color-text-accent); }
      .ose-note {
        margin: 1rem 0 0;
        padding: 0.6rem 0.85rem;
        border-left: 3px solid var(--sl-color-hairline);
        border-radius: 0 6px 6px 0;
        background: var(--sl-color-bg-inline-code);
        font-size: var(--sl-text-sm);
        line-height: 1.6;
        color: var(--sl-color-gray-2);
      }
      .ose-note p { margin: 0 0 0.5rem; }
      .ose-note p:last-child { margin-bottom: 0; }
      @media (max-width: 30rem) {
        .orbit-setup-explorer { padding: 0.85rem; }
        .ose-facts { grid-template-columns: minmax(0, 1fr); gap: 0.1rem; }
        .ose-facts dt { margin-top: 0.55rem; }
      }
---

## Runtime Paths

Orbit spawns official provider CLIs as supervised subprocesses under an
`FsProfile` and policy guardrails. The agent CLI is responsible for talking to
its provider.

This is the only agent execution path. The `backend: http | cli | auto`
selector was retired: an activity, job, or config that still declares
`backend: cli` keeps working and the value is ignored, while `http` and `auto`
are rejected with a migration message rather than being remapped onto the CLI
agent.

## Providers

A **provider** is a canonical agent family id. An **executor** is the shipped
spawn recipe — command, static flags, output format, sandbox backend — that
Orbit uses to run that family. The two are separate: a provider id can be
recognized by Orbit without a CLI executor existing for it, and one executor
(`local-shell`) runs no agent at all.

Every canonical provider id, and what it can actually execute today:

| Provider | Executor command | Reasoning `effort` | Status |
|---|---|---|---|
| `claude` | `claude` | `low`–`max`, via `--effort` | Active |
| `codex` | `codex exec` | `low`–`max`, via `--config model_reasoning_effort` | Active |
| `antigravity` | `agy` | `low`, `medium`, `high`, via `--effort` | Active |
| `grok` | `grok` | Model-specific, via `--reasoning-effort` | Active |
| `copilot` | `copilot` | Not supported | Active |
| `cursor` | `cursor-agent` | Not supported | Active |
| `pi` | `pi` | `low`–`max`, via `--thinking` | Active |
| `opencode` | `opencode run` | `high`, `max`, via `--variant` | Active |
| `gemini` | `gemini` | Not supported | Legacy |
| `ollama` | — | Not supported | No shipped crew or executor |
| `openai_compat` | — | Not supported | No CLI runtime; dispatch fails |

`openai_compat` has no CLI runtime, so dispatching to it fails structurally
instead of falling back to another family. `ollama` is a recognized provider
id, but Orbit ships no `ollama` executor definition and no `ollama` crew, and
`orbit init` never seeds one — nothing gives it a headless agent contract.
Neither id is rewritten: a crew naming one loads, and the refusal comes at
dispatch rather than the run being quietly re-pointed at a family that does
have an executor.

Orbit ships one more executor, **`local-shell`**, which is not a provider. It
runs deterministic local commands for `local_shell` steps and carries no model,
prompt, or agent tool authority, so it is never named by a crew. Unlike the
agent executors it declares no sandbox by default; see
[Platform Support](#platform-support).

### Deprecated aliases

Five legacy vendor spellings still resolve, and keep resolving so persisted
identities are never broken — but they are deprecated and emit a warning:

| Alias | Resolves to |
|---|---|
| `anthropic` | `claude` |
| `openai`, `chatgpt` | `codex` |
| `google` | `gemini` |
| `xai` | `grok` |

`openai-compat` is an accepted, non-deprecated spelling of `openai_compat`. The
alias table is closed: any other string is an unknown provider and is rejected
rather than guessed at. In particular, the model vendor named *inside* a Pi or
OpenCode run (`pi --provider anthropic`, `opencode --model anthropic/...`) is
not an Orbit provider id and never re-points the run at another execution lane.

## Set up an executor

Pick an executor to see what it needs and a starter crew you can paste into
`config.toml`. Every snippet below is a complete example file and is selectable
whether or not JavaScript is enabled; the Copy button is an enhancement.

<div class="orbit-setup-explorer not-content">
<p class="ose-lede">These are <strong>examples</strong>, not availability guarantees. Orbit passes the <code>model</code> string to the provider CLI verbatim — whether your account can actually run it is between you and that provider. Orbit does not read, store, or send credentials; authenticate each CLI with its own vendor instructions first.</p>
<fieldset class="ose-field">
<legend>1. Executor</legend>
<div class="ose-choices">
<span class="ose-choice"><input type="radio" name="ose-provider" id="ose-p-claude" value="claude" checked><label for="ose-p-claude">claude</label></span>
<span class="ose-choice"><input type="radio" name="ose-provider" id="ose-p-codex" value="codex"><label for="ose-p-codex">codex</label></span>
<span class="ose-choice"><input type="radio" name="ose-provider" id="ose-p-antigravity" value="antigravity"><label for="ose-p-antigravity">antigravity</label></span>
<span class="ose-choice"><input type="radio" name="ose-provider" id="ose-p-grok" value="grok"><label for="ose-p-grok">grok</label></span>
<span class="ose-choice"><input type="radio" name="ose-provider" id="ose-p-copilot" value="copilot"><label for="ose-p-copilot">copilot</label></span>
<span class="ose-choice"><input type="radio" name="ose-provider" id="ose-p-cursor" value="cursor"><label for="ose-p-cursor">cursor</label></span>
<span class="ose-choice"><input type="radio" name="ose-provider" id="ose-p-pi" value="pi"><label for="ose-p-pi">pi</label></span>
<span class="ose-choice"><input type="radio" name="ose-provider" id="ose-p-opencode" value="opencode"><label for="ose-p-opencode">opencode</label></span>
<span class="ose-choice"><input type="radio" name="ose-provider" id="ose-p-gemini" value="gemini"><label for="ose-p-gemini">gemini <span class="ose-choice-note">legacy</span></label></span>
</div>
</fieldset>
<div class="ose-panel" id="ose-panel-claude">
<dl class="ose-facts">
<dt>Binary on <code>PATH</code></dt><dd><code>claude</code></dd>
<dt>Example model</dt><dd><code>opus</code></dd>
<dt>Reasoning effort</dt><dd>Supported — the full crew vocabulary, rendered as <code>claude --effort &lt;value&gt;</code>.</dd>
</dl>
<fieldset class="ose-field">
<legend>2. Reasoning effort (optional)</legend>
<div class="ose-choices">
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-claude" id="ose-e-claude-none" value="none" checked><label for="ose-e-claude-none">omit</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-claude" id="ose-e-claude-low" value="low"><label for="ose-e-claude-low">low</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-claude" id="ose-e-claude-medium" value="medium"><label for="ose-e-claude-medium">medium</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-claude" id="ose-e-claude-high" value="high"><label for="ose-e-claude-high">high</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-claude" id="ose-e-claude-xhigh" value="xhigh"><label for="ose-e-claude-xhigh">xhigh</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-claude" id="ose-e-claude-max" value="max"><label for="ose-e-claude-max">max</label></span>
</div>
</fieldset>
<div class="ose-snippet">

<pre class="ose-code"><code>[workflow]
default_crew = "opus"

[crews.opus]
provider = "claude"
model = "opus"
<span class="ose-eff" data-effort="low">effort = "low"
</span><span class="ose-eff" data-effort="medium">effort = "medium"
</span><span class="ose-eff" data-effort="high">effort = "high"
</span><span class="ose-eff" data-effort="xhigh">effort = "xhigh"
</span><span class="ose-eff" data-effort="max">effort = "max"
</span></code></pre>

<div class="ose-actions"><button class="ose-copy" type="button">Copy config</button><span class="ose-status" role="status" aria-live="polite"></span></div>
</div>
<div class="ose-note"><p>Omitting <code>effort</code> leaves the model's own default alone — Orbit passes no effort argument at all.</p></div>
</div>
<div class="ose-panel" id="ose-panel-codex">
<dl class="ose-facts">
<dt>Binary on <code>PATH</code></dt><dd><code>codex</code></dd>
<dt>Example model</dt><dd><code>gpt-5.6-sol</code></dd>
<dt>Reasoning effort</dt><dd>Supported — the full crew vocabulary, rendered as <code>codex exec --config model_reasoning_effort="&lt;value&gt;"</code>.</dd>
</dl>
<fieldset class="ose-field">
<legend>2. Reasoning effort (optional)</legend>
<div class="ose-choices">
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-codex" id="ose-e-codex-none" value="none" checked><label for="ose-e-codex-none">omit</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-codex" id="ose-e-codex-low" value="low"><label for="ose-e-codex-low">low</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-codex" id="ose-e-codex-medium" value="medium"><label for="ose-e-codex-medium">medium</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-codex" id="ose-e-codex-high" value="high"><label for="ose-e-codex-high">high</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-codex" id="ose-e-codex-xhigh" value="xhigh"><label for="ose-e-codex-xhigh">xhigh</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-codex" id="ose-e-codex-max" value="max"><label for="ose-e-codex-max">max</label></span>
</div>
</fieldset>
<div class="ose-snippet">

<pre class="ose-code"><code>[workflow]
default_crew = "sol"

[crews.sol]
provider = "codex"
model = "gpt-5.6-sol"
<span class="ose-eff" data-effort="low">effort = "low"
</span><span class="ose-eff" data-effort="medium">effort = "medium"
</span><span class="ose-eff" data-effort="high">effort = "high"
</span><span class="ose-eff" data-effort="xhigh">effort = "xhigh"
</span><span class="ose-eff" data-effort="max">effort = "max"
</span></code></pre>

<div class="ose-actions"><button class="ose-copy" type="button">Copy config</button><span class="ose-status" role="status" aria-live="polite"></span></div>
</div>
<div class="ose-note"><p>Codex sandbox mode and approval policy are separate settings — see <a href="../../reference/config/#settable-keys"><code>execution.codex.*</code></a>.</p></div>
</div>
<div class="ose-panel" id="ose-panel-antigravity">
<dl class="ose-facts">
<dt>Binary on <code>PATH</code></dt><dd><code>agy</code></dd>
<dt>Example model</dt><dd><code>gemini-3.8-flash-high</code></dd>
<dt>Reasoning effort</dt><dd>Supported for <code>low</code>, <code>medium</code>, <code>high</code> only, rendered as <code>agy --effort &lt;value&gt;</code>.</dd>
</dl>
<fieldset class="ose-field">
<legend>2. Reasoning effort (optional)</legend>
<div class="ose-choices">
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-antigravity" id="ose-e-antigravity-none" value="none" checked><label for="ose-e-antigravity-none">omit</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-antigravity" id="ose-e-antigravity-low" value="low"><label for="ose-e-antigravity-low">low</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-antigravity" id="ose-e-antigravity-medium" value="medium"><label for="ose-e-antigravity-medium">medium</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-antigravity" id="ose-e-antigravity-high" value="high"><label for="ose-e-antigravity-high">high</label></span>
</div>
</fieldset>
<div class="ose-snippet">

<pre class="ose-code"><code>[workflow]
default_crew = "antigravity"

[crews.antigravity]
provider = "antigravity"
model = "gemini-3.8-flash-high"
<span class="ose-eff" data-effort="low">effort = "low"
</span><span class="ose-eff" data-effort="medium">effort = "medium"
</span><span class="ose-eff" data-effort="high">effort = "high"
</span></code></pre>

<div class="ose-actions"><button class="ose-copy" type="button">Copy config</button><span class="ose-status" role="status" aria-live="polite"></span></div>
</div>
<div class="ose-note"><p><code>xhigh</code> and <code>max</code> are deliberately absent: <code>agy --effort</code> does not define them, and Orbit rejects the config rather than quietly downgrading to <code>high</code>.</p><p>Antigravity model ids carry their own effort suffix and must come from <code>agy models</code>. A bare Gemini CLI id such as <code>gemini-3.8-flash</code> is rejected at config load — it is not rewritten into a suffixed slug.</p></div>
</div>
<div class="ose-panel" id="ose-panel-grok">
<dl class="ose-facts">
<dt>Binary on <code>PATH</code></dt><dd><code>grok</code></dd>
<dt>Example model</dt><dd><code>grok-4.6</code></dd>
<dt>Reasoning effort</dt><dd>Supported per model, rendered as <code>grok --reasoning-effort &lt;value&gt;</code>.</dd>
</dl>
<fieldset class="ose-field">
<legend>2. Reasoning effort for <code>grok-4.6</code> (optional)</legend>
<div class="ose-choices">
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-grok" id="ose-e-grok-none" value="none" checked><label for="ose-e-grok-none">omit</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-grok" id="ose-e-grok-low" value="low"><label for="ose-e-grok-low">low</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-grok" id="ose-e-grok-medium" value="medium"><label for="ose-e-grok-medium">medium</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-grok" id="ose-e-grok-high" value="high"><label for="ose-e-grok-high">high</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-grok" id="ose-e-grok-xhigh" value="xhigh"><label for="ose-e-grok-xhigh">xhigh</label></span>
</div>
</fieldset>
<div class="ose-snippet">

<pre class="ose-code"><code>[workflow]
default_crew = "grok"

[crews.grok]
provider = "grok"
model = "grok-4.6"
<span class="ose-eff" data-effort="low">effort = "low"
</span><span class="ose-eff" data-effort="medium">effort = "medium"
</span><span class="ose-eff" data-effort="high">effort = "high"
</span><span class="ose-eff" data-effort="xhigh">effort = "xhigh"
</span></code></pre>

<div class="ose-actions"><button class="ose-copy" type="button">Copy config</button><span class="ose-status" role="status" aria-live="polite"></span></div>
</div>
<div class="ose-note"><p>Grok effort is the one model-specific case. This picker shows the <code>grok-4.6</code> set; <code>grok-4.5</code> accepts <code>low</code>, <code>medium</code>, <code>high</code> and rejects <code>xhigh</code>.</p><p>Effort is verified only for those two models. Setting <code>effort</code> alongside any other Grok model — or alongside no model at all — fails config load instead of being sent to a CLI that might reinterpret it.</p></div>
</div>
<div class="ose-panel" id="ose-panel-copilot">
<dl class="ose-facts">
<dt>Binary on <code>PATH</code></dt><dd><code>copilot</code> (npm <code>@github/copilot</code>)</dd>
<dt>Example model</dt><dd><code>claude-sonnet-5</code></dd>
<dt>Reasoning effort</dt><dd>Not supported.</dd>
</dl>
<div class="ose-snippet">

<pre class="ose-code"><code>[workflow]
default_crew = "copilot"

[crews.copilot]
provider = "copilot"
model = "claude-sonnet-5"
</code></pre>

<div class="ose-actions"><button class="ose-copy" type="button">Copy config</button><span class="ose-status" role="status" aria-live="polite"></span></div>
</div>
<div class="ose-note"><p>No effort picker is shown because Orbit has no verified Copilot effort contract. Adding <code>effort</code> to this crew fails config load with <em>provider 'copilot' does not support configured reasoning effort</em> — the key is never silently dropped.</p><p>The retired <code>gh-copilot</code> gh extension is a different tool and is not an Orbit executor.</p></div>
</div>
<div class="ose-panel" id="ose-panel-cursor">
<dl class="ose-facts">
<dt>Binary on <code>PATH</code></dt><dd><code>cursor-agent</code></dd>
<dt>Example model</dt><dd><code>gpt-5</code></dd>
<dt>Reasoning effort</dt><dd>Not supported.</dd>
</dl>
<div class="ose-snippet">

<pre class="ose-code"><code>[workflow]
default_crew = "cursor"

[crews.cursor]
provider = "cursor"
model = "gpt-5"
</code></pre>

<div class="ose-actions"><button class="ose-copy" type="button">Copy config</button><span class="ose-status" role="status" aria-live="polite"></span></div>
</div>
<div class="ose-note"><p>Adding <code>effort</code> to this crew fails config load rather than being ignored.</p><p>Orbit dispatches to the standalone headless <code>cursor-agent</code> binary. Having the Cursor editor installed is not evidence that this executable is on <code>PATH</code>.</p></div>
</div>
<div class="ose-panel" id="ose-panel-pi">
<dl class="ose-facts">
<dt>Binary on <code>PATH</code></dt><dd><code>pi</code> (npm <code>@earendil-works/pi-coding-agent</code>)</dd>
<dt>Example model</dt><dd><code>sonnet</code></dd>
<dt>Reasoning effort</dt><dd>Supported — the full crew vocabulary, rendered as <code>pi --thinking &lt;value&gt;</code>.</dd>
</dl>
<fieldset class="ose-field">
<legend>2. Reasoning effort (optional)</legend>
<div class="ose-choices">
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-pi" id="ose-e-pi-none" value="none" checked><label for="ose-e-pi-none">omit</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-pi" id="ose-e-pi-low" value="low"><label for="ose-e-pi-low">low</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-pi" id="ose-e-pi-medium" value="medium"><label for="ose-e-pi-medium">medium</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-pi" id="ose-e-pi-high" value="high"><label for="ose-e-pi-high">high</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-pi" id="ose-e-pi-xhigh" value="xhigh"><label for="ose-e-pi-xhigh">xhigh</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-pi" id="ose-e-pi-max" value="max"><label for="ose-e-pi-max">max</label></span>
</div>
</fieldset>
<div class="ose-snippet">

<pre class="ose-code"><code>[workflow]
default_crew = "pi"

[crews.pi]
provider = "pi"
model = "sonnet"
<span class="ose-eff" data-effort="low">effort = "low"
</span><span class="ose-eff" data-effort="medium">effort = "medium"
</span><span class="ose-eff" data-effort="high">effort = "high"
</span><span class="ose-eff" data-effort="xhigh">effort = "xhigh"
</span><span class="ose-eff" data-effort="max">effort = "max"
</span></code></pre>

<div class="ose-actions"><button class="ose-copy" type="button">Copy config</button><span class="ose-status" role="status" aria-live="polite"></span></div>
</div>
<div class="ose-note"><p>Pi's <code>--thinking</code> vocabulary is model-independent and a strict superset of Orbit's, so the whole crew set is accepted here.</p><p>Pi's own <code>--provider</code> flag names the model vendor <em>inside</em> a Pi run. It is not an Orbit provider: <code>provider = "pi"</code> is what selects this lane.</p></div>
</div>
<div class="ose-panel" id="ose-panel-opencode">
<dl class="ose-facts">
<dt>Binary on <code>PATH</code></dt><dd><code>opencode</code></dd>
<dt>Example model</dt><dd><code>anthropic/claude-sonnet-4-5</code></dd>
<dt>Reasoning effort</dt><dd>Supported for <code>high</code> and <code>max</code> only, rendered as <code>opencode run --variant &lt;value&gt;</code>.</dd>
</dl>
<fieldset class="ose-field">
<legend>2. Reasoning effort (optional)</legend>
<div class="ose-choices">
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-opencode" id="ose-e-opencode-none" value="none" checked><label for="ose-e-opencode-none">omit</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-opencode" id="ose-e-opencode-high" value="high"><label for="ose-e-opencode-high">high</label></span>
<span class="ose-choice"><input class="ose-eff-radio" type="radio" name="ose-effort-opencode" id="ose-e-opencode-max" value="max"><label for="ose-e-opencode-max">max</label></span>
</div>
</fieldset>
<div class="ose-snippet">

<pre class="ose-code"><code>[workflow]
default_crew = "opencode"

[crews.opencode]
provider = "opencode"
model = "anthropic/claude-sonnet-4-5"
<span class="ose-eff" data-effort="high">effort = "high"
</span><span class="ose-eff" data-effort="max">effort = "max"
</span></code></pre>

<div class="ose-actions"><button class="ose-copy" type="button">Copy config</button><span class="ose-status" role="status" aria-live="polite"></span></div>
</div>
<div class="ose-note"><p><code>low</code>, <code>medium</code>, and <code>xhigh</code> are missing because <code>opencode run --variant</code> forwards the value straight to whichever model provider <code>--model</code> selected, and OpenCode publishes no provider-independent vocabulary. Only the two spellings its own help text names are accepted; the rest are rejected rather than remapped onto <code>minimal</code> or <code>high</code>.</p><p>The vendor prefix in <code>anthropic/claude-sonnet-4-5</code> is OpenCode's own model addressing. It does not make this an Anthropic lane — <code>provider = "opencode"</code> is what Orbit dispatches through.</p></div>
</div>
<div class="ose-panel" id="ose-panel-gemini">
<dl class="ose-facts">
<dt>Binary on <code>PATH</code></dt><dd><code>gemini</code></dd>
<dt>Example model</dt><dd><code>gemini-3.8-flash</code></dd>
<dt>Reasoning effort</dt><dd>Not supported.</dd>
</dl>
<div class="ose-snippet">

<pre class="ose-code"><code>[workflow]
default_crew = "gemini"

[crews.gemini]
provider = "gemini"
model = "gemini-3.8-flash"
</code></pre>

<div class="ose-actions"><button class="ose-copy" type="button">Copy config</button><span class="ose-status" role="status" aria-live="polite"></span></div>
</div>
<div class="ose-note"><p><strong>This is the legacy Google lane.</strong> Individual Gemini CLI accounts stopped on 2026-06-18; enterprise Gemini Code Assist and API-key authentication remain available on it. New setups should prefer the <code>antigravity</code> executor, which is what <code>orbit init</code> now picks when <code>agy</code> is installed.</p><p>Adding <code>effort</code> to this crew fails config load rather than being ignored.</p></div>
</div>
</div>

<script is:inline>
  (function () {
    var root = document.querySelector(".orbit-setup-explorer");
    if (!root) return;
    // Progressive enhancement: selection is CSS-only, so the Copy control is
    // revealed only once this handler is attached.
    root.classList.add("ose-js");

    function snippetText(pre) {
      var text = "";
      pre.querySelector("code").childNodes.forEach(function (node) {
        if (node.nodeType === 3) {
          text += node.textContent;
        } else if (node.nodeType === 1 && window.getComputedStyle(node).display !== "none") {
          text += node.textContent;
        }
      });
      return text;
    }

    function setStatus(status, message, state) {
      status.textContent = message;
      if (state) {
        status.setAttribute("data-state", state);
      } else {
        status.removeAttribute("data-state");
      }
    }

    root.addEventListener("click", function (event) {
      var button = event.target.closest && event.target.closest(".ose-copy");
      if (!button) return;
      var panel = button.closest(".ose-panel");
      var status = panel.querySelector(".ose-status");
      var text = snippetText(panel.querySelector(".ose-code"));
      if (!navigator.clipboard || !navigator.clipboard.writeText) {
        setStatus(status, "Clipboard unavailable here. Select the snippet and copy it manually.", "error");
        return;
      }
      navigator.clipboard.writeText(text).then(
        function () {
          setStatus(status, "Copied the crew config to the clipboard.", "ok");
        },
        function () {
          setStatus(status, "Copy failed. Select the snippet and copy it manually.", "error");
        }
      );
    });

    // Never leave a copy result standing next to a snippet it no longer describes.
    root.addEventListener("change", function () {
      root.querySelectorAll(".ose-status").forEach(function (status) {
        setStatus(status, "", null);
      });
    });
  })();
</script>

Before a first dispatch you also need an authenticated provider CLI and, on
Linux, a working sandbox. Both are covered in
[Install Orbit](../../getting-started/install/#prerequisites).

## Tool Allowlists

Agent-loop activities declare the tool names an agent may call. Empty means no tools are allowed.

```yaml
spec:
  type: agent_loop
  tools:
    - orbit.task.show
    - orbit.search
```

`on_denial` controls whether a denied tool call terminates the loop or returns a
structured error for the agent to handle. Under agent dispatch, tool allowlist
enforcement is delegated to the harness and recorded in the audit trail.

## Crews

A **crew** is one named provider-model assignment. Activities do not carry a
model-selection role: a task names a crew, and a run resolves it at dispatch —
an explicit crew on the activity input first, then the task's `crew` field, then
`[workflow] default_crew`.

```toml
[crews.sol]
provider = "codex"
model = "gpt-5.6-sol"
effort = "high"
```

`effort` is an optional reasoning-budget request, forwarded through each
provider's own argument and validated against the provider **and** model at
config load. An unsupported combination is rejected outright — Orbit never
downgrades a value to the nearest supported one, and never accepts a key it
would then ignore. The per-provider sets are in
[Configuration](../../reference/config/#reasoning-effort).

`[workflow] system_crew` is a separate assignment used for system activities
synthesized at runtime, such as step-failure recovery. It never inherits a
failed task's crew or the workspace default.

Reassigning work between providers is always explicit — `orbit task update <id>
--crew <name>`. Nothing in Orbit silently moves a task to a different provider.

## Platform Support

Orbit wraps the spawned agent subprocess in an OS-level sandbox scoped by the
activity's resolved `FsProfile`. `orbit init` persists the host-appropriate
backend into the shipped executor assets:

- **macOS** — `sandbox-exec`, with the profile compiled to SBPL.
- **Linux** — Bubblewrap via a trusted `/usr/bin/bwrap`. Writes are confined to
  the resolved profile; host filesystem reads and host network access remain
  available, so read rules and network egress stay delegated. Dispatch **fails
  closed** if `bwrap` is missing or its namespace-and-mount probe fails, unless
  the executor explicitly sets `allow_fallback: true`.
- **Windows and other platforms** — no OS-level wrapper. Process supervision,
  tool allowlists, and in-process guards for Orbit's own built-in tools still
  apply.

The bundled `local-shell` executor declares no sandbox on any platform, by
design. See [Install Orbit](../../getting-started/install/#prepare-the-sandbox)
for the Linux prerequisites.
