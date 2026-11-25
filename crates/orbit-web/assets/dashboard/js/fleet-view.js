// Observe › Fleet presentation. Registry mutation and probe state stay in hosts.js.
import { el, formatDateTime, pinnedInfoHint, relativeTime } from './common.js';
import { describeResource, hostVerdict } from './host-resources.js';
import { describeProviderReading } from './provider-usage.js';

export function fleetButton(label, help, click, cls = '') {
  const attrs = { type: 'button', class: `settings-btn ${cls}`.trim(), text: label };
  const ariaLabel = label || help;
  if (ariaLabel) attrs['aria-label'] = ariaLabel;
  if (help && help !== label && help !== ariaLabel) {
    attrs['aria-description'] = help;
  }
  const button = el('button', attrs);
  if (click) button.addEventListener('click', click);
  return button;
}

export function fleetFacts(rows) {
  return el('dl', { class: 'fleet-facts' }, rows.map(([label, value]) => el('div', {}, [
    el('dt', { text: label }), el('dd', {}, [value ?? 'Unknown']),
  ])));
}

/// A host's health reading is still pending until hosts.js lands it: there is
/// no entry for it yet, or its probe is in flight (`checking`). hosts.js lands
/// rows that need no resource read before the fan-out, so only a host whose
/// read is outstanding is pending. Only a settled reading may state that a host
/// has no drain or no telemetry.
const readingPending = reading => !reading || reading.kind === 'checking';

const expandedLists = new Set();
function section(title, content, help = null, summary = null) {
  let more = null;
  if (content.children.length > 7) {
    const rest = content.cloneNode(false);
    while (content.children.length > 6) rest.append(content.children[6]);
    more = el('details', { class: 'fleet-more' }, [el('summary', { text: `Show ${rest.children.length} more` }), rest]);
    more.open = expandedLists.has(title);
    more.addEventListener('toggle', () => { if (more.open) expandedLists.add(title); else expandedLists.delete(title); });
  }
  return el('section', { class: 'fleet-section' }, [
    el('header', { class: 'fleet-section-head' }, [el('h2', { text: title }), help ? pinnedInfoHint(help) : null, summary ? el('span', { class: 'fleet-section-summary muted', text: summary }) : null]), content, more,
  ]);
}

function resourceMeter(payload, resource, os, loading) {
  const reading = describeResource(payload, resource);
  const label = resource === 'cpu' ? (!payload || reading.utilization ? 'CPU' : 'Load') : { memory: 'Mem', disk: 'Disk', io: 'I/O' }[resource];
  const unknown = reading.value === '-';
  if (loading) reading.detail = 'Reading telemetry…';
  if (resource === 'io' && unknown && os === 'macos') reading.detail = 'I/O pressure unavailable on macOS; Linux PSI full avg60 is not reported.';
  return el('div', { class: `fleet-metric ${reading.severity}`, 'aria-label': `${label}: ${unknown ? reading.detail : reading.value + reading.suffix}` }, [
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
    const loading = readingPending(reading);
    const verdict = hostVerdict(payload);
    const state = row.checking ? 'Checking' : row.reachable === false ? 'Unreachable' : row.reachable ? (row.error ? 'Answered' : reading?.resourcesError ? 'Last reached' : 'Online') : 'Unprobed';
    const card = el('button', {
      type: 'button', class: 'card fleet-host-card',
      'aria-label': `Inspect ${row.name}, diagnostics and registry actions`,
    });
    card.addEventListener('click', () => openHost(row.machine_id));
    card.dataset.key = row.machine_id;
    card.setAttribute('aria-haspopup', 'dialog');
    card.append(
      el('div', { class: 'fleet-card-head' }, [
        el('h3', {}, [row.name, row.local ? el('span', { class: 'host-local', text: ' [local]' }) : null]),
        el('span', { class: `fleet-state ${row.reachable ? 'ok' : 'unknown'}`, text: state }),
      ]),
      el('div', { class: 'fleet-metrics' }, ['cpu', 'memory', 'disk', 'io'].map(resource => resourceMeter(payload, resource, row.host_os, loading))),
      fleetFacts([['Machine ID', row.machine_id], ['SSH', row.ssh || (row.local ? 'Local' : 'Unknown')], ['Prefix', row.task_prefix || 'Unknown'], ['Orbit', row.binary_version || 'Unknown']]),
      el('span', { class: `fleet-card-health${verdict.status === 'held' ? ' warn' : ''}`, text: row.checking ? 'Probe in progress' : row.reachable === false ? 'Probe failed · inspect details' : loading ? 'Reading telemetry…' : !payload ? reading?.resourcesError || 'Telemetry unavailable' : `${payload.stale ? 'Stale' : 'Sampled'} ${verdict.age} · admission ${verdict.status}` }),
    );
    grid.append(card);
  }
  return section('Hosts', grid, `${hosts.length} registered. Select a host for protocol, workspace, health and registry details. I/O is Linux PSI full avg60; unavailable readings are never zero.`);
}

export function fleetDrainSection(hosts, health, selectScope) {
  const list = el('div', { class: 'card card-flush fleet-work-list' });
  let pending = false;
  for (const host of hosts) {
    const reading = health.get(host.machine_id);
    if (readingPending(reading)) { pending = true; continue; }
    if (reading.kind !== 'ready') continue;
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
  if (!list.childElementCount) list.append(el('p', { class: 'muted', text: pending ? 'Checking drains…' : 'No live drains.' }));
  return section('Drains', list);
}

// Account labels are host-local crew names, not globally unique identities.
// Only shares_default_login proves that two crew accounts share a login.
// Never sum percentages or deduplicate identical names across machines.
export function fleetProviderSection(hosts, health) {
  const list = el('div', { class: 'card card-flush fleet-account-list' });
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
        const flags = [reading.stale ? 'stale' : null, reading.partial ? 'partial' : null].filter(Boolean);
        const reset = reading.resetsAt ? `Resets ${formatDateTime(reading.resetsAt)}` : 'Reset unknown';
        windows.append(el('div', { class: 'fleet-quota' }, [
          el('span', {}, [reading.label, el('span', { class: 'mono', text: reading.value })]),
          el('span', { class: `fleet-track ${reading.severity}${reading.used == null ? ' unknown' : ''}` }, [el('span', { style: { width: `${reading.fill}%` } })]),
          el('span', { class: 'muted', text: [reset, ...flags].join(' · ') }),
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

const SETTINGS_LINK = '#config/machine?key=machine.web.trusted_hosts';
const REFUSAL_LABELS = {
  no_identity: 'No Tailscale login',
  invalid_identity: 'Unreadable login',
  unlisted_login: 'Login not listed',
  origin_mismatch: 'Origin mismatch',
  sec_fetch_site: 'Cross-site request',
};

function remoteWarning(warning) {
  const url = `https://${warning.host}`;
  if (warning.code === 'serve_host_not_trusted') return `Serve sends ${url} here, but it is not a trusted host: every request is refused.`;
  if (warning.code === 'trusted_host_not_served') return `No Serve handler sends ${url} to this dashboard.`;
  if (warning.code === 'funnel_enabled') return `Funnel is on for ${url}. Funnel requests carry no login and are refused.`;
  return `${warning.code}: ${warning.host}`;
}

function serveFact(tailscale, port) {
  if (tailscale?.state === 'missing') return 'tailscale not on PATH';
  if (tailscale?.state !== 'ok') return tailscale?.error || 'Unknown';
  const here = (tailscale.handlers || []).filter(handler => handler.to_dashboard);
  if (!here.length) return port ? `No handler for :${port}` : 'No handler for this dashboard';
  return el('div', {}, here.map(handler => el('div', { text: `${handler.url} → :${port}${handler.funnel ? ' · Funnel on' : ''}` })));
}

/// Observe › Fleet › Remote access for the selected host: the trusted-host
/// policy, the Tailscale Serve check and what came through since start.
/// `reading` is `{payload}` or `{error}`; null while the first read is out.
export function fleetRemoteAccessSections(reading, hostName) {
  const help = `Requests to ${hostName} through Tailscale Serve. Trusted hosts and logins are read when orbit web serve starts; connections and refusals are kept in memory and reset on restart. Loopback requests are not counted.`;
  const card = el('div', { class: 'card fleet-remote-card' });
  if (!reading) {
    card.append(el('p', { class: 'muted', text: 'Checking remote access…' }));
    return [section('Remote access', card, help)];
  }
  if (reading.error) {
    card.append(el('p', { class: 'muted', text: reading.error }));
    return [section('Remote access', card, help)];
  }
  const body = reading.payload;
  const warnings = (body.warnings || []).map(remoteWarning);
  if (body.restart_required) warnings.push('Config changed since start. Restart orbit web serve to apply it.');
  if (body.config_error) warnings.push(`Config unreadable: ${body.config_error}`);
  const status = (body.warnings || []).length ? 'Misconfigured' : body.enabled ? 'On' : 'Off';
  const settings = el('a', { text: 'Settings › Machine' });
  settings.href = SETTINGS_LINK;
  const facts = [['Status', el('span', { class: `fleet-state ${status === 'On' ? 'ok' : status === 'Off' ? 'unknown' : 'held'}`, text: status })]];
  if (body.enabled) {
    facts.push(['Trusted', el('div', {}, (body.trusted_hosts || []).map(host => {
      const link = el('a', { text: host.url, target: '_blank', rel: 'noopener noreferrer' });
      link.href = host.url;
      return el('div', {}, [link]);
    }))]);
    facts.push(['Logins', (body.trusted_logins || []).join(', ')]);
  } else {
    facts.push(['Set up', el('span', {}, ['Trusted hosts and logins in ', settings])]);
  }
  facts.push(['Serve', serveFact(body.tailscale, body.dashboard_port)]);
  card.append(fleetFacts(facts));
  for (const text of warnings) card.append(el('p', { class: 'fleet-remote-warning', text }));
  if (body.enabled) {
    const edit = el('a', { text: 'Edit in Settings › Machine' });
    edit.href = SETTINGS_LINK;
    card.append(el('p', { class: 'fleet-remote-edit' }, [edit]));
  }
  const nodes = [section('Remote access', card, help)];
  const connections = body.connections || {};
  const devices = connections.devices || [];
  const refusals = body.refusals?.items || [];
  if (!body.enabled && !devices.length && !refusals.length) return nodes;

  const since = connections.tracking_since ? `since ${formatDateTime(connections.tracking_since)}` : 'since start';
  const list = el('div', { class: 'card card-flush fleet-remote-list' });
  for (const device of devices) {
    list.append(el('div', { class: 'fleet-remote-row' }, [
      el('span', { class: 'fleet-primary' }, [el('span', { text: device.device || device.ip || 'Unknown device' }), el('span', { class: 'muted mono', text: device.device ? device.ip : '' })]),
      el('span', { text: device.login }),
      el('span', { class: 'mono', text: `${device.open_streams} live · ${device.requests_15m} req` }),
      el('span', { class: 'muted', text: relativeTime(device.last_seen) || formatDateTime(device.last_seen) }),
    ]));
  }
  if (!devices.length) list.append(el('p', { class: 'muted', text: `No connections ${since}.` }));
  nodes.push(section('Remote connections', list,
    `Devices by the tailnet IP Serve forwards for, named by tailscale whois. "live" counts open event streams; "req" counts requests in the last ${connections.window_minutes ?? 15} min. In memory ${since}.`,
    `${connections.open_streams ?? 0} live · ${connections.active_devices ?? 0} active`));

  const refused = el('div', { class: 'card card-flush fleet-remote-list' });
  for (const item of refusals) {
    refused.append(el('div', { class: 'fleet-remote-row fleet-remote-refusal' }, [
      el('span', { class: 'fleet-primary' }, [el('span', { text: REFUSAL_LABELS[item.reason] || item.reason }), el('span', { class: 'muted', text: item.login || '' })]),
      el('span', { class: 'mono', text: `${item.count_24h}` }),
      el('span', { class: 'muted', text: relativeTime(item.last_at) || formatDateTime(item.last_at) }),
    ]));
  }
  if (!refusals.length) refused.append(el('p', { class: 'muted', text: `No refusals in ${body.refusals?.window_hours ?? 24} h.` }));
  nodes.push(section('Remote refusals', refused,
    'Requests to a trusted host that were refused, by reason, over the last 24 h. Only an unlisted login keeps the login attempted.',
    `${body.refusals?.total ?? 0} in ${body.refusals?.window_hours ?? 24} h`));
  return nodes;
}
