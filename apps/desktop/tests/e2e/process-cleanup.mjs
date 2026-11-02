import { execFile } from 'node:child_process'
import { setTimeout as delay } from 'node:timers/promises'
import { promisify } from 'node:util'

const exec = promisify(execFile)

async function processTable() {
  const { stdout } = await exec('ps', ['-axo', 'pid=,ppid=,pgid=,lstart='], {
    env: { ...process.env, LC_ALL: 'C' },
    maxBuffer: 8 * 1024 * 1024,
    timeout: 5_000,
  })
  return stdout.split('\n').flatMap((line) => {
    const fields = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.+?)\s*$/.exec(line)
    return fields
      ? [{ pid: +fields[1], parent: +fields[2], group: +fields[3], started: fields[4] }]
      : []
  })
}

function signal(pid, value) {
  try {
    process.kill(pid, value)
  } catch (error) {
    if (error.code !== 'ESRCH') throw error
  }
}

// The runner starts each test in its own process group. Playwright starts
// Electron in another group, so killing just the test's Node process leaks its
// browser/backend. Capture descendants before signalling, including those groups.
// Never select processes by name, executable or the user's application data.
export async function stopTestProcessTree(child, graceMs = 5_000) {
  if (!child.pid) return
  if (process.platform === 'win32') {
    if (child.exitCode !== null || child.signalCode !== null) return
    try {
      await exec('taskkill', ['/PID', String(child.pid), '/T', '/F'], {
        windowsHide: true,
        timeout: 10_000,
      })
    } catch (error) {
      if (child.exitCode === null && child.signalCode === null) throw error
    }
    return
  }
  const table = await processTable()
  const root = table.find((entry) => entry.pid === child.pid)
  if (!root) return
  const owned = new Map([[root.pid, root]])
  let changed = true
  while (changed) {
    changed = false
    for (const entry of table) {
      if (owned.has(entry.parent) && !owned.has(entry.pid)) {
        owned.set(entry.pid, entry)
        changed = true
      }
    }
  }
  const groups = new Set(
    [...owned.values()].filter((entry) => entry.group === entry.pid).map((entry) => entry.group),
  )
  const send = (entries, value) => {
    const sent = new Set()
    for (const entry of entries) {
      const target = groups.has(entry.group) ? -entry.group : entry.pid
      if (!sent.has(target)) signal(target, value)
      sent.add(target)
    }
  }
  const remaining = async () =>
    (await processTable()).filter((entry) => {
      const previous = owned.get(entry.pid)
      // PID reuse must never kill an unrelated process after the grace period.
      return previous?.started === entry.started && previous.group === entry.group
    })
  send([...owned.values()], 'SIGTERM')
  const deadline = Date.now() + graceMs
  let live
  do {
    live = await remaining()
    if (!live.length) return
    if (Date.now() >= deadline) break
    await delay(Math.min(100, deadline - Date.now()))
  } while (true)
  send(live, 'SIGKILL')
}
