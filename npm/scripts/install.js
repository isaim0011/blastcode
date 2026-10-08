const fs = require('fs');
const path = require('path');
const os = require('os');
const https = require('https');
const { execSync } = require('child_process');

const VERSION = 'v0.1.0';
const BIN_DIR = path.join(__dirname, '..', 'bin');
const EXE = os.platform() === 'win32' ? 'blast.exe' : 'blast';
const TARGET_PATH = path.join(BIN_DIR, EXE);

function getAssetInfo() {
  const platform = os.platform();
  const arch = os.arch();

  if (platform === 'linux' && arch === 'x64') {
    return { name: 'blast-linux-x86_64.tar.gz', format: 'tar' };
  }
  if (platform === 'darwin' && arch === 'arm64') {
    return { name: 'blast-macos-arm64.tar.gz', format: 'tar' };
  }
  if (platform === 'darwin' && arch === 'x64') {
    return { name: 'blast-macos-x86_64.tar.gz', format: 'tar' };
  }
  if (platform === 'win32' && arch === 'x64') {
    return { name: 'blast-windows-x64.zip', format: 'zip' };
  }
  return null;
}

function download(url, dest) {
  return new Promise((resolve, reject) => {
    https.get(url, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        return download(res.headers.location, dest).then(resolve).catch(reject);
      }
      if (res.statusCode !== 200) {
        return reject(new Error(`Download failed: HTTP ${res.statusCode}`));
      }
      const file = fs.createWriteStream(dest);
      res.pipe(file);
      file.on('finish', () => file.close(resolve));
    }).on('error', reject);
  });
}

async function main() {
  const asset = getAssetInfo();
  if (!asset) {
    console.warn(`[blastcode] Prebuilt binary not available for ${os.platform()} ${os.arch()}. You can install via 'cargo install blastcode'.`);
    return;
  }

  const url = `https://github.com/isaim0011/blastcode/releases/download/${VERSION}/${asset.name}`;
  const archivePath = path.join(BIN_DIR, asset.name);

  try {
    if (!fs.existsSync(BIN_DIR)) fs.mkdirSync(BIN_DIR, { recursive: true });
    console.log(`[blastcode] Downloading native binary from GitHub Releases (${asset.name})...`);
    await download(url, archivePath);

    if (asset.format === 'tar') {
      execSync(`tar -xzf "${archivePath}" -C "${BIN_DIR}"`);
      fs.unlinkSync(archivePath);
    } else if (asset.format === 'zip') {
      if (os.platform() === 'win32') {
        execSync(`powershell -Command "Expand-Archive -Path '${archivePath}' -DestinationPath '${BIN_DIR}' -Force"`);
      }
      fs.unlinkSync(archivePath);
    }

    if (fs.existsSync(TARGET_PATH)) {
      if (os.platform() !== 'win32') {
        fs.chmodSync(TARGET_PATH, 0o755);
      }
      console.log(`[blastcode] Binary installed successfully at ${TARGET_PATH}`);
    }
  } catch (err) {
    console.warn(`[blastcode] Warning: Auto-download failed (${err.message}). If using source, compile with cargo build.`);
  }
}

main();
