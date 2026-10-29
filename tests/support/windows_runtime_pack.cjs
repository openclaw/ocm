'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const cp = require('node:child_process');
const {createHash, randomUUID} = require('node:crypto');

assert.equal(process.platform, 'win32', 'This proof requires native Windows');
const binary = path.resolve(process.argv[2]);
const root = path.resolve(process.argv[3]);
assert.equal(path.basename(path.dirname(root)), 'ocm-tests');
assert.ok(fs.statSync(binary).isFile());

function readRecord(file) {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')); }
  catch (error) {
    if (error.code === 'ENOENT' || error instanceof SyntaxError) return undefined;
    throw error;
  }
}

async function main() {
  const repo = path.join(root, 'repo & %PATH% !literal! 雪');
  const nonce = randomUUID();
  const capture = name => path.join(root, `pack-${nonce}.${name}`);
  const publish = (file, value) => {
    fs.writeFileSync(`${file}.tmp`, JSON.stringify(value), {flag: 'wx'});
    fs.renameSync(`${file}.tmp`, file);
  };
  const beat = path.join(root, 'heartbeat');
  const bootstrap = path.join(root, 'bootstrap.cjs');
  fs.mkdirSync(repo);
  fs.writeFileSync(path.join(root, 'capture-nonce.tmp'), nonce, {flag: 'wx'});
  fs.renameSync(path.join(root, 'capture-nonce.tmp'), path.join(root, 'capture-nonce'));
  fs.writeFileSync(path.join(root, 'bootstrap.cjs'), `
const fs = require('node:fs');
const path = require('node:path');
const identity = {nonce: process.env.PACK_CAPTURE_NONCE, pid: process.pid};
const capture = name => path.join(process.env.PACK_CAPTURE_ROOT, 'pack-' + identity.nonce + '.' + name);
const read = file => {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')); }
  catch (error) {
    if (error.code === 'ENOENT' || error instanceof SyntaxError) return undefined;
    throw error;
  }
};
const publish = (file, value) => {
  fs.writeFileSync(file + '.tmp', JSON.stringify(value), {flag: 'wx'});
  fs.renameSync(file + '.tmp', file);
};
function gateBootstrap() {
  let first = false;
  try {
    fs.closeSync(fs.openSync(capture('bootstrap.claim'), 'wx'));
    first = true;
  } catch (error) {
    if (error.code !== 'EEXIST') throw error;
  }
  if (first) publish(capture('bootstrap.ready'), identity);
  const deadline = Date.now() + 30000;
  const sleeper = new Int32Array(new SharedArrayBuffer(4));
  let proof;
  while (true) {
    if (first && proof === undefined) {
      const challenge = read(capture('bootstrap.challenge'));
      if (challenge?.nonce === identity.nonce && challenge.pid === identity.pid &&
          typeof challenge.proof === 'string' && challenge.proof.length > 0) {
        proof = challenge.proof;
        publish(capture('bootstrap.pinned'), {...identity, proof});
      }
    }
    const pinned = first ? {...identity, proof} : read(capture('bootstrap.pinned'));
    const release = read(capture('bootstrap.release'));
    if (pinned?.nonce === identity.nonce && Number.isInteger(pinned.pid) && pinned.pid > 0 &&
        typeof pinned.proof === 'string' && pinned.proof.length > 0 &&
        release?.nonce === identity.nonce && release.pid === pinned.pid &&
        release.proof === pinned.proof) return;
    if (Date.now() >= deadline) throw new Error('Timed out waiting for the packing bootstrap capture');
    Atomics.wait(sleeper, 0, 0, 20);
  }
}
gateBootstrap();
`);
  fs.writeFileSync(path.join(repo, 'package.json'), JSON.stringify({
    name: 'openclaw', version: '2026.4.25', bin: {openclaw: 'openclaw.mjs'},
    engines: {node: '>=22.22.3'}, files: ['openclaw.mjs'],
    scripts: {postpack: 'node postpack.cjs'},
  }));
  fs.writeFileSync(path.join(repo, 'openclaw.mjs'), 'console.log("fixture");');
  fs.writeFileSync(path.join(repo, 'postpack.cjs'), `
const fs = require('node:fs');
if (process.env.PACK_FAIL === '1') process.exit(23);
if (process.env.PACK_BEAT) {
  const path = require('node:path');
  const identity = {pid: process.pid, nonce: process.env.PACK_CAPTURE_NONCE};
  const capture = name => path.join(process.env.PACK_CAPTURE_ROOT, 'pack-' + identity.nonce + '.' + name);
  const read = file => {
    try { return JSON.parse(fs.readFileSync(file, 'utf8')); }
    catch (error) {
      if (error.code === 'ENOENT' || error instanceof SyntaxError) return undefined;
      throw error;
    }
  };
  const publish = (file, value) => {
    fs.writeFileSync(file + '.tmp', JSON.stringify(value), {flag: 'wx'});
    fs.renameSync(file + '.tmp', file);
  };
  publish(capture('postpack.ready'), identity);
  const deadline = Date.now() + 120000;
  let held = false;
  let proof;
  setInterval(() => {
    if (Date.now() >= deadline) throw new Error('Timed out waiting for the packing fixture');
    if (held) {
      if (read(capture('postpack.exit'))?.nonce === identity.nonce) process.exit(0);
      return;
    }
    const challenge = read(capture('postpack.challenge'));
    if (proof === undefined && challenge?.nonce === identity.nonce &&
        challenge.pid === process.pid && typeof challenge.proof === 'string' &&
        challenge.proof.length > 0) {
      proof = challenge.proof;
      publish(capture('postpack.pinned'), {...identity, proof});
    }
    const request = read(capture('postpack.done'));
    if (request?.nonce === identity.nonce) {
      const release = read(capture('postpack.release'));
      if (request.hold && proof !== undefined && release?.nonce === identity.nonce &&
          release.pid === process.pid && release.proof === proof) {
        held = true;
        publish(capture('postpack.held'), identity);
        return;
      }
      process.exit(0);
    }
    fs.writeFileSync(process.env.PACK_BEAT, String(Date.now()));
  }, 50);
}
`);
  const env = {};
  for (const key of ['PATH', 'PATHEXT', 'SystemRoot', 'WINDIR', 'COMSPEC']) {
    const entry = Object.entries(process.env).find(([name]) => name.toUpperCase() === key.toUpperCase());
    if (entry) env[key] = entry[1];
  }
  const npm = cp.execFileSync('where.exe', ['npm.cmd'], {encoding: 'utf8'})
    .trim().split(/\r?\n/)[0];
  // This audited launcher keeps its prefix shell waiting at the first preload gate.
  const launcherHash = createHash('sha256')
    .update(fs.readFileSync(npm, 'utf8').replace(/\r\n/g, '\n')).digest('hex');
  assert.equal(launcherHash, 'da2a1e1b9b3ba4c979618f51d63b4c5e164336050bcecc946c9b002b4bc4541b',
    'Native packing fixture requires the audited stock npm.cmd bootstrap; ' +
    'the selected launcher changed and needs source requalification: ' + npm);
  console.log(JSON.stringify({npmLauncher: npm, launcherHash, nodeVersion: process.version}));
  Object.assign(env, {
    HOME: path.join(root, 'home'), USERPROFILE: path.join(root, 'home'),
    OCM_HOME: path.join(root, 'ocm-home'), OCM_INTERNAL_SKIP_SERVICE_READINESS: '1',
    TEMP: root, TMP: root,
    APPDATA: path.join(root, 'appdata'), LOCALAPPDATA: path.join(root, 'localappdata'),
    npm_config_cache: path.join(root, 'cache'), npm_config_offline: 'true',
    OCM_INTERNAL_NPM_BIN: npm,
    PACK_BEAT: beat, PACK_CAPTURE_NONCE: nonce, PACK_CAPTURE_ROOT: root,
  });
  fs.mkdirSync(env.HOME, {recursive: true});
  fs.mkdirSync(env.OCM_HOME, {recursive: true});
  fs.writeFileSync(path.join(root, 'npm-bin'), env.OCM_INTERNAL_NPM_BIN);
  const log = fs.openSync(path.join(root, 'pack.log'), 'w');
  const child = cp.spawn(binary, ['runtime', 'build-local', 'stable', '--repo', repo, '--json'],
    {cwd: repo, env: {...env,
      NODE_OPTIONS: '--require ' + JSON.stringify(bootstrap.replace(/\\/g, '/')),
    }, stdio: ['ignore', log, log]});
  let startError;
  child.on('error', error => { startError = error; });
  const wait = async predicate => {
    const deadline = Date.now() + 30000;
    while (!predicate()) {
      if (startError) throw startError;
      assert(Date.now() < deadline, 'Timed out: ' + fs.readFileSync(path.join(root, 'pack.log'), 'utf8'));
      await new Promise(resolve => setTimeout(resolve, 50));
    }
  };
  let postpackReleased = false;
  let completed = false;
  try {
    if (child.pid !== undefined) fs.writeFileSync(path.join(root, 'owner-pid'), String(child.pid));
    await wait(() => fs.existsSync(beat));
    await wait(() => {
      const release = readRecord(capture('postpack.release'));
      const pinned = readRecord(capture('postpack.pinned'));
      return pinned?.nonce === nonce && typeof pinned.proof === 'string' &&
        release?.nonce === nonce && release.pid === pinned.pid && release.proof === pinned.proof;
    });
    postpackReleased = true;
    const runtimes = path.join(env.OCM_HOME, 'runtimes');
    const scratch = fs.readdirSync(runtimes).find(name =>
      name.startsWith('.stable.stage-') || name.startsWith('.stable.pack-'));
    assert(scratch, 'Native packing must have created its scratch');
    const archive = path.join(runtimes, scratch,
      ...(scratch.startsWith('.stable.stage-') ? ['pack'] : []), 'openclaw-2026.4.25.tgz');
    assert(fs.existsSync(archive), 'Postpack must run after archive creation');
    const bytes = fs.readFileSync(archive);
    child.kill();
    await wait(() => child.exitCode !== null || child.signalCode !== null);
    fs.writeFileSync(path.join(root, "owner-killed"), "killed");
    await wait(() => fs.existsSync(path.join(root, "helper-terminated")));
    const before = fs.readFileSync(beat, 'utf8');
    await wait(() => fs.readFileSync(beat, 'utf8') !== before);
    const other = cp.spawnSync(binary, ['runtime', 'install', 'other', '--path', process.execPath, '--json'],
      {cwd: repo, env, encoding: 'utf8', timeout: 30000});
    assert.equal(other.status, 0, other.stderr);
    const after = fs.readFileSync(beat, 'utf8');
    await wait(() => fs.readFileSync(beat, 'utf8') !== after);
    console.log(JSON.stringify({platform: process.platform, scratch,
      holderTerminated: true, packChildStillRunning: true, archivePreserved: fs.existsSync(archive)}));
    assert(fs.existsSync(archive), 'Recovery deleted the archive while native postpack remained active');
    assert.deepEqual(fs.readFileSync(archive), bytes);
    publish(capture('final.ready'), {nonce, pid: process.pid, other: {
      pid: other.pid, status: other.status, signal: other.signal,
      error: other.error ? String(other.error) : null,
    }});
    await wait(() => readRecord(capture('final.release'))?.nonce === nonce);
    completed = true;
  } finally {
    try {
      const hold = completed && postpackReleased;
      publish(capture('postpack.done'), {nonce, hold});
      if (child.exitCode === null && child.signalCode === null) child.kill();
      if (hold) await wait(() => readRecord(capture('postpack.held'))?.nonce === nonce);
    } finally {
      fs.closeSync(log);
    }
  }
}

// The Rust wrapper assigns this process to its private Windows Job first.
process.stdin.once('data', () => {
  process.stdin.destroy();
  main().catch(error => { console.error(error); process.exitCode = 1; });
});
