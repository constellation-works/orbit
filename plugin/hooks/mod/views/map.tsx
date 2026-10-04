// The Orbital map: every open task as a satellite. Distance from the core
// is distance from done; the sector is the task's type. Surfaces that draw
// SVG get the picture; every surface gets the rings as rows of buttons.

import type { ElementConstructor, SvgProps } from 'claude-code'

import type { OrbitSnapshot, OrbitTask } from '../../../types'
import type { Actions } from './kit'
import { ACCENT, clip, lane, laneOf, RING, SECTORS, type OpenStatus } from '../model'
import type { Kit } from './kit'

export type MapData = {
  snapshot: OrbitSnapshot
  selected: string | null
  active: string | null
  columns: number
}

/** Ring order from the core outwards. */
const RINGS: readonly OpenStatus[] = ['review', 'in-progress', 'blocked', 'backlog', 'proposed']

const HEX: Record<OpenStatus, string> = {
  proposed: '#8E97AB',
  backlog: '#B7BFCE',
  'in-progress': '#4CC9F0',
  review: '#F2C94C',
  blocked: '#FF7A66',
}

export function mapView(kit: Kit, data: MapData, act: Actions, Svg?: ElementConstructor<SvgProps>) {
  const { Box, Button, Text } = kit
  const { snapshot, columns } = data
  const chosen = data.selected === null ? undefined : snapshot.tasks.find(task => task.id === data.selected)

  return (
    <Box flexDirection="column">
      {Svg ? (
        <Svg source={orbital(snapshot, data.selected, data.active)} alt={`Orbital map of ${snapshot.tasks.length} open tasks in ${snapshot.workspace}`} width={Math.min(560, columns * 8)} isInteractive />
      ) : null}
      <Text wrap="truncate-end">
        <Text color={ACCENT}>◉ core</Text>
        <Text dimColor>
          {' '}
          ✓ {snapshot.doneRecently.length} done in 24h · distance from the core is distance from done
        </Text>
      </Text>
      {RINGS.map(status => {
        const spec = laneOf(status)
        const tasks = lane(snapshot.tasks, status)
        return (
          <Box flexWrap="wrap" columnGap={1}>
            <Box width={14}>
              <Text color={spec?.color}>
                {spec?.glyph} {spec?.name.toLowerCase()}
              </Text>
            </Box>
            {tasks.length === 0 ? <Text dimColor>—</Text> : null}
            {tasks.slice(0, 16).map(task => (
              <Button key={`sat:${task.id}`} label={`${data.active === task.id ? '◉' : ''}${task.id}`} plain dimColor={data.selected !== task.id} onPress={() => act.select(task.id)} />
            ))}
            {tasks.length > 16 ? <Text dimColor>+{tasks.length - 16}</Text> : null}
          </Box>
        )
      })}
      {links(snapshot).length > 0 ? (
        <Text dimColor wrap="truncate-end">
          links: {links(snapshot).map(([from, to]) => `${from} → ${to}`).join('  ')}
        </Text>
      ) : null}
      {chosen ? (
        <Box flexDirection="column" marginTop={1} borderStyle="round" borderDimColor paddingX={1}>
          <Text wrap="truncate-end">
            <Text bold color={ACCENT}>
              {chosen.id}
            </Text>
            <Text dimColor>
              {' '}
              {chosen.status} · {chosen.type} · {chosen.priority}
            </Text>
          </Text>
          <Text>{clip(chosen.title, Math.max(20, columns * 2))}</Text>
          <Text dimColor>Next: {nextStep(chosen, snapshot)}</Text>
          <Box columnGap={1}>
            <Button
              key="map-open"
              label="Open on board"
              hotkey="o"
              onPress={() => {
                act.select(chosen.id)
                act.setView('board')
              }}
            />
            {chosen.status === 'backlog' ? <Button key="map-ship" label="Ship…" hotkey="s" variant="primary" onPress={() => void act.ship(chosen.id)} /> : null}
          </Box>
        </Box>
      ) : (
        <Text dimColor>Select a satellite for its next step.</Text>
      )}
    </Box>
  )
}

/** Dependency pairs between open tasks: [dependency, dependent]. */
function links(snapshot: OrbitSnapshot): [string, string][] {
  const open = new Set(snapshot.tasks.map(task => task.id))
  return snapshot.tasks.flatMap(task => task.deps.filter(dep => open.has(dep)).map(dep => [dep, task.id] as [string, string]))
}

function nextStep(task: OrbitTask, snapshot: OrbitSnapshot): string {
  const open = new Set(snapshot.tasks.map(item => item.id))
  const waiting = task.deps.filter(dep => open.has(dep))
  if (waiting.length > 0) return `waits for ${waiting.join(', ')}`
  if (task.status === 'proposed') return 'approve it to the backlog, or reject it'
  if (task.status === 'backlog') return 'ready to ship'
  if (task.status === 'in-progress') return task.runId ? `running in ${task.runId}` : 'being worked'
  if (task.status === 'review') return 'review and accept it'
  if (task.status === 'blocked') return 'rescue it from its last run'
  return task.status
}

const escape = (value: string): string => value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')

/** The orbital picture: rings by status, sectors by type, lines for dependencies. */
export function orbital(snapshot: OrbitSnapshot, selected: string | null, active: string | null): string {
  const size = 560
  const center = size / 2
  const reach = 230
  const span = 360 / SECTORS.length
  const at = (radius: number, degrees: number) => {
    const rad = ((degrees - 90) * Math.PI) / 180
    return { x: center + radius * Math.cos(rad), y: center + radius * Math.sin(rad) }
  }
  const sectorOf = (type: string) => Math.max(0, SECTORS.indexOf(type as (typeof SECTORS)[number]))

  const place = new Map<string, { x: number; y: number }>()
  for (const status of RINGS) {
    for (let sector = 0; sector < SECTORS.length; sector += 1) {
      const group = lane(snapshot.tasks, status).filter(task => sectorOf(task.type) === sector)
      group.forEach((task, index) => {
        const degrees = sector * span + (span * (index + 1)) / (group.length + 1)
        place.set(task.id, at(RING[status] * reach, degrees))
      })
    }
  }

  const rings = RINGS.filter(status => status !== 'blocked')
    .map(status => `<circle cx="${center}" cy="${center}" r="${(RING[status] * reach).toFixed(1)}" fill="none" stroke="${HEX[status]}" stroke-opacity="0.35" ${status === 'proposed' ? 'stroke-dasharray="2 6"' : ''}/>`)
    .join('')
  const dividers = SECTORS.map((_, sector) => {
    const from = at(46, sector * span)
    const to = at(reach + 12, sector * span)
    return `<line x1="${from.x.toFixed(1)}" y1="${from.y.toFixed(1)}" x2="${to.x.toFixed(1)}" y2="${to.y.toFixed(1)}" stroke="#6E788C" stroke-opacity="0.35" stroke-dasharray="3 5"/>`
  }).join('')
  const names = SECTORS.map((name, sector) => {
    const p = at(reach + 26, sector * span + span / 2)
    return `<text x="${p.x.toFixed(1)}" y="${p.y.toFixed(1)}" text-anchor="middle" font-family="ui-monospace, monospace" font-size="12" fill="#8A93A6">${name}</text>`
  }).join('')
  const lines = links(snapshot)
    .map(([from, to]) => {
      const a = place.get(from)
      const b = place.get(to)
      return a && b ? `<line x1="${a.x.toFixed(1)}" y1="${a.y.toFixed(1)}" x2="${b.x.toFixed(1)}" y2="${b.y.toFixed(1)}" stroke="#8A93A6" stroke-opacity="0.6"/>` : ''
    })
    .join('')
  const radius: Record<string, number> = { critical: 10, high: 8, medium: 6, low: 5 }
  const satellites = snapshot.tasks
    .map(task => {
      const p = place.get(task.id)
      if (!p || !(task.status in HEX)) return ''
      const status = task.status as OpenStatus
      const r = radius[task.priority] ?? 6
      const isSelected = task.id === selected
      const halo = status === 'blocked' ? `<circle cx="${p.x.toFixed(1)}" cy="${p.y.toFixed(1)}" r="${r + 7}" fill="none" stroke="${HEX.blocked}" stroke-dasharray="3 3"/>` : ''
      const right = p.x >= center - 4
      return `${halo}<circle cx="${p.x.toFixed(1)}" cy="${p.y.toFixed(1)}" r="${isSelected ? r + 2 : r}" fill="${task.id === active ? ACCENT : HEX[status]}" stroke="${isSelected ? '#E7EAF0' : 'none'}" stroke-width="2"><title>${escape(`${task.id} · ${task.status} · ${task.title}`)}</title></circle><text x="${(p.x + (right ? r + 5 : -(r + 5))).toFixed(1)}" y="${(p.y + 4).toFixed(1)}" text-anchor="${right ? 'start' : 'end'}" font-family="ui-monospace, monospace" font-size="10" fill="${isSelected ? '#E7EAF0' : '#8A93A6'}">${escape(task.id)}</text>`
    })
    .join('')
  const core = `<circle cx="${center}" cy="${center}" r="38" fill="none" stroke="${ACCENT}" stroke-width="1.5"/><text x="${center}" y="${center - 4}" text-anchor="middle" font-family="ui-monospace, monospace" font-size="10" letter-spacing="2" fill="${ACCENT}">DONE</text><text x="${center}" y="${center + 16}" text-anchor="middle" font-family="ui-monospace, monospace" font-size="18" font-weight="600" fill="${ACCENT}">${snapshot.doneRecently.length}</text>`

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" width="${size}" height="${size}">${rings}${dividers}${names}${lines}${core}${satellites}</svg>`
}

