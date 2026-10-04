// The Orbit pane: a tab row (Board, Ship), a one-line notice, and the
// view the person picked.

import type { ElementConstructor, SvgProps } from 'claude-code'

import type { OrbitShip, OrbitSnapshot, OrbitView } from '../../../types'
import type { Actions } from './kit'
import { ACCENT, ago } from '../model'
import { boardView } from './board'
import type { Kit } from './kit'
import { shipView } from './ship'

export type PaneData = {
  snapshot: OrbitSnapshot | null
  error: string | null
  view: OrbitView
  selected: string | null
  openLanes: string[] | null
  active: string | null
  ship: OrbitShip | null
  flash: string | null
  now: number
  columns: number
}

const TABS: readonly { view: OrbitView; label: string; hotkey: string }[] = [
  { view: 'board', label: 'Board', hotkey: '1' },
  { view: 'ship', label: 'Ship', hotkey: '2' },
]

export function pane(kit: Kit, data: PaneData, act: Actions, Svg?: ElementConstructor<SvgProps>) {
  const { Box, Button, Text } = kit
  const { snapshot, error, view } = data
  const where = snapshot ? `${snapshot.workspace}${snapshot.host ? `@${snapshot.host}` : ''} · ${error ? `stale: ${error}` : ago(data.now - snapshot.at)}` : ''

  let body
  if (snapshot === null) {
    body = <Text dimColor>{error ? `Orbit unavailable: ${error}` : 'Reading the workspace…'}</Text>
  } else if (view === 'ship') {
    body = shipView(kit, { snapshot, ship: data.ship, now: data.now, columns: data.columns }, act, Svg)
  } else {
    body = boardView(kit, { snapshot, selected: data.selected, active: data.active, openLanes: data.openLanes, now: data.now }, act)
  }

  return (
    <Box flexDirection="column">
      <Box columnGap={1} flexWrap="wrap">
        {TABS.map(tab => (
          <Button
            key={`tab:${tab.view}`}
            label={tab.label}
            hotkey={tab.view === view ? undefined : tab.hotkey}
            variant={tab.view === view ? 'primary' : undefined}
            onPress={() => act.setView(tab.view)}
          />
        ))}
        <Text dimColor wrap="truncate-end">
          {where}
        </Text>
        <Button key="refresh" label="Refresh" hotkey="g" plain dimColor onPress={act.refresh} />
        <Button key="close" label="Close" role="dismiss" plain dimColor onPress={act.closePane} />
      </Box>
      {data.flash ? (
        <Box columnGap={1}>
          <Text color={ACCENT} wrap="truncate-end">
            {data.flash}
          </Text>
          <Button key="dismiss-flash" label="×" plain dimColor onPress={act.clearFlash} />
        </Box>
      ) : null}
      <Box marginTop={1} flexDirection="column">
        {body}
      </Box>
    </Box>
  )
}
