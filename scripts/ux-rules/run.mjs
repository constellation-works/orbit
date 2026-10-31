// UX rule runner. Loads scripts/ux-rules/<surface>/*.mjs and writes ux-rules.json.

import { readdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { writeFile } from "node:fs/promises";

import { createCliContext, resultOf } from "./lib/cli-context.mjs";
import { loadManifest } from "./lib/manifest.mjs";
import {
  closeTarget,
  createUiContext,
  expandTargets,
  resolveRoute,
  startFixtureServer,
  uiResultOf,
} from "./lib/ui-context.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const DEFAULT_UI_MANIFEST = "crates/orbit-web/visual/views.toml";

function usage() {
  return [
    "usage: node scripts/ux-rules/run.mjs --surface cli|ui --out <dir> [--orbit <binary>] [--rule <id>]",
    "       node scripts/ux-rules/run.mjs --surface ui --out <dir> (--base-url <url> | --fixture) [--manifest <path>...] [--rule <id>]",
    "",
    "Loads each module under scripts/ux-rules/<surface>/ that exports",
    "`rule` and `check`. UI targets are manifest views x rule widths x",
    "themes x states, against --base-url (live, read-only) or the view's",
    "manifest fixture server with --fixture. --manifest may repeat; the",
    `default is ${DEFAULT_UI_MANIFEST}.`,
  ].join("\n");
}

function parseArgs(argv) {
  const args = {
    surface: "",
    out: "",
    orbit: "",
    rule: "",
    baseUrl: "",
    fixture: false,
    manifests: [],
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
    } else if (token === "--base-url" && next) {
      args.baseUrl = next;
      index += 1;
    } else if (token === "--manifest" && next) {
      args.manifests.push(next);
      index += 1;
    } else if (token === "--fixture") {
      args.fixture = true;
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
  if (args.surface === "ui") {
    if (Boolean(args.baseUrl) === args.fixture) {
      throw new Error("--surface ui needs exactly one of --base-url or --fixture");
    }
    if (args.manifests.length === 0) {
      args.manifests = [DEFAULT_UI_MANIFEST];
    }
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

async function runUiModules(modules, args) {
  const repoRoot = process.cwd();
  const manifests = [];
  for (const manifestPath of args.manifests) {
    manifests.push(await loadManifest(path.resolve(repoRoot, manifestPath)));
  }
  // Expand every rule's targets, then group by page target so one
  // navigation serves every rule that declared it.
  const byTarget = new Map();
  for (const loaded of modules) {
    const targets = expandTargets(loaded.rule, manifests);
    if (targets.length === 0) {
      byTarget.set(`no-target:${loaded.rule.id}`, { noTargetRule: loaded });
      continue;
    }
    for (const target of targets) {
      if (!byTarget.has(target.id)) byTarget.set(target.id, { target, rules: [] });
      byTarget.get(target.id).rules.push(loaded);
    }
  }
  const ctx = await createUiContext({ outDir: args.out, repoRoot });
  const servers = new Map();
  const results = [];
  const serverFor = async (target) => {
    if (!args.fixture) return { baseUrl: args.baseUrl, placeholders: {} };
    const fixture = target.state === "empty" ? `${target.viewDef.fixture}:empty` : target.viewDef.fixture;
    const key = `${target.app}:${fixture}`;
    if (!servers.has(key)) {
      servers.set(key, await startFixtureServer(target.appDef, fixture, repoRoot));
    }
    return servers.get(key);
  };
  try {
    for (const entry of byTarget.values()) {
      if (entry.noTargetRule) {
        const { rule } = entry.noTargetRule;
        results.push(
          uiResultOf(rule, { id: rule.id, view: rule.id, width: 0, theme: "", state: "" }, "error", {
            measurements: { reason: "rule declared no targets in the loaded manifests" },
          }),
        );
        continue;
      }
      const { target, rules } = entry;
      let server;
      try {
        server = await serverFor(target);
      } catch (error) {
        for (const loaded of rules) {
          results.push(
            uiResultOf(loaded.rule, target, "error", {
              measurements: { reason: "fixture setup failed", message: error instanceof Error ? error.message : String(error) },
            }),
          );
        }
        continue;
      }
      const { missing } = resolveRoute(target.viewDef.route, server.placeholders);
      if (missing) {
        for (const loaded of rules) {
          results.push(
            uiResultOf(loaded.rule, target, "skipped", {
              measurements: { reason: `route placeholder {${missing}} is unresolved in this mode` },
            }),
          );
        }
        continue;
      }
      try {
        await ctx.openTarget(target, server.baseUrl, server.placeholders);
      } catch (error) {
        for (const loaded of rules) {
          results.push(
            uiResultOf(loaded.rule, target, "error", {
              measurements: { reason: "render_timeout", message: error instanceof Error ? error.message : String(error) },
            }),
          );
        }
        await closeTarget(ctx);
        continue;
      }
      ctx.drainBlocked();
      for (const loaded of rules) {
        const { rule, check } = loaded;
        ctx.currentRule = rule.id;
        ctx.drainBlocked();
        let produced;
        try {
          produced = await check(ctx);
        } catch (error) {
          produced = uiResultOf(rule, target, "error", {
            measurements: { message: error instanceof Error ? error.message : String(error) },
          });
        }
        ctx.currentRule = "";
        const blocked = ctx.drainBlocked().filter((entry) => entry.rule === rule.id);
        let list = normalizeProduced(rule, target, produced);
        if (blocked.length > 0) {
          list = [
            uiResultOf(rule, target, "error", {
              measurements: { reason: "rule attempted a non-GET request; blocked read-only", blocked },
            }),
          ];
        }
        for (const result of list) {
          if (result.status === "fail") {
            const shot = await ctx.screenshotFor(rule, target).catch(() => "");
            if (shot && !result.evidence.includes(shot)) result.evidence.push(shot);
          }
          results.push(result);
        }
      }
      await closeTarget(ctx);
    }
  } finally {
    for (const server of servers.values()) server.stop();
    await ctx.close();
  }
  const reportTarget = args.fixture
    ? { mode: "fixture", manifests: args.manifests }
    : { mode: "live", base_url: args.baseUrl, manifests: args.manifests };
  return { ctx, results, reportTarget };
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
  let ctx = null;
  let results;
  let reportTarget;
  let kit;
  if (args.surface === "cli") {
    ctx = await createCliContext({
      orbit: args.orbit,
      outDir: args.out,
      repoRoot: process.cwd(),
    });
    results = [];
    for (const loaded of modules) {
      results.push(...(await runModule(loaded, ctx)));
    }
    reportTarget = { mode: "scratch", orbit: path.resolve(args.orbit) };
  } else {
    const ui = await runUiModules(modules, args);
    ctx = ui.ctx;
    results = ui.results;
    reportTarget = ui.reportTarget;
    kit = ui.ctx.kit;
  }
  results.sort((left, right) => left.fingerprint.localeCompare(right.fingerprint));
  const report = {
    schema_version: 1,
    surface: args.surface,
    commit: ctx?.commit ?? "",
    host: ctx?.host ?? "",
    ...(kit ? { kit } : {}),
    target: reportTarget,
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
