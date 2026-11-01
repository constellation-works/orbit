import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, existsSync, writeFileSync, rmSync } from "node:fs";
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

const probeRunner = `import {writeFileSync} from 'node:fs';\nimport path from 'node:path';\nconst args=process.argv;\nconst out=args[args.indexOf('--out')+1];\nwriteFileSync(path.join(out,'ux-rules.json'),JSON.stringify({commit:'abc',results:[{rule:'x.example',status:'pass',fingerprint:'x',measurements:{ld:process.env.LD_LIBRARY_PATH||'',fonts:process.env.FONTCONFIG_FILE||'',pwModule:process.env.PLAYWRIGHT_MODULE||''}}]}));\n`;

function runStepProbe(dir, kit, extraEnv = {}) {
  const runner = path.join(dir, "runner.mjs");
  writeFileSync(runner, probeRunner);
  const env = { ...process.env, ...extraEnv };
  for (const name of ["UX_RULES_ORBIT", "PLAYWRIGHT_MODULE", "PLAYWRIGHT_BROWSERS_PATH"]) delete env[name];
  if (!("LD_LIBRARY_PATH" in extraEnv)) delete env.LD_LIBRARY_PATH;
  if (!("FONTCONFIG_FILE" in extraEnv)) delete env.FONTCONFIG_FILE;
  const text = execFileSync(process.execPath, ["scripts/ux-rules/step.mjs", "--browser-kit", kit], {
    input: JSON.stringify({ surfaces: ["ui"], runner, orbit: process.execPath }),
    env, encoding: "utf8",
  });
  return JSON.parse(text).results[0].measurements;
}

test("step gives the ui child the kit's unpacked libraries and font config", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "ux-step-libs-test-"));
  try {
    const kit = path.join(dir, "browser-kit");
    const lib = path.join(kit, "root/usr/lib/x86_64-linux-gnu");
    mkdirSync(lib, { recursive: true });
    mkdirSync(path.join(kit, "root/usr/lib/not-a-triplet"));
    writeFileSync(path.join(kit, "fonts.conf"), "<fontconfig/>\n");
    const measured = runStepProbe(dir, kit);
    assert.equal(measured.ld, lib);
    assert.equal(measured.fonts, path.join(kit, "fonts.conf"));
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("step keeps inherited loader and font settings and leaves system-library kits alone", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "ux-step-sys-test-"));
  try {
    const kit = path.join(dir, "browser-kit");
    const lib = path.join(kit, "root/usr/lib/x86_64-linux-gnu");
    mkdirSync(lib, { recursive: true });
    writeFileSync(path.join(kit, "fonts.conf"), "<fontconfig/>\n");
    const measured = runStepProbe(dir, kit, { LD_LIBRARY_PATH: "/host/lib", FONTCONFIG_FILE: "/host/fonts.conf" });
    assert.equal(measured.ld, `${lib}${path.delimiter}/host/lib`);
    assert.equal(measured.fonts, "/host/fonts.conf");

    const plain = path.join(dir, "plain-kit");
    mkdirSync(path.join(plain, "browsers"), { recursive: true });
    const untouched = runStepProbe(dir, plain);
    assert.equal(untouched.ld, "");
    assert.equal(untouched.fonts, "");
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("step exports evidence before deleting surface scratch", () => {
  const scratch = process.env.ORBIT_SCRATCH_DIR || path.resolve(".orbit/tmp");
  mkdirSync(scratch, { recursive: true });
  const dir = mkdtempSync(path.join(scratch, "ux-step-evidence-"));
  try {
    const out = path.join(dir, "declared");
    mkdirSync(out);
    const runner = path.join(dir, "runner.mjs");
    writeFileSync(runner, `import {writeFileSync,mkdirSync} from 'node:fs'; import path from 'node:path';
const out=process.argv[process.argv.indexOf('--out')+1];
mkdirSync(path.join(out,'nested'));
writeFileSync(path.join(out,'nested/capture.png'), Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGMwmPDgPwAFNAKg4ubHmAAAAABJRU5ErkJggg==', 'base64'));
writeFileSync(path.join(out,'ux-rules.json'),JSON.stringify({results:[{rule:'ui.x',status:'fail',fingerprint:'x',evidence:['nested/capture.png']}]}));`);
    const report = JSON.parse(execFileSync(process.execPath, ["scripts/ux-rules/step.mjs"], {
      input: JSON.stringify({ surfaces: ["ui", "ui"], runner, orbit: process.execPath }),
      env: { ...process.env, TMPDIR: dir, ORBIT_SCRIPT_OUT: out }, encoding: "utf8",
    }));
    const evidence = report.results[0].evidence[0];
    const bytes = readFileSync(path.join(out, evidence));
    assert.equal(bytes.toString("hex", 0, 8), "89504e470d0a1a0a");
    assert.equal(evidence, `${createHash("sha256").update(bytes).digest("hex")}.png`);
    assert.equal(report.results[0].fingerprint, "x");
    assert.deepEqual(report.results[0].evidence, report.results[1].evidence);
    assert.equal(readdirSync(out).length, 1, "equal captures across surfaces share one output");
    assert.equal(report.results[0].status, "fail");
    assert.ok(!readdirSync(dir).some((name) => name.startsWith("ux-rules-")), "surface scratch must be removed");
    assert.ok(!existsSync(path.join(dir, "capture.png")));
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("step refuses evidence loss and unsafe or exhausted exports explicitly", () => {
  const scratch = process.env.ORBIT_SCRATCH_DIR || path.resolve(".orbit/tmp");
  mkdirSync(scratch, { recursive: true });
  const dir = mkdtempSync(path.join(scratch, "ux-evidence-rejected-"));
  try {
    const runner = path.join(dir, "runner.mjs");
    writeFileSync(runner, `import {writeFileSync,symlinkSync} from 'node:fs'; import path from 'node:path';
const out=process.argv[process.argv.indexOf('--out')+1];
const mode=process.argv[process.argv.indexOf('--surface')+1];
let evidence=['capture.png'];
if(mode==='missing'){} else if(mode==='malformed')evidence=[42];
else if(mode==='escaped')evidence=['../capture.png'];
else if(mode==='symlink')symlinkSync('/etc/passwd',path.join(out,'capture.png'));
else if(mode==='parent-symlink'){evidence=['nested/passwd'];symlinkSync('/etc',path.join(out,'nested'));}
else if(mode==='extension'){evidence=['capture.abcdefghijkl'];writeFileSync(path.join(out,evidence[0]),'x');}
else if(mode==='conflict'){evidence=['capture.png','capture.txt'];for(const name of evidence)writeFileSync(path.join(out,name),'x');}
else if(mode==='large')writeFileSync(path.join(out,'capture.png'),Buffer.alloc(1048577));
else if(mode==='count'){evidence=Array.from({length:65},(_,i)=>i+'.png');for(const [i,name] of evidence.entries())writeFileSync(path.join(out,name),String(i));}
writeFileSync(path.join(out,'ux-rules.json'),JSON.stringify({results:[{rule:'x',status:'fail',fingerprint:'x',evidence}]}));`);
    for (const [mode, diagnostic] of [
      ["missing", /ENOENT/], ["malformed", /invalid evidence path/],
      ["escaped", /invalid evidence path/], ["symlink", /symlink/], ["parent-symlink", /symlinks are refused/],
      ["large", /1048576 bytes/], ["count", /64 unique/],
      ["extension", /ASCII file extension/], ["conflict", /conflicting file types/],
    ]) {
      const out = path.join(dir, mode);
      mkdirSync(out);
      assert.throws(() => execFileSync(process.execPath, ["scripts/ux-rules/step.mjs"], {
        input: JSON.stringify({ surfaces: [mode], runner, orbit: process.execPath }),
        env: { ...process.env, TMPDIR: dir, ORBIT_SCRIPT_OUT: out }, encoding: "utf8", stdio: ["pipe", "pipe", "pipe"],
      }), (error) => {
        assert.match(error.stderr, diagnostic, mode);
        assert.equal(error.stdout, "", "no successful report may claim unavailable evidence");
        return true;
      });
      assert.ok(!readdirSync(dir).some((name) => name.startsWith("ux-rules-")), mode);
    }
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
