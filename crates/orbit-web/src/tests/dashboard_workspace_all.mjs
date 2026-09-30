// "All workspaces" is a choice the address must be able to carry. It used to
// be written as the absence of `?workspace=`, which a reload reads as "pick the
// default workspace", so the aggregate view silently turned into one workspace.
import assert from "node:assert/strict";

location.search = "?workspace=all&window=7d";

const requestedWorkspaces = [];
const respond = (payload) => ({ ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) });
globalThis.fetch = async (path) => {
  const url = new URL(path, "http://dashboard.test");
  if (url.pathname === "/api/workspaces") {
    return respond([
      { id: "one", name: "one", status: "active", is_default: true },
      { id: "two", name: "two", status: "active" },
    ]);
  }
  requestedWorkspaces.push({ path: url.pathname, workspace: url.searchParams.get("workspace") });
  if (url.pathname === "/api/tasks/all") return respond({ items: [], total: 0, limit: 50, truncated: false });
  return respond([]);
};

await import("./app.js");
for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));

const { getWorkspace, isAggregateView, persistScopeToUrl } = await import("./js/common.js");
const search = () => new URLSearchParams(location.search);

assert.equal(getWorkspace(), null, "a link asking for all workspaces opens the aggregate view, not the default workspace");
assert.equal(isAggregateView(), true);
assert.ok(
  requestedWorkspaces.every((request) => request.workspace === null),
  `the aggregate view must not scope any request to a workspace: ${JSON.stringify(requestedWorkspaces)}`,
);
persistScopeToUrl();
assert.equal(search().get("workspace"), "all", "the aggregate choice stays in the address so a reload restores it");
assert.equal(search().get("window"), "7d", "the window survives beside it");

const rail = document.getElementById("rail-workspace");
const select = rail.children.find((node) => node.id === "workspace-select");
assert.ok(select, "the workspace selector is built");
const allOption = select.children.find((option) => option.value === "");
assert.equal(allOption.selected, true, "the selector shows All workspaces selected");

// Choosing a workspace replaces the token with its id; choosing All again restores it.
select.value = "two";
select.listeners.change();
assert.equal(getWorkspace(), "two");
assert.equal(search().get("workspace"), "two");
select.value = "";
select.listeners.change();
assert.equal(getWorkspace(), null);
assert.equal(search().get("workspace"), "all", "choosing All workspaces writes the token, not nothing");
