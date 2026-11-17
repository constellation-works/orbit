// Observe › Fleet presentation. Registry mutation and probe state stay in hosts.js.
import { el, formatDateTime, pinnedInfoHint } from './common.js';
import { describeResource, hostVerdict } from './host-resources.js';
import { describeProviderReading } from './provider-usage.js';

export function fleetButton(label, help, click, cls = '') {
  const button = el('button', { type: 'button', class: `settings-btn ${cls}`.trim(), text: label, 'aria-label': label || help, 'aria-description': help });
  button.addEventListener('click', click);
  return pinnedInfoHint(help, { trigger: button, preserveAction: true });
}

export function fleetFacts(rows) {
  return el('dl', { class: 'fleet-facts' }, rows.map(([label, value]) => el('div', {}, [
    el('dt', { text: label }), el('dd', {}, [value ?? 'Unknown']),
  ])));
}

const expandedLists = new Set();
function section(title, content, help = null) {
  let more = null;
  if (content.children.length > 7) {
    const rest = content.cloneNode(false);
    while (content.children.length > 6) rest.append(content.children[6]);
    more = el('details', { class: 'fleet-more' }, [el('summary', { text: `Show ${rest.children.length} more` }), rest]);
    more.open = expandedLists.has(title);
    more.addEventListener('toggle', () => { if (more.open) expandedLists.add(title); else expandedLists.delete(title); });
  }
  return el('section', { class: 'fleet-section' }, [
    el('header', { class: 'fleet-section-head' }, [el('h2', { text: title }), help ? pinnedInfoHint(help) : null]), content, more,
  ]);
}

function resourceMeter(payload, resource, os) {
  const reading = describeResource(payload, resource);
  const label = resource === 'cpu' ? (!payload || reading.utilization ? 'CPU' : 'Load') : { memory: 'Mem', disk: 'Disk', io: 'I/O' }[resource];
  const unknown = reading.value === '-';
  if (resource === 'io' && unknown && os === 'macos') reading.detail = 'I/O pressure unavailable on macOS; Linux PSI full avg60 is not reported.';
  return el('div', { class: `fleet-metric ${reading.severity}`, title: reading.detail, 'aria-label': `${label}: ${unknown ? reading.detail : reading.value + reading.suffix}` }, [
    el('span', { text: label }),
    el('span', { class: `fleet-track${unknown ? ' unknown' : ''}`, 'aria-hidden': 'true' }, [el('span', { style: { width: `${reading.fill}%` } })]),
    el('span', { class: 'mono', text: unknown ? '—' : reading.value }),
  ]);
}

export function fleetHostCards(hosts, health, openHost) {
  const grid = el('div', { class: 'fleet-host-grid' });
  // Divisors prevent an orphan card when the inventory has an odd size.
  grid.dataset.columns = hosts.length % 3 === 0 ? '3' : hosts.length % 2 === 0 ? '2' : '1';
  for (const row of hosts) {
    const reading = health.get(row.machine_id);
    const payload = row.reachable && !row.checking ? reading?.resources : null;
    const verdict = hostVerdict(payload);
    const state = row.checking ? 'Checking' : row.reachable === false ? 'Unreachable' : row.reachable ? (row.error ? 'Answered' : reading?.resourcesError ? 'Last reached' : 'Online') : 'Unprobed';
    const card = fleetButton('', `Inspect ${row.name}, diagnostics and registry actions`, () => openHost(row.machine_id), 'fleet-host-card');
    card.dataset.key = row.machine_id;
    card.setAttribute('aria-haspopup', 'dialog');
    card.append(
      el('div', { class: 'fleet-card-head' }, [
        el('h3', {}, [row.name, row.local ? el('span', { class: 'host-local', text: ' [local]' }) : null]),
        el('span', { class: `fleet-state ${row.reachable ? 'ok' : 'unknown'}`, text: state }),
      ]),
      el('div', { class: 'fleet-metrics' }, ['cpu', 'memory', 'disk', 'io'].map(resource => resourceMeter(payload, resource, row.host_os))),
      fleetFacts([['Machine ID', row.machine_id], ['SSH', row.ssh || (row.local ? 'Local' : 'Unknown')], ['Prefix', row.task_prefix || 'Unknown'], ['Orbit', row.binary_version || 'Unknown']]),
      el('span', { class: `fleet-card-health${verdict.status === 'held' ? ' warn' : ''}`, text: row.checking ? 'Probe in progress' : row.reachable === false ? 'Probe failed · inspect details' : !payload ? reading?.resourcesError || 'Telemetry unavailable' : `${payload.stale ? 'Stale' : 'Sampled'} ${verdict.age} · admission ${verdict.status}` }),
    );
    grid.append(card);
  }
  return section('Hosts', grid, `${hosts.length} registered. Select a host for protocol, workspace, health and registry details. I/O is Linux PSI full avg60; unavailable readings are never zero.`);
}

export function fleetDrainSection(hosts, health, selectScope) {
  const list = el('div', { class: 'fleet-work-list' });
  for (const host of hosts) {
    const reading = health.get(host.machine_id);
    if (!reading || reading.kind !== 'ready') continue;
    for (const row of reading.drains || []) {
      const drains = [
        ...(row.phase && row.phase !== 'idle' ? [{ phase: row.phase, mode: 'Owner drain', stopped: row.ownerStopped, run: row.runId, work: row.work }] : []),
        ...(row.pull ? [{ phase: row.pullStopped ? 'Admissions stopped' : 'pulling', mode: 'Pull drain', stopped: row.pullStopped, run: row.pullRunId, work: row.pullWork }] : []),
      ];
      for (const drain of drains) {
        const button = fleetButton('', `Open ${row.name} on ${host.name} for work, drain controls and admission details`, () => selectScope(host.local ? null : host.name, row.id), 'fleet-work-row');
        button.append(
          el('span', { class: 'fleet-primary' }, [el('span', { text: row.name }), el('span', { class: 'muted', text: `${drain.mode}${drain.run ? ` · ${drain.run}` : ''}` })]),
          el('span', { text: host.name }),
          el('span', { class: `fleet-state ${drain.stopped || row.holds?.length ? 'held' : drain.phase}`, text: drain.phase.replaceAll('_', ' ') }),
        );
        const detail = [drain.error, drain.stopped ? 'admissions stopped' : null, ...(row.holds || []).filter(hold => hold !== 'admissions stopped'), Number.isFinite(drain.work) ? `${drain.work} working` : null, row.topWait ? `Waiting: ${row.topWait.replaceAll('_', ' ')}` : null].filter(Boolean);
        if (detail.length) button.append(el('span', { class: 'fleet-work-detail', text: detail.join(' · ') }));
        list.append(button);
      }
    }
  }
  if (!list.childElementCount) list.append(el('p', { class: 'muted', text: 'No live drains.' }));
  return section('Drains', list);
}

// Account labels are host-local crew names, not globally unique identities.
// Only shares_default_login proves that two crew accounts share a login.
// Never sum percentages or deduplicate identical names across machines.
export function fleetProviderSection(hosts, health) {
  const list = el('div', { class: 'fleet-account-list' });
  for (const host of hosts) {
    const payload = health.get(host.machine_id)?.resources;
    const pressures = payload?.provider_pressures || [];
    const sharing = new Set(pressures.filter(p => p.shares_default_login).map(p => `${p.provider}/${p.account}`));
    const groups = new Map();
    const groupFor = item => {
      const shared = sharing.has(`${item.provider}/${item.account}`);
      const account = shared ? null : item.account;
      const key = JSON.stringify([item.provider, account || null]);
      if (!groups.has(key)) groups.set(key, { provider: item.provider, account, shared, readings: new Map(), gated: false });
      const group = groups.get(key);
      group.shared ||= shared;
      group.gated ||= item.gated === true;
      return group;
    };
    for (const pressure of pressures) groupFor(pressure);
    for (const reading of payload?.provider_limits || []) {
      const group = groupFor(reading);
      const key = JSON.stringify([reading.scope, reading.window]);
      const previous = group.readings.get(key);
      if (!previous || Date.parse(reading.observed_at) > Date.parse(previous.observed_at)) group.readings.set(key, reading);
    }
    if (!groups.size) {
      list.append(el('div', { class: 'fleet-account fleet-account-unavailable' }, [el('span', { text: host.name }), el('span', { class: 'muted', text: 'Provider usage and account scope unknown' })]));
    }
    for (const group of groups.values()) {
      const readings = [...group.readings.values()].map(reading => describeProviderReading(reading));
      const unknown = !readings.length || readings.some(r => r.used == null || r.stale || r.partial);
      const state = group.gated ? 'Held' : unknown ? 'Unknown' : readings.some(r => r.severity === 'critical') ? 'At limit' : readings.some(r => r.severity === 'elevated') ? 'Near limit' : 'Available';
      const windows = el('div', { class: 'fleet-account-windows' });
      for (const reading of readings) {
        windows.append(el('div', { class: 'fleet-quota' }, [
          el('span', {}, [reading.label, el('span', { class: 'mono', text: `${reading.value}${reading.stale ? ' · stale' : ''}${reading.partial ? ' · partial' : ''}` })]),
          el('span', { class: `fleet-track ${reading.severity}${reading.used == null ? ' unknown' : ''}` }, [el('span', { style: { width: `${reading.fill}%` } })]),
          el('span', { class: 'muted', text: reading.resetsAt ? `Resets ${formatDateTime(reading.resetsAt)}` : 'Reset unknown' }),
        ]));
      }
      if (!readings.length) windows.append(el('span', { class: 'muted', text: 'No usage windows reported' }));
      list.append(el('div', { class: 'fleet-account' }, [
        el('span', { class: 'fleet-primary' }, [el('span', { text: group.provider }), el('span', { class: 'muted', text: `${host.name} · ${group.account || (group.shared ? 'shared login' : 'default login')}` })]),
        windows, el('span', { class: `fleet-state ${state === 'Available' ? 'ok' : state === 'Held' || state === 'Near limit' || state === 'At limit' ? 'held' : 'unknown'}`, text: state }),
      ]));
    }
  }
  return section('Provider capacity', list, 'Usage windows are per account, not per machine capacity. Explicitly shared logins count once within a host. Cross-host account identity is unknown; matching crew names do not prove a shared account. Availability reflects reported windows only.');
}
