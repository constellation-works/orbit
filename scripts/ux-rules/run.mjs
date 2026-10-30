// UX rule runner. Loads scripts/ux-rules/<surface>/*.mjs and writes ux-rules.json.

import { readdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { writeFile } from "node:fs/promises";

import { createCliContext, resultOf } from "./lib/cli-context.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));

function usage() {
  return [
    "usage: node scripts/ux-rules/run.mjs --surface cli|ui --out <dir> [--orbit <binary>] [--rule <id>]",
    "",
    "Loads each module under scripts/ux-rules/<surface>/ that exports",
    "`rule` and `check`. --surface ui with no modules exits non-zero.",
  ].join("\n");
}

function parseArgs(argv) {
  const args = {
    surface: "",
    out: "",
    orbit: "",
    rule: "",
  };
  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];
    const next = argv[index + 1];
    if (token === "--surface" && next) {
      args.surface = next;
      index += 1;
    } else if (token === "--out" && next) {
      args.out = next;
      index += 1;
    } else if (token === "--orbit" && next) {
      args.orbit = next;
      index += 1;
    } else if (token === "--rule" && next) {
      args.rule = next;
      index += 1;
    } else if (token === "--help" || token === "-h") {
      process.stdout.write(`${usage()}\n`);
      process.exit(0);
    } else {
      throw new Error(`unknown argument: ${token}`);
    }
  }
  if (args.surface !== "cli" && args.surface !== "ui") {
    throw new Error("--surface must be cli or ui");
  }
  if (!args.out) {
    throw new Error("--out is required");
  }
  if (args.surface === "cli" && !args.orbit) {
    throw new Error("--orbit is required for --surface cli");
  }
  return args;
}

function ruleMatches(rule, wanted) {
  if (!wanted) {
    return true;
  }
  return rule.id === wanted || rule.id === `cli.${wanted}` || rule.id === `ui.${wanted}` || rule.id.endsWith(`.${wanted}`);
}

async function loadModules(surface, wanted) {
  const directory = path.join(HERE, surface);
  let names = [];
  try {
    names = (await readdir(directory)).filter((name) => name.endsWith(".mjs")).sort();
  } catch (error) {
    if (error.code !== "ENOENT") {
      throw error;
    }
  }
  const modules = [];
  for (const name of names) {
    const loaded = await import(pathToFileURL(path.join(directory, name)).href);
    if (!loaded.rule || typeof loaded.check !== "function") {
      throw new Error(`${surface}/${name} must export rule and check`);
    }
    if (ruleMatches(loaded.rule, wanted)) {
      modules.push(loaded);
    }
  }
  return modules;
}

async function runModule(loaded, ctx) {
  const { rule, check } = loaded;
  if (Array.isArray(rule.targets) && rule.targets.length === 0) {
    return [
      resultOf(rule, { id: rule.id, argv: [] }, "error", {
        selector: "matched no target",
        measurements: { reason: "rule declared no targets" },
      }),
    ];
  }
  if (Array.isArray(rule.targets)) {
    const results = [];
    for (const target of rule.targets) {
      try {
        ctx.target = target;
        const produced = await check(ctx);
        const normalized = normalizeProduced(rule, target, produced);
        results.push(...(normalized.length ? normalized : [resultOf(rule, target, "error", {
          measurements: { reason: "rule produced no results" },
        })]));
      } catch (error) {
        results.push(
          resultOf(rule, target, "error", {
            selector: "throw",
            measurements: { message: error instanceof Error ? error.message : String(error) },
          }),
        );
      }
    }
    return results;
  }
  try {
    ctx.target = null;
    const produced = await check(ctx);
    const results = normalizeProduced(rule, { id: rule.id, argv: [] }, produced);
    if (results.length === 0) {
      return [
        resultOf(rule, { id: rule.id, argv: [] }, "error", {
          selector: "matched no target",
          measurements: { reason: "rule produced no results" },
        }),
      ];
    }
    return results;
  } catch (error) {
    return [
      resultOf(rule, { id: rule.id, argv: [] }, "error", {
        selector: "throw",
        measurements: { message: error instanceof Error ? error.message : String(error) },
      }),
    ];
  }
}

function normalizeProduced(rule, target, produced) {
  if (produced == null) {
    return [
      resultOf(rule, target, "error", {
        selector: "throw",
        measurements: { message: "check returned no result" },
      }),
    ];
  }
  const list = Array.isArray(produced) ? produced : [produced];
  return list.filter(Boolean);
}

function summarize(results) {
  const summary = { pass: 0, fail: 0, error: 0, skipped: 0 };
  for (const result of results) {
    if (Object.hasOwn(summary, result.status)) {
      summary[result.status] += 1;
    }
  }
  return summary;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const modules = await loadModules(args.surface, args.rule);
  if (modules.length === 0) {
    const why = args.rule
      ? `no rules were found for surface ${args.surface} matching ${args.rule}`
      : `no rules were found for surface ${args.surface}`;
    process.stderr.write(`${why}\n`);
    process.exitCode = 1;
    return;
  }
  const ctx =
    args.surface === "cli"
      ? await createCliContext({
          orbit: args.orbit,
          outDir: args.out,
          repoRoot: process.cwd(),
        })
      : null;
  const results = [];
  for (const loaded of modules) {
    results.push(...(await runModule(loaded, ctx ?? {})));
  }
  results.sort((left, right) => left.fingerprint.localeCompare(right.fingerprint));
  const report = {
    schema_version: 1,
    surface: args.surface,
    commit: ctx?.commit ?? "",
    host: ctx?.host ?? "",
    target: ctx
      ? { mode: "scratch", orbit: path.resolve(args.orbit) }
      : { mode: "unconfigured" },
    results,
    summary: summarize(results),
  };
  const destination = path.join(path.resolve(args.out), "ux-rules.json");
  await writeFile(destination, `${JSON.stringify(report, null, 2)}\n`);
  process.stdout.write(
    `${destination} pass=${report.summary.pass} fail=${report.summary.fail} error=${report.summary.error} skipped=${report.summary.skipped}\n`,
  );
}

main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
});
