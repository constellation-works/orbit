// Script-step adapter: run each surface independently and return one document.
import { execFile } from "node:child_process";
import { constants } from "node:fs";
import { createHash } from "node:crypto";
import { access, mkdtemp, mkdir, readdir, open, rm, stat, writeFile } from "node:fs/promises";
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
  return { playwrightModule, playwrightBrowsersPath, ...(await unpackedKitRuntime(locations.browserKit)) };
}

// The kit installer's Linux-without-root layout: libraries unpacked under
// `<kit>/root/{usr/lib,lib}/*-linux-gnu` and a `<kit>/fonts.conf`. These are
// the paths its `env.sh` exports; the adapter derives them from the granted
// kit root instead of evaluating that shell file. Hosts using system
// libraries or macOS have neither, so nothing is set for them.
async function unpackedKitRuntime(root) {
  if (!root) return { libraryDirs: [], fontsConf: "" };
  const libraryDirs = [];
  for (const parent of ["usr/lib", "lib"]) {
    const base = path.join(root, "root", parent);
    let names;
    try { names = await readdir(base); } catch { continue; }
    for (const name of names.filter((n) => n.endsWith("-linux-gnu")).sort()) {
      try { if ((await stat(path.join(base, name))).isDirectory()) libraryDirs.push(path.join(base, name)); } catch { /* skip */ }
    }
  }
  const fontsConf = await firstExisting([path.join(root, "fonts.conf")]);
  return { libraryDirs, fontsConf };
}


// Directory-relative opens use pinned handles, including each intermediate
// directory. O_NOFOLLOW on only the final file would still race a directory
// swapped for a symlink. The Linux script profile already mounts /proc.
async function readSurfaceFile(root, relative, limit) {
  if (typeof relative !== "string" || !relative || path.isAbsolute(relative) || relative.includes("\\") || relative.split("/").some((part) => !part || part === "." || part === "..")) {
    throw new Error(`invalid evidence path: ${JSON.stringify(relative)}`);
  }
  if (process.platform !== "linux" || constants.O_NOFOLLOW === undefined || constants.O_DIRECTORY === undefined) {
    throw new Error("safe evidence reads require the Linux script profile with /proc/self/fd; run this adapter there");
  }
  const handles = [];
  let directory = root;
  try {
    const parts = relative.split("/");
    for (const part of parts.slice(0, -1)) {
      directory = await open(`/proc/self/fd/${directory.fd}/${part}`, constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
      handles.push(directory);
    }
    const file = await open(`/proc/self/fd/${directory.fd}/${parts.at(-1)}`, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
    handles.push(file);
    const info = await file.stat();
    if (!info.isFile() || info.nlink !== 1 || info.size > limit) throw new Error(`evidence must be a single-link regular file no larger than ${limit} bytes: ${relative}`);
    const bytes = Buffer.alloc(limit + 1);
    let total = 0;
    while (total < bytes.length) {
      const { bytesRead } = await file.read(bytes, total, bytes.length - total, total);
      if (bytesRead === 0) break;
      total += bytesRead;
    }
    if (total > limit) throw new Error(`evidence exceeds ${limit} bytes: ${relative}`);
    return bytes.subarray(0, total);
  } catch (error) {
    throw new Error(`cannot safely read evidence ${relative}: ${error.message}; symlinks are refused; regenerate the evidence and rerun`);
  } finally {
    for (const handle of handles.reverse()) await handle.close();
  }
}

// Content names deduplicate repeated captures across surfaces and results.
async function exportEvidence(results, root, exported) {
  for (const result of results) {
    if (result.evidence === undefined) continue;
    if (!Array.isArray(result.evidence)) throw new Error("evidence must be an array of relative paths");
    const names = [];
    for (const relative of result.evidence) {
      const bytes = await readSurfaceFile(root, relative, 1024 * 1024);
      let suffix = path.extname(relative).toLowerCase();
      if (!/^\.[a-z0-9]{1,10}$/.test(suffix)) throw new Error(`evidence needs a simple ASCII file extension of at most 10 characters: ${relative}`);
      if (suffix === ".jpeg") suffix = ".jpg";
      const digest = createHash("sha256").update(bytes).digest("hex");
      let name = exported.get(digest);
      if (name && path.extname(name) !== suffix) throw new Error(`identical evidence has conflicting file types: ${name} and ${relative}; use one consistent evidence type`);
      if (!name) {
        name = `${digest}${suffix}`;
        if (exported.size >= 64) throw new Error("evidence exceeds 64 unique output files; reduce captures or split the run; the adapter limit is 64");
        if (!process.env.ORBIT_SCRIPT_OUT) throw new Error("evidence requires declared ORBIT_SCRIPT_OUT outputs");
        await writeFile(path.join(process.env.ORBIT_SCRIPT_OUT, name), bytes, { flag: "wx" });
        exported.set(digest, name);
      }
      names.push(name);
    }
    result.evidence = names;
  }
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
  const exported = new Map();
  try {
    for (const [index, surface] of input.surfaces.entries()) {
      const out = path.join(scratch, String(index));
      await mkdir(out);
      const root = await open(out, constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW);
      const args = [input.runner || "scripts/ux-rules/run.mjs", "--surface", surface, "--out", out];
      try {
        const orbit = await orbitBinary(locations);
        if (surface === "cli") args.push("--orbit", orbit);
        if (surface === "ui") args.push("--fixture");
        const env = { ...process.env, ORBIT_SCRATCH_DIR: out, ORBIT_VISUAL_ORBIT: orbit };
        if (kit.playwrightModule) env.PLAYWRIGHT_MODULE = kit.playwrightModule;
        if (kit.playwrightBrowsersPath) env.PLAYWRIGHT_BROWSERS_PATH = kit.playwrightBrowsersPath;
        // Kit libraries go in front of any inherited loader path; an explicit
        // FONTCONFIG_FILE already in the environment is kept.
        if (kit.libraryDirs.length) {
          env.LD_LIBRARY_PATH = [...kit.libraryDirs, ...(process.env.LD_LIBRARY_PATH ? [process.env.LD_LIBRARY_PATH] : [])].join(path.delimiter);
        }
        if (kit.fontsConf && !process.env.FONTCONFIG_FILE) env.FONTCONFIG_FILE = kit.fontsConf;
        await exec(process.execPath, args, { env, maxBuffer: 4 * 1024 * 1024 });
        const reportBytes = await readSurfaceFile(root, "ux-rules.json", 4 * 1024 * 1024).catch((error) => { error.evidenceFailure = true; throw error; });
        const report = JSON.parse(reportBytes.toString("utf8"));
        if (!Array.isArray(report.results)) throw new Error("runner report has no results array");
        commit ||= report.commit || "";
        await exportEvidence(report.results, root, exported).catch((error) => { error.evidenceFailure = true; throw error; });
        results.push(...report.results.map((result) => ({ ...result, surface })));
      } catch (error) {
        if (error.evidenceFailure) throw error;
        const detail = (error.stderr || error.message || String(error)).trim();
        results.push({ surface, rule: `${surface}.runner`, target: { id: surface, argv: [] }, status: "error", severity: "medium", measurements: { reason: detail }, evidence: [], fingerprint: `${surface}.runner|${surface}` });
      } finally { await root.close(); }
    }
    const summary = { pass: 0, fail: 0, error: 0, skipped: 0 };
    for (const result of results) if (Object.hasOwn(summary, result.status)) summary[result.status]++;
    process.stdout.write(`${JSON.stringify({ schema_version: 1, surface: "mixed", commit, results, summary })}\n`);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

main().catch((error) => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });
