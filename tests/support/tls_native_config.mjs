// Installed-package capability fixture. It models only the bounded native
// config command contract and delegates Gateway HTTP(S) to the finite peer.
import fs from 'node:fs';
import path from 'node:path';
import {parseEnv} from 'node:util';
import {pathToFileURL} from 'node:url';
import {spawn} from 'node:child_process';

if (process.argv[2] === 'config') {
  const blocked = behavior === 'interrupt' && fs.existsSync(queries + '.block');
  let descendantPid;
  if (blocked) {
    // Inherit both capture pipes, so killing just the direct helper is insufficient.
    const descendant = spawn(process.execPath, ['-e',
      `require('node:fs').writeFileSync(${JSON.stringify(queries + '.descendant-ready')}, String(process.pid));
       process.stdout.write('NATIVE_CONFIG_PRIVATE_SENTINEL_DESCENDANT');
       setTimeout(() => process.exit(0), 45000);`
    ], {stdio: ['ignore', 'inherit', 'inherit']});
    descendantPid = descendant.pid;
  }
  fs.appendFileSync(queries, JSON.stringify({pid: process.pid, descendantPid, args: process.argv.slice(2)}) + '\n');
  if (behavior === 'failure' || behavior === 'timeout' || blocked) {
    process.stdout.write('NATIVE_CONFIG_PRIVATE_SENTINEL_STDOUT');
    process.stderr.write('NATIVE_CONFIG_PRIVATE_SENTINEL_STDERR');
    if (behavior === 'failure') {
      process.exitCode = 1;
    } else {
      // The OCM lookup must settle this child before its own readiness deadline.
      setTimeout(() => process.exit(0), 45000);
    }
  } else {
    const values = parseEnv(fs.readFileSync(path.join(path.dirname(process.env.OPENCLAW_CONFIG_PATH), '.env'), 'utf8'));
    const config = JSON.parse(fs.readFileSync(process.env.OPENCLAW_CONFIG_PATH, 'utf8'));
    const certificate = config.gateway.tls.certPath.replace(/\$\{([^}]+)\}/g,
      (_, name) => process.env[name] ?? values[name] ?? '');
    process.stdout.write(JSON.stringify(certificate));
  }
} else if (process.argv[2] === 'gateway' && process.argv[3] === 'run') {
  process.argv = [process.argv[0], peer, mode, requests];
  await import(pathToFileURL(peer).href);
} else if (process.argv[2] === '--version') {
  process.stdout.write('OpenClaw 0.0.0\n');
} else {
  // Capabilities and unrelated commands are not implemented by this finite peer.
  process.exitCode = 2;
}
