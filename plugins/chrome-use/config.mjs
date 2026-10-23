import { readFile, lstat, mkdir } from 'node:fs/promises';
import path from 'node:path';
import { EXTENSION_ID } from './constants.mjs';
export async function readConfig(configPath = process.env.OPENHARNESS_CHROME_CONFIG) {
  if (!configPath || !path.isAbsolute(configPath)) throw new Error('Prepare the Chrome connection in Open Harness Extensions first.');
  const [stat, dir] = await Promise.all([lstat(configPath), lstat(path.dirname(configPath))]);
  if (!stat.isFile() || stat.isSymbolicLink() || !dir.isDirectory() || dir.isSymbolicLink()) throw new Error('Chrome bridge configuration must be a regular private file.');
  if (process.platform !== 'win32' && ((stat.mode & 0o077) || (dir.mode & 0o077) || stat.uid !== process.getuid() || dir.uid !== process.getuid())) throw new Error('Chrome bridge configuration permissions must be 0600 in a 0700 directory owned by this user.');
  const config = JSON.parse(await readFile(configPath, 'utf8'));
  if (config.version !== 1 || config.extensionId !== EXTENSION_ID || !/^[a-f0-9]{64}$/.test(config.token) || typeof config.socketPath !== 'string' || !path.isAbsolute(config.socketPath) || config.socketPath.length > 100) throw new Error('Invalid Chrome bridge configuration.');
  await mkdir(path.dirname(config.socketPath), { mode: 0o700, recursive: true });
  const socketDir = await lstat(path.dirname(config.socketPath));
  if (!socketDir.isDirectory() || socketDir.isSymbolicLink() || (socketDir.mode & 0o077) || socketDir.uid !== process.getuid()) throw new Error('Chrome bridge socket directory must be private (0700), owned by this user, and not a symlink.');
  return config;
}
