import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
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

test("step finds orbit and the browser kit from explicit locations, not env passthrough", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "ux-step-kit-test-"));
  try {
    const kit = path.join(dir, "browser-kit");
    mkdirSync(path.join(kit, "kit/node_modules/playwright"), { recursive: true });
    writeFileSync(path.join(kit, "kit/node_modules/playwright/index.mjs"), "export {};\n");
    mkdirSync(path.join(kit, "browsers"));
    const orbit = path.join(dir, "orbit");
    writeFileSync(orbit, "");
    const runner = path.join(dir, "runner.mjs");
    writeFileSync(runner, `import {writeFileSync} from 'node:fs';\nimport path from 'node:path';\nconst args=process.argv;\nconst out=args[args.indexOf('--out')+1];\nconst orbit=args.includes('--orbit')?args[args.indexOf('--orbit')+1]:'';\nwriteFileSync(path.join(out,'ux-rules.json'),JSON.stringify({commit:'abc',results:[{rule:'x.example',status:'pass',fingerprint:'x',measurements:{orbit,pwModule:process.env.PLAYWRIGHT_MODULE||'',pwBrowsers:process.env.PLAYWRIGHT_BROWSERS_PATH||''}}]}));\n`);
    const env = { ...process.env };
    delete env.UX_RULES_ORBIT;
    delete env.PLAYWRIGHT_MODULE;
    delete env.PLAYWRIGHT_BROWSERS_PATH;
    const text = execFileSync(process.execPath, ["scripts/ux-rules/step.mjs", "--browser-kit", kit], {
      input: JSON.stringify({ surfaces: ["cli", "ui"], runner, orbit }),
      env, encoding: "utf8",
    });
    const report = JSON.parse(text);
    assert.equal(report.results[0].measurements.orbit, orbit);
    for (const result of report.results) {
      assert.equal(result.measurements.pwModule, path.join(kit, "kit/node_modules/playwright/index.mjs"));
      assert.equal(result.measurements.pwBrowsers, path.join(kit, "browsers"));
    }
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
