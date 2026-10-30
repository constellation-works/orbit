// CLI runner context: scratch `orbit init`, piped and PTY capture, help parsing.
//
// PTY captures delete COLUMNS and record the width ioctl reports back. The
// renderer reads COLUMNS first and the terminal width only after that
// (output/sink.rs resolve_width), so a PTY whose COLUMNS is set does not
// prove which width the renderer used.

import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { Buffer } from "node:buffer";
import path from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

const CAPTURE_PY = String.raw`
import json, os, sys, pty, select, subprocess, fcntl, termios, struct, time, tty

req = json.load(open(sys.argv[1], encoding="utf-8"))
out_path = sys.argv[2]
argv = req["argv"]
env = {key: str(value) for key, value in req["env"].items()}
env.pop("COLUMNS", None)
cwd = req["cwd"]
cols = req.get("columns")
timeout = float(req.get("timeout", 45))

def finish(payload):
    with open(out_path, "w", encoding="utf-8") as handle:
        json.dump(payload, handle)

def run_piped():
    try:
        proc = subprocess.run(
            argv, cwd=cwd, env=env, capture_output=True, timeout=timeout
        )
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout or b""
        stderr = exc.stderr or b""
        finish({
            "exitCode": -1,
            "rendererWidth": 0,
            "stdout": stdout.decode("utf-8", "replace"),
            "stderr": stderr.decode("utf-8", "replace") + "\ntimeout",
            "mode": "piped",
        })
        return
    finish({
        "exitCode": proc.returncode,
        "rendererWidth": 0,
        "stdout": proc.stdout.decode("utf-8", "replace"),
        "stderr": proc.stderr.decode("utf-8", "replace"),
        "mode": "piped",
    })

def run_pty():
    master, slave = pty.openpty()
    tty.setraw(slave)
    packed = struct.pack("HHHH", 48, int(cols), 0, 0)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, packed)
    reported = struct.unpack(
        "HHHH", fcntl.ioctl(slave, termios.TIOCGWINSZ, b"\0" * 8)
    )[1]
    proc = subprocess.Popen(
        argv,
        cwd=cwd,
        env=env,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        close_fds=True,
    )
    os.close(slave)
    chunks = []
    deadline = time.time() + timeout
    timed_out = False
    while True:
        remaining = deadline - time.time()
        if remaining <= 0:
            timed_out = True
            break
        ready, _, _ = select.select([master], [], [], min(0.2, remaining))
        if ready:
            try:
                data = os.read(master, 65536)
            except OSError:
                break
            if not data:
                break
            chunks.append(data)
            continue
        if proc.poll() is not None:
            while True:
                ready, _, _ = select.select([master], [], [], 0.05)
                if not ready:
                    break
                try:
                    data = os.read(master, 65536)
                except OSError:
                    data = b""
                if not data:
                    break
                chunks.append(data)
            break
    if timed_out and proc.poll() is None:
        proc.kill()
    code = proc.wait(timeout=5)
    os.close(master)
    text = b"".join(chunks).decode("utf-8", "replace")
    finish({
        "exitCode": -1 if timed_out else code,
        "rendererWidth": reported,
        "stdout": text,
        "stderr": "timeout" if timed_out else "",
        "mode": "pty",
    })

if cols is None:
    run_piped()
else:
    run_pty()
`;

const HEADING_SKIP = new Set([
  "Usage",
  "Arguments",
  "Options",
  "Commands",
  "Global options",
]);

// Grouped help rows are a single subcommand token plus a description.
// Example blocks (`orbit run auto ...`) and prose are not that template.
const SUBCOMMAND_NAME = /^[a-z][a-z0-9-]*$/;

/**
 * @param {string} text
 */
export function stripTerminal(text) {
  return text.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "").replace(/\r/g, "");
}

/**
 * @param {string} text
 */
export function parseGroupedSections(text, strict = false) {
  const lines = stripTerminal(text).split("\n");
  const heading = /^([A-Z][A-Za-z][A-Za-z /_-]*):$/;
  const sections = [];
  for (let index = 0; index < lines.length; index += 1) {
    const match = lines[index].match(heading);
    if (!match || HEADING_SKIP.has(match[1])) {
      continue;
    }
    const rows = [];
    for (let cursor = index + 1; cursor < lines.length; cursor += 1) {
      const line = lines[cursor];
      if (line === "") {
        continue;
      }
      if (heading.test(line) || line.startsWith("Options:") || line.startsWith("Global options")) {
        break;
      }
      if (!line.startsWith(" ")) {
        break;
      }
      rows.push(line);
    }
    if (rows.length > 0 && isGroupedCommandSection(rows, strict)) {
      sections.push({ name: match[1], rows });
    }
  }
  return sections;
}

/**
 * @param {string} line
 */
function classifyRow(line) {
  if (!line.startsWith("  ") || line.length < 3) {
    return null;
  }
  if (line[2] === " ") {
    const indent = line.match(/^ */)[0].length;
    return { type: "cont", indent, text: line.slice(indent), line };
  }
  const name = line.slice(2).match(/^\S+/)[0];
  if (name.startsWith("-")) {
    return null;
  }
  const rest = line.slice(2 + name.length);
  const gap = rest.match(/^ */)[0].length;
  const desc = rest.slice(gap);
  return {
    type: "command",
    name,
    gap: desc.length > 0 ? gap : 0,
    desc,
    column: desc.length > 0 ? 2 + name.length + gap : null,
    line,
  };
}

/**
 * Grouped-help layout at one rendered width.
 * `rendererWidth` 0 is the piped sink (no wrapping).
 * @param {string} text
 * @param {number} rendererWidth
 */
export function inspectGroupedHelp(text, rendererWidth) {
  const sections = [];
  for (const section of parseGroupedSections(text)) {
    const rows = section.rows.map(classifyRow).filter(Boolean);
    const commands = rows.filter((row) => row.type === "command");
    if (commands.length === 0) {
      continue;
    }
    const described = commands.filter((row) => row.column !== null);
    const columns = described.map((row) => row.column);
    const column = columns.length > 0 ? columns[0] : null;
    const aligned =
      columns.every((value) => value === column) &&
      rows
        .filter((row) => row.type === "cont")
        .every((row) => column !== null && row.indent === column);
    const gaps = described.map((row) => row.gap);
    const minGap = gaps.length > 0 ? Math.min(...gaps) : 0;
    const handBreaks = [];
    let hangingOk = true;
    for (let index = 0; index < rows.length; index += 1) {
      const row = rows[index];
      if (row.type !== "command") {
        continue;
      }
      const followers = [];
      for (let next = index + 1; next < rows.length; next += 1) {
        if (rows[next].type !== "cont") {
          break;
        }
        followers.push(rows[next]);
      }
      if (rendererWidth === 0) {
        if (followers.length > 0) {
          handBreaks.push(row.name);
        }
        continue;
      }
      const expected = row.column ?? column;
      for (const follower of followers) {
        if (expected === null || follower.indent !== expected) {
          hangingOk = false;
        }
      }
      const descLines = [];
      if (row.desc.length > 0) {
        descLines.push({ visual: row.line.length, text: row.desc });
      }
      for (const follower of followers) {
        descLines.push({ visual: follower.line.length, text: follower.text });
      }
      for (let lineIndex = 0; lineIndex < descLines.length - 1; lineIndex += 1) {
        const current = descLines[lineIndex];
        const nextWord = descLines[lineIndex + 1].text.split(/\s+/)[0] ?? "";
        const spare = rendererWidth - current.visual;
        if (nextWord.length > 0 && spare >= nextWord.length + 1) {
          handBreaks.push(row.name);
          break;
        }
      }
    }
    sections.push({
      name: section.name,
      column,
      minGap,
      aligned: aligned && minGap >= 2,
      hangingOk,
      handBreaks,
    });
  }
  return sections;
}

function isGroupedCommandRow(row) {
  return (
    SUBCOMMAND_NAME.test(row.name) && row.name !== "orbit" &&
    (row.desc.length === 0 || row.gap >= 1)
  );
}

function isGroupedCommandSection(rows, strict = false) {
  const commands = rows.map(classifyRow).filter((row) => row && row.type === "command");
  return commands.length > 0 && commands.every((row) => isGroupedCommandRow(row) &&
    (!strict || row.desc.length === 0 || row.gap >= 2));
}

/**
 * @param {string} helpText
 */
export function subcommandNames(helpText) {
  const lines = stripTerminal(helpText).split("\n");
  const fromCommands = [];
  let inCommands = false;
  for (const line of lines) {
    if (line === "Commands:") {
      inCommands = true;
      continue;
    }
    if (!inCommands) {
      continue;
    }
    if (line.trim() === "") {
      continue;
    }
    if (!line.startsWith("  ") || line[2] === " ") {
      if (fromCommands.length > 0) {
        break;
      }
      inCommands = false;
      continue;
    }
    const name = line.trim().split(/\s+/)[0];
    if (SUBCOMMAND_NAME.test(name)) {
      fromCommands.push(name);
    }
  }
  if (fromCommands.length > 0) {
    return [...new Set(fromCommands)];
  }
  const names = [];
  for (const section of parseGroupedSections(helpText, true)) {
    for (const row of section.rows) {
      const classified = classifyRow(row);
      if (classified && classified.type === "command" && isGroupedCommandRow(classified) &&
          (classified.desc.length === 0 || classified.gap >= 2)) {
        names.push(classified.name);
      }
    }
  }
  return [...new Set(names)];
}

/**
 * @param {string} helpText
 */
export function flagEntries(helpText) {
  const lines = stripTerminal(helpText).split("\n");
  const entries = [];
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (!/^\s+(-\S|--\S)/.test(line)) {
      continue;
    }
    const indent = line.match(/^\s*/)[0].length;
    const long = line.match(/--([A-Za-z0-9][A-Za-z0-9-]*)/);
    const short = line.match(/^\s+(-[A-Za-z0-9])/);
    const description = [];
    let cursor = index + 1;
    while (cursor < lines.length) {
      const next = lines[cursor];
      if (next.trim() === "") {
        let look = cursor + 1;
        while (look < lines.length && lines[look].trim() === "") {
          look += 1;
        }
        if (look >= lines.length) {
          break;
        }
        const aheadIndent = lines[look].match(/^\s*/)[0].length;
        if (aheadIndent <= indent) {
          break;
        }
        cursor += 1;
        continue;
      }
      const nextIndent = next.match(/^\s*/)[0].length;
      const trimmed = next.trim();
      // A short option is indented less than a long-only option, so indent
      // alone cannot tell a wrapped line from the next flag.
      if (/^\s+(-\S|--\S)/.test(next) || nextIndent <= indent || trimmed === "Possible values:") {
        break;
      }
      // Clap's [default:], [possible values:] and [aliases:] lines are
      // annotations, not a second line of the flag's prose.
      if (/^\[(possible values|default|aliases|env):/i.test(trimmed)) {
        cursor += 1;
        continue;
      }
      description.push(next);
      cursor += 1;
    }
    entries.push({
      flag: long ? `--${long[1]}` : short ? short[1] : "flag",
      lines: description,
    });
    index = Math.max(index, cursor - 1);
  }
  return entries;
}

/**
 * @param {object} rule
 * @param {string | { id: string, argv?: string[] }} target
 * @param {"pass" | "fail" | "error" | "skipped"} status
 * @param {{ measurements?: object, evidence?: string[], selector?: string, argv?: string[] }} [fields]
 */
export function resultOf(rule, target, status, fields = {}) {
  const targetId = typeof target === "string" ? target : target.id;
  const argv = fields.argv ?? (typeof target === "string" ? [] : target.argv ?? []);
  const selector = argv.join(" ") || targetId;
  return {
    rule: rule.id,
    target: typeof target === "string" ? { id: target, argv } : { ...target, argv },
    status,
    severity: rule.severity,
    measurements: fields.measurements ?? {},
    evidence: fields.evidence ?? [],
    fingerprint: [rule.id, targetId, selector].join("|"),
  };
}

export function utf8Bytes(text) {
  return Buffer.byteLength(text, "utf8");
}

/**
 * @param {string} text
 */
export function topLevelShape(text) {
  const value = JSON.parse(text);
  if (Array.isArray(value)) {
    return { type: "array" };
  }
  if (value === null) {
    return { type: "null" };
  }
  if (typeof value === "object") {
    const keys = Object.keys(value).sort();
    const types = {};
    for (const key of keys) {
      const child = value[key];
      if (child === null) {
        types[key] = "null";
      } else if (Array.isArray(child)) {
        types[key] = "array";
      } else {
        types[key] = typeof child;
      }
    }
    return { type: "object", keys, types };
  }
  return { type: typeof value };
}

function safeName(value) {
  return String(value).replace(/[^A-Za-z0-9._-]+/g, "_").slice(0, 80);
}

/**
 * @param {{ orbit: string, outDir: string, repoRoot: string }} options
 */
export async function createCliContext(options) {
  const outDir = path.resolve(options.outDir);
  await mkdir(outDir, { recursive: true });
  const scratch = await mkdtemp(path.join(outDir, "scratch-"));
  const homeDir = path.join(scratch, "home");
  const rootDir = path.join(scratch, "root");
  const workDir = path.join(scratch, "work");
  await mkdir(homeDir, { recursive: true });
  await mkdir(rootDir, { recursive: true });
  await mkdir(workDir, { recursive: true });
  await mkdir(path.join(outDir, ".runner"), { recursive: true });
  const pythonPath = path.join(outDir, ".runner", "capture.py");
  await writeFile(pythonPath, CAPTURE_PY);

  const env = {
    PATH: process.env.PATH ?? "",
    HOME: homeDir,
    LANG: process.env.LANG || "C.UTF-8",
    LC_ALL: process.env.LC_ALL || process.env.LANG || "C.UTF-8",
    TERM: "dumb",
    NO_COLOR: "1",
    USER: process.env.USER || "ux-rules",
    LOGNAME: process.env.LOGNAME || process.env.USER || "ux-rules",
    ORBIT_SKIP_HOST_PREREQUISITES: "1",
  };

  const ctx = {
    orbit: path.resolve(options.orbit),
    outDir,
    rootDir,
    workDir,
    homeDir,
    repoRoot: options.repoRoot,
    commit: "",
    host: "",
    cache: new Map(),
    pythonPath,
    env,
  };

  const git = await execFileAsync("git", ["rev-parse", "HEAD"], {
    cwd: options.repoRoot,
    env: { ...process.env, GIT_OPTIONAL_LOCKS: "0" },
  });
  ctx.commit = git.stdout.trim();

  let host = "";
  {
    const init = await executeCapture(ctx, {
      argv: [
        "init",
        "--non-interactive",
        "--machine-name",
        "ux-rules",
        "--task-prefix",
        "UXRS",
        "--skip-host-prerequisites",
      ],
      json: true,
      timeout: 120,
    });
    if (init.exitCode !== 0) {
      throw new Error(
        `orbit init failed (${init.exitCode}): ${init.stderr || init.stdout}`,
      );
    }
    const parsed = JSON.parse(init.stdout);
    host = parsed.machine?.id ?? "";
    if (!host.startsWith("hm_")) {
      throw new Error("orbit init did not report a machine id");
    }
  }
  ctx.host = host;

  ctx.capture = (request) => capture(ctx, request);
  ctx.discoverCommands = () => discoverCommands(ctx);
  ctx.listMcpTools = () => listMcpTools(ctx);
  ctx.writeEvidence = (ruleId, targetId, label, text) =>
    writeEvidence(ctx, ruleId, targetId, label, text);
  return ctx;
}

async function executeCapture(ctx, request) {
  const argv = [ctx.orbit, "--root", ctx.rootDir];
  if (request.json) {
    argv.push("--json");
  }
  argv.push(...request.argv);
  const payload = {
    argv,
    env: ctx.env,
    cwd: ctx.workDir,
    columns: request.width ?? null,
    timeout: request.timeout ?? 45,
  };
  const id = `${Date.now()}-${Math.random().toString(16).slice(2)}`;
  const requestPath = path.join(ctx.outDir, ".runner", `req-${id}.json`);
  const resultPath = path.join(ctx.outDir, ".runner", `res-${id}.json`);
  await writeFile(requestPath, JSON.stringify(payload));
  await execFileAsync("python3", [ctx.pythonPath, requestPath, resultPath], {
    cwd: ctx.workDir,
    timeout: (payload.timeout + 15) * 1000,
  });
  const parsed = JSON.parse(await readFile(resultPath, "utf8"));
  parsed.stdout = stripTerminal(parsed.stdout ?? "");
  parsed.stderr = stripTerminal(parsed.stderr ?? "");
  return parsed;
}

async function capture(ctx, request) {
  const widthKey = request.width ?? "pipe";
  const key = [
    widthKey,
    request.json ? "json" : "text",
    request.argv.join("\0"),
  ].join("|");
  if (!request.fresh && ctx.cache.has(key)) {
    return ctx.cache.get(key);
  }
  const result = await executeCapture(ctx, request);
  if (!request.fresh) {
    ctx.cache.set(key, result);
  }
  return result;
}

async function discoverCommands(ctx) {
  if (ctx.commands) {
    return ctx.commands;
  }
  const rootHelp = await capture(ctx, { argv: ["--help"] });
  if (rootHelp.exitCode !== 0) {
    throw new Error(`orbit --help failed: ${rootHelp.stderr}`);
  }
  const queue = subcommandNames(rootHelp.stdout).map((name) => [name]);
  const seen = new Set([""]);
  const commands = [[]];
  while (queue.length > 0) {
    const argv = queue.shift();
    const key = argv.join(" ");
    if (seen.has(key)) {
      continue;
    }
    if (argv.length > 6 || commands.length >= 400) {
      throw new Error("command discovery exceeded the CLI tree bound");
    }
    seen.add(key);
    commands.push(argv);
    const help = await capture(ctx, { argv: [...argv, "--help"] });
    if (help.exitCode !== 0) {
      continue;
    }
    for (const name of subcommandNames(help.stdout)) {
      queue.push([...argv, name]);
    }
  }
  commands.sort((left, right) => left.join(" ").localeCompare(right.join(" ")));
  ctx.commands = commands;
  return commands;
}

function readMcpMessage(stream, accept, timeoutMs) {
  return new Promise((resolve, reject) => {
    let buffer = "";
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`timed out waiting for an MCP response; pending=${buffer.slice(0, 180)}`));
    }, timeoutMs);
    const onData = (chunk) => {
      buffer += chunk.toString("utf8");
      let newline = buffer.indexOf("\n");
      while (newline >= 0) {
        const line = buffer.slice(0, newline).trim();
        buffer = buffer.slice(newline + 1);
        newline = buffer.indexOf("\n");
        if (!line.startsWith("{")) {
          continue;
        }
        let message;
        try {
          message = JSON.parse(line);
        } catch {
          continue;
        }
        if (accept(message)) {
          cleanup();
          resolve(message);
          return;
        }
      }
    };
    const onEnd = () => {
      cleanup();
      reject(new Error("MCP server closed before the expected response"));
    };
    function cleanup() {
      clearTimeout(timer);
      stream.off("data", onData);
      stream.off("end", onEnd);
      stream.off("error", onError);
    }
    function onError(error) {
      cleanup();
      reject(error);
    }
    stream.on("data", onData);
    stream.on("end", onEnd);
    stream.on("error", onError);
  });
}

async function listMcpTools(ctx) {
  if (ctx.mcpTools) {
    return ctx.mcpTools;
  }
  const { spawn } = await import("node:child_process");
  const proc = spawn(ctx.orbit, ["--root", ctx.rootDir, "mcp", "serve"], {
    cwd: ctx.workDir,
    env: ctx.env,
    stdio: ["pipe", "pipe", "pipe"],
  });
  let stderr = "";
  proc.stderr.on("data", (chunk) => {
    stderr += chunk.toString("utf8");
    if (stderr.length > 4000) {
      stderr = stderr.slice(-4000);
    }
  });
  const send = (message) => {
    proc.stdin.write(`${JSON.stringify(message)}\n`);
  };
  const tools = [];
  try {
    const initialize = readMcpMessage(proc.stdout, (message) => message.id === 1, 60000);
    send({
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "ux-rules", version: "1" },
      },
    });
    const initialized = await initialize;
    if (initialized.error) {
      throw new Error(
        `MCP initialize failed: ${JSON.stringify(initialized.error)} ${stderr.trim()}`,
      );
    }
    send({ jsonrpc: "2.0", method: "notifications/initialized" });
    let cursor;
    for (let page = 0; page < 20; page += 1) {
      const id = page + 2;
      const pending = readMcpMessage(proc.stdout, (message) => message.id === id, 120000);
      const params = cursor ? { cursor } : {};
      send({ jsonrpc: "2.0", id, method: "tools/list", params });
      const listed = await pending;
      if (listed.error) {
        throw new Error(`MCP tools/list failed: ${JSON.stringify(listed.error)} ${stderr.trim()}`);
      }
      const batch = listed.result?.tools ?? [];
      if (!Array.isArray(batch)) {
        throw new Error("MCP tools/list returned no tools array");
      }
      tools.push(...batch);
      cursor = listed.result?.nextCursor;
      if (!cursor) {
        break;
      }
    }
  } finally {
    proc.kill();
  }
  if (tools.length === 0) {
    throw new Error(`MCP tools/list returned no tools ${stderr.trim()}`);
  }
  ctx.mcpTools = tools;
  return tools;
}

async function writeEvidence(ctx, ruleId, targetId, label, text) {
  const relative = path.join(
    "evidence",
    safeName(ruleId),
    `${safeName(targetId)}-${safeName(label)}.txt`,
  );
  const absolute = path.join(ctx.outDir, relative);
  await mkdir(path.dirname(absolute), { recursive: true });
  await writeFile(absolute, text);
  return relative.split(path.sep).join("/");
}
