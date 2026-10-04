// The Orbit mod against a fake `orbit`: every process the mod runs is
// answered here, so the tests read what the mod draws and runs, not a
// workspace's live tasks.

import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

const SURFACES = ['terminal', 'desktop'] as const

const TASKS = [
  { id: 'ORB-1', title: 'Fix the band', status: 'blocked', priority: 'high', type: 'bug', acceptance_criteria: ['band shows'], job_run_id: 'jrun-1', updated_at: '2026-10-03T10:00:00Z' },
  { id: 'ORB-2', title: 'Ship the mod', status: 'backlog', priority: 'medium', type: 'feature', acceptance_criteria: [], updated_at: '2026-10-03T10:00:00Z' },
  { id: 'ORB-3', title: 'Review the board', status: 'review', priority: 'low', type: 'chore', acceptance_criteria: [], updated_at: '2026-10-03T10:00:00Z' },
  { id: 'ORB-4', title: 'A proposal', status: 'proposed', priority: 'low', type: 'feature', acceptance_criteria: [], updated_at: '2026-10-03T10:00:00Z' },
]

type Ran = { args: string[]; isRemote: boolean }

/** Answers every process the mod runs as an owner checkout of workspace `demo` would. */
function fakeOrbit(on: On, ran: Ran[]): void {
  on('process.run', (_$, e) => {
    const isRemote = e.argv[0] === 'ssh'
    const args = isRemote ? (e.argv.at(-1) ?? '').split(' ').slice(2).map(arg => arg.replace(/^'|'$/g, '')) : e.argv.slice(4)
    ran.push({ args, isRemote })
    const reply = (stdout: unknown) => ({ value: { exitCode: 0, stdout: JSON.stringify(stdout), stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
    if (args[0] === 'workspace') return reply({ registered: true, workspace: { name: 'demo' }, checkout: { role: 'owner' } })
    if (args[0] === 'task' && args[1] === 'list') return reply(args.includes('done') ? [] : TASKS)
    if (args[0] === 'run' && args[1] === 'readiness') return reply({ tasks: [{ task_id: args[2], eligible: true, conflicts: [] }] })
    return reply({})
  })
  stubSession(on)
}

/** What the engine itself would answer beneath the mod in a session. */
function stubSession(on: On): void {
  on('command.register', (_$, e) => ({ value: { command: e.name } }))
  on('ui.status', () => ({ value: undefined }))
  on('ui.toast', () => ({ value: undefined }))
  on('ui.open', () => ({ value: { isPlaced: true } }))
  on('session.start', () => ({ cwd: '/work/demo' }))
}

const SITE = { scroll: { offset: 0, bodyRows: 40 }, view: {} }
const BAND = { component: 'AbovePrompt', props: { hasSurvey: false, isWorking: false, maxRows: 4, bodyColumns: 120, ...SITE } } as const
const PANE = { component: 'Pane', requestId: 'orbit', props: { title: 'Orbit', isFocused: true, bodyColumns: 120, placement: 'dock', ...SITE } } as const

test('the band counts the workspace and offers a rescue for the blocked task', async ($, on) => {
  const ran: Ran[] = []
  fakeOrbit(on, ran)
  const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
  await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
  await clock.advance(0)

  expect(ran.some(run => run.args.join(' ') === 'workspace show --format json')).toBe(true)
  expect(ran.filter(run => run.args[0] === 'task').every(run => run.args.includes('--workspace') && run.args.includes('demo'))).toBe(true)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ plugin: 'orbit', surface, ...BAND })
    expect(await ui.find({ key: 'rescue' })).toBeDefined()
    expect(await ui.find({ key: 'board' })).toBeDefined()
    await ui.unmount()
  }
})

test('the board approves a proposed task with `orbit task update --approve`', async ($, on) => {
  const ran: Ran[] = []
  fakeOrbit(on, ran)
  const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
  await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
  await clock.advance(0)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ plugin: 'orbit', surface, ...PANE })
    for (const task of TASKS) expect(await ui.find({ key: `card:${task.id}` })).toBeDefined()
    await ui.press({ key: 'card:ORB-4' })
    ran.length = 0
    await ui.press({ key: 'approve' })
    expect(ran.some(run => run.args.slice(0, 4).join(' ') === 'task update ORB-4 --approve')).toBe(true)
    await ui.unmount()
  }
})

test('Ship… on a backlog task opens the preflight with Launch armed', async ($, on) => {
  fakeOrbit(on, [])
  const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
  await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
  await clock.advance(0)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ plugin: 'orbit', surface, ...PANE })
    await ui.press({ key: 'tab:board' })
    await ui.press({ key: 'card:ORB-2' })
    await ui.press({ key: 'ship' })
    expect(await ui.find({ key: 'launch' })).toBeDefined()
    await ui.press({ key: 'clear' })
    await ui.unmount()
  }
})

test('a replica checkout reads its owner over SSH when ownerHost is set', { options: { ownerHost: 'owner-box' } }, async ($, on) => {
  const ran: Ran[] = []
  on('process.run', (_$, e) => {
    const isRemote = e.argv[0] === 'ssh'
    ran.push({ args: [...e.argv], isRemote })
    const stdout = isRemote ? '[]' : JSON.stringify({ registered: true, workspace: { name: 'demo' }, checkout: { role: 'replica' } })
    return { value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  stubSession(on)
  const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
  await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
  await clock.advance(0)

  const remote = ran.filter(run => run.isRemote)
  expect(remote.length).toBeGreaterThan(0)
  expect(remote.every(run => run.args.includes('owner-box') && run.args.includes('BatchMode=yes'))).toBe(true)
})
