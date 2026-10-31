// A CLI command and its MCP tool agree on required inputs and stated defaults.

import { resultOf } from "../lib/cli-context.mjs";

export const rule = {
  id: "cli.mcp-cli-parity",
  title: "CLI commands and MCP tools agree on required inputs and defaults",
  severity: "medium",
  surface: "cli",
};

const PROVENANCE = new Set(["model", "agent"]);

function normalize(value) {
  return String(value).toLowerCase().replace(/-/g, "_");
}

function cliKey(argv) {
  return argv.map(normalize).join("_");
}

function toolKey(name) {
  let rest = name;
  if (rest.startsWith("orbit.")) {
    rest = rest.slice("orbit.".length);
  } else if (rest.startsWith("orbit_")) {
    rest = rest.slice("orbit_".length);
  }
  return rest.replaceAll(".", "_").replaceAll("-", "_");
}

function requiredFromUsage(usage, commandArgv) {
  const body = usage.replace(/^Usage:\s*/, "");
  const chunks = [];
  let depth = 0;
  let buffer = "";
  for (const character of body) {
    if (character === "[") {
      if (depth === 0) {
        chunks.push({ required: true, text: buffer });
        buffer = "";
      }
      depth += 1;
      if (depth > 1) {
        buffer += character;
      }
      continue;
    }
    if (character === "]") {
      depth -= 1;
      if (depth === 0) {
        chunks.push({ required: false, text: buffer });
        buffer = "";
      } else if (depth > 0) {
        buffer += character;
      }
      continue;
    }
    buffer += character;
  }
  if (buffer) {
    chunks.push({ required: depth === 0, text: buffer });
  }
  const requiredText = chunks
    .filter((chunk) => chunk.required)
    .map((chunk) => chunk.text)
    .join(" ");
  const names = new Set();
  for (const match of requiredText.matchAll(/<([A-Za-z0-9_-]+)>/g)) {
    names.add(normalize(match[1]));
  }
  for (const match of requiredText.matchAll(/--([A-Za-z0-9-]+)/g)) {
    names.add(normalize(match[1]));
  }
  for (const part of commandArgv) {
    names.delete(normalize(part));
  }
  names.delete("orbit");
  names.delete("options");
  for (const name of PROVENANCE) {
    names.delete(name);
  }
  return names;
}

function globalOptionNames(helpText) {
  const names = new Set();
  let inGlobal = false;
  for (const line of helpText.split("\n")) {
    if (line.startsWith("Global options")) {
      inGlobal = true;
      continue;
    }
    if (!inGlobal) {
      continue;
    }
    if (line.trim() === "") {
      continue;
    }
    if (!line.startsWith(" ")) {
      break;
    }
    for (const match of line.matchAll(/--([A-Za-z0-9-]+)/g)) {
      names.add(normalize(match[1]));
    }
  }
  return names;
}

function defaultsFromHelp(helpText) {
  const defaults = new Map();
  for (const line of helpText.split("\n")) {
    const flag = line.match(/--([A-Za-z0-9-]+)/);
    const value = line.match(/\[default:\s*([^\]]+)\]|\(default:\s*([^)]+)\)/i);
    if (!flag || !value) {
      continue;
    }
    defaults.set(normalize(flag[1]), (value[1] ?? value[2]).trim());
  }
  return defaults;
}

function jsonDefault(value) {
  if (typeof value === "string") {
    return value;
  }
  return JSON.stringify(value);
}

export async function check(ctx) {
  const tools = await ctx.listMcpTools();
  const commands = await ctx.discoverCommands();
  const byKey = new Map(commands.map((argv) => [cliKey(argv), argv]));
  const results = [];
  for (const tool of [...tools].sort((left, right) => left.name.localeCompare(right.name))) {
    const key = toolKey(tool.name);
    const argv = byKey.get(key);
    const target = { id: tool.name, argv: argv ?? [tool.name] };
    if (!argv) {
      results.push(
        resultOf(rule, target, "skipped", {
          selector: "no CLI command",
          measurements: { tool_key: key },
        }),
      );
      continue;
    }
    const help = await ctx.capture({ argv: [...argv, "--help"] });
    if (help.exitCode !== 0) {
      results.push(
        resultOf(rule, target, "error", {
          selector: "throw",
          measurements: { exit_code: help.exitCode },
        }),
      );
      continue;
    }
    const usage = help.stdout.split("\n").find((line) => line.startsWith("Usage:")) ?? "";
    const cliRequired = requiredFromUsage(usage, argv);
    const globals = globalOptionNames(help.stdout);
    const schema = tool.inputSchema ?? {};
    const mcpRequired = new Set(
      (schema.required ?? []).map(normalize).filter((name) => !PROVENANCE.has(name)),
    );
    // Global options such as --workspace are the CLI's transport for a field
    // MCP has to require. They are not a missing per-command input.
    const missingOnCli = [...mcpRequired]
      .filter((name) => !cliRequired.has(name) && !globals.has(name))
      .sort();
    const missingOnMcp = [...cliRequired].filter((name) => !mcpRequired.has(name)).sort();
    const cliDefaults = defaultsFromHelp(help.stdout);
    const defaultMismatches = [];
    for (const [name, spec] of Object.entries(schema.properties ?? {})) {
      if (!spec || typeof spec !== "object" || spec.default === undefined) {
        continue;
      }
      const cliDefault = cliDefaults.get(normalize(name));
      if (cliDefault === undefined) {
        continue;
      }
      const mcpDefault = jsonDefault(spec.default);
      if (mcpDefault !== cliDefault) {
        defaultMismatches.push({ name, cli: cliDefault, mcp: mcpDefault });
      }
    }
    defaultMismatches.sort((left, right) => left.name.localeCompare(right.name));
    const failed =
      missingOnCli.length > 0 || missingOnMcp.length > 0 || defaultMismatches.length > 0;
    const selector = failed
      ? [
          ...missingOnCli.map((name) => `mcp-required:${name}`),
          ...missingOnMcp.map((name) => `cli-required:${name}`),
          ...defaultMismatches.map((item) => `default:${item.name}`),
        ]
          .sort()
          .join(",")
      : "";
    results.push(
      resultOf(rule, target, failed ? "fail" : "pass", {
        selector,
        measurements: {
          cli: argv.join(" "),
          usage,
          cli_required: [...cliRequired].sort(),
          mcp_required: [...mcpRequired].sort(),
          missing_on_cli: missingOnCli,
          missing_on_mcp: missingOnMcp,
          default_mismatches: defaultMismatches,
        },
      }),
    );
  }
  return results;
}
