#!/usr/bin/env node
// Runs the native antigravity-notifier binary from the platform package npm installed beside this one.
// Under npx that package lives in a cache that can be cleared, so ANTIGRAVITY_NOTIFIER_EPHEMERAL tells
// `antigravity-notifier pair` / `antigravity-notifier setup` to copy the binary somewhere permanent before writing the hooks.
'use strict';
const { spawnSync } = require('child_process');
const path = require('path');

const pkg = `@touchtap/antigravity-notifier-${process.platform}-${process.arch}`;
const exe = process.platform === 'win32' ? 'antigravity-notifier.exe' : 'antigravity-notifier';
let binary;
try {
  binary = require.resolve(`${pkg}/bin/${exe}`);
} catch {
  console.error(`antigravity-notifier: no build for ${process.platform}-${process.arch} (looked for ${pkg}).`);
  console.error('See https://antigravitynotifier.com/cli for other ways to install.');
  process.exit(1);
}

const ephemeral = binary.split(path.sep).includes('_npx');
const result = spawnSync(binary, process.argv.slice(2), {
  stdio: 'inherit',
  env: ephemeral ? { ...process.env, ANTIGRAVITY_NOTIFIER_EPHEMERAL: '1' } : process.env,
});
if (result.error) {
  console.error(`antigravity-notifier: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
