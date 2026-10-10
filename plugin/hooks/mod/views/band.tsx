// The band above the prompt: the workspace's counts, the task this session
// is on, a ship in flight, and the first blocked task.

import type { OrbitShip, OrbitSnapshot } from '../../../types'
import type { Actions } from './kit'
import { ACCENT, ago, bar, clip, clock, PIPELINE, shipPosition, tally } from '../model'
import type { Kit } from './kit'

export type BandData = {
  snapshot: OrbitSnapshot | null
  error: string | null
  active: string | null
  ship: OrbitShip | null
  now: number
  columns: number
  isCompact: boolean
}

const RECENT_LANDING_MS = 10 * 60_000

export function band(kit: Kit, data: BandData, act: Actions) {
  const { Box, Button, Text } = kit
  const { snapshot, error, active, ship, now, columns } = data

  const hide = <Button key="hide" label="Hide" plain dimColor onPress={act.hideBand} />
  const board = <Button key="board" label="Board" plain dimColor onPress={() => act.setView('board')} />

  if (snapshot === null) {
    return (
      <Box>
        <Text dimColor wrap="truncate-end">
          orbit · {error ?? 'reading…'}{' '}
        </Text>
        {hide}
      </Box>
    )
  }

  const counts = tally(snapshot.tasks)
  const isFlying = ship !== null && (ship.phase === 'launching' || ship.phase === 'flying')
  const isRecentLanding = ship !== null && (ship.phase === 'landed' || ship.phase === 'failed' || ship.phase === 'held' || ship.phase === 'skipped') && ship.finishedAt !== null && now - ship.finishedAt < RECENT_LANDING_MS

  if (data.isCompact || columns < 80) {
    return (
      <Box>
        <Text wrap="truncate-end">
          <Text bold>orbit</Text> <Text color="cyan">▶{counts['in-progress']}</Text>{' '}
          <Text color={counts.blocked > 0 ? 'red' : undefined} dimColor={counts.blocked === 0}>
            ■{counts.blocked}
          </Text>{' '}
          <Text color="yellow">◎{counts.review}</Text>
          {isFlying && ship ? <Text color={ACCENT}> ⇡{ship.taskId}</Text> : null}
          {!isFlying && active ? <Text color={ACCENT}> ◉{active}</Text> : null}{' '}
        </Text>
        {board}
      </Box>
    )
  }

  const where = `${snapshot.workspace}${snapshot.host ? `@${snapshot.host}` : ''}`
  const age = error ? `stale: ${error}` : ago(now - snapshot.at)
  const blocked = snapshot.tasks.find(task => task.status === 'blocked')
  const focus = active ? snapshot.tasks.find(task => task.id === active) : undefined
  const isBusy = isFlying || isRecentLanding || active !== null
  const titleRoom = Math.max(12, columns - 70)

  const lead =
    (isFlying || isRecentLanding) && ship ? (
      shipLead(kit, ship, now)
    ) : active !== null ? (
      <Text>
        <Text bold color={ACCENT}>
          ◉ {active}
        </Text>{' '}
        {clip(focus?.title ?? 'not in the open tasks', titleRoom)}
        <Text dimColor> · {focus?.status ?? 'closed'} </Text>
      </Text>
    ) : null

  return (
    <Box flexDirection="column">
      <Box>
        {lead}
        <Text wrap="truncate-end">
          {isBusy ? null : <Text bold>orbit</Text>}
          {isBusy ? null : <Text dimColor> {where} </Text>}
          <Text color={counts['in-progress'] > 0 ? 'cyan' : undefined} dimColor={counts['in-progress'] === 0}>
            {isBusy ? ` ▶${counts['in-progress']}` : ` ▶ ${counts['in-progress']} running`}
          </Text>
          <Text color={counts.blocked > 0 ? 'red' : undefined} dimColor={counts.blocked === 0}>
            {isBusy ? ` ■${counts.blocked}` : `  ■ ${counts.blocked} blocked`}
          </Text>
          <Text color={counts.review > 0 ? 'yellow' : undefined} dimColor={counts.review === 0}>
            {isBusy ? ` ◎${counts.review}` : `  ◎ ${counts.review} review`}
          </Text>
          <Text dimColor>{isBusy ? '  ' : `   ${counts.backlog} backlog · ${counts.proposed} proposed · ${age}  `}</Text>
        </Text>
        {isFlying || isRecentLanding ? <Button key="track" label="Track" plain dimColor onPress={() => act.setView('ship')} /> : null}
        {isFlying || isRecentLanding ? <Text> </Text> : null}
        {board}
        <Text> </Text>
        {hide}
      </Box>
      {blocked ? (
        <Box>
          <Text wrap="truncate-end">
            <Text bold color="red">
              ■ {blocked.id} blocked
            </Text>{' '}
            {clip(blocked.title, Math.max(12, columns - 48))}{' '}
          </Text>
          <Button
            key="open-blocked"
            label="Open"
            plain
            dimColor
            onPress={() => {
              act.select(blocked.id)
              act.setView('board')
            }}
          />
          <Text> </Text>
          <Button key="rescue" label="Rescue here" plain onPress={() => void act.rescue(blocked.id)} />
        </Box>
      ) : null}
    </Box>
  )
}

function shipLead(kit: Kit, ship: OrbitShip, now: number) {
  const { Text } = kit
  const { label, done } = shipPosition(ship)
  const { filled, empty } = bar(done, PIPELINE.length, 16)
  const elapsed = ship.startedAt === null ? '' : `  ${clock((ship.finishedAt ?? now) - ship.startedAt)}`

  if (ship.phase === 'landed') {
    return (
      <Text>
        <Text bold color="green">
          ✓ {ship.taskId} landed
        </Text>
        <Text dimColor>
          {' '}
          {ship.runId ?? ''}
          {elapsed}{' '}
        </Text>
      </Text>
    )
  }
  if (ship.phase === 'failed') {
    return (
      <Text>
        <Text bold color="red">
          ✗ {ship.taskId} ship failed
        </Text>
        <Text dimColor> at {label} </Text>
      </Text>
    )
  }
  if (ship.phase === 'held' || ship.phase === 'skipped') {
    return (
      <Text>
        <Text bold color={ship.phase === 'held' ? 'yellow' : undefined}>
          ■ {ship.taskId} ship {ship.phase}
        </Text>
        <Text dimColor>
          {' '}
          {ship.runId ?? ''}
          {elapsed}{' '}
        </Text>
      </Text>
    )
  }
  return (
    <Text>
      <Text bold color={ACCENT}>
        ⇡ shipping {ship.taskId}
      </Text>
      <Text dimColor> {ship.runId ?? 'launching'} </Text>
      <Text color="cyan">{label}</Text>
      <Text dimColor>
        {' '}
        {done}/{PIPELINE.length}{' '}
      </Text>
      <Text color={ACCENT}>{filled}</Text>
      <Text dimColor>
        {empty}
        {elapsed}{' '}
      </Text>
    </Text>
  )
}
