import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { setTimeout as delay } from 'node:timers/promises'
import { test } from 'node:test'
import { stopTestProcessTree } from './process-cleanup.mjs'

test(
  'timeout cleanup kills detached descendants without touching a sibling process',
  {
    skip: process.platform === 'win32',
    timeout: 10_000,
  },
  async () => {
    const descendant = `
    process.on('SIGTERM', () => {});
    console.log('READY ' + process.pid);
    setInterval(() => {}, 1000);
  `
    const script = `
    const { spawn } = require('node:child_process');
    process.on('SIGTERM', () => {});
    spawn(process.execPath, ['-e', ${JSON.stringify(descendant)}], {
      detached: true, stdio: ['ignore', 'inherit', 'inherit']
    });
    setInterval(() => {}, 1000);
  `
    const sibling = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {
      detached: true,
      stdio: 'ignore',
    })
    const child = spawn(process.execPath, ['-e', script], {
      detached: true,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    let descendantPid
    const exited = once(child, 'exit')
    try {
      const [ready] = await once(child.stdout, 'data')
      descendantPid = Number(/READY (\d+)/.exec(ready.toString())?.[1])
      assert(descendantPid)
      await stopTestProcessTree(child, 100)
      await exited
      for (let attempt = 0; attempt < 30; attempt++) {
        try {
          process.kill(descendantPid, 0)
        } catch (error) {
          if (error.code === 'ESRCH') {
            descendantPid = undefined
            break
          }
          throw error
        }
        await delay(50)
      }
      assert.equal(descendantPid, undefined, 'detached Electron-like process must be gone')
      assert.doesNotThrow(() => process.kill(sibling.pid, 0))
    } finally {
      for (const pid of [child.pid, descendantPid, sibling.pid]) {
        if (!pid) continue
        try {
          process.kill(pid, 'SIGKILL')
        } catch (error) {
          if (error.code !== 'ESRCH') throw error
        }
      }
    }
  },
)
