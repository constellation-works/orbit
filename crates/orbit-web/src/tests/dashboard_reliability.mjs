// The Reliability view is the one Health panel that used to fetch outside the
// shared panel-request path: a failed read left four blank panels under a green
// connection line, and a window with no settled runs drew a blank chart. These
// scenarios drive the shipped module against a fetch stub and observe what the
// panels say.
import assert from "node:assert/strict";

const { fetchAndRenderReliability } = await import("./js/reliability.js");

const node = (id) => document.getElementById(id);
const reply = (payload, status = 200) => ({
  ok: status === 200,
  status,
  json: async () => payload,
  text: async () => JSON.stringify(payload),
});

const payloadWith = (counts) => ({
  window: { label: "24h", bucket: "hour" },
  totals: { job_runs: { counts: { total: counts.succeeded + counts.failed } } },
  workspaces: [
    {
      workspace_id: "one",
      job_runs: {
        over_time: [{ bucket_start: "2026-09-29T00:00:00Z", counts: { ...counts, total: counts.succeeded + counts.failed } }],
      },
    },
  ],
});

let respond = () => reply({ error: "reliability boom" }, 500);
globalThis.fetch = async () => respond();

await assert.rejects(fetchAndRenderReliability(), /reliability boom/, "a failed read must reach the caller");
assert.ok(
  node("reliability-status").textContent.includes("Unable to load") &&
    node("reliability-status").textContent.includes("reliability boom"),
  `a failed read is stated in the panel: ${node("reliability-status").textContent}`,
);

respond = () => reply(payloadWith({ succeeded: 0, failed: 0 }));
await fetchAndRenderReliability();
assert.ok(
  !node("reliability-status").textContent.includes("Unable to load"),
  "a later successful read clears the failure",
);
assert.equal(
  node("reliability-over-time").children.length,
  1,
  "a window with no settled runs renders one explanation, not a series of empty bars",
);
assert.equal(node("reliability-over-time").children[0].dataset.key, "empty");

respond = () => reply(payloadWith({ succeeded: 3, failed: 1 }));
await fetchAndRenderReliability();
const chartChildren = node("reliability-over-time").children;
assert.equal(chartChildren.length, 1, "one bucket draws one bar");
assert.notEqual(chartChildren[0].dataset.key, "empty", "settled runs draw bars, not the empty state");
