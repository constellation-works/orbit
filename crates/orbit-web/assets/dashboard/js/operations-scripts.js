// Automation › Scripts: each workspace script's grant state, the diff since
// the last grant and its open grant request. Granting is an operator-session
// action bound to the digest the operator reviewed here.

import { captureWorkspaceVisit, el, fetchJson, requestJson, requestPanel } from './common.js';
import { actionReason, explainUnavailable, feedback, setRailSubtabCount } from './operations-shared.js';

const STATE_LABELS = {
  granted: "granted",
  stale: "stale",
  ungranted: "ungranted",
  revoked: "revoked",
  orphaned: "orphaned",
  witness_refused: "witness refused",
};

const short = digest => (digest ? String(digest).slice(0, 12) : "-");
const scriptDiffOpen = new Map();
const scriptDiffScroll = new Map();

function capabilityText(caps) {
  if (!caps) return "none";
  const parts = [`network ${caps.network || "none"}`, `host ${caps.host_role || "owner"}`];
  if (caps.read_roots?.length) parts.push(`read ${caps.read_roots.join(", ")}`);
  if (caps.write_roots?.length) parts.push(`write ${caps.write_roots.join(", ")}`);
  if (caps.env_pass?.length) parts.push(`env ${caps.env_pass.join(", ")}`);
  if (caps.timeout_seconds != null) parts.push(`timeout ${caps.timeout_seconds}s`);
  if (caps.output_max_files != null) parts.push(`≤${caps.output_max_files} outputs`);
  if (caps.output_max_file_bytes != null) parts.push(`≤${caps.output_max_file_bytes} B/output`);
  return parts.join(" · ");
}

function fact(label, value) {
  return el("div", { class: "script-fact" }, [
    el("span", { class: "script-fact-label", text: label }),
    el("span", { class: "script-fact-value", text: value }),
  ]);
}

function diffBlock(script) {
  if (!script.diff) return null;
  const lines = script.diff.split("\n").map(line => {
    const kind = line.startsWith("+++") || line.startsWith("---") ? "head"
      : line.startsWith("+") ? "add"
      : line.startsWith("-") ? "del"
      : line.startsWith("@@") ? "hunk" : "ctx";
    return el("span", { class: `script-diff-${kind}`, text: `${line}\n` });
  });
  const pre = el("pre", {}, lines);
  const details = el("details", { class: "script-diff" }, [
    el("summary", { text: script.granted_digest ? "Diff since the granted bytes" : "Files to grant" }),
    pre,
  ]);
  details.open = scriptDiffOpen.has(script.name) ? scriptDiffOpen.get(script.name) : script.state !== "granted";
  details.addEventListener("toggle", () => scriptDiffOpen.set(script.name, details.open));
  pre.addEventListener("scroll", () => scriptDiffScroll.set(script.name, pre.scrollTop), { passive: true });
  requestAnimationFrame(() => { const s = scriptDiffScroll.get(script.name); if (s) pre.scrollTop = s; });
  return details;
}

function grantButton(payload, script) {
  if (script.state === "granted" || !script.current_digest || script.state === "orphaned") return null;
  const button = el("button", { class: "operation-button", type: "button", text: `Grant ${short(script.current_digest)}` });
  const reason = actionReason(payload, "grant");
  if (reason) {
    button.disabled = true;
    return explainUnavailable(button, reason);
  }
  button.addEventListener("click", async () => {
    button.disabled = true;
    // Pin the host and workspace the operator reviewed: the request stays
    // bound to this visit even when the operator switches scope mid-flight,
    // and a late reply never paints or refreshes another visit (A→B→A is a
    // new visit, so returning to the same id is still stale).
    const visit = captureWorkspaceVisit();
    const grantPath = `/api/scripts/${encodeURIComponent(script.name)}/grant`;
    const grantBody = { digest: script.current_digest };
    try {
      await requestJson(grantPath, "POST", grantBody, visit);
    } catch (error) {
      if (!visit.isCurrent() || error?.code === "stale_workspace_visit") return;
      button.disabled = false;
      feedback("script-operation-feedback", "error", `Grant refused: ${error.message || error}`);
      return;
    }
    if (!visit.isCurrent()) return;
    feedback("script-operation-feedback", "success", `Granted ${script.name} at ${short(script.current_digest)}.`);
    try {
      await fetchAndRenderScripts();
    } catch (error) {
      // A successful grant followed by a failed readback keeps the grant
      // outcome: the refresh failure is a separate warning, not a refusal,
      // and the stale Grant button stays disabled so it cannot invite a
      // duplicate write. Mirrors runOperation's refresh-failure handling.
      if (!visit.isCurrent()) return;
      document.getElementById("script-operation-feedback")?.appendChild(el("span", {
        text: `\nRefresh failed: ${error.message || error}. The grant succeeded; refresh before granting again.`,
      }));
    }
  });
  return button;
}

function scriptCard(payload, script) {
  const state = el("span", { class: "pill", text: STATE_LABELS[script.state] || script.state });
  state.dataset.status = script.state === "granted" ? "done" : script.state === "stale" ? "blocked" : "todo";
  const facts = [
    fact("Entry", script.entry || "-"),
    fact("Current digest", short(script.current_digest)),
    fact("Granted digest", short(script.granted_digest)),
  ];
  if (script.stale_commit) facts.push(fact("Changed by", script.stale_commit));
  facts.push(fact("Requests", capabilityText(script.requested)));
  facts.push(fact("Grant holds", capabilityText(script.granted)));
  if (script.grant?.grantor) facts.push(fact("Granted by", `${script.grant.grantor} (${script.grant.provenance || "-"})`));
  const request = script.open_request;
  const requestRow = request
    ? el("div", { class: "script-request" }, [
        el("span", { class: "script-fact-label", text: "Open request" }),
        el("span", { class: "script-fact-value", text: `${request.id} on ${request.task_id}: ${request.summary || request.question || ""}` }),
      ])
    : null;
  return el("article", { class: "script-card", "aria-label": `Script ${script.name}` }, [
    el("header", { class: "script-card-head" }, [
      el("span", { class: "script-name", text: script.name }),
      state,
      grantButton(payload, script),
    ]),
    script.detail ? el("p", { class: "script-detail", text: script.detail }) : null,
    el("div", { class: "script-facts" }, facts),
    requestRow,
    diffBlock(script),
  ]);
}

function renderScripts(payload) {
  const body = document.getElementById("scripts-body");
  const count = document.getElementById("scripts-count");
  if (!body) return;
  const scripts = payload?.scripts || [];
  const pending = scripts.filter(script => script.state !== "granted").length;
  if (count) count.textContent = String(scripts.length);
  setRailSubtabCount("rail-count-ops-scripts", pending ? String(pending) : "");
  body.replaceChildren();
  if (!scripts.length) {
    body.append(el("div", { class: "empty-state", text: "No workspace scripts. Add a manifest under .orbit/resources/scripts/ to declare one." }));
    return;
  }
  for (const script of scripts) body.append(scriptCard(payload, script));
}

export async function fetchAndRenderScripts() {
  await requestPanel("scripts-body", "scripts", () => fetchJson("/api/scripts"), renderScripts, "scripts-count");
}
