import { el, getHost, getServingHost } from './common.js';
import { FLEET_CACHE_MS, fetchFleetDrains, peekFleetDrains, replaceFleetDrains } from './fleet-drains.js';
import { $, operationsState, selectedWorkspace } from './operations-shared.js';

export function loadFleetSnapshot({ refreshWhenStale, onPartial = null }) {
  const seen = peekFleetDrains();
  if (seen && (!refreshWhenStale || seen.fresh)) return Promise.resolve(seen.payload);
  return fetchFleetDrains({ onPartial });
}

export function paintAggregateFleet(payload) {
  const body = $("fleet-drains-body");
  if (body) replaceFleetDrains(body, payload, operationsState.context.selectFleetScope);
}

const LIVE_DRAIN_PHASES = ["draining", "pulling", "winding_down"];

function railDrainWords({ phase, used, ceiling, held, liveCount }) {
  if (liveCount) return { text: `${liveCount} live`, spoken: `${liveCount} ${liveCount === 1 ? "workspace" : "workspaces"} live` };
  const slots = used != null && ceiling > 0 ? { text: `${used}/${ceiling}`, spoken: `${used} of ${ceiling} slots in use` } : { text: "live", spoken: "" };
  const words = phase === "pulling" ? { text: "pull", spoken: "pull drain" }
    : phase === "winding_down" ? { text: "winding down", spoken: "winding down" }
    : { text: slots.text, spoken: ["draining", slots.spoken].filter(Boolean).join(", ") };
  return held ? { text: `${words.text} · held`, spoken: `${words.spoken}, new tasks held` } : words;
}

// The rail's Drain item says whether a drain runs, in dot and words, from any
// view: the selected workspace's, or how many are live under All workspaces.
export function paintRailDrainReading({ phase = "idle", used = null, ceiling = null, held = false, liveCount = 0 }) {
  const state = $("rail-drain-state");
  const link = $("rail-drain");
  if (!state || !link) return;
  const live = Boolean(liveCount) || LIVE_DRAIN_PHASES.includes(phase);
  state.hidden = !live;
  state.classList.toggle("held", live && held);
  if (!live) {
    state.replaceChildren();
    delete state.dataset.drainState;
    link.setAttribute("aria-label", "Drain");
    link.removeAttribute("title");
    return;
  }
  const { text, spoken } = railDrainWords({ phase, used, ceiling, held, liveCount });
  state.dataset.drainState = liveCount ? "draining" : phase;
  state.replaceChildren(el("span", { class: "drain-dot", "aria-hidden": "true" }), text);
  link.setAttribute("aria-label", `Drain: ${spoken}`);
  link.title = `Drain: ${spoken}`;
}

function railDrainReadingFromFleet(payload) {
  const rows = (payload?.items || []).filter(row => row.workspace_id && !row.error && !row.pending);
  const workspace = selectedWorkspace();
  if (!workspace) return { liveCount: rows.filter(row => LIVE_DRAIN_PHASES.includes(row.phase)).length };
  const host = getHost() || getServingHost()?.name;
  const row = rows.find(row => row.workspace_id === workspace.id && (row.host_selector == null || row.host_selector === host));
  return row ? { phase: row.phase, used: row.slots_used, ceiling: row.slots_ceiling, held: Boolean(row.holds?.length) } : null;
}

// Repaint from the last fleet read when the selection moves, without asking the hosts.
export function repaintRailDrainFromFleet() {
  paintRailDrainReading(railDrainReadingFromFleet(peekFleetDrains()?.payload) || {});
}

function paintFleetSurfaces(payload) {
  const reading = railDrainReadingFromFleet(payload);
  if (reading) paintRailDrainReading(reading);
  paintAggregateFleet(payload);
}

let fleetRefreshStarted = false;

// One read after hosts are known, then again on the fleet cadence. This is
// not a panel-poll job: a view's 30s refresh must not fan out to every host.

export function startFleetIndicatorRefresh() {
  const refresh = () => {
    return fetchFleetDrains({ force: true, onPartial: payload => paintFleetSurfaces(payload) }).then(payload => {
      const seen = peekFleetDrains();
      if (seen && seen.payload === payload) paintFleetSurfaces(payload);
    });
  };
  const arm = () => {
    const timer = setTimeout(() => {
      if (document.hidden) {
        arm();
        return;
      }
      refresh().catch(error => console.error(error)).finally(arm);
    }, FLEET_CACHE_MS);
    if (typeof timer === "object" && typeof timer.unref === "function") timer.unref();
  };
  if (!fleetRefreshStarted) {
    fleetRefreshStarted = true;
    document.addEventListener("visibilitychange", () => {
      if (document.hidden) return;
      const seen = peekFleetDrains();
      if (seen?.fresh) return;
      refresh().catch(error => console.error(error));
    });
    arm();
  }
  const seen = peekFleetDrains();
  if (seen?.fresh) {
    paintFleetSurfaces(seen.payload);
    return Promise.resolve(seen.payload);
  }
  return fetchFleetDrains({ onPartial: payload => paintFleetSurfaces(payload) }).then(payload => {
    paintFleetSurfaces(payload);
    return payload;
  });
}
