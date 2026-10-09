// The Ship view: preflight checks and the launch, then the run's progress
// through the twelve steps of the PR pipeline.

import type { ElementConstructor, SvgProps } from 'claude-code'

import type { OrbitShip, OrbitSnapshot, OrbitStepState } from '../../../types'
import type { Actions } from './kit'
import { ACCENT, clip, clock, lane, PIPELINE, shipPosition, STEP_GLYPH } from '../model'
import type { Kit } from './kit'

export type ShipData = {
  snapshot: OrbitSnapshot
  ship: OrbitShip | null
  now: number
  columns: number
}

const STEP_COLOR: Record<OrbitStepState, string | undefined> = { done: ACCENT, active: 'cyan', failed: 'red', pending: undefined }

const PHASE: Record<OrbitShip['phase'], { label: string; color: string | undefined }> = {
  preflight: { label: 'READY', color: undefined },
  launching: { label: 'LAUNCHING', color: 'cyan' },
  flying: { label: 'IN FLIGHT', color: 'cyan' },
  landed: { label: 'LANDED', color: 'green' },
  failed: { label: 'FAILED', color: 'red' },
  held: { label: 'HELD', color: 'yellow' },
  skipped: { label: 'SKIPPED', color: undefined },
}

export function shipView(kit: Kit, data: ShipData, act: Actions, Svg?: ElementConstructor<SvgProps>) {
  const { Box, Button, Text } = kit
  const { snapshot, ship, now, columns } = data

  if (ship === null) {
    const ready = lane(snapshot.tasks, 'backlog')
    return (
      <Box flexDirection="column">
        <Text>Choose a backlog task to ship.</Text>
        {ready.length === 0 ? <Text dimColor>The backlog is empty.</Text> : null}
        {ready.slice(0, 12).map(task => (
          <Button key={`pick:${task.id}`} label={clip(`${task.id} ${task.title}`, Math.max(12, columns - 4))} plain dimColor onPress={() => void act.ship(task.id)} />
        ))}
      </Box>
    )
  }

  const task = [...snapshot.tasks, ...snapshot.doneRecently].find(item => item.id === ship.taskId)
  const phase = PHASE[ship.phase]
  const elapsed = ship.startedAt === null ? 'T+00:00' : clock((ship.finishedAt ?? now) - ship.startedAt)
  const meta = [task?.crew ? `crew ${task.crew}` : null, task?.priority, snapshot.workspace, ship.runId].filter(Boolean).join(' · ')

  return (
    <Box flexDirection="column">
      <Box justifyContent="space-between">
        <Text bold color={ACCENT}>
          SHIP · {ship.taskId}
        </Text>
        <Text>
          <Text color={phase.color} bold>
            {phase.label}
          </Text>
          <Text dimColor> {elapsed}</Text>
        </Text>
      </Box>
      <Text bold>{task?.title ?? 'not in the open tasks'}</Text>
      <Text dimColor wrap="truncate-end">
        {meta}
      </Text>
      {ship.phase === 'preflight' ? preflightBlock(kit, ship, act) : flightBlock(kit, ship, columns, act, Svg)}
    </Box>
  )
}

function preflightBlock(kit: Kit, ship: OrbitShip, act: Actions) {
  const { Box, Button, Text } = kit
  const isClear = ship.checks.slice(0, 2).every(check => check.isOk)
  return (
    <Box flexDirection="column" marginTop={1}>
      <Text dimColor>Preflight</Text>
      {ship.checks.map(check => (
        <Text wrap="truncate-end">
          <Text color={check.isOk ? 'green' : 'red'}>{check.isOk ? '✓' : '✗'}</Text> {check.label}
          <Text dimColor> · {check.detail}</Text>
        </Text>
      ))}
      <Box marginTop={1} columnGap={1}>
        {isClear ? <Button key="launch" label="Launch" hotkey="l" variant="primary" onPress={act.launch} /> : null}
        <Button key="back" label="Back to board" hotkey="b" onPress={() => act.setView('board')} />
        <Button key="clear" label="Clear" hotkey="c" onPress={act.resetShip} />
      </Box>
      {isClear ? (
        <Text dimColor>Launch runs orbit run ship {ship.taskId} after you confirm.</Text>
      ) : (
        <Text color="red">Only a backlog task with its dependencies done can ship.</Text>
      )}
    </Box>
  )
}

function flightBlock(kit: Kit, ship: OrbitShip, columns: number, act: Actions, Svg?: ElementConstructor<SvgProps>) {
  const { Box, Button, Text } = kit
  const { label, done } = shipPosition(ship)
  const perRow = Math.max(1, Math.min(4, Math.floor(columns / 16)))
  const rows: number[][] = []
  for (let start = 0; start < PIPELINE.length; start += perRow) rows.push(PIPELINE.slice(start, start + perRow).map((_, offset) => start + offset))
  const isOver = ship.phase === 'landed' || ship.phase === 'failed' || ship.phase === 'held' || ship.phase === 'skipped'

  return (
    <Box flexDirection="column" marginTop={1}>
      {Svg ? <Svg source={trajectory(ship.steps)} alt={`Ship track: ${done} of ${PIPELINE.length} steps done, at ${label}`} width={Math.min(720, columns * 8)} /> : null}
      <Text wrap="truncate-end">
        {ship.steps.map((state, index) => (
          <Text color={STEP_COLOR[state]} dimColor={state === 'pending'}>
            {index === 0 ? '' : ship.steps[index - 1] === 'done' ? '━' : '─'}
            {STEP_GLYPH[state]}
          </Text>
        ))}
        <Text dimColor>
          {'  '}
          {label} {done}/{PIPELINE.length}
        </Text>
      </Text>
      <Box flexDirection="column" marginTop={1}>
        {rows.map(row => (
          <Box>
            {row.map(index => {
              const state = ship.steps[index] ?? 'pending'
              return (
                <Box width={16}>
                  <Text color={STEP_COLOR[state]} dimColor={state === 'pending'}>
                    {STEP_GLYPH[state]} {PIPELINE[index]?.label}
                  </Text>
                </Box>
              )
            })}
          </Box>
        ))}
      </Box>
      {ship.message ? <Text color={ship.phase === 'failed' ? 'red' : undefined}>{ship.message}</Text> : null}
      {ship.phase === 'landed' ? <Text color="green">Landed. The task moves on to review; its PR is open against the base branch.</Text> : null}
      {ship.phase === 'held' ? <Text color="yellow">Held. Delivery is waiting on external review evidence; the run is over and the task stays as it is.</Text> : null}
      {ship.phase === 'skipped' ? <Text dimColor>Skipped. The run ended without doing any work.</Text> : null}
      <Box marginTop={1} columnGap={1}>
        {ship.phase === 'failed' ? <Button key="rescue" label="Rescue here" hotkey="r" variant="primary" onPress={() => void act.rescue(ship.taskId)} /> : null}
        <Button key="back" label="Back to board" hotkey="b" onPress={() => act.setView('board')} />
        {isOver ? <Button key="clear" label="Clear" hotkey="c" onPress={act.resetShip} /> : null}
      </Box>
      {ship.phase === 'flying' ? <Text dimColor>Safe to keep working; the band and status line follow the run.</Text> : null}
    </Box>
  )
}

/** The ship track as a rising trajectory, for surfaces that draw SVG. */
export function trajectory(steps: readonly OrbitStepState[]): string {
  const width = 720
  const height = 170
  const count = PIPELINE.length
  const point = (index: number) => {
    const f = index / (count - 1)
    return { x: 24 + f * (width - 48), y: 140 - 110 * Math.sin((f * Math.PI) / 2) }
  }
  const path = (upTo: number) =>
    Array.from({ length: upTo + 1 }, (_, index) => point(index))
      .map((p, index) => `${index === 0 ? 'M' : 'L'}${p.x.toFixed(1)} ${p.y.toFixed(1)}`)
      .join(' ')
  const reached = steps.reduce((last, state, index) => (state === 'pending' ? last : index), -1)
  const fill: Record<OrbitStepState, string> = { done: ACCENT, active: '#0B0E14', failed: '#FF7A66', pending: 'none' }
  const ring: Record<OrbitStepState, string> = { done: ACCENT, active: '#4CC9F0', failed: '#FF7A66', pending: '#6E788C' }

  const nodes = steps
    .map((state, index) => {
      const p = point(index)
      const labelY = index < 4 ? p.y + 22 : p.y - 14
      const name = PIPELINE[index]?.label ?? ''
      return `<circle cx="${p.x.toFixed(1)}" cy="${p.y.toFixed(1)}" r="${state === 'active' ? 8 : 6}" fill="${fill[state]}" stroke="${ring[state]}" stroke-width="2"/><text x="${p.x.toFixed(1)}" y="${labelY.toFixed(1)}" text-anchor="middle" font-family="ui-monospace, monospace" font-size="11" fill="${state === 'pending' ? '#8A93A6' : ring[state]}">${name}</text>`
    })
    .join('')

  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" width="${width}" height="${height}"><path d="${path(count - 1)}" fill="none" stroke="#6E788C" stroke-opacity="0.5" stroke-width="2" stroke-dasharray="4 6"/>${reached > 0 ? `<path d="${path(reached)}" fill="none" stroke="${ACCENT}" stroke-width="3" stroke-linecap="round"/>` : ''}${nodes}</svg>`
}

