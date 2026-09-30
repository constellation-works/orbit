// A link can name a workspace the server no longer serves (renamed, removed, or
// unavailable). The dashboard used to keep that scope while the selector fell
// back to "All workspaces", so every panel failed with "unknown workspace" and
// re-choosing the selector's visible option changed nothing. It now falls back
// to the server's default workspace and repairs the address.
import assert from "node:assert/strict";

location.search = "?workspace=gone";

const requestedWorkspaces = [];
const respond = (payload) => ({ ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) });
globalThis.fetch = async (path) => {
  const url = new URL(path, "http://dashboard.test");
  const workspace = url.searchParams.get("workspace");
  if (url.pathname !== "/api/workspaces") requestedWorkspaces.push(workspace);
  if (url.pathname === "/api/workspaces") {
    return respond([
      { id: "one", name: "one", status: "active", is_default: true },
      { id: "two", name: "two", status: "active" },
      { id: "away", name: "away", status: "unavailable" },
    ]);
  }
  if (url.pathname === "/api/tasks") return respond({ items: [], total: 0, limit: 50, truncated: false });
  return respond([]);
};

await import("./app.js");
for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));

const { getWorkspace } = await import("./js/common.js");

assert.equal(getWorkspace(), "one", "an unknown workspace in the link falls back to the default workspace");
assert.ok(requestedWorkspaces.length > 0, "the dashboard loads panels after choosing the fallback");
assert.ok(
  requestedWorkspaces.every((workspace) => workspace === "one"),
  `no request may target the unknown workspace: ${JSON.stringify(requestedWorkspaces)}`,
);
assert.equal(
  new URLSearchParams(location.search).get("workspace"),
  "one",
  "the address is repaired so a reload does not repeat the failure",
);
