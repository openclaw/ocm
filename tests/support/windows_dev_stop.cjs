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
let nextNativeRequest = 1;
let pendingReply = Buffer.alloc(0);
const replyChunk = Buffer.alloc(4096);
const nativeIdentities = new Map();
function native(op, fields = {}) {
  const id = nextNativeRequest++;
  const request = Buffer.from(JSON.stringify({nativeWindowsStopProof:1, id, op, ...fields}) + '\n');
  assert.ok(request.length <= 4096, 'Native fixture request exceeded its bound');
  for (let offset = 0; offset < request.length;) {
    const written = fs.writeSync(1, request, offset, request.length - offset);
    if (written === 0) throw new Error('Native fixture request pipe closed');
    offset += written;
  }
  // The Rust wrapper owns the deadline and kills this entire private Job on
  // timeout. Only one request is outstanding, so replies cannot accumulate.
  while (!pendingReply.includes(10)) {
    const count = fs.readSync(0, replyChunk, 0, replyChunk.length, null);
    if (count === 0) throw new Error('Native fixture response pipe closed');
    pendingReply = Buffer.concat([pendingReply, replyChunk.subarray(0, count)]);
    if (pendingReply.length > 65536) throw new Error('Native fixture reply exceeded its bound');
  }
  const end = pendingReply.indexOf(10);
  const reply = JSON.parse(pendingReply.subarray(0, end).toString('utf8'));
  pendingReply = pendingReply.subarray(end + 1);
  assert.equal(reply.id, id, 'Native fixture response was out of order');
  if (!reply.ok) throw new Error('Native fixture ' + op + ' failed: ' + reply.error);
  return reply.result;
}
function alive(pid) {
  assert.ok(Number.isInteger(pid) && pid > 0);
  const startedAt = nativeIdentities.get(pid);
  assert.ok(startedAt, 'Process must be captured before observation');
  return native('running', {pid, startedAt}).running;
}
async function waitFor(predicate, message, timeout = 15000) {
  const until = Date.now() + timeout;
  while (!predicate()) {
    if (Date.now() >= until) throw new Error(typeof message === 'function' ? message() : message);
    await new Promise(resolve => setTimeout(resolve, 40));
  }
}
function startIdentity(pid) {
  assert.ok(Number.isInteger(pid) && pid > 0);
  const observed = native('capture', {pid});
  assert.ok(observed.running, 'Fixture process exited before identity capture');
  assert.match(observed.startedAt, /^\d+$/);
  nativeIdentities.set(pid, observed.startedAt);
  return observed.startedAt;
}
function killRecordedProcess(pid, startedAt) {
  assert.equal(nativeIdentities.get(pid), startedAt, 'Refusing an uncaptured fixture identity');
  return native('terminate', {pid, startedAt}).stopped;
}
function capture(child) {
  let text = '';
  for (const stream of [child.stdout, child.stderr]) if (stream) stream.on('data', chunk => {text = (text + chunk.toString()).slice(-12000);});
  child.on('error', error => {text = (text + '\nSpawn error: ' + error.message).slice(-12000);});
  return () => text;
}
function sessionPath(name) { return path.join(env.OCM_HOME, 'source-watch', name + '.session'); }
function session(name) { return JSON.parse(fs.readFileSync(sessionPath(name), 'utf8')); }
async function start(name, watching = true, withUi = false) {
  const directory = path.join(root, name);
  const repo = path.join(directory, 'repo');
  fs.mkdirSync(path.join(repo, 'scripts'), {recursive:true});
  fs.mkdirSync(path.join(repo, 'extensions'), {recursive:true});
  // Preserve this required source directory in the plain-mode Git worktree.
  fs.writeFileSync(path.join(repo, 'extensions', '.gitkeep'), '');
  fs.writeFileSync(path.join(repo, 'package.json'), JSON.stringify({name:'openclaw', version:'2026.9.9'}));
  fs.writeFileSync(path.join(repo, 'openclaw.mjs'), '// Isolated built-entry fixture.\n');
  const ready = path.join(directory, 'ready');
  const rootPid = path.join(directory, 'root.pid');
  const descendantPid = path.join(directory, 'descendant.pid');
  const watch = [
    'import fs from "node:fs";',
    'import {spawn} from "node:child_process";',
    'const child=spawn(process.execPath,["-e","setInterval(()=>{},1000)"],{stdio:"ignore",windowsHide:true});',
    'child.once("spawn",()=>{fs.writeFileSync(' + JSON.stringify(rootPid) + ',String(process.pid));fs.writeFileSync(' + JSON.stringify(descendantPid) + ',String(child.pid));fs.writeFileSync(' + JSON.stringify(ready) + ',process.argv[1]);});',
    'setInterval(()=>{},1000);'
  ].join('\n');
  fs.writeFileSync(path.join(repo, 'scripts', 'watch-node.mjs'), watch);
  fs.writeFileSync(path.join(repo, 'scripts', 'run-node.mjs'), watch);
  if (withUi) {
    fs.mkdirSync(path.join(repo, 'ui'), {recursive:true});
    fs.writeFileSync(path.join(repo, 'ui', 'package.json'), '{"name":"ui-fixture"}');
    fs.writeFileSync(path.join(repo, 'ui', 'index.html'), '<!doctype html><title>UI fixture</title>');
    fs.copyFileSync(path.join(__dirname, 'dev_ui.cjs'), path.join(repo, 'scripts', 'dev-ui-fixture.cjs'));
    fs.writeFileSync(path.join(repo, 'scripts', 'ui.js'), "require('./dev-ui-fixture.cjs');\n");
    for (const entry of ['watch-node.mjs', 'run-node.mjs']) {
      fs.writeFileSync(path.join(repo, 'scripts', entry), "import './dev-ui-fixture.cjs';\n");
    }
    fs.writeFileSync(path.join(repo, 'openclaw.mjs'), "import './scripts/dev-ui-fixture.cjs';\n");
    for (const name of ['vite', 'dompurify']) {
      const module = path.join(repo, 'node_modules', name);
      fs.mkdirSync(module, {recursive:true});
      fs.writeFileSync(path.join(module, 'package.json'), JSON.stringify({name, main:'index.js'}));
      fs.writeFileSync(path.join(module, 'index.js'), "throw new Error('UI inspection must not execute package code');\n");
    }
    fs.writeFileSync(path.join(directory, 'dashboard-hold'), 'hold');
  }
  fs.writeFileSync(path.join(repo, 'SENTINEL'), 'preserve source');
  const init = cp.spawnSync('git', ['init','--quiet',repo], {env, encoding:'utf8', timeout:15000});
  if (init.error) throw init.error;
  assert.equal(init.status, 0, init.stderr);
  const envRoot = path.join(directory, 'env');
  const port = String(21901 + 32 * tracked.length);
  if (watching) {
    run(['env','create',name,'--runtime','proof-node','--root',envRoot,'--port',port]);
  } else {
    for (const args of [['add','.'], ['-c','user.name=OCM Tests','-c','user.email=tests@example.com','-c','commit.gpgsign=false','commit','--quiet','-m','fixture']]) {
      const saved = cp.spawnSync('git', ['-C',repo,...args], {env, encoding:'utf8', timeout:15000});
      if (saved.error) throw saved.error;
      assert.equal(saved.status, 0, saved.stderr);
    }
  }
  if (withUi) {
    fs.mkdirSync(path.join(envRoot, '.openclaw'), {recursive:true});
    fs.writeFileSync(path.join(envRoot, '.openclaw', 'openclaw.json'), JSON.stringify({
      gateway:{controlUi:{basePath:'/console'}, auth:{mode:'token', token:'synthetic-fixture-auth'}},
    }));
  }
  const args = ['dev',name,'--repo',repo,...(watching ? ['--watch','--force'] : ['--root',envRoot,'--port',port]), ...(withUi ? ['--ui'] : [])];
  const sourceEnv = withUi ? {...env, OCM_TEST_DEV_UI_DIR:directory, OCM_TEST_DEV_UI_DESCENDANTS:'1'} : env;
  const controller = cp.spawn(binary, args, {cwd:root, env:sourceEnv, stdio:['ignore','pipe','pipe'], windowsHide:true});
  const output = capture(controller);
  const record = {name, directory, repo, controller, output, identities:[]};
  tracked.push(record);
  assert.ok(controller.pid, 'Controller did not spawn');
  const controllerIdentity = {pid:controller.pid, startedAt:startIdentity(controller.pid)};
  record.identities.push(controllerIdentity);
  const gatewayFile = path.join(directory, 'gateway.json');
  const uiFile = path.join(directory, 'ui.json');
  await waitFor(() => withUi ? fs.existsSync(gatewayFile) && fs.existsSync(uiFile) : fs.existsSync(ready),
    () => 'Source process did not start: ' + name + '\n' + output());
  const owner = session(name);
  const gateway = withUi ? JSON.parse(fs.readFileSync(gatewayFile)) : {
    pid:Number(fs.readFileSync(rootPid,'utf8')), descendantPid:Number(fs.readFileSync(descendantPid,'utf8')),
  };
  if (!withUi) assert.equal(path.basename(fs.readFileSync(ready, 'utf8')), watching ? 'watch-node.mjs' : 'run-node.mjs');
  const watcher = gateway.pid;
  const descendant = gateway.descendantPid;
  assert.equal(owner.controller.pid, controller.pid);
  assert.equal(owner.kind, withUi ? 'ocm-source-ui-session-v1' : 'ocm-source-foreground-session-v1');
  assert.equal(owner.watching, watching);
  assert.deepEqual(owner.controller, controllerIdentity);
  const childIdentity = {pid:watcher, startedAt:startIdentity(watcher)};
  assert.deepEqual(withUi ? owner.ui.children.gateway : owner.child, childIdentity);
  assert.equal(owner.childSpawnPending, false);
  record.identities.push(childIdentity, {pid:descendant, startedAt:startIdentity(descendant)});
  record.sourcePids = [watcher, descendant];
  if (withUi) {
    const ui = JSON.parse(fs.readFileSync(uiFile));
    const uiIdentity = {pid:ui.pid, startedAt:startIdentity(ui.pid)};
    assert.deepEqual(owner.ui.children.ui, uiIdentity);
    assert.equal(owner.ui.pending, null);
    assert.equal(owner.ui.target.port, ui.port);
    assert.equal(gateway.cwd, ui.cwd);
    assert.ok(ui.args.includes('--strictPort'));
    record.identities.push(uiIdentity, {pid:ui.descendantPid, startedAt:startIdentity(ui.descendantPid)});
    record.sourcePids.push(ui.pid, ui.descendantPid);
    assert.ok(!fs.existsSync(path.join(directory, 'dashboard-attempts')), 'Gateway document readiness was bypassed');
    fs.writeFileSync(path.join(directory, 'gateway-document-ready'), 'ready');
    const helperFile = path.join(directory, 'dashboard-attempt-1');
    await waitFor(() => fs.existsSync(helperFile), () => 'Native handoff did not start\n' + output());
    const pid = Number(fs.readFileSync(helperFile));
    const helperIdentity = {pid, startedAt:startIdentity(pid)};
    assert.deepEqual(session(name).ui.children.command, helperIdentity);
    record.identities.push(helperIdentity);
    record.sourcePids.push(pid);
    record.helperPid = pid;
  }
  assert.ok(record.identities.every(identity => /^\d+$/.test(identity.startedAt)));
  assert.ok(record.sourcePids.every(alive));
  record.original = fs.readFileSync(sessionPath(name));
  record.watcher = watcher;
  record.descendant = descendant;
  record.source = watching ? repo : JSON.parse(run(['env','show',name,'--json']).stdout).devWorktreeRoot;
  fs.writeFileSync(path.join(envRoot, 'SENTINEL'), 'preserve environment');
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
  assert.ok(killRecordedProcess(record.controller.pid, record.identities[0].startedAt), 'Controller was not running before the crash');
  await waitFor(() => record.controller.exitCode !== null || record.controller.signalCode !== null, 'Controller did not exit');
  await waitFor(() => record.sourcePids.every(pid => !alive(pid)), 'Kill-on-close jobs left an owned source process running');
}
async function checkPreserved(record) {
  assert.equal(fs.readFileSync(path.join(record.directory,'env','SENTINEL'),'utf8'), 'preserve environment');
  assert.equal(fs.readFileSync(path.join(record.repo,'SENTINEL'),'utf8'), 'preserve source');
  assert.equal(fs.readFileSync(path.join(record.source,'SENTINEL'),'utf8'), 'preserve source');
}
function cleanup() {
  if (cleanupPromise) return cleanupPromise;
  cleanupPromise = (async () => {
    const errors = [];
    for (const record of tracked) for (const identity of record.identities) {
      try {
        killRecordedProcess(identity.pid, identity.startedAt);
        await waitFor(() => !alive(identity.pid), 'Owned fixture process survived cleanup', 8000);
      } catch (error) { errors.push(error); }
    }
    try { fs.rmSync(root, {recursive:true, force:true, maxRetries:20, retryDelay:50}); }
    catch (error) { errors.push(error); }
    if (errors.length) throw new AggregateError(errors, 'Native fixture cleanup was incomplete');
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
    // The wrapper acknowledges only after assigning Node to its private Job.
    // No subprocess may be created before this handshake completes.
    assert.equal(native('ready').ready, true);
    run(['runtime','add','proof-node','--path',process.execPath]);
    const normal = await start('normal.stop', true, true);
    fs.rmSync(path.join(normal.directory, 'dashboard-hold'));
    await waitFor(() => !alive(normal.helperPid) && !session(normal.name).ui.children.command && normal.output().includes('UI: '),
      'Completed initial handoff was not acknowledged');
    assert.match(normal.output(), /UI: http:\/\/127\.0\.0\.1:\d+\/#bootstrapToken=synthetic-owner-grant-1/);
    assert.ok(!normal.output().includes('synthetic-legacy'));
    const active = JSON.parse(run(['dev','status',normal.name,'--json']).stdout);
    assert.equal(active.sourceWatch.state, 'active', 'Held watch lease was not readable by dev status');
    assert.equal(active.sourceWatch.watching, true);
    const originalOwner = session(normal.name);
    const componentPids = normal.sourcePids.filter(pid => pid !== normal.helperPid);
    const reuseArgs = ['dev',normal.name,'--repo',normal.repo,'--watch','--force','--ui'];
    const hold = path.join(normal.directory, 'dashboard-hold');
    const attempts = () => fs.readFileSync(path.join(normal.directory, 'dashboard-attempts'), 'utf8').trim().split(/\r?\n/).length;
    let controllerOutputClosed = false;
    normal.controller.once('close', () => {controllerOutputClosed = true;});
    const assertOriginalOwner = () => {
      const current = session(normal.name);
      assert.equal(current.leaseId, originalOwner.leaseId, 'Requester replaced the watch lease');
      assert.equal(current.envRoot, originalOwner.envRoot);
      assert.deepEqual(current.controller, originalOwner.controller);
      assert.deepEqual(current.ui.target, originalOwner.ui.target, 'Reuse changed the captured UI address');
      assert.deepEqual(current.ui.children.gateway, originalOwner.ui.children.gateway);
      assert.deepEqual(current.ui.children.ui, originalOwner.ui.children.ui);
      assert.equal(current.ui.pending, null);
      assert.equal(current.closed, false);
      assert.ok([normal.controller.pid, ...componentPids].every(alive), 'Requester exit stopped an original process');
      assert.ok(!fs.readFileSync(sessionPath(normal.name), 'utf8').includes('synthetic-'), 'Session persisted a native grant');
    };
    const assertFreshGrant = (response, ordinal) => {
      const links = [...response.stdout.matchAll(/^UI: (http:\/\/\S+)$/gm)];
      assert.equal(links.length, 1, 'Reused CLI must return one native grant\n' + response.stdout + response.stderr);
      const link = new URL(links[0][1]);
      assert.equal(link.origin, 'http://127.0.0.1:' + originalOwner.ui.target.port);
      assert.equal(link.pathname, '/');
      const fragment = new URLSearchParams(link.hash.slice(1));
      assert.equal(fragment.get('bootstrapToken'), 'synthetic-owner-grant-' + ordinal);
      assert.equal(fragment.get('bootstrapProfile'), 'control-ui-owner');
      assert.equal(fragment.get('gatewayUrl'), originalOwner.ui.target.gatewayUrl.replace(/^http:/, 'ws:'));
      assert.ok(!response.stdout.includes('synthetic-legacy') && !response.stderr.includes('synthetic-'));
      assert.equal(attempts(), ordinal, 'Reuse launched an extra native helper');
    };
    const startCli = (args) => {
      const child = cp.spawn(binary, args, {cwd:root, env, stdio:['ignore','pipe','pipe'], windowsHide:true});
      const output = capture(child);
      let stdout = '';
      let closed = false;
      child.stdout.on('data', chunk => {stdout = (stdout + chunk.toString()).slice(-12000);});
      child.once('close', () => {closed = true;});
      assert.ok(child.pid, 'CLI process did not spawn');
      return {child, output, stdout:() => stdout, closed:() => closed};
    };
    const captureCli = (request) => {
      const identity = {pid:request.child.pid, startedAt:startIdentity(request.child.pid)};
      normal.identities.push(identity);
      return {...request, identity};
    };
    const heldRequest = async (ordinal) => {
      fs.writeFileSync(hold, 'hold');
      const helperFile = path.join(normal.directory, 'dashboard-attempt-' + ordinal);
      const deadline = Date.now() + 15000;
      let request;
      let pid = 0;
      for (;;) {
        assert.ok(Date.now() < deadline, 'UI handoff listener did not become available');
        request = startCli(reuseArgs);
        await waitFor(() => {
          if (fs.existsSync(helperFile)) pid = Number(fs.readFileSync(helperFile));
          return (Number.isInteger(pid) && pid > 0) || request.closed();
        }, () => 'Reused native helper did not start\n' + request.output() + normal.output(), deadline - Date.now());
        if (Number.isInteger(pid) && pid > 0) break;
        // Command metadata can clear before the preceding ACK releases the pipe.
        // Retry only an explicit pending response that created no native helper.
        assert.equal(request.child.exitCode, 0, request.output());
        assert.match(request.output(), /UI link pending/);
        assert.ok(!request.output().includes('synthetic-'));
        assert.equal(attempts(), ordinal - 1, 'Pending requester changed native helper ownership');
      }
      assert.ok(!request.closed(), 'Held requester exited before its native helper completed');
      const helperIdentity = {pid, startedAt:startIdentity(pid)};
      normal.identities.push(helperIdentity);
      normal.sourcePids.push(pid);
      assert.deepEqual(session(normal.name).ui.children.command, helperIdentity, 'Requester took helper ownership');
      assert.equal(attempts(), ordinal);
      assertOriginalOwner();
      return {...captureCli(request), helperIdentity};
    };

    assertFreshGrant(run(reuseArgs), 2);
    await waitFor(() => !session(normal.name).ui.children.command, 'Fresh handoff retained completed helper ownership');
    assertOriginalOwner();

    const abandoned = await heldRequest(3);
    const busy = run(reuseArgs);
    assert.match(busy.stdout + busy.stderr, /UI link pending/);
    assert.ok(!busy.stdout.includes('synthetic-') && !busy.stderr.includes('synthetic-'));
    assert.equal(attempts(), 3, 'Busy requester started a second native helper');
    assert.deepEqual(session(normal.name).ui.children.command, abandoned.helperIdentity);
    assertOriginalOwner();
    assert.ok(killRecordedProcess(abandoned.identity.pid, abandoned.identity.startedAt), 'Requester exited before termination');
    await waitFor(abandoned.closed, 'Terminated requester kept its output open');
    assert.ok(alive(abandoned.helperIdentity.pid), 'Requester exit killed the controller-owned helper');
    assert.deepEqual(session(normal.name).ui.children.command, abandoned.helperIdentity);
    assertOriginalOwner();
    fs.rmSync(hold);
    await waitFor(() => !alive(abandoned.helperIdentity.pid) && !session(normal.name).ui.children.command,
      'Abandoned handoff helper did not complete under the original controller');
    assert.equal(attempts(), 3, 'Abandoned requester caused a replacement helper');
    assert.ok(!abandoned.output().includes('synthetic-owner-grant'), 'Abandoned requester received a grant');
    assert.ok(!normal.output().includes('synthetic-owner-grant-3'), 'Abandoned grant reached the original terminal');
    assertFreshGrant(run(reuseArgs), 4);
    await waitFor(() => !session(normal.name).ui.children.command, 'Next handoff retained completed helper ownership');
    assertOriginalOwner();
    assert.equal(JSON.parse(run(['dev','status',normal.name,'--json']).stdout).sourceWatch.state, 'active');

    // Keep a native helper active while named stop closes the original component
    // Jobs. Release it only after those trees stop; the outer fixture Job stays open.
    const stoppingRequest = await heldRequest(5);
    const stopping = captureCli(startCli(['dev','stop',normal.name,'--json']));
    await waitFor(() => componentPids.every(pid => !alive(pid)), 'Named stop left a Gateway/UI tree running');
    assert.ok(alive(stoppingRequest.helperIdentity.pid), 'Native helper ownership ended before completion');
    assert.ok(alive(normal.controller.pid), 'Controller exited before its native helper completed');
    assert.ok(alive(stopping.identity.pid), 'Named stop returned before its native helper completed');
    assert.deepEqual(session(normal.name).ui.children.command, stoppingRequest.helperIdentity);
    assert.equal(session(normal.name).closed, false);
    fs.rmSync(hold);
    await waitFor(stopping.closed, () => 'Named stop did not complete\n' + stopping.output());
    assert.equal(stopping.child.exitCode, 0, stopping.output());
    const stopped = JSON.parse(stopping.stdout());
    assert.equal(stopped.envName, normal.name);
    assert.equal(stopped.stopped, true);
    assert.equal(stopped.serviceRestored, false);
    assert.equal(session(normal.name).closed, true);
    assert.ok(!fs.readFileSync(sessionPath(normal.name), 'utf8').includes('synthetic-'), 'Closed session persisted a native grant');
    await waitFor(() => normal.sourcePids.every(pid => !alive(pid)), 'Named stop left an owned helper running');
    await waitFor(() => normal.controller.exitCode !== null && controllerOutputClosed, 'Controller did not acknowledge named stop');
    await waitFor(stoppingRequest.closed, 'Stopped handoff requester did not exit');
    assert.equal(attempts(), 5, 'Stop launched an extra native helper');
    assert.ok(!stoppingRequest.output().includes('synthetic-owner-grant'), 'Stop delivered a cancelled grant');
    assert.deepEqual(normal.output().match(/synthetic-owner-grant-\d+/g), ['synthetic-owner-grant-1']);
    await checkPreserved(normal);
    const again = JSON.parse(run(['dev','stop',normal.name,'--json']).stdout);
    assert.equal(again.stopped, false);
    results.push('native fresh UI reuse, abandoned grant discarded, original helper/component ownership, named stop, env/source preservation');

    const plain = await start('plain.stop', false);
    const plainStatus = JSON.parse(run(['dev','status',plain.name,'--json']).stdout);
    assert.equal(plainStatus.sourceWatch.state, 'active');
    assert.equal(plainStatus.sourceWatch.watching, false);
    await stop(plain.name);
    await waitFor(() => !alive(plain.watcher) && !alive(plain.descendant), 'Plain named stop left its source tree running');
    await waitFor(() => plain.controller.exitCode !== null, 'Plain controller did not acknowledge named stop');
    await checkPreserved(plain);
    results.push('native plain named stop, descendant cleanup, env/source preservation');

    const crashed = await start('crashed', true, true);
    await crash(crashed);
    await stop(crashed.name);
    await checkPreserved(crashed);
    results.push('controller crash, Gateway/UI/helper job cleanup, closed recovery');

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
