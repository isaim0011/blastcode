#!/usr/bin/env node
const path = require('path');
const os = require('os');
const { spawn } = require('child_process');

const exe = os.platform() === 'win32' ? 'blast.exe' : 'blast';
const binPath = path.join(__dirname, exe);

const child = spawn(binPath, process.argv.slice(2), {
  stdio: 'inherit',
  windowsHide: true,
});

child.on('exit', (code) => {
  process.exit(code || 0);
});

child.on('error', (err) => {
  console.error(`[blastcode] Failed to execute ${binPath}:`, err.message);
  process.exit(1);
});
