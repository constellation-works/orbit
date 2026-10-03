import assert from "node:assert/strict";

// A broken bookmark must leave a usable destination rather than interrupting
// initialization with URIError. Drive initial load and subsequent hash changes.
location.hash = "#runs/%E0%A4%A";
window.history = {
  replaceState: (_, __, url) => { location._hash = new URL(url, location.href).hash; },
};
const requests = [];
globalThis.fetch = async (path) => {
  const url = new URL(path, "http://dashboard.test");
  requests.push(url.pathname);
  const payload = url.pathname === "/api/workspaces"
    ? [{ id: "one", name: "One", status: "active", is_default: true }]
    : [];
  return { ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) };
};
await import("./app.js");
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
for (let i = 0; i < 5; i++) await tick();
assert.equal(document.title, "Runs · orbit", "a malformed bookmarked run opens the Runs list");
assert.match(location.hash, /^#diagnostics\/runs/, "the address is repaired to a usable run-list route");
assert.ok(!requests.some((path) => path.includes("%E0")), "a malformed ID is never sent to the server");

for (const id of ["%", "%GG", "%FF", "%C0%AF"]) {
  assert.doesNotThrow(() => { location.hash = `#runs/${id}`; });
  assert.equal(document.title, "Runs · orbit");
  assert.match(location.hash, /^#diagnostics\/runs/);
}

location.hash = "#tasks";
assert.equal(document.title, "Tasks · orbit", "navigation still works after invalid bookmarks");
location.hash = "#runs/run%20with%25percent/events";
assert.equal(document.title, "Runs · orbit", "valid percent-encoded run IDs keep working");
assert.equal(location.hash, "#runs/run%20with%25percent/events");
const { getActiveRunId } = await import("./js/run-detail.js");
for (let i = 0; i < 5; i++) await tick();
assert.equal(getActiveRunId(), "run with%percent");
