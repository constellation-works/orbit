// The pure rules the mod's hooks lean on.

import { expect, test } from 'claude-code/testing'

import { federatedHosts, remoteArgv } from '../cli'
import { withTaskTrailer } from '../model'

test('a commit gains the task trailer once, and only a commit', () => {
  const rewritten = withTaskTrailer(`git commit -m "fix"`, 'ORB-7')
  expect(rewritten).not.toBeNull()
  expect(withTaskTrailer(rewritten ?? '', 'ORB-7')).toBeNull()
  expect(withTaskTrailer('git -C repo commit -m x', 'ORB-7')).not.toBeNull()
  expect(withTaskTrailer('git commit --amend --no-edit', 'ORB-7')).toBeNull()
  expect(withTaskTrailer('git status', 'ORB-7')).toBeNull()
})

test('remote arguments reach the owner as one quoted word each', () => {
  const argv = remoteArgv('box', ['task', 'list', "it's; rm -rf ~"])
  expect(argv.slice(0, 7)).toEqual(['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5', '--', 'box'])
  expect(argv.at(-1)?.endsWith(`'it'\\''s; rm -rf ~'`)).toBe(true)
})

test('federated destinations come from each ssh line, refusing option-shaped hosts', () => {
  const toml = '[[destinations]]\nssh = "dk-server-2"\nmachine_id = "hm_1"\n\n[[destinations]]\nssh = "-oProxyCommand=x"\n\n[[destinations]]\n  ssh = "daniel@box.ts.net" # tailnet\n'
  expect(federatedHosts(toml)).toEqual(['dk-server-2', 'daniel@box.ts.net'])
  expect(federatedHosts('')).toEqual([])
})
