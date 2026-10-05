// The Board view: one collapsible section per status, most urgent first, each
// task a full-width row whose details open beneath it when pressed.

import type { OrbitSnapshot, OrbitTask } from '../../../types'
import type { Actions } from './kit'
import { ACCENT, ago, DONE_COLOR, lane, LANES, laneOf, type OpenStatus } from '../model'
import type { Kit } from './kit'

export type BoardData = {
  snapshot: OrbitSnapshot
  selected: string | null
  active: string | null
  /** Sections the person opened; null keeps the defaults (every one but Done). */
  openLanes: string[] | null
  now: number
}

type Section = { id: string; name: string; glyph: string; color: string; tasks: OrbitTask[] }

/** Attention first: what is stuck, then what is moving, then what waits. */
const ORDER: readonly OpenStatus[] = ['blocked', 'in-progress', 'review', 'proposed', 'backlog']
export const DONE_SECTION = 'done'
export const DEFAULT_OPEN: readonly string[] = ORDER
const ROW_LIMIT = 20
/** Priorities worth a color; medium and low read as the default. */
const PRIORITY_COLOR: Record<string, string> = { critical: 'red', high: 'yellow' }

export const isOpen = (openLanes: string[] | null, id: string): boolean => (openLanes ?? DEFAULT_OPEN).includes(id)

export function boardView(kit: Kit, data: BoardData, act: Actions) {
  const { Box, Text } = kit
  const { snapshot } = data
  const sections: Section[] = [
    ...ORDER.map(status => {
      const spec = LANES.find(item => item.status === status)
      return { id: status, name: spec?.name ?? status, glyph: spec?.glyph ?? '·', color: spec?.color ?? 'white', tasks: lane(snapshot.tasks, status) }
    }),
    { id: DONE_SECTION, name: 'Done today', glyph: '✓', color: DONE_COLOR, tasks: snapshot.doneRecently },
  ]

  return (
    <Box flexDirection="column" rowGap={1}>
      {snapshot.tasks.length === 0 ? <Text dimColor>No open tasks in {snapshot.workspace}.</Text> : null}
      {sections.map(section => sectionView(kit, section, data, act))}
    </Box>
  )
}

function sectionView(kit: Kit, section: Section, data: BoardData, act: Actions) {
  const { Box, Button, Text } = kit
  const count = section.tasks.length
  const isQuiet = count === 0
  // An empty section is its header alone, whatever the person left it as.
  const open = !isQuiet && isOpen(data.openLanes, section.id)

  return (
    <Box key={`section:${section.id}`} flexDirection="column">
      <Box columnGap={1}>
        <Text dimColor>{isQuiet ? ' ' : open ? '▾' : '▸'}</Text>
        <Text color={isQuiet ? undefined : section.color} dimColor={isQuiet}>
          {section.glyph}
        </Text>
        <Button key={`lane:${section.id}`} label={section.name} plain dimColor={isQuiet} onPress={() => act.toggleLane(section.id)} />
        <Text dimColor>{count}</Text>
      </Box>
      {open ? (
        <Box flexDirection="column" paddingLeft={2} rowGap={0}>
          {section.tasks.slice(0, ROW_LIMIT).map(task => taskRow(kit, task, data, act))}
          {count > ROW_LIMIT ? <Text dimColor>{`+${count - ROW_LIMIT} more · /orbit-board <id> opens one`}</Text> : null}
        </Box>
      ) : null}
    </Box>
  )
}

function taskRow(kit: Kit, task: OrbitTask, data: BoardData, act: Actions) {
  const { Box, Button, Text } = kit
  const isSelected = data.selected === task.id
  const isMine = data.active === task.id
  const updated = Date.parse(task.updatedAt)
  const urgent = PRIORITY_COLOR[task.priority]
  const meta = [task.type, task.crew, Number.isFinite(updated) ? ago(data.now - updated) : null].filter(Boolean).join(' · ')

  return (
    <Box key={`row:${task.id}`} flexDirection="column" marginTop={isSelected ? 1 : 0} marginBottom={isSelected ? 1 : 0}>
      <Box columnGap={1}>
        <Text color={ACCENT}>{isSelected ? '▾' : isMine ? '◉' : ' '}</Text>
        <Button key={`card:${task.id}`} label={task.id} plain dimColor={!isSelected && !isMine} onPress={() => act.select(isSelected ? null : task.id)} />
        <Box flexDirection="column" flexShrink={1}>
          <Text bold={isSelected} wrap="wrap">
            {task.title}
          </Text>
          <Text wrap="truncate-end">
            {urgent ? <Text color={urgent}>{task.priority} · </Text> : null}
            <Text dimColor>{meta}</Text>
          </Text>
        </Box>
      </Box>
      {isSelected ? details(kit, task, data, act) : null}
    </Box>
  )
}

function details(kit: Kit, task: OrbitTask, data: BoardData, act: Actions) {
  const { Box, Button, Text } = kit
  const spec = laneOf(task.status)
  const facts = [task.complexity ? `complexity ${task.complexity}` : null, task.deps.length > 0 ? `depends on ${task.deps.join(', ')}` : null, task.runId ? `last run ${task.runId}` : null].filter(Boolean)
  const buttons: { key: string; label: string; hotkey: string; isPrimary?: boolean; press: () => void }[] = []

  if (task.status === 'proposed') {
    buttons.push({ key: 'approve', label: 'Approve', hotkey: 'a', isPrimary: true, press: () => act.approve(task.id) })
    buttons.push({ key: 'reject', label: 'Reject', hotkey: 'x', press: () => void act.reject(task.id) })
  }
  if (task.status === 'backlog') {
    buttons.push({ key: 'ship', label: 'Ship…', hotkey: 's', isPrimary: true, press: () => void act.ship(task.id) })
    buttons.push({ key: 'work', label: 'Work here', hotkey: 'w', press: () => void act.workHere(task.id) })
  }
  if (task.status === 'in-progress') {
    if (task.runId) buttons.push({ key: 'track', label: 'Track run', hotkey: 't', isPrimary: true, press: () => void act.track(task.id, task.runId ?? '') })
    if (data.active !== task.id) buttons.push({ key: 'work', label: 'Work here', hotkey: 'w', press: () => void act.workHere(task.id) })
  }
  if (task.status === 'review') {
    buttons.push({ key: 'accept', label: 'Accept → done', hotkey: 'a', isPrimary: true, press: () => void act.accept(task.id) })
    buttons.push({ key: 'work', label: 'Work here', hotkey: 'w', press: () => void act.workHere(task.id) })
  }
  if (task.status === 'blocked') {
    buttons.push({ key: 'rescue', label: 'Rescue here', hotkey: 'r', isPrimary: true, press: () => void act.rescue(task.id) })
    if (task.runId) buttons.push({ key: 'track', label: 'Open run', hotkey: 't', press: () => void act.track(task.id, task.runId ?? '') })
  }

  return (
    <Box key="details" flexDirection="column" marginLeft={2} marginTop={1} borderStyle="round" borderColor={spec?.color} borderDimColor paddingX={1}>
      {facts.length > 0 ? (
        <Text dimColor wrap="wrap">
          {facts.join(' · ')}
        </Text>
      ) : null}
      {task.criteria.length > 0 ? <Text bold>Acceptance criteria</Text> : <Text dimColor>No acceptance criteria recorded.</Text>}
      {task.criteria.slice(0, 8).map((item, index) => (
        <Box key={`criterion:${index}`} columnGap={1}>
          <Text dimColor>○</Text>
          <Text wrap="wrap">{item}</Text>
        </Box>
      ))}
      {task.criteria.length > 8 ? <Text dimColor>+{task.criteria.length - 8} more criteria</Text> : null}
      {buttons.length > 0 ? (
        <Box marginTop={1} columnGap={1} flexWrap="wrap">
          {buttons.map(button => (
            <Button key={button.key} label={button.label} hotkey={button.hotkey} variant={button.isPrimary ? 'primary' : undefined} onPress={button.press} />
          ))}
        </Box>
      ) : null}
    </Box>
  )
}
