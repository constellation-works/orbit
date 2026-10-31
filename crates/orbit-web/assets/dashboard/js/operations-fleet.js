import { FLEET_CACHE_MS, fetchFleetDrains, fleetDrainLabel, peekFleetDrains, replaceFleetDrains } from './fleet-drains.js';
import { setDrainReadiness } from './drain-waits.js';
import { $, operationsState } from './operations-shared.js';

let fleetOperationsCallbacks = null;

export function configureOperationsFleetCallbacks(callbacks) {
  fleetOperationsCallbacks = callbacks;
}

export function loadFleetSnapshot({ refreshWhenStale, onPartial = null }) {
  const seen = peekFleetDrains();
  if (seen && (!refreshWhenStale || seen.fresh)) return Promise.resolve(seen.payload);
  return fetchFleetDrains({ onPartial });
}

export function paintAggregateFleet(payload) {
  setDrainReadiness({});
  fleetOperationsCallbacks.updateDrainIndicators('fleet', 'Fleet drains');
  const card = $("auto-drain-panel");
  if (card) {
    card.dataset.drainState = 'fleet';
    const title = card.querySelector('.drain-title');
    if (title) title.textContent = 'Fleet drains';
  }
  const pill = $("auto-drain-phase");
  if (pill) pill.dataset.drainState = 'fleet';
  const dot = $("auto-drain-dot");
  if (dot) dot.className = 'drain-dot';
  const body = $("auto-drain-body");
  if (body) replaceFleetDrains(body, payload, operationsState.context.selectFleetScope);
  fleetOperationsCallbacks.clearStartCard("auto-drain-quick-start");
  const label = $("auto-drain-phase-label");
  if (label) label.textContent = "Fleet";
  const when = $("auto-drain-when");
  if (when) when.textContent = "";
  renderFleetIndicator(payload);
}

function paintFleetSurfaces(payload) {
  renderFleetIndicator(payload);
  const card = $("auto-drain-panel");
  if (card?.dataset.drainState !== "fleet") return;
  const body = $("auto-drain-body");
  if (body) replaceFleetDrains(body, payload, operationsState.context.selectFleetScope);
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

export function renderFleetIndicator(payload) {
  const global = $("global-drain-state");
  if (!global) return;
  global.hidden = false;
  const label = fleetDrainLabel(payload);
  const active = payload.items.some(row => ['draining', 'pulling'].includes(row.phase));
  global.dataset.drainState = active ? 'draining' : payload.items.some(row => row.phase === 'winding_down') ? 'winding_down' : 'idle';
  global.setAttribute('aria-label', `${label}. Open fleet drains`);
  global.querySelector('.global-drain-label').textContent = label;
  global.querySelector('.global-drain-running').textContent = '';
  // Open the fleet through the same scope switch used by its rows.
  if (!global.dataset.fleetWired) {
    global.addEventListener('click', () => operationsState.context.selectFleetScope(null, null));
    global.dataset.fleetWired = 'true';
  }
}
