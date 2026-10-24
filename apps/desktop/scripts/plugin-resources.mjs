import { access, copyFile, cp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { basename, relative, resolve, sep } from 'node:path'

/** Stage only bundled plugins, preserving source manifests for other platforms. */
export async function stagePluginResources({ source, destination, platform, windowsLauncher }) {
  if (platform === 'win32') {
    await access(windowsLauncher)
    await access(resolve(source, 'computer-use/windows/bin/OpenHarnessComputerUse.exe'))
  }
  await mkdir(destination, { recursive: true })
  for (const [name, server] of [
    ['browser-use', 'browser'],
    ['chrome-use', 'chrome'],
    ['computer-use', 'computer'],
  ]) {
    const target = resolve(destination, name)
    await rm(target, { recursive: true, force: true })
    if (name === 'computer-use' && !['darwin', 'win32'].includes(platform)) continue
    const pluginSource = resolve(source, name)
    await cp(pluginSource, target, {
      recursive: true,
      filter: (path) => {
        const filename = basename(path)
        if (['tests', 'dist', 'node_modules', '.DS_Store', 'obj'].includes(filename)) return false
        if (filename.startsWith('ComputerUseFixture.') || filename.endsWith('.pdb')) return false
        const first = relative(pluginSource, path).split(sep)[0]
        if (name === 'computer-use' && first === 'windows' && platform !== 'win32') return false
        if (name === 'computer-use' && first === 'bin' && platform !== 'darwin') return false
        return true
      },
    })
    if (platform === 'win32') {
      await copyFile(windowsLauncher, resolve(target, 'launch.exe'))
      const manifestPath = resolve(target, 'mcp.json')
      const manifest = JSON.parse(await readFile(manifestPath, 'utf8'))
      manifest.mcpServers[server].command = './launch.exe'
      manifest.mcpServers[server].args = ['--mcp']
      await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`)
    }
  }
}
