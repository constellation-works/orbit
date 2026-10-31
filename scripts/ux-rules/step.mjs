// Script-step adapter: run each surface independently and return one document.
import { execFile } from "node:child_process";
import { access, mkdtemp, mkdir, readFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";

const exec = promisify(execFile);

// Locations the sandbox cannot pass through the environment are carried
// explicitly: as manifest `args` (`--orbit`, `--browser-kit`) or as step
// input fields (`orbit`, `browser_kit`). Granted read roots are bound at
// their host paths inside the sandbox, so those absolute paths stay valid.
// Environment names remain a fallback for direct, unsandboxed runs.
function stepLocations(argv, input) {
  const fromArgv = {};
  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];
    const equals = token.indexOf("=");
    const flag = equals === -1 ? token : token.slice(0, equals);
    const glued = equals === -1 ? undefined : token.slice(equals + 1);
    const key = {
      "--orbit": "orbit",
      "--browser-kit": "browserKit",
      "--playwright-module": "playwrightModule",
      "--playwright-browsers-path": "playwrightBrowsersPath",
    }[flag];
    if (!key) continue;
    const value = glued ?? argv[index + 1];
    if (glued === undefined) index += 1;
    if (typeof value === "string" && value) fromArgv[key] = value;
  }
  const fromInput = {};
  for (const [field, key] of [
    ["orbit", "orbit"],
    ["browser_kit", "browserKit"],
    ["playwright_module", "playwrightModule"],
    ["playwright_browsers_path", "playwrightBrowsersPath"],
  ]) {
    if (input[field] !== undefined && typeof input[field] !== "string") {
      throw new Error(`${field} must be a string`);
    }
    if (input[field]) fromInput[key] = input[field];
  }
  // An explicit step input wins over the manifest args, which are the
  // template's host defaults.
  return { ...fromArgv, ...fromInput };
}

async function firstExisting(candidates) {
  for (const candidate of candidates) {
    try { await access(candidate); return candidate; } catch { /* try next */ }
  }
  return "";
}

async function orbitBinary(locations) {
  if (locations.orbit) return locations.orbit;
  if (process.env.UX_RULES_ORBIT) return process.env.UX_RULES_ORBIT;
  for (const directory of (process.env.PATH || "").split(path.delimiter)) {
    if (!directory) continue;
    const candidate = path.resolve(directory, "orbit");
    try { await access(candidate); return candidate; } catch { /* try next */ }
  }
  throw new Error("orbit binary not found: pass --orbit in the manifest args (or the orbit step input) naming a granted read root, or set UX_RULES_ORBIT outside the sandbox");
}

async function browserKit(locations) {
  let playwrightModule = locations.playwrightModule || process.env.PLAYWRIGHT_MODULE || "";
  let playwrightBrowsersPath =
    locations.playwrightBrowsersPath || process.env.PLAYWRIGHT_BROWSERS_PATH || "";
  if (locations.browserKit) {
    const root = locations.browserKit;
    playwrightModule ||= await firstExisting([
      path.join(root, "kit/node_modules/playwright/index.mjs"),
      path.join(root, "node_modules/playwright/index.mjs"),
    ]);
    playwrightBrowsersPath ||= await firstExisting([path.join(root, "browsers")]);
  }
  return { playwrightModule, playwrightBrowsersPath };
}

async function main() {
  let inputText = "";
  for await (const chunk of process.stdin) inputText += chunk;
  const input = JSON.parse(inputText);
  if (!Array.isArray(input.surfaces) || input.surfaces.some((s) => typeof s !== "string")) {
    throw new Error("surfaces must be an array of strings");
  }
  const locations = stepLocations(process.argv.slice(2), input);
  const kit = await browserKit(locations);
  const scratch = await mkdtemp(path.join(os.tmpdir(), "ux-rules-"));
  const results = [];
  let commit = "";
  try {
    for (const [index, surface] of input.surfaces.entries()) {
      const out = path.join(scratch, String(index));
      await mkdir(out);
      const args = [input.runner || "scripts/ux-rules/run.mjs", "--surface", surface, "--out", out];
      try {
        const orbit = await orbitBinary(locations);
        if (surface === "cli") args.push("--orbit", orbit);
        if (surface === "ui") args.push("--fixture");
        const env = { ...process.env, ORBIT_SCRATCH_DIR: out, ORBIT_VISUAL_ORBIT: orbit };
        if (kit.playwrightModule) env.PLAYWRIGHT_MODULE = kit.playwrightModule;
        if (kit.playwrightBrowsersPath) env.PLAYWRIGHT_BROWSERS_PATH = kit.playwrightBrowsersPath;
        await exec(process.execPath, args, { env, maxBuffer: 4 * 1024 * 1024 });
        const report = JSON.parse(await readFile(path.join(out, "ux-rules.json"), "utf8"));
        if (!Array.isArray(report.results)) throw new Error("runner report has no results array");
        commit ||= report.commit || "";
        results.push(...report.results.map((result) => ({ ...result, surface })));
      } catch (error) {
        const detail = (error.stderr || error.message || String(error)).trim();
        results.push({ surface, rule: `${surface}.runner`, target: { id: surface, argv: [] }, status: "error", severity: "medium", measurements: { reason: detail }, evidence: [], fingerprint: `${surface}.runner|${surface}` });
      }
    }
    const summary = { pass: 0, fail: 0, error: 0, skipped: 0 };
    for (const result of results) if (Object.hasOwn(summary, result.status)) summary[result.status]++;
    process.stdout.write(`${JSON.stringify({ schema_version: 1, surface: "mixed", commit, results, summary })}\n`);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

main().catch((error) => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });
