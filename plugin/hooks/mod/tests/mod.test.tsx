// The Orbit mod against a fake `orbit`: every process the mod runs is
// answered here, so the tests read what the mod draws and runs, not a
// workspace's live tasks.

import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'
import type { OrbitShip } from '../../../types'
import { PIPELINE } from '../model'

const SHIP_POLL_MS = 5000
const SURFACES = ['terminal', 'desktop'] as const

const TASKS = [
  { id: 'ORB-1', title: 'Fix the band', status: 'blocked', priority: 'high', type: 'bug', acceptance_criteria: ['band shows'], job_run_id: 'jrun-1', updated_at: '2026-10-03T10:00:00Z' },
  { id: 'ORB-2', title: 'Ship the mod', status: 'backlog', priority: 'medium', type: 'feature', acceptance_criteria: [], updated_at: '2026-10-03T10:00:00Z' },
  { id: 'ORB-3', title: 'Review the board', status: 'review', priority: 'low', type: 'chore', acceptance_criteria: [], updated_at: '2026-10-03T10:00:00Z' },
  { id: 'ORB-4', title: 'A proposal', status: 'proposed', priority: 'low', type: 'feature', acceptance_criteria: [], updated_at: '2026-10-03T10:00:00Z' },
]

type Ran = { args: string[]; isRemote: boolean }

function observeShip(on: On): () => OrbitShip | null {
  let ship: OrbitShip | null = null
  on('state.set', (_$, e, next) => {
    if (e.key === 'ship') ship = e.value as OrbitShip | null
    return next(e)
  })
  return () => ship
}

/** Answers every process the mod runs as an owner checkout of workspace `demo` would. */
function fakeOrbit(on: On, ran: Ran[], answer?: (args: string[]) => unknown): void {
  on('process.run', (_$, e) => {
    const isRemote = e.argv[0] === 'ssh'
    const args = isRemote ? (e.argv.at(-1) ?? '').split(' ').slice(2).map(arg => arg.replace(/^'|'$/g, '')) : e.argv.slice(4)
    ran.push({ args, isRemote })
    const reply = (stdout: unknown) => ({ value: { exitCode: 0, stdout: JSON.stringify(stdout), stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
    const answered = answer?.(args)
    if (answered !== undefined) return reply(answered)
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
    // The selection outlives the mount: open the row only where it is shut.
    if ((await ui.find({ key: 'approve' })) === undefined) await ui.press({ key: 'card:ORB-4' })
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
    if ((await ui.find({ key: 'ship' })) === undefined) await ui.press({ key: 'card:ORB-2' })
    await ui.press({ key: 'ship' })
    expect(await ui.find({ key: 'launch' })).toBeDefined()
    await ui.press({ key: 'clear' })
    await ui.unmount()
  }
})

for (const surface of SURFACES) {
  test(`Launch follows the leaf's steps while its coordinator is running on ${surface}`, async ($, on) => {
    const ran: Ran[] = []
    let dispatched = false
    let completed = false
    let landed = false
    fakeOrbit(on, ran, args => {
      if (args[0] !== 'run') return undefined
      if (args[1] === 'ship') return { run_id: 'jrun-coordinator' }
      if (args[1] === 'show') {
        const runId = args[2]
        const job = runId === 'jrun-coordinator' ? 'task_auto_pipeline' : runId === 'jrun-gate' ? 'task_gate_pipeline' : 'task_pr_pipeline'
        const children = runId === 'jrun-coordinator'
          ? (dispatched ? [{ child_run_id: 'jrun-other', job_name: 'task_pr_pipeline' }, { child_run_id: 'jrun-gate', job_name: 'task_gate_pipeline' }] : [])
          : runId === 'jrun-gate' ? [{ child_run_id: 'jrun-leaf', job_name: 'task_pr_pipeline' }] : []
        return {
          run: { job_id: job, task_ids: [runId === 'jrun-other' ? 'ORB-1' : 'ORB-2'], state: landed || (runId === 'jrun-leaf' && completed) ? 'success' : 'running' },
          pipeline_state: { child_dispatches: children },
        }
      }
      if (args[1] === 'events') return args[2] === 'jrun-leaf'
        ? { events: completed ? PIPELINE.map(step => ({ event_type: 'step.finished', step_id: step.id })) : [
            { event_type: 'step.started', step_id: 'worktree' },
            { event_type: 'step.finished', step_id: 'worktree' },
            { event_type: 'step.started', step_id: 'implement_bundle' },
          ] }
        : { events: [{ event_type: 'step.started', step_id: 'gate_invoke' }] }
      return undefined
    })
    on('tool.call', { tool: 'AskUserQuestion' }, () => ({ result: { answers: { 'Ship ORB-2 through the PR pipeline?': 'Ship it' } } }))
    const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
    const ship = observeShip(on)
    await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
    await clock.advance(0)
    await $.command.run({ command: 'orbit-ship', args: 'ORB-2' } as never)
    const ui = await $.ui.mount({ plugin: 'orbit', surface, ...PANE })
    await ui.press({ key: 'launch' })
    await clock.advance(5000)
    expect(ship()?.phase).toBe('flying')
    expect(ship()?.steps.every(state => state === 'pending')).toBe(true)

    dispatched = true
    await clock.advance(5000)
    const flying = ship()
    expect(flying?.runId).toBe('jrun-coordinator')
    expect(flying?.phase).toBe('flying')
    expect(flying?.steps.slice(0, 3)).toEqual(['done', 'active', 'pending'])
    expect(ran.some(run => run.args.slice(0, 3).join(' ') === 'run events jrun-leaf')).toBe(true)
    expect(ran.some(run => run.args.slice(0, 3).join(' ') === 'run events jrun-other')).toBe(false)
    expect(await ui.find({ key: 'clear' })).toBeUndefined()

    completed = true
    await clock.advance(5000)
    expect(ship()?.steps.every(state => state === 'done')).toBe(true)
    expect(ship()?.phase).toBe('flying')
    landed = true
    await clock.advance(5000)
    expect(ship()?.phase).toBe('landed')
    expect(await ui.find({ key: 'clear' })).toBeDefined()
    const polls = ran.length
    await clock.advance(5000)
    expect(ran.length).toBe(polls)
    await ui.unmount()
  })
}

test('a coordinator failure before dispatch ends the ship with no completed steps', async ($, on) => {
  let failed = false
  fakeOrbit(on, [], args => {
    if (args[0] !== 'run') return undefined
    if (args[1] === 'ship') return { run_id: 'jrun-coordinator' }
    if (args[1] === 'show') return { run: { job_id: 'task_auto_pipeline', state: failed ? 'failed' : 'running', error_message: failed ? 'Admission failed\nDetails' : null }, pipeline_state: null }
    if (args[1] === 'events') return { events: [] }
    return undefined
  })
  on('tool.call', { tool: 'AskUserQuestion' }, () => ({ result: { answers: { 'Ship ORB-2 through the PR pipeline?': 'Ship it' } } }))
  const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
  const ship = observeShip(on)
  await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
  await clock.advance(0)
  await $.command.run({ command: 'orbit-ship', args: 'ORB-2' } as never)
  const ui = await $.ui.mount({ plugin: 'orbit', surface: 'terminal', ...PANE })
  await ui.press({ key: 'launch' })
  failed = true
  await clock.advance(5000)
  const stopped = ship()
  expect(stopped?.phase).toBe('failed')
  expect(stopped?.message).toBe('Admission failed')
  expect(stopped?.steps.every(state => state === 'pending')).toBe(true)
  expect(await ui.find({ key: 'rescue' })).toBeDefined()
  await ui.unmount()
})

for (const state of ['held', 'skipped'] as const) {
  test(`a tracked run that ends ${state} stops polling and is neither landed nor failed`, async ($, on) => {
    const ship = observeShip(on)
    const ran: Ran[] = []
    fakeOrbit(on, ran, args => {
      if (args[0] === 'task' && args[1] === 'list') return args.includes('done') ? [] : TASKS.map(task => task.id === 'ORB-1' ? { ...task, status: 'in-progress' } : task)
      if (args[0] !== 'run') return undefined
      if (args[1] === 'show') return { run: { job_id: 'task_pr_pipeline', state, error_message: null } }
      if (args[1] === 'events') return { events: [{ event_type: 'step.finished', step_id: 'worktree' }] }
      return undefined
    })
    const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
    await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
    await clock.advance(0)
    const ui = await $.ui.mount({ plugin: 'orbit', surface: 'terminal', ...PANE })
    await ui.press({ key: 'card:ORB-1' })
    await ui.press({ key: 'track' })
    expect(ship()?.phase).toBe(state)
    expect(ship()?.finishedAt).not.toBeNull()
    expect(ship()?.steps.slice(0, 2)).toEqual(['done', 'pending'])
    expect(await ui.find({ key: 'clear' })).toBeDefined()
    expect(await ui.find({ key: 'rescue' })).toBeUndefined()
    const polls = ran.length
    await clock.advance(SHIP_POLL_MS * 3)
    expect(ran.filter(run => run.args[0] === 'run').length).toBe(ran.slice(0, polls).filter(run => run.args[0] === 'run').length)
    await ui.unmount()
  })
}

test('Board tracking reads the leaf directly and marks its active step failed', async ($, on) => {
  const ship = observeShip(on)
  fakeOrbit(on, [], args => {
    if (args[0] === 'task' && args[1] === 'list') return args.includes('done') ? [] : TASKS.map(task => task.id === 'ORB-1' ? { ...task, status: 'in-progress' } : task)
    if (args[0] !== 'run') return undefined
    if (args[1] === 'show') return { run: { job_id: 'task_pr_pipeline', state: 'failed', error_message: 'Implementation failed' } }
    if (args[1] === 'events') return { events: [{ event_type: 'step.finished', step_id: 'worktree' }, { event_type: 'step.started', step_id: 'implement_bundle' }] }
    return undefined
  })
  const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
  await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
  await clock.advance(0)
  const ui = await $.ui.mount({ plugin: 'orbit', surface: 'terminal', ...PANE })
  await ui.press({ key: 'card:ORB-1' })
  await ui.press({ key: 'track' })
  const stopped = ship()
  expect(stopped?.runId).toBe('jrun-1')
  expect(stopped?.phase).toBe('failed')
  expect(stopped?.steps.slice(0, 3)).toEqual(['done', 'failed', 'pending'])
  await ui.unmount()
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

test('opening the board reads the workspace again when the first read left nothing', async ($, on) => {
  const ran: Ran[] = []
  fakeOrbit(on, ran)
  // The first snapshot write goes nowhere, as one made before the session is bound does.
  let isDropped = false
  on('state.set', ($, e, next) => {
    if (isDropped || e.key !== 'snapshot') return next(e)
    isDropped = true
    return { value: { isSet: true, version: 0 } }
  })
  const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
  await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
  await clock.advance(0)
  expect(isDropped).toBe(true)
  const reads = ran.filter(run => run.args[0] === 'task').length

  await clock.advance(6000)
  await $.command.run({ command: 'orbit-board', args: '' } as never)
  await clock.advance(0)

  expect(ran.filter(run => run.args[0] === 'task').length).toBeGreaterThan(reads)
  const ui = await $.ui.mount({ plugin: 'orbit', surface: 'terminal', ...PANE })
  await ui.press({ key: 'tab:board' })
  expect(await ui.find({ key: 'card:ORB-2' })).toBeDefined()
  await ui.unmount()
})

test('a failed read is retried from the band, and the board fills once it succeeds', async ($, on) => {
  const ran: Ran[] = []
  let isDown = true
  on('process.run', (_$, e) => {
    const args = e.argv.slice(4)
    ran.push({ args, isRemote: false })
    const reply = (stdout: unknown, exitCode = 0) => ({ value: { exitCode, stdout: JSON.stringify(stdout), stderr: exitCode === 0 ? '' : 'owner unreachable', isStdoutTruncated: false, isStderrTruncated: false } })
    if (args[0] === 'workspace') return reply({ registered: true, workspace: { name: 'demo' }, checkout: { role: 'owner' } })
    if (isDown) return reply(null, 1)
    return reply(args.includes('done') ? [] : TASKS)
  })
  stubSession(on)
  const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
  await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
  await clock.advance(0)

  isDown = false
  await clock.advance(31_000)
  const band = await $.ui.mount({ plugin: 'orbit', surface: 'terminal', ...BAND })
  await clock.advance(0)
  await band.unmount()

  const ui = await $.ui.mount({ plugin: 'orbit', surface: 'desktop', ...PANE })
  await ui.press({ key: 'tab:board' })
  expect(await ui.find({ key: 'card:ORB-2' })).toBeDefined()
  await ui.unmount()
})

test('the pane reads the workspace when it is drawn before session.start', async ($, on) => {
  const ran: Ran[] = []
  fakeOrbit(on, ran)
  on('session.cwd', () => ({ value: '/work/demo' }))
  const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })

  const first = await $.ui.mount({ plugin: 'orbit', surface: 'desktop', ...PANE })
  await clock.advance(0)
  await first.unmount()

  expect(ran.some(run => run.args[0] === 'task')).toBe(true)
  const ui = await $.ui.mount({ plugin: 'orbit', surface: 'desktop', ...PANE, props: { ...PANE.props, bodyColumns: undefined as never } })
  await ui.press({ key: 'tab:board' })
  expect(await ui.find({ key: 'card:ORB-2' })).toBeDefined()
  await ui.unmount()
})

test('an unregistered checkout reads the owner the federated MCP names when ownerHost is unset', async ($, on) => {
  const ran: string[][] = []
  on('process.run', (_$, e) => {
    ran.push([...e.argv])
    const reply = (stdout: string) => ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
    if (e.argv[0] === '/bin/sh' && (e.argv[2] ?? '').includes('mcp-destinations.toml')) return reply('[[destinations]]\nssh = "fed-box"\nmachine_id = "hm_1"\n')
    if (e.argv[0] === 'git') return reply('/work/demo\n')
    if (e.argv[0] === 'ssh') return reply(JSON.stringify((e.argv.at(-1) ?? '').includes("'done'") ? [] : TASKS))
    return reply(JSON.stringify({ registered: false, workspace: null, checkout: null }))
  })
  stubSession(on)
  const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
  await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
  await clock.advance(0)

  const remote = ran.filter(argv => argv[0] === 'ssh')
  expect(remote.length).toBeGreaterThan(0)
  expect(remote.every(argv => argv.includes('fed-box') && (argv.at(-1) ?? '').includes("'--workspace' 'demo'"))).toBe(true)
  const ui = await $.ui.mount({ plugin: 'orbit', surface: 'desktop', ...PANE })
  await ui.press({ key: 'tab:board' })
  expect(await ui.find({ key: 'card:ORB-2' })).toBeDefined()
  await ui.unmount()
})

test('board sections fold: Done starts shut, and a header press shuts or opens its section', async ($, on) => {
  fakeOrbit(on, [])
  const clock = mock.clock(on, { now: Date.parse('2026-10-03T10:05:00Z') })
  await $.session.start({ cwd: '/work/demo', source: 'startup' } as never)
  await clock.advance(0)

  for (const surface of SURFACES) {
    const ui = await $.ui.mount({ plugin: 'orbit', surface, ...PANE })
    await ui.press({ key: 'tab:board' })
    for (const section of ['blocked', 'in-progress', 'review', 'proposed', 'backlog', 'done']) expect(await ui.find({ key: `lane:${section}` })).toBeDefined()
    expect(await ui.find({ key: 'card:ORB-2' })).toBeDefined()
    await ui.press({ key: 'lane:backlog' })
    expect(await ui.find({ key: 'card:ORB-2' })).toBeUndefined()
    await ui.press({ key: 'lane:backlog' })
    expect(await ui.find({ key: 'card:ORB-2' })).toBeDefined()
    await ui.press({ key: 'card:ORB-1' })
    expect(await ui.find({ key: 'rescue' })).toBeDefined()
    await ui.press({ key: 'card:ORB-1' })
    expect(await ui.find({ key: 'rescue' })).toBeUndefined()
    await ui.unmount()
  }
})
