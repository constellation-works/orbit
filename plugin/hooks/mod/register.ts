import type { EngineInterface, Register } from 'claude-code'
import { createLinkifier } from './linkify'

const LOCAL_DASHBOARD = 'http://localhost:7878'
const STARTUP_TIMEOUT_MS = 2000

// Claude follows engine calls into functions declared beside the hook.
async function readOrbit($: EngineInterface, cwd: string, args: string[]): Promise<unknown> {
  try {
    const command = ['/bin/sh', '-c', 'PATH="$HOME/.orbit/bin:$PATH" exec orbit "$@"', 'orbit', ...args]
    const result = await $.process.run(command, { cwd, timeoutMs: STARTUP_TIMEOUT_MS })
    if (result.exitCode === 0 && !result.isStdoutTruncated) return JSON.parse(result.stdout)
  } catch {
    // An older or unavailable CLI must leave the conversation usable.
  }
  return null
}

function taskPrefixes(registry: unknown): string[] {
  const hosts = (registry as { hosts?: unknown } | null)?.hosts
  if (!Array.isArray(hosts)) return []

  return hosts
    .map(host => host?.task_prefix)
    .filter((prefix): prefix is string => typeof prefix === 'string')
}

function dashboardOrigin(config: unknown): string {
  const value = (config as { value?: unknown } | null)?.value
  // Orbit normalizes this setting to an HTTP(S) origin without a trailing slash.
  if (typeof value === 'string' && /^https?:\/\/[^\s/?#@<>"\\]+\/?$/.test(value)) {
    return value.replace(/\/$/, '')
  }
  return LOCAL_DASHBOARD
}

export const register: Register = on => {
  let linkify = createLinkifier([], LOCAL_DASHBOARD)

  on('session.start', async ($, event, next) => {
    const started = await next(event)
    const [registry, config] = await Promise.all([
      readOrbit($, event.cwd, ['host', 'list', '--no-probe', '--json']),
      readOrbit($, event.cwd, ['config', 'get', 'machine.web.public_url', '--scope', 'global', '--json']),
    ])
    linkify = createLinkifier(taskPrefixes(registry), dashboardOrigin(config))
    return started
  })

  on('ui.render', { component: 'AssistantMessage' }, (_engine, event, next) => {
    const text = linkify(event.props.text)
    return text === event.props.text ? next(event) : next({ ...event, props: { ...event.props, text } })
  })
}
