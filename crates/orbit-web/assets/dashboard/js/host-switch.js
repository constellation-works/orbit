// Host switcher [ORB-14680]: show another registered host's dashboard through
// the serving host's `/api/on/<host>/…` forward.
//
// The picker sits above the workspace picker and lists the serving host first,
// then the remotes of its host file (`/api/hosts?probe=false`, so opening it
// never probes). `?host=` names the selection; with none, the last choice this
// browser made is used while that host is still registered. The selected
// host's state is read from `/api/hosts/<host>/connection` when it is selected
// and on each refresh. A host that cannot be shown replaces every panel with
// one host-level state; version skew and refused writes are persistent notes.
// common.js owns the selection and rewrites every API path for it.

import {
  el,
  fetchJson,
  findRegisteredHost,
  getHost,
  getRegisteredHosts,
  getServingHost,
  hostDisplayName,
  hostLabel,
  hostWriteRefusal,
  setHost,
  setHostUnavailable,
  setHostWrites,
  setRegisteredHosts,
} from './common.js';
import { resetHostResources } from './host-resources.js';

const $ = (id) => document.getElementById(id);

const REMEMBERED_HOST_KEY = "orbit.dashboard.host";

/// Codes the forward answers for the host itself rather than for one request.
/// Any panel failing with one of these means the host cannot be shown.
const HOST_LEVEL_CODES = new Set([
  "unknown_host",
  "unreachable_destination",
  "process_timeout",
  "host_identity_mismatch",
  "host_too_old",
]);

const FAILURE_HEADLINES = {
  unknown_host: "is not a registered host",
  unreachable_destination: "is unreachable",
  process_timeout: "did not answer in time",
  host_identity_mismatch: "is not the machine its host file names",
  host_too_old: "runs a dashboard too old to show here",
};

let failure = null;
let connection = null;
let onSelect = () => {};

function rememberedHost() {
  try {
    return window.localStorage.getItem(REMEMBERED_HOST_KEY);
  } catch (_) {
    return null;
  }
}

function rememberHost(name) {
  try {
    if (name) window.localStorage.setItem(REMEMBERED_HOST_KEY, name);
    else window.localStorage.removeItem(REMEMBERED_HOST_KEY);
  } catch (_) {
    // A browser without storage still switches; it just will not remember.
  }
}

/// Read the serving host's host file, pick the initial host and build the
/// picker. The URL always wins; a remembered host only fills a bare URL, and
/// is forgotten once it is no longer registered. `select` is called with the
/// chosen name (null for the serving host) when the operator switches.
export async function initHostSwitcher({ select }) {
  onSelect = select;
  try {
    const payload = await fetchJson("/api/hosts?probe=false");
    setRegisteredHosts(payload && payload.hosts);
  } catch (error) {
    console.error("Failed to read registered hosts", error);
    setRegisteredHosts([]);
  }
  const linked = new URLSearchParams(window.location.search).get("host");
  if (linked) {
    setHost(linked);
  } else {
    const remembered = rememberedHost();
    const row = remembered ? findRegisteredHost(remembered) : null;
    if (remembered && (!row || row.local)) rememberHost(null);
    setHost(row && !row.local ? row.name : null);
  }
  buildPicker();
  renderHostChrome();
}

/// Switch to `name` (null or the serving host's name for the serving host),
/// remember it, and say so. Returns false when nothing changed.
export function chooseHost(name) {
  if (!setHost(name)) return false;
  rememberHost(getHost());
  connection = null;
  clearHostFailure();
  resetHostResources();
  buildPicker();
  renderHostChrome();
  announce(`Showing ${hostDisplayName()}`);
  return true;
}

function buildPicker() {
  const container = $("rail-host");
  if (!container) return;
  const select = el("select", { class: "workspace-select host-select", title: "Host" });
  select.id = "host-select";
  const serving = getServingHost();
  const servingOption = el("option", { text: `${serving ? serving.name : "This host"} (serving host)` });
  servingOption.value = "";
  select.appendChild(servingOption);
  for (const row of getRegisteredHosts()) {
    if (row.local) continue;
    const option = el("option", { text: row.name });
    option.value = row.name;
    select.appendChild(option);
  }
  const current = getHost();
  if (current && !findRegisteredHost(current)) {
    const option = el("option", { text: `${current} (not registered)` });
    option.value = current;
    select.appendChild(option);
  }
  select.value = current || "";
  select.addEventListener("change", () => onSelect(select.value || null));
  const label = el("label", { class: "rail-scope-label", text: "Host" });
  label.htmlFor = "host-select";
  container.replaceChildren(label, select);
}

/// Name the selected host on the top-bar chip group and the rail connection
/// line, and keep the picker in step with it.
export function renderHostChrome() {
  const name = hostDisplayName();
  const group = $("host-resource-group");
  if (group) {
    group.setAttribute("aria-label", `Live resources of ${name}`);
    group.classList.toggle("remote", !!getHost());
  }
  const chipName = $("host-resource-name");
  if (chipName) {
    chipName.textContent = name;
    chipName.title = getHost() ? `Readings from ${name}` : `Readings from ${name}, which serves this dashboard`;
  }
  const conn = $("conn-host");
  if (conn) conn.textContent = name;
  const select = $("host-select");
  if (select) select.value = getHost() || "";
  renderBanners();
}

function announce(text) {
  const region = $("host-announcer");
  if (region) region.textContent = text;
}

/// Read the selected host's connection state. True when it can be shown; on
/// the serving host there is nothing to read. An unreachable host is a 200
/// with `reachable: false`; a failed read of the route itself is treated the
/// same, with its own code.
export async function checkHostConnection() {
  const host = getHost();
  if (!host) {
    clearHostFailure();
    return true;
  }
  let state;
  try {
    state = await fetchJson(`/api/hosts/${encodeURIComponent(host)}/connection`);
  } catch (error) {
    state = { reachable: false, error: { code: error.code || "connection_state_unavailable", message: error.message } };
  }
  if (host !== getHost()) return false;
  connection = state;
  setHostWrites(state && state.forward_writes);
  if (!state || state.reachable !== true) {
    const error = (state && state.error) || {};
    showHostFailure({ code: error.code || "unreachable_destination", message: error.message || `${host} is unreachable` });
    return false;
  }
  clearHostFailure();
  return true;
}

/// The host-level failure a panel error stands for, or null when the error
/// belongs to that panel alone.
export function hostLevelFailure(error) {
  if (!getHost() || !error || !HOST_LEVEL_CODES.has(error.code)) return null;
  return { code: error.code, message: error.message || error.code };
}

export function getHostFailure() {
  return failure;
}

/// Replace every panel with one state naming the host, the code and the
/// message. The picker stays in the rail and the state offers a way back.
export function showHostFailure(next) {
  const changed = !failure || failure.code !== next.code || failure.message !== next.message;
  failure = next;
  setHostUnavailable(true);
  const main = document.querySelector(".main-col");
  if (main) main.classList.add("host-unavailable");
  const section = $("host-state");
  if (!section) return;
  section.hidden = false;
  if (!changed && section.childElementCount) return;
  const host = getHost();
  const serving = getServingHost();
  const back = el("button", { class: "host-state-back", text: `Back to ${serving ? serving.name : "the serving host"}` });
  back.type = "button";
  back.addEventListener("click", () => onSelect(null));
  const retry = el("button", { class: "host-state-retry", text: "Retry" });
  retry.type = "button";
  retry.addEventListener("click", () => onSelect(host));
  const title = el("h2", { text: `${host} ${FAILURE_HEADLINES[next.code] || "cannot be shown"}` });
  title.id = "host-state-title";
  section.replaceChildren(
    title,
    el("p", {}, [el("span", { class: "host-state-code", text: next.code }), `: ${next.message}`]),
    el("p", { text: `Panels for ${host} return when it can be reached. Pick another host above, or go back to the host serving this dashboard.` }),
    el("div", { class: "host-state-actions" }, [retry, back]),
  );
  announce(`${host} ${FAILURE_HEADLINES[next.code] || "cannot be shown"}: ${next.code}`);
  renderBanners();
}

function clearHostFailure() {
  if (!getHost()) {
    connection = null;
    setHostWrites(null);
  }
  setHostUnavailable(false);
  const main = document.querySelector(".main-col");
  if (main) main.classList.remove("host-unavailable");
  const section = $("host-state");
  if (section) {
    section.hidden = true;
    section.replaceChildren();
  }
  failure = null;
  renderBanners();
}

function versionText(version, fingerprint) {
  const parts = [version ? `orbit ${version}` : "an unknown version"];
  if (fingerprint) parts.push(`protocol ${fingerprint}`);
  return parts.join(", ");
}

// Skew is reported, never refused, so it is a persistent note above the
// panels. Refused writes say why every write control on this host is off.
function renderBanners() {
  const skew = $("host-skew");
  if (skew) {
    const serving = getRegisteredHosts().find((row) => row.local);
    const show = !!(getHost() && !failure && connection && connection.skew);
    skew.hidden = !show;
    skew.textContent = show
      ? `Version skew: ${hostLabel()} runs ${versionText(connection.binary_version, connection.protocol_fingerprint)}; `
        + `${serving ? serving.name : "the serving host"} runs ${versionText(serving && serving.binary_version, serving && serving.protocol_fingerprint)}. `
        + "Views and actions follow what that host supports."
      : "";
  }
  const readOnly = $("host-read-only");
  if (readOnly) {
    const refusal = failure ? "" : hostWriteRefusal();
    readOnly.hidden = !refusal;
    readOnly.textContent = refusal;
  }
}
