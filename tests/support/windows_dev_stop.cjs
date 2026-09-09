'use strict';
const assert = require('node:assert/strict');
const cp = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

if (process.platform !== 'win32') throw new Error('This proof requires native Windows');
const binary = path.resolve(process.argv[2] || '');
if (!process.argv[2] || !fs.statSync(binary).isFile()) throw new Error('Pass the exact rebuilt OCM binary');
if (!process.argv[3]) throw new Error('Pass the wrapper-owned TestDir root');
const root = path.resolve(process.argv[3]);
assert.equal(path.basename(path.dirname(root)), 'ocm-tests');
assert.ok(fs.lstatSync(root).isDirectory(), 'Fixture root must be an existing directory');
assert.ok(fs.readdirSync(root).every(name => ['home','ocm-home'].includes(name)), 'Fixture root contains unrelated files');
assert.equal(path.resolve(process.env.HOME || '').toLowerCase(), path.join(root,'home').toLowerCase());
assert.equal(path.resolve(process.env.OCM_HOME || '').toLowerCase(), path.join(root,'ocm-home').toLowerCase());
const env = {};
for (const key of ['PATH', 'Path', 'SystemRoot', 'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'PATHEXT', 'TEMP', 'TMP', 'PROCESSOR_ARCHITECTURE']) {
  if (process.env[key]) env[key] = process.env[key];
}
env.OCM_HOME = path.join(root, 'ocm-home');
env.HOME = path.join(root, 'home');
env.USERPROFILE = env.HOME;
env.LOCALAPPDATA = path.join(env.HOME, 'AppData', 'Local');
env.APPDATA = path.join(env.HOME, 'AppData', 'Roaming');
for (const directory of [env.OCM_HOME, env.HOME, env.LOCALAPPDATA, env.APPDATA]) fs.mkdirSync(directory, {recursive:true});
const tracked = [];
const results = [];
let failed = false;
let abortRequested = false;
let cleanupPromise;

function run(args, expectedSuccess = true) {
  if (abortRequested) throw new Error('Proof interrupted');
  const result = cp.spawnSync(binary, args, {cwd:root, env, encoding:'utf8', timeout:20000, maxBuffer:1024*1024});
  if (result.error) throw result.error;
  if (expectedSuccess) assert.equal(result.status, 0, 'OCM failed: ' + args.slice(0,3).join(' ') + '\n' + result.stderr);
  return result;
}
function alive(pid) {
  assert.ok(Number.isInteger(pid) && pid > 0);
  try { process.kill(pid, 0); return true; }
  catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}
async function waitFor(predicate, message, timeout = 15000) {
  const until = Date.now() + timeout;
  while (!predicate()) {
    if (Date.now() >= until) throw new Error(typeof message === 'function' ? message() : message);
    await new Promise(resolve => setTimeout(resolve, 40));
  }
}
function powershell(script) {
  const result = cp.spawnSync('powershell.exe', ['-NoLogo','-NoProfile','-NonInteractive','-Command',script], {cwd:root, env, encoding:'utf8', timeout:15000});
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error('Windows process identity operation failed: ' + result.stderr);
  return result.stdout.trim();
}
function startIdentity(pid) {
  assert.ok(Number.isInteger(pid) && pid > 0);
  return powershell('$ownedProcess=Get-Process -Id ' + pid + ' -ErrorAction SilentlyContinue; if($null -ne $ownedProcess){try{$null=$ownedProcess.Handle;[Console]::Write($ownedProcess.StartTime.ToFileTimeUtc().ToString([Globalization.CultureInfo]::InvariantCulture))}finally{$ownedProcess.Dispose()}}');
}
function killRecordedProcess(pid, startedAt) {
  if (!alive(pid)) return false;
  assert.match(String(startedAt), /^\d+$/);
  const result = powershell('$ownedProcess=Get-Process -Id ' + pid + ' -ErrorAction SilentlyContinue; if($null -ne $ownedProcess){try{$null=$ownedProcess.Handle;$actualStart=$ownedProcess.StartTime.ToFileTimeUtc().ToString([Globalization.CultureInfo]::InvariantCulture);if($actualStart -eq "' + startedAt + '"){$ownedProcess.Kill();$null=$ownedProcess.WaitForExit(5000);[Console]::Write("stopped")}else{[Console]::Write("different")}}finally{$ownedProcess.Dispose()}}');
  return result === 'stopped';
}
function capture(child) {
  let text = '';
  for (const stream of [child.stdout, child.stderr]) if (stream) stream.on('data', chunk => {text = (text + chunk.toString()).slice(-12000);});
  child.on('error', error => {text = (text + '\nSpawn error: ' + error.message).slice(-12000);});
  return () => text;
}
function sessionPath(name) { return path.join(env.OCM_HOME, 'source-watch', name + '.session'); }
function session(name) { return JSON.parse(fs.readFileSync(sessionPath(name), 'utf8')); }
async function start(name) {
  const directory = path.join(root, name);
  const repo = path.join(directory, 'repo');
  fs.mkdirSync(path.join(repo, 'scripts'), {recursive:true});
  fs.mkdirSync(path.join(repo, 'extensions'), {recursive:true});
  fs.writeFileSync(path.join(repo, 'package.json'), JSON.stringify({name:'openclaw', version:'2026.9.9'}));
  fs.writeFileSync(path.join(repo, 'scripts', 'run-node.mjs'), '// Isolated source fixture.\n');
  fs.writeFileSync(path.join(repo, 'openclaw.mjs'), '// Isolated built-entry fixture.\n');
  const ready = path.join(directory, 'ready');
  const rootPid = path.join(directory, 'root.pid');
  const descendantPid = path.join(directory, 'descendant.pid');
  const watch = [
    'import fs from "node:fs";',
    'import {spawn} from "node:child_process";',
    'const child=spawn(process.execPath,["-e","setInterval(()=>{},1000)"],{stdio:"ignore",windowsHide:true});',
    'child.once("spawn",()=>{fs.writeFileSync(' + JSON.stringify(rootPid) + ',String(process.pid));fs.writeFileSync(' + JSON.stringify(descendantPid) + ',String(child.pid));fs.writeFileSync(' + JSON.stringify(ready) + ',"ready");});',
    'setInterval(()=>{},1000);'
  ].join('\n');
  fs.writeFileSync(path.join(repo, 'scripts', 'watch-node.mjs'), watch);
  const init = cp.spawnSync('git', ['init','--quiet',repo], {env, encoding:'utf8', timeout:15000});
  if (init.error) throw init.error;
  assert.equal(init.status, 0, init.stderr);
  run(['env','create',name,'--runtime','proof-node','--root',path.join(directory,'env'),'--port',String(21901 + 32 * tracked.length)]);
  fs.writeFileSync(path.join(directory, 'env', 'SENTINEL'), 'preserve environment');
  fs.writeFileSync(path.join(repo, 'SENTINEL'), 'preserve source');
  const controller = cp.spawn(binary, ['dev',name,'--repo',repo,'--watch','--force'], {cwd:root, env, stdio:['ignore','pipe','pipe'], windowsHide:true});
  const output = capture(controller);
  const record = {name, directory, repo, controller, output, identities:[]};
  tracked.push(record);
  await waitFor(() => fs.existsSync(ready), () => 'Watch did not start: ' + name + '\n' + output());
  const owner = session(name);
  const watcher = Number(fs.readFileSync(rootPid,'utf8'));
  const descendant = Number(fs.readFileSync(descendantPid,'utf8'));
  assert.equal(owner.controller.pid, controller.pid);
  assert.equal(owner.child.pid, watcher);
  assert.equal(owner.childSpawnPending, false);
  record.identities = [owner.controller, owner.child, {pid:descendant, startedAt:startIdentity(descendant)}];
  assert.ok(record.identities.every(identity => /^\d+$/.test(identity.startedAt)));
  assert.ok(alive(watcher) && alive(descendant));
  record.original = fs.readFileSync(sessionPath(name));
  record.watcher = watcher;
  record.descendant = descendant;
  return record;
}
async function stop(name) {
  const response = run(['dev','stop',name,'--json']);
  const result = JSON.parse(response.stdout);
  assert.equal(result.envName, name);
  assert.equal(result.stopped, true);
  assert.equal(result.serviceRestored, false);
  assert.equal(session(name).closed, true);
}
async function crash(record) {
  record.controller.kill('SIGKILL');
  await waitFor(() => record.controller.exitCode !== null || record.controller.signalCode !== null, 'Controller did not exit');
  await waitFor(() => !alive(record.watcher) && !alive(record.descendant), 'Kill-on-close job did not stop both source processes');
}
async function checkPreserved(record) {
  assert.equal(fs.readFileSync(path.join(record.directory,'env','SENTINEL'),'utf8'), 'preserve environment');
  assert.equal(fs.readFileSync(path.join(record.repo,'SENTINEL'),'utf8'), 'preserve source');
}
function cleanup() {
  if (cleanupPromise) return cleanupPromise;
  cleanupPromise = (async () => {
  for (const record of tracked) {
    if (record.controller && record.controller.exitCode === null && record.controller.signalCode === null) record.controller.kill('SIGKILL');
  }
  for (const record of tracked) for (const identity of record.identities) {
    if (killRecordedProcess(identity.pid, identity.startedAt)) {
      await waitFor(() => !alive(identity.pid) || startIdentity(identity.pid) !== identity.startedAt, 'Owned fixture process survived cleanup', 8000);
    }
  }
  fs.rmSync(root, {recursive:true,force:true});
  })();
  return cleanupPromise;
}

for (const [signal, code] of [['SIGINT',130],['SIGTERM',143]]) process.once(signal, () => {
  failed = true;
  abortRequested = true;
  cleanup().then(() => process.exit(code), error => {console.error('Interrupted cleanup failed: ' + error.message); process.exit(1);});
});

(async () => {
  try {
    run(['runtime','add','proof-node','--path',process.execPath]);
    const normal = await start('normal.stop');
    await stop(normal.name);
    await waitFor(() => !alive(normal.watcher) && !alive(normal.descendant), 'Named stop left its source tree running');
    await waitFor(() => normal.controller.exitCode !== null, 'Controller did not acknowledge named stop');
    await checkPreserved(normal);
    const again = JSON.parse(run(['dev','stop',normal.name,'--json']).stdout);
    assert.equal(again.stopped, false);
    results.push('native named stop, descendant cleanup, repeat stop, env/source preservation');

    const crashed = await start('crashed');
    await crash(crashed);
    await stop(crashed.name);
    await checkPreserved(crashed);
    results.push('controller crash, kill-on-close job cleanup, closed recovery');

    const reused = await start('reused');
    await crash(reused);
    const unrelated = cp.spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{env,stdio:'ignore',windowsHide:true});
    await waitFor(() => unrelated.pid !== undefined, 'Unrelated fixture process did not spawn');
    const unrelatedIdentity = {pid:unrelated.pid, startedAt:startIdentity(unrelated.pid)};
    tracked.push({controller:unrelated, identities:[unrelatedIdentity]});
    const stale = session(reused.name);
    stale.child = {pid:unrelated.pid, startedAt:'different-recorded-start'};
    const staleBytes = Buffer.from(JSON.stringify(stale));
    fs.writeFileSync(sessionPath(reused.name), staleBytes);
    const refused = run(['dev','stop',reused.name,'--json'], false);
    assert.notEqual(refused.status, 0);
    assert.match(refused.stderr, /PID was reused/);
    assert.ok(alive(unrelated.pid), 'Stop terminated the unrelated PID');
    assert.deepEqual(fs.readFileSync(sessionPath(reused.name)), staleBytes);
    fs.writeFileSync(sessionPath(reused.name), reused.original);
    await stop(reused.name);
    results.push('native stop refuses mismatched process identity and preserves unrelated process');

    const pending = await start('pending');
    await crash(pending);
    const uncertain = session(pending.name);
    uncertain.child = null;
    uncertain.childSpawnPending = true;
    uncertain.closed = false;
    uncertain.completion = null;
    const pendingBytes = Buffer.from(JSON.stringify(uncertain));
    fs.writeFileSync(sessionPath(pending.name), pendingBytes);
    const rejected = run(['dev','stop',pending.name,'--json'], false);
    assert.notEqual(rejected.status, 0);
    assert.match(rejected.stderr, /before publishing child ownership/);
    assert.deepEqual(fs.readFileSync(sessionPath(pending.name)), pendingBytes);
    fs.writeFileSync(sessionPath(pending.name), pending.original);
    await stop(pending.name);
    results.push('native stop retains an unverified pending-spawn record');
  } catch (error) {
    failed = true;
    console.error(error.stack || String(error));
  } finally {
    try { await cleanup(); }
    catch (error) { failed = true; console.error('Fixture cleanup failed: ' + error.message); }
    console.log(JSON.stringify({platform:process.platform, results, passed:!failed, fixtureRemoved:!fs.existsSync(root)}));
    process.exitCode = failed ? 1 : 0;
  }
})().catch(error => {console.error(error.stack); process.exitCode=1;});
