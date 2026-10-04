// The Board view: open tasks in lanes by status, and the selected task's
// details with the actions that apply to its status.

import type { OrbitSnapshot, OrbitTask } from '../../../types'
import type { Actions } from './kit'
import { ACCENT, clip, lane, LANES, laneOf } from '../model'
import type { Kit } from './kit'

export type BoardData = {
  snapshot: OrbitSnapshot
  selected: string | null
  active: string | null
  columns: number
}

const WIDE = 100

export function boardView(kit: Kit, data: BoardData, act: Actions) {
  const { Box, Text } = kit
  const { snapshot, columns } = data
  const isWide = columns >= WIDE
  const laneWidth = isWide ? Math.floor((columns - (LANES.length - 1)) / LANES.length) : columns
  const limit = isWide ? 10 : 5
  const chosen = data.selected === null ? undefined : [...snapshot.tasks, ...snapshot.doneRecently].find(task => task.id === data.selected)

  const lanes = LANES.map(spec => {
    const tasks = lane(snapshot.tasks, spec.status)
    return (
      <Box key={`lane:${spec.status}`} flexDirection="column" width={isWide ? laneWidth : undefined} marginBottom={isWide ? 0 : 1}>
        <Text wrap="truncate-end">
          <Text color={spec.color}>{spec.glyph}</Text> <Text bold>{spec.name}</Text> <Text dimColor>{tasks.length}</Text>
        </Text>
        {tasks.slice(0, limit).map(task => cardRow(kit, task, data, laneWidth, act))}
        {tasks.length > limit ? <Text dimColor>+{tasks.length - limit} more</Text> : null}
        {tasks.length === 0 ? <Text dimColor>—</Text> : null}
      </Box>
    )
  })

  return (
    <Box flexDirection="column">
      {snapshot.tasks.length === 0 ? <Text dimColor>No open tasks in {snapshot.workspace}.</Text> : null}
      <Box flexDirection={isWide ? 'row' : 'column'} columnGap={1}>
        {lanes}
      </Box>
      {snapshot.doneRecently.length > 0 ? (
        <Text dimColor wrap="truncate-end">
          <Text color="green">✓</Text> done in 24h: {snapshot.doneRecently.map(task => task.id).join(' ')}
        </Text>
      ) : null}
      {chosen ? details(kit, chosen, data, act) : <Text dimColor>Select a task for its criteria, dependencies and actions.</Text>}
    </Box>
  )
}

function cardRow(kit: Kit, task: OrbitTask, data: BoardData, width: number, act: Actions) {
  const { Box, Button, Text } = kit
  const isSelected = data.selected === task.id
  const isMine = data.active === task.id
  const label = clip(`${isMine ? '◉ ' : ''}${task.id} ${task.title}`, Math.max(8, width - 2))
  return (
    <Box key={`row:${task.id}`}>
      <Text color={ACCENT}>{isSelected ? '▸' : ' '}</Text>
      <Button key={`card:${task.id}`} label={label} plain dimColor={!isSelected && !isMine} onPress={() => act.select(task.id)} />
    </Box>
  )
}

function details(kit: Kit, task: OrbitTask, data: BoardData, act: Actions) {
  const { Box, Button, Text } = kit
  const spec = laneOf(task.status)
  const meta = [task.status, task.priority, task.type, task.complexity, task.crew ? `crew ${task.crew}` : null].filter(Boolean).join(' · ')
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
    <Box key="details" flexDirection="column" marginTop={1} borderStyle="round" borderDimColor paddingX={1}>
      <Text wrap="truncate-end">
        <Text bold color={ACCENT}>
          {task.id}
        </Text>{' '}
        <Text color={spec?.color}>{spec?.glyph ?? '✓'}</Text>
        <Text dimColor> {meta}</Text>
      </Text>
      <Text bold>{task.title}</Text>
      {task.deps.length > 0 || task.runId ? (
        <Text dimColor wrap="truncate-end">
          {task.deps.length > 0 ? `depends on ${task.deps.join(', ')}` : ''}
          {task.deps.length > 0 && task.runId ? ' · ' : ''}
          {task.runId ? `last run ${task.runId}` : ''}
        </Text>
      ) : null}
      {task.criteria.slice(0, 6).map(item => (
        <Text>
          <Text dimColor>○ </Text>
          {item}
        </Text>
      ))}
      {task.criteria.length > 6 ? <Text dimColor>+{task.criteria.length - 6} more criteria</Text> : null}
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
