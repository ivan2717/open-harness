import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { accessSync, constants, copyFileSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = path.dirname(fileURLToPath(import.meta.url));
if (process.platform !== 'darwin' || !['arm64', 'x64'].includes(process.arch)) {
  throw new Error('Computer Use helper requires macOS 14 or later on arm64 or x86_64.');
}
const args = process.argv.slice(2);
if (args.length && (args.length !== 2 || args[0] !== '--output' || !path.isAbsolute(args[1]))) throw new Error('Use --output with an absolute staging directory.');
const bin = args[1] ?? path.join(root, 'bin');
const output = path.join(bin, 'computer-helper');
const app = path.join(bin, 'Open Harness Computer Use.app');
const appExecutable = path.join(app, 'Contents', 'MacOS', 'OpenHarnessComputerUse');
const stamp = path.join(bin, 'build-stamp.json');
const sources = ['Main', 'Access', 'Input', 'IPC'].map(name => `native/${name}.swift`);
const manifest = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>dev.openharness.computeruse</string>
<key>CFBundleName</key><string>Open Harness Computer Use</string>
<key>CFBundleDisplayName</key><string>Open Harness Computer Use</string>
<key>CFBundleExecutable</key><string>OpenHarnessComputerUse</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>CFBundleShortVersionString</key><string>0.2.0</string>
<key>LSMinimumSystemVersion</key><string>14.0</string>
<key>LSUIElement</key><true/>
</dict></plist>\n`;
const target = `${process.arch === 'arm64' ? 'arm64' : 'x86_64'}-apple-macosx14.0`;
function probe(args) {
  const result = spawnSync('xcrun', args, { encoding: 'utf8', maxBuffer: 64 * 1024 });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Unable to inspect the Swift toolchain: ${result.stderr.trim()}`);
  return result.stdout.trim();
}
const toolchain = probe(['swiftc', '--version']);
const sdk = probe(['--sdk', 'macosx', '--show-sdk-version']);
const compiler = probe(['--find', 'swiftc']);
const digest = createHash('sha256');
digest.update(JSON.stringify({ format: 2, arch: process.arch, target, toolchain, sdk, compiler }));
for (const relative of ['build.mjs', ...sources]) {
  digest.update(`\0${relative}\0`);
  digest.update(readFileSync(path.join(root, relative)));
}
const inputsHash = digest.digest('hex');
const fileHash = file => createHash('sha256').update(readFileSync(file)).digest('hex');
let cached = false;
try {
  const saved = JSON.parse(readFileSync(stamp, 'utf8'));
  accessSync(output, constants.X_OK);
  cached = statSync(output).isFile() && saved.inputs_hash === inputsHash && saved.binary_hash === fileHash(output)
    && saved.app_binary_hash === fileHash(appExecutable)
    && readFileSync(path.join(app, 'Contents', 'Info.plist'), 'utf8') === manifest
    && spawnSync('/usr/bin/codesign', ['--verify', '--strict', app], { stdio: 'ignore' }).status === 0;
} catch { /* Missing, corrupt or stale cache is rebuilt without trusting its stamp. */ }

if (cached) {
  console.log('Computer Use app and test helper are unchanged; preserving their existing bytes and signatures.');
} else {
  mkdirSync(bin, { recursive: true });
  const temporary = mkdtempSync(path.join(tmpdir(), 'openharness-swift-'));
  // Keep the output basename stable for the compiler's ad-hoc signing identity.
  const candidate = path.join(temporary, 'computer-helper');
  try {
    const result = spawnSync('xcrun', ['swiftc', '-module-cache-path', path.join(temporary, 'modules'), '-parse-as-library', '-O', '-target', target, ...sources.map(source => path.join(root, source)), '-o', candidate], { stdio: 'inherit' });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`Swift helper compilation failed (${result.status ?? 'signal'}). The previous helper was preserved.`);
    const candidateApp = path.join(temporary, path.basename(app));
    const candidateExecutable = path.join(candidateApp, 'Contents', 'MacOS', 'OpenHarnessComputerUse');
    mkdirSync(path.dirname(candidateExecutable), { recursive: true });
    copyFileSync(candidate, candidateExecutable);
    writeFileSync(path.join(candidateApp, 'Contents', 'Info.plist'), manifest);
    for (const codesignArgs of [['--force', '--sign', '-', '--identifier', 'dev.openharness.computeruse', candidateApp], ['--verify', '--strict', candidateApp]]) {
      const signed = spawnSync('/usr/bin/codesign', codesignArgs, { encoding: 'utf8' });
      if (signed.error) throw signed.error;
      if (signed.status !== 0) throw new Error(`Computer Use app signature failed: ${signed.stderr}`);
    }
    const metadata = { inputs_hash: inputsHash, binary_hash: fileHash(candidate), app_binary_hash: fileHash(candidateExecutable), arch: process.arch, target, toolchain, sdk };
    const stagedStamp = path.join(temporary, 'build-stamp.json');
    writeFileSync(stagedStamp, `${JSON.stringify(metadata, null, 2)}\n`);
    rmSync(app, { recursive: true, force: true });
    renameSync(candidateApp, app);
    renameSync(candidate, output);
    renameSync(stagedStamp, stamp);
    console.log('Built macOS Open Harness Computer Use.app and isolated test helper. No app was launched or system permission requested.');
  } finally { rmSync(temporary, { recursive: true, force: true }); }
}
