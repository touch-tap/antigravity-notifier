#!/usr/bin/env node
// Runs the native codex-notifier binary from the platform package npm installed beside this one.
// Under npx that package lives in a cache that can be cleared, so CODEX_NOTIFIER_EPHEMERAL tells
// `codex-notifier pair` / `codex-notifier setup` to copy the binary somewhere permanent before writing the hooks.
'use strict';
const { spawnSync } = require('child_process');
const path = require('path');

const pkg = `@touchtap/codex-notifier-${process.platform}-${process.arch}`;
const exe = process.platform === 'win32' ? 'codex-notifier.exe' : 'codex-notifier';
let binary;
try {
  binary = require.resolve(`${pkg}/bin/${exe}`);
} catch {
  console.error(`codex-notifier: no build for ${process.platform}-${process.arch} (looked for ${pkg}).`);
  console.error('See https://codexnotifier.com/cli for other ways to install.');
  process.exit(1);
}

const ephemeral = binary.split(path.sep).includes('_npx');
const result = spawnSync(binary, process.argv.slice(2), {
  stdio: 'inherit',
  env: ephemeral ? { ...process.env, CODEX_NOTIFIER_EPHEMERAL: '1' } : process.env,
});
if (result.error) {
  console.error(`codex-notifier: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
