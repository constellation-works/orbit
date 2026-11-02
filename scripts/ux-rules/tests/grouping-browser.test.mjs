// ORB-21111: real browser -> public runner output. No Orbit state or builds.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm, statfs, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { gunzipSync } from "node:zlib";
import test from "node:test";

const exec = promisify(execFile);
const enabled = Boolean(process.env.PLAYWRIGHT_MODULE);
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");

async function fixture(t) {
  const disk = await statfs(os.tmpdir());
  assert.ok(1 - disk.bavail / disk.blocks < 0.8, "disk gate: create no fixtures at 80% capacity");
  const root = await mkdtemp(path.join(os.tmpdir(), "ux-grouping-browser-"));
  const state = { repaired: false, unknown: false };
  const server = createServer((request, response) => {
    const url = new URL(request.url, "http://localhost");
    if (url.pathname === "/shared.js") {
      response.setHeader("Content-Type", "text/javascript");
      response.end('function readShared(){fetch("/api/shared?workspace=alpha");fetch("/api/independent?workspace=beta");} readShared();');
    } else if (url.pathname.startsWith("/api/")) {
      response.writeHead(state.repaired ? 200 : 503, { "Content-Type": "application/json" });
      response.end('{}');
    } else if (url.pathname.startsWith("/console")) {
      response.end(`<!doctype html><body id="ready">Console fixture${state.unknown ? '<script>console.error("503 Service Unavailable")</script>' : '<script src="/shared.js"></script>'}</body>`);
    } else {
      response.end(`<!doctype html><style>body{margin:24px;background:#111;color:white}p{font-size:9px;width:50vw;padding:8px;background:#345}p:first-child{font-size:${state.repaired ? 14 : 9}px}</style><main id="ready"><p class="same">Repeated measured label</p><p class="same">Repeated measured label</p></main>`);
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const baseUrl = `http://127.0.0.1:${server.address().port}`;
  const run = async (name, rule, views, extra = []) => {
    const manifest = path.join(root, `${name}.toml`);
    await writeFile(manifest, `schema_version = 1\n[app.fixture]\nsetup = ["unused"]\nthemes = ["dark"]\n${views.map(([id, route]) => `[[view]]\nid = "${id}"\napp = "fixture"\ntitle = "Fixture"\nroute = "${route}"\nfixture = "unused"\nready = "#ready"\n`).join("")}`);
    const out = path.join(root, name);
    await exec(process.execPath, ["scripts/ux-rules/run.mjs", "--surface", "ui", "--base-url", baseUrl, "--manifest", manifest, "--rule", rule, "--out", out, ...extra], { maxBuffer: 4 * 1024 * 1024 });
    return { report: JSON.parse(await readFile(path.join(out, "ux-rules.json"), "utf8")), out };
  };
  t.after(async () => {
    await new Promise((resolve) => server.close(resolve));
    if (process.env.UX_GROUPING_KEEP_FIXTURE === "1") console.log(`ORB-21111 browser receipt: ${root}`);
    else await rm(root, { recursive: true, force: true });
  });
  return { root, state, run };
}

test("public runner groups one measured element across widths, retains distinct repeated elements and exact screenshots, and measures partial repairs", { skip: !enabled, timeout: 120000 }, async (t) => {
  const { run, state } = await fixture(t);
  const first = await run("layout-before", "ui.undersized-text", [["layout", "/layout"]]);
  const failures = first.report.results.filter((result) => result.status === "fail");
  assert.equal(first.report.schema_version, 1);
  assert.equal(failures.length, 4);
  const groups = Map.groupBy(failures, (result) => result.defect.key);
  assert.equal(groups.size, 2, "a shared nonunique selector must never collapse distinct elements");
  for (const members of groups.values()) {
    assert.deepEqual(members.map((result) => result.target.width).sort((a, b) => a - b), [375, 1440]);
    const images = [];
    for (const result of members) {
      assert.equal(result.original_fingerprint, `ui.undersized-text|layout|dark|${result.target.width}|default`);
      assert.notEqual(result.fingerprint, result.original_fingerprint);
      assert.equal(result.measurements.observation.selector, "p.same");
      assert.equal(result.evidence.length, 1);
      images.push(digest(await readFile(path.join(first.out, result.evidence[0]))));
    }
    assert.notEqual(images[0], images[1], "both original viewport PNG byte identities survive");
  }
  state.repaired = true;
  const after = await run("layout-partial", "ui.undersized-text", [["layout", "/layout"]]);
  assert.equal(after.report.summary.fail, 2);
  assert.equal(after.report.summary.pass, 2);
  assert.equal(after.report.summary.omitted_untracked_passing, 2, "untracked passing members are counted, not listed");
  for (const result of after.report.results) {
    assert.ok(failures.some((before) => before.fingerprint === result.fingerprint && before.defect.key === result.defect.key), "member identity survives measurement and pass/fail transitions");
  }
  // An owner context makes the repaired group's passing members reportable.
  const repaired = [...groups.keys()].find((key) => !after.report.results.some((result) => result.defect.key === key));
  const context = path.join(path.dirname(after.out), "owner-context.json");
  await writeFile(context, JSON.stringify({ schema_version: 1, groups: [{ surface: "ui", rule: "ui.undersized-text", defect: { version: 1, key: repaired } }] }));
  const tracked = await run("layout-tracked", "ui.undersized-text", [["layout", "/layout"]], ["--owner-context", context]);
  const passing = tracked.report.results.filter((result) => result.defect?.key === repaired);
  assert.deepEqual(passing.map((result) => [result.status, result.target.width]).sort(), [["pass", 1440], ["pass", 375]]);
  assert.equal(tracked.report.summary.tracked_groups, 1);
  // The archive holds every raw observation losslessly.
  const parts = await Promise.all(tracked.report.archive.parts.map((part) => readFile(path.join(tracked.out, part.path))));
  const raw = gunzipSync(Buffer.concat(parts));
  assert.equal(digest(raw), tracked.report.archive.uncompressed_sha256);
  assert.equal(JSON.parse(raw).results.length, tracked.report.summary.observations);
  const secondPage = await run("layout-other-view", "ui.undersized-text", [["other-layout", "/layout"]]);
  assert.equal(secondPage.report.results.some((result) => groups.has(result.defect.key)), false, "cross-page sharing needs explicit component evidence");
});

test("public runner attributes two independent 503 endpoints to shared initiators across pages, preserves query, and never merges generic 503 strings", { skip: !enabled, timeout: 120000 }, async (t) => {
  const { run, state } = await fixture(t);
  const first = await run("console-before", "ui.console-clean", [["console-one", "/console-one"], ["console-two", "/console-two"]]);
  const failures = first.report.results.filter((result) => result.status === "fail");
  assert.equal(failures.length, 8, JSON.stringify(failures));
  const groups = Map.groupBy(failures, (result) => result.defect.key);
  assert.equal(groups.size, 2, "each endpoint has one proven shared source cause across two pages and two widths");
  for (const members of groups.values()) {
    assert.equal(members.length, 4);
    for (const member of members) {
      const cause = member.measurements.observation;
      assert.equal(cause.method, "GET");
      assert.equal(cause.status, 503);
      assert.match(cause.endpoint, /\?workspace=(alpha|beta)$/);
      assert.match(cause.source, /shared\.js/);
      assert.equal(cause.failure_class, "http-error");
    }
  }
  state.repaired = true;
  const after = await run("console-after", "ui.console-clean", [["console-one", "/console-one"], ["console-two", "/console-two"]]);
  assert.equal(after.report.summary.fail, 0);
  // Repaired groups are untracked here, so their passes live in the archive.
  const raw = JSON.parse(gunzipSync(Buffer.concat(await Promise.all(after.report.archive.parts.map((part) => readFile(path.join(after.out, part.path)))))));
  for (const before of failures) assert.ok(raw.results.some((result) => result.status === "pass" && result.fingerprint === before.fingerprint), "an actual 200 response credits the same known request cause");
  assert.equal(raw.results.some((result) => result.status === "error"), false, "successful responses never become diagnostics errors");
  state.unknown = true;
  const unknown = await run("console-unknown", "ui.console-clean", [["console-one", "/console-one"], ["console-two", "/console-two"]]);
  const unknownFailures = unknown.report.results.filter((result) => result.status === "fail");
  assert.equal(unknownFailures.length, 4);
  assert.equal(new Set(unknownFailures.map((result) => result.defect.key)).size, 4, "generic status text without source evidence stays target scoped");
});
