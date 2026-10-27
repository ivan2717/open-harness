import { readFile, realpath } from 'node:fs/promises'
import { isAbsolute, join, relative, resolve, basename } from 'node:path'

const parsed = text => { try { return JSON.parse(text) } catch { return null } }
const successful = run => (run.messages ?? []).filter(message => message.role === 'tool'
  && message.is_error === false && message.status === 'complete')

// Bind the source to the host-returned run and message IDs. A correct parent
// alias is equivalent to a raw ID; a plausible quote from another run is not.
export function childReadCorrection(children, rootId, sequence, correction) {
  if (!rootId || !Number.isInteger(sequence)) return false
  return children.some(run => successful(run).some(message => {
    if (message.tool_name !== 'agent_control') return false
    const output = parsed(message.text)
    if (output?.agent_id !== rootId) return false
    const messages = Array.isArray(output.messages) ? output.messages : [{
      id: output.message_id, role: output.role, text: output.text,
    }]
    return messages.some(source => source.id === `event-${sequence}` && source.role === 'user'
      && source.truncated !== true && source.text === correction)
  }))
}

export function correctionCited(evidence, sequence) {
  const text = JSON.stringify(evidence ?? '')
  const source = text.includes('CORRECTION-ORCHID') || (Number.isInteger(sequence)
    && new RegExp(`(?:event-|#|(?:序号|sequence)\\s*[:：]?\\s*)${sequence}(?![\\w-])`, 'i').test(text))
  return source && /R303/.test(text) && /settled|已结算/.test(text)
}

// Deliberately recognize only a direct interpreter invocation. Shell fragments,
// inline code and mere mentions in echo commands require manual review rather
// than receiving an automatic script-execution pass.
function invocation(command) {
  if (typeof command !== 'string') return null
  const words = [], pattern = /\s*(?:"([^"]*)"|'([^']*)'|([^\s"']+))/gy
  let offset = 0
  for (;;) {
    pattern.lastIndex = offset
    const match = pattern.exec(command)
    if (!match) break
    const word = match[1] ?? match[2] ?? match[3]
    if (/[;|<>`\r\n]|\$\(/.test(word)) return null
    words.push(word)
    offset = pattern.lastIndex
  }
  if (command.slice(offset).trim()) return null
  if (words[0] === '&') words.shift() // PowerShell's explicit call operator.
  if (words.some(word => word.includes('&'))) return null
  const executable = basename((words.shift() ?? '').replaceAll('\\', '/'))
  if (!/^(?:node|python\d*(?:\.\d+)?|bash|sh|zsh|pwsh|powershell)(?:\.exe)?$/i.test(executable)) return null
  for (const word of words) {
    if (['-c', '-m', '-e', '--eval', '-Command', '-EncodedCommand'].includes(word)) return null
    if (word.startsWith('-')) continue
    return /\.(?:[cm]?js|py|sh|ps1)$/i.test(word) ? word : null
  }
  return null
}

export async function childExecutedScript(children, reviews, workspace) {
  workspace = await realpath(workspace)
  const scratch = await realpath(join(workspace, '.openharness-tmp')).catch(() => null)
  if (!scratch) return { passed: false, reason: 'No temporary script directory' }
  for (const child of children) {
    const review = reviews.find(item => item.run_id === child.id)?.review
    if (!review || review.review_incomplete) continue
    for (const record of review.commands ?? []) {
      if (record.status !== 'completed' || record.exit_code !== 0) continue
      const script = invocation(record.command)
      if (!script) continue
      const target = await realpath(resolve(workspace, script)).catch(() => null)
      if (!target) continue
      const path = relative(scratch, target)
      if (!path || path === '..' || path.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) || isAbsolute(path)) continue
      const content = await readFile(target).catch(() => null)
      if (!content?.length) continue
      const observed = successful(child).some(message => {
        if (!['run_command', 'manage_command'].includes(message.tool_name)) return false
        const output = parsed(message.text)
        return output?.command_id === record.id && output.status === 'completed' && output.exit_code === 0
      })
      if (observed) return { passed: true, run_id: child.id, command_id: record.id, script: relative(workspace, target) }
    }
  }
  return { passed: false, reason: 'No child-owned completed command and matching tool result for an existing temporary script' }
}
