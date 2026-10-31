import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

test("step merges surfaces and records a runner failure", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "ux-step-test-"));
  try {
    const runner = path.join(dir, "runner.mjs");
    writeFileSync(runner, `import {writeFileSync} from 'node:fs';\nimport path from 'node:path';\nconst args=process.argv; const surface=args[args.indexOf('--surface')+1];\nif(surface==='bad'){console.error('missing prerequisite');process.exit(1)}\nconst out=args[args.indexOf('--out')+1];\nwriteFileSync(path.join(out,'ux-rules.json'),JSON.stringify({commit:'abc',results:[{rule:surface+'.example',status:'pass',fingerprint:surface}]}));\n`);
    const text = execFileSync(process.execPath, ["scripts/ux-rules/step.mjs"], {
      input: JSON.stringify({ surfaces: ["cli", "ui", "bad"], runner }),
      env: { ...process.env, UX_RULES_ORBIT: process.execPath }, encoding: "utf8",
    });
    const report = JSON.parse(text);
    assert.equal(report.surface, "mixed");
    assert.equal(report.commit, "abc");
    assert.deepEqual(report.results.map((r) => [r.surface, r.status]), [["cli", "pass"], ["ui", "pass"], ["bad", "error"]]);
    assert.match(report.results[2].measurements.reason, /missing prerequisite/);
    assert.deepEqual(report.summary, { pass: 2, fail: 0, error: 1, skipped: 0 });
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
