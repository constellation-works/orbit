// Start policy and checkout information shared by the Tasks dock.

export const AUTO_DRAIN_APPROVE_RULE = "Approval needs context files and an assessed complexity (or the no-diff-expected tag) plus a clean task-pilot verification. Tasks tagged no-auto-approve are skipped.";

export function drainCheckout(payload, workspaceId = "") {
  const options = payload?.start_options && typeof payload.start_options === "object" ? payload.start_options : {};
  const replica = options.checkout ? options.checkout === "replica" : payload?.replica === true;
  workspaceId = options.workspace_id || workspaceId;
  return {
    replica,
    workspaceId,
    ownerHost: options.owner_host || "",
    pullSelector: options.pull_selector || "",
    pullRefusal: options.pull_refusal || "",
  };
}
