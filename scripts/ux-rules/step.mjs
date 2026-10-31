// Script-step adapter: run each surface independently and return one document.
import { execFile } from "node:child_process";
import { access, mkdtemp, mkdir, readFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";

const exec = promisify(execFile);

async function orbitBinary() {
  if (process.env.UX_RULES_ORBIT) return process.env.UX_RULES_ORBIT;
  for (const directory of (process.env.PATH || "").split(path.delimiter)) {
    const candidate = path.resolve(directory, "orbit");
    try { await access(candidate); return candidate; } catch { /* try next */ }
  }
  throw new Error("orbit binary not found on PATH; set UX_RULES_ORBIT");
}

async function main() {
  let inputText = "";
  for await (const chunk of process.stdin) inputText += chunk;
  const input = JSON.parse(inputText);
  if (!Array.isArray(input.surfaces) || input.surfaces.some((s) => typeof s !== "string")) {
    throw new Error("surfaces must be an array of strings");
  }
  const scratch = await mkdtemp(path.join(os.tmpdir(), "ux-rules-"));
  const results = [];
  let commit = "";
  try {
    for (const [index, surface] of input.surfaces.entries()) {
      const out = path.join(scratch, String(index));
      await mkdir(out);
      const args = [input.runner || "scripts/ux-rules/run.mjs", "--surface", surface, "--out", out];
      try {
        const orbit = await orbitBinary();
        if (surface === "cli") args.push("--orbit", orbit);
        if (surface === "ui") args.push("--fixture");
        await exec(process.execPath, args, { env: { ...process.env, ORBIT_SCRATCH_DIR: out, ORBIT_VISUAL_ORBIT: orbit }, maxBuffer: 4 * 1024 * 1024 });
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
