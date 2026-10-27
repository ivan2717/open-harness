// Run with the desktop Electron executable. The existing model vault is read
// only; decrypted credentials stay in this process and the local API request.
const { app, safeStorage } = require('electron')
const { readFile } = require('node:fs/promises')
const { createInterface } = require('node:readline')
const { pathToFileURL } = require('node:url')
const { join } = require('node:path')

app.setName('Open Harness')
if (process.env.OPENHARNESS_EVAL_USER_DATA) app.setPath('userData', process.env.OPENHARNESS_EVAL_USER_DATA)
app.whenReady().then(async () => {
  if (!safeStorage.isEncryptionAvailable()) throw new Error('OS credential encryption is unavailable')
  const saved = JSON.parse(safeStorage.decryptString(await readFile(process.env.OPENHARNESS_EVAL_VAULT)))
  if (!Array.isArray(saved)) throw new Error('Invalid model vault')
  console.log(JSON.stringify({event:'models', models:saved.map(({id,input}) => ({id,model:input.model,protocol:input.protocol,host:new URL(input.base_url).hostname,reasoning_mode:input.reasoning_mode,reasoning_effort:input.reasoning_effort,context_window:input.context_window}))}))
  const lines = createInterface({input:process.stdin})
  for await (const line of lines) {
    if (!line.trim()) continue
    const options = JSON.parse(line)
    if (options.action === 'quit') break
    const selected = saved.find(({id}) => id === options.model_id)
    if (!selected) throw new Error('Select an existing model id')
    const runner = options.action === 'continuity' ? 'continuity.mjs' : 'live.mjs'
    const {runEvaluation} = await import(pathToFileURL(join(__dirname, runner)).href)
    await runEvaluation({...options, configuration:selected.input})
    break
  }
  app.quit()
}).catch(() => {
  // Never print exception payloads that might include decrypted configuration.
  console.error('Evaluation could not start or complete; inspect the redacted evaluation logs.')
  app.exit(1)
})
