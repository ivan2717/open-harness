import { cp, mkdir } from 'node:fs/promises'
import { resolve } from 'node:path'
import { verifiedDownload } from './workspace-runtime-bundle.mjs'
import { prepareWindowsNode } from './workspace-runtime-node-windows.mjs'
import { verifyNodeBinary } from './workspace-runtime-node.mjs'

const [cache, destination] = process.argv.slice(2)
if (!cache || !destination) throw new Error('Usage: build-windows-node.mjs CACHE DESTINATION')
const payload = await prepareWindowsNode({ cache: resolve(cache), downloadFile: verifiedDownload })
verifyNodeBinary(payload, 'win32', 'x64')
await mkdir(resolve(destination), { recursive: true })
await cp(payload, resolve(destination), { recursive: true })
console.log(`AppContainer-compatible Node staged at ${resolve(destination)}`)
