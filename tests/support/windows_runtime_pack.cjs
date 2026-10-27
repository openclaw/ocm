'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const cp = require('node:child_process');

assert.equal(process.platform, 'win32', 'This proof requires native Windows');
const binary = path.resolve(process.argv[2]);
const root = path.resolve(process.argv[3]);
assert.equal(path.basename(path.dirname(root)), 'ocm-tests');
assert.ok(fs.statSync(binary).isFile());

async function main() {
  const repo = path.join(root, 'repo & %PATH% !literal! 雪');
  const beat = path.join(root, 'heartbeat');
  const done = path.join(root, 'done');
  const finished = path.join(root, 'finished');
  fs.mkdirSync(repo);
  fs.writeFileSync(path.join(repo, 'package.json'), JSON.stringify({
    name: 'openclaw', version: '2026.4.25', bin: {openclaw: 'openclaw.mjs'},
    engines: {node: '>=22.22.3'}, files: ['openclaw.mjs'],
    scripts: {postpack: 'node postpack.cjs'},
  }));
  fs.writeFileSync(path.join(repo, 'openclaw.mjs'), 'console.log("fixture");');
  fs.writeFileSync(path.join(repo, 'postpack.cjs'), `
const fs = require('node:fs');
if (process.env.PACK_FAIL === '1') process.exit(23);
if (process.env.PACK_BEAT) setInterval(() => {
  if (fs.existsSync(process.env.PACK_DONE)) {
    fs.writeFileSync(process.env.PACK_FINISHED, 'done');
    process.exit(0);
  }
  fs.writeFileSync(process.env.PACK_BEAT, String(Date.now()));
}, 50);
`);
  const env = {};
  for (const key of ['PATH', 'PATHEXT', 'SystemRoot', 'WINDIR', 'COMSPEC']) {
    const entry = Object.entries(process.env).find(([name]) => name.toUpperCase() === key.toUpperCase());
    if (entry) env[key] = entry[1];
  }
  Object.assign(env, {
    HOME: path.join(root, 'home'), USERPROFILE: path.join(root, 'home'),
    OCM_HOME: path.join(root, 'ocm-home'), OCM_INTERNAL_SKIP_SERVICE_READINESS: '1',
    TEMP: root, TMP: root,
    APPDATA: path.join(root, 'appdata'), LOCALAPPDATA: path.join(root, 'localappdata'),
    npm_config_cache: path.join(root, 'cache'), npm_config_offline: 'true',
    OCM_INTERNAL_NPM_BIN: cp.execFileSync('where.exe', ['npm.cmd'], {encoding: 'utf8'})
      .trim().split(/\r?\n/)[0],
    PACK_BEAT: beat, PACK_DONE: done, PACK_FINISHED: finished,
  });
  fs.mkdirSync(env.HOME, {recursive: true});
  fs.mkdirSync(env.OCM_HOME, {recursive: true});
  fs.writeFileSync(path.join(root, 'npm-bin'), env.OCM_INTERNAL_NPM_BIN);
  const log = fs.openSync(path.join(root, 'pack.log'), 'w');
  const child = cp.spawn(binary, ['runtime', 'build-local', 'stable', '--repo', repo, '--json'],
    {cwd: repo, env, stdio: ['ignore', log, log]});
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
  try {
    await wait(() => fs.existsSync(beat));
    fs.writeFileSync(path.join(root, "owner-pid"), String(child.pid));
    await wait(() => fs.existsSync(path.join(root, "helper-observed")));
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
  } finally {
    fs.writeFileSync(done, 'done');
    if (child.exitCode === null && child.signalCode === null) child.kill();
    if (fs.existsSync(beat)) await wait(() => fs.existsSync(finished));
    fs.closeSync(log);
  }
}

// The Rust wrapper assigns this process to its private Windows Job first.
process.stdin.once('data', () => {
  process.stdin.destroy();
  main().catch(error => { console.error(error); process.exitCode = 1; });
});
