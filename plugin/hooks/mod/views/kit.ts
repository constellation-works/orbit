import type { BoxProps, ButtonProps, ElementConstructor, TextProps } from 'claude-code'

import type { OrbitView } from '../../../types'

/** The elements every surface draws that the Orbit views use. */
export type Kit = {
  Box: ElementConstructor<BoxProps>
  Text: ElementConstructor<TextProps>
  Button: ElementConstructor<ButtonProps>
}

/** What the views' Buttons run: each a press, never run unasked. */
export type Actions = {
  refresh: () => void
  setView: (view: OrbitView) => void
  select: (taskId: string | null) => void
  toggleLane: (section: string) => void
  clearFlash: () => void
  hideBand: () => void
  closePane: () => void
  approve: (taskId: string) => void
  accept: (taskId: string) => Promise<void>
  reject: (taskId: string) => Promise<void>
  workHere: (taskId: string) => Promise<void>
  rescue: (taskId: string) => Promise<void>
  ship: (taskId: string) => Promise<void>
  launch: () => void
  track: (taskId: string, runId: string) => Promise<void>
  resetShip: () => void
}
