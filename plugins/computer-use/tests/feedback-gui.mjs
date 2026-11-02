import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

if (process.platform !== 'darwin') throw new Error('Use feedback.test.mjs on Windows.');
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const temporary = mkdtempSync(path.join(tmpdir(), 'openharness-feedback-gui-'));
try {
  const binary = path.join(temporary, 'feedback-test');
  execFileSync('xcrun', ['swiftc', '-parse-as-library', '-module-cache-path', path.join(temporary, 'modules'),
    ...['Access', 'Windows', 'Observation', 'VisualGuard', 'Input', 'Feedback', 'FeedbackCursor', 'FeedbackPreview'].map(name => path.join(root, `native/${name}.swift`)),
    path.join(root, 'tests/FeedbackTests.swift'), '-o', binary], { stdio: 'inherit', timeout: 120_000 });
  execFileSync(binary, [], { stdio: 'inherit', timeout: 30_000 });
} finally { rmSync(temporary, { recursive: true, force: true }); }
