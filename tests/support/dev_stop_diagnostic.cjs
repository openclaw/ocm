'use strict';

const prefix = 'OCM dev failure: ';
const keys = ['diagnostic', 'stage', 'operation', 'role', 'os_error', 'cleanup_verified', 'stop_observed'];
const stages = ['ui-loop', 'child-poll', 'child-stop', 'child-finish', 'role-clear', 'server-close', 'override-clear'];
const operations = ['parent-create', 'temp-open', 'temp-write', 'replace', 'pipe-accept', 'pipe-duplicate', 'pipe-disconnect-stale', 'pipe-disconnect-rejected'];

// The controller capture may also contain owner grants, errors and paths. Never
// forward a line from it, even one with our marker: reconstruct closed values.
function controllerFailure(output) {
  if (typeof output !== 'string') return {status:'absent'};
  const lines = output.slice(-12000).split('\n').filter(line => line.startsWith(prefix));
  if (lines.length === 0) return {status:'absent'};
  if (lines.length !== 1 || lines[0].length > 512) return {status:'invalid'};
  let value;
  try { value = JSON.parse(lines[0].slice(prefix.length)); }
  catch { return {status:'invalid'}; }
  if (!value || typeof value !== 'object' || Array.isArray(value)
      || Object.keys(value).length !== keys.length || !keys.every(key => Object.hasOwn(value, key))
      || value.diagnostic !== 'windows-dev-stop' || !stages.includes(value.stage)
      || (value.operation !== null && !operations.includes(value.operation))
      || (value.role !== null && !['gateway', 'ui', 'command'].includes(value.role))
      || (value.os_error !== null && (!Number.isInteger(value.os_error) || value.os_error < -2147483648 || value.os_error > 2147483647))
      || typeof value.cleanup_verified !== 'boolean' || typeof value.stop_observed !== 'boolean') {
    return {status:'invalid'};
  }
  return {
    status:'present', diagnostic:'windows-dev-stop', stage:value.stage,
    operation:value.operation, role:value.role, os_error:value.os_error,
    cleanup_verified:value.cleanup_verified, stop_observed:value.stop_observed,
  };
}

module.exports = {controllerFailure};

if (require.main === module) {
  const assert = require('node:assert/strict');
  const secret = 'synthetic-owner-grant-C:\\private\\session-error';
  const safe = {diagnostic:'windows-dev-stop', stage:'role-clear', operation:'replace', role:'command', os_error:5, cleanup_verified:true, stop_observed:true};
  const line = value => prefix + JSON.stringify(value) + '\n';
  // Match the producer's own leading separator after unterminated source bytes.
  const framed = value => '\n' + line(value);
  assert.deepEqual(controllerFailure(secret + framed(safe) + secret), {status:'present', ...safe});
  for (const key of keys) {
    const missing = {...safe}; delete missing[key];
    assert.deepEqual(controllerFailure(line(missing)), {status:'invalid'});
    assert.deepEqual(controllerFailure(line({...safe, [key]:secret})), {status:'invalid'});
  }
  for (const value of [null, [], secret, {...safe, extra:secret}, {...safe, os_error:1.5}, {...safe, os_error:2147483648}, {...safe, os_error:-2147483649}]) {
    assert.deepEqual(controllerFailure(line(value)), {status:'invalid'});
  }
  assert.deepEqual(controllerFailure(line(safe) + line(safe)), {status:'invalid'});
  assert.deepEqual(controllerFailure(prefix + '{"stage":'), {status:'invalid'});
  assert.deepEqual(controllerFailure(line({...safe, extra:secret.repeat(100)})), {status:'invalid'});
  assert.deepEqual(controllerFailure(secret), {status:'absent'});
  assert.deepEqual(controllerFailure(line(safe) + 'x'.repeat(12000)), {status:'absent'});
  assert.deepEqual(controllerFailure(line({...safe, operation:null, role:null, os_error:null})), {status:'present', ...safe, operation:null, role:null, os_error:null});
  for (const stage of stages) for (const operation of operations) {
    const result = controllerFailure(line({...safe, stage, operation}));
    assert.equal(result.status, 'present');
    assert.ok(!JSON.stringify(result).includes(secret));
  }
  console.log('closed diagnostic fields, privacy negatives, bounds and missing-record controls passed');
}
