// The pure rules the mod's hooks lean on.

import { expect, test } from 'claude-code/testing'

import { remoteArgv } from '../cli'
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
