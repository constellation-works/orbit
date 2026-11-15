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

import { filingDocument } from "../lib/filing-report.mjs";

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
    } else if (url.pathname === "/boom.js") {
      response.setHeader("Content-Type", "text/javascript");
      response.end(state.repaired ? "void 0;" : 'function explode(){throw new Error("fixture exploded");} explode();');
    } else if (url.pathname === "/errors") {
      response.end('<!doctype html><body id="ready">Error fixture<script src="/boom.js"></script></body>');
    } else if (url.pathname === "/ambiguous") {
      // No id or data-key anywhere: the elements have no stable identity.
      response.end('<!doctype html><style>p{font-size:9px}</style><div id="ready">Ambiguous fixture</div><section><p>first unanchored label</p><p>second unanchored label</p></section>');
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

test("public runner keeps failures without a stable element identity on their aggregate fingerprint and archives them", { skip: !enabled, timeout: 120000 }, async (t) => {
  const { run } = await fixture(t);
  const first = await run("ambiguous-before", "ui.undersized-text", [["ambiguous", "/ambiguous"]]);
  const failing = first.report.results.filter((result) => result.status === "fail");
  assert.ok(failing.length > 0, JSON.stringify(first.report.summary));
  for (const result of failing) {
    assert.equal(result.defect, undefined);
    assert.equal(result.original_fingerprint, undefined, "no per-member task identity from DOM order");
    assert.equal(result.fingerprint, `ui.undersized-text|ambiguous|dark|${result.target.width}|default`);
    assert.ok(result.measurements.ambiguous_failures > 0);
  }
  assert.equal(first.report.summary.ambiguous_failures, failing.reduce((sum, result) => sum + result.measurements.ambiguous_failures, 0));
  const raw = JSON.parse(gunzipSync(Buffer.concat(await Promise.all(first.report.archive.parts.map((part) => readFile(path.join(first.out, part.path)))))));
  assert.equal(raw.results.filter((result) => result.status === "fail" && !result.defect).length, first.report.summary.ambiguous_failures);
  assert.ok(raw.aggregates.every((aggregate) => Array.isArray(aggregate.observations)), "declared observations are archived before deduplication");
  const again = await run("ambiguous-again", "ui.undersized-text", [["ambiguous", "/ambiguous"]]);
  assert.deepEqual(again.report.results.map((result) => [result.fingerprint, result.status, result.measurements]),
    first.report.results.map((result) => [result.fingerprint, result.status, result.measurements]));
});

test("public runner credits a tracked page-error cause once its page's console scope completes clean", { skip: !enabled, timeout: 120000 }, async (t) => {
  const { run, state } = await fixture(t);
  const views = [["errors", "/errors"]];
  const first = await run("errors-before", "ui.console-clean", views);
  const failures = first.report.results.filter((result) => result.status === "fail");
  assert.ok(failures.length > 0 && failures.every((result) => result.measurements.observation.kind === "page-error" && result.defect), JSON.stringify(failures));
  const groups = Map.groupBy(failures, (result) => result.defect.key);
  const context = path.join(path.dirname(first.out), "errors-owner-context.json");
  await writeFile(context, JSON.stringify({ schema_version: 1, groups: [...groups.values()].map((members) => ({
    surface: "ui", rule: "ui.console-clean", defect: members[0].defect, members: members.map((member) => member.original_fingerprint),
  })) }));
  state.repaired = true;
  const untracked = await run("errors-untracked", "ui.console-clean", views);
  assert.equal(untracked.report.summary.absence_credits, 0);
  const after = await run("errors-after", "ui.console-clean", views, ["--owner-context", context]);
  assert.equal(after.report.summary.fail, 0);
  for (const before of failures) {
    const credit = after.report.results.find((result) => result.fingerprint === before.fingerprint);
    assert.equal(credit?.status, "pass", `${before.fingerprint} needs a pass with its own identity`);
    assert.deepEqual(credit.target, before.target);
    assert.deepEqual(credit.defect, before.defect);
  }
  assert.equal(after.report.summary.absence_credits, failures.length);
});

test("filingDocument counts only tracked groups absent from reported observations", () => {
  const member = (key, fingerprint) => ({
    surface: "ui",
    rule: "ui.x",
    defect: { version: 1, key },
    status: "fail",
    fingerprint,
    original_fingerprint: fingerprint,
    target: {},
    measurements: {},
    evidence: [],
  });
  const contextFor = (keys) => ({
    schema_version: 1,
    groups: keys.map((key) => ({ surface: "ui", rule: "ui.x", defect: { version: 1, key } })),
  });

  const observed = filingDocument({
    surface: "ui",
    results: [member("k1", "f1")],
    ownerContext: contextFor(["k1"]),
  });
  assert.equal(observed.summary.tracked_groups, 1);
  assert.equal(observed.summary.tracked_groups_unobserved, 0);

  const missing = filingDocument({ surface: "ui", results: [], ownerContext: contextFor(["k1"]) });
  assert.equal(missing.summary.tracked_groups_unobserved, 1);

  const partial = filingDocument({
    surface: "ui",
    results: [member("k1", "f1")],
    ownerContext: contextFor(["k1", "k2"]),
  });
  assert.equal(partial.summary.tracked_groups_unobserved, 1);
});
