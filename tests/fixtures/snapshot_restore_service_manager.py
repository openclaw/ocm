#!/usr/bin/env python3
import json, os, subprocess, sys
from pathlib import Path
from datetime import datetime, timezone
case = Path(os.environ['OCM_HOME']).parent
home = Path(os.environ['OCM_HOME'])
root = home / 'envs/source'
marker = root / '.openclaw/workspace/restore-marker.txt'
printed = case / 'launchctl-print.txt'
args = sys.argv[1:]
with (case / 'launchctl.log').open('a') as f: f.write(' '.join(args)+'\n')
def metadata():
    return next(x for x in json.loads((home/'envs.json').read_text())['envs'] if x['name']=='source')
def event(kind):
    meta=metadata()
    row={'event':kind,'marker':marker.read_text().strip(),'enabled':meta['serviceEnabled'],'running':meta['serviceRunning'],'operations':[str(p) for p in (home/'envs').glob('.source-ocm-restore-*')]}
    with (case/'admission-events.jsonl').open('a') as f: f.write(json.dumps(row)+'\n')
def runtime():
    meta=metadata()
    active=printed.exists() and meta['serviceEnabled'] and meta['serviceRunning']
    port=meta.get('gatewayPort') or 18789
    shared={'envName':'source','bindingKind':'launcher','bindingName':'stable','pid':4242,'restartCount':0,'childPort':port,'stdoutPath':str(case/'stdout.log'),'stderrPath':str(case/'stderr.log')}
    data={'kind':'ocm-supervisor-runtime','ocmHome':str(home),'daemonVersion':os.environ['OCM_TEST_VERSION'],'gatewayAdmission':json.loads(os.environ['OCM_TEST_GATEWAY_ADMISSION']),'updatedAt':datetime.now(timezone.utc).isoformat().replace('+00:00','Z'),'children':[shared] if active else [],'services':[dict(shared,gatewayState='running',restartHandoff='none')] if active else []}
    mode = (case/'mode').read_text()
    if (case/'fault-path').exists() and 'retained-' in mode:
        p=home/'supervisor/runtime.json'
        if (case/'retained-runtime').exists(): return
        if 'retained-sibling' in mode: shared['envName']='sibling'
        data['children']=[shared]
        data['services']=[dict(shared,gatewayState='running',restartHandoff='none')]
        if 'retained-terminal' in mode:
            data['children']=[]
            data['services'][0].update(pid=None,gatewayState='stopped')
        if 'retained-foreign' in mode:
            data.update(ocmHome=str(case/'foreign-store'),children=[],services=[])
        # Retained legacy records do not establish current process identity.
        data.pop('gatewayAdmission')
        (case/'retained-runtime').write_text('preserve until owner retires')
    p=home/'supervisor/runtime.json'
    p.parent.mkdir(parents=True,exist_ok=True)
    tmp=p.with_suffix('.fixture-tmp')
    tmp.write_text(json.dumps(data));tmp.replace(p)
if args[0]=='bootstrap':
    (case/'daemon-definition').write_text('path = '+args[-1])
    if (case/'armed').exists():
        counter=case/'bootstrap-count'
        count=int(counter.read_text())+1 if counter.exists() else 1
        counter.write_text(str(count))
        if count==1:
            event('acceptance-start-rejected')
            mode=(case/'mode').read_text()
            target=root if mode.startswith('full-incomplete') else root/'.openclaw' if mode=='scoped-incomplete' else marker if mode=='cleanup-warning' else None
            if target is not None:
                assert target.resolve().is_relative_to(case.resolve())
                subprocess.run(['/usr/bin/chflags','uchg',str(target)],check=True)
                (case/'fault-path').write_text(str(target))
                if mode.endswith('sync-fails'):
                    subprocess.run(['/usr/bin/chflags', 'uchg', str(home/'supervisor/state.json')], check=True)
            printed.unlink(missing_ok=True)
            runtime()
            print('forced snapshot acceptance failure',file=sys.stderr)
            sys.exit(1)
        event('service-restart-admitted')
    printed.write_text('state = running\npid = '+os.environ['OCM_TEST_NATIVE_DAEMON_PID']+'\npath = '+args[-1]+'\n')
    runtime();sys.exit(0)
if args[0] in ('bootout','unload'):
    if (case/'fault-path').exists() and (case/'mode').read_text().endswith('unload-fails'):
        print('forced unload failure', file=sys.stderr)
        sys.exit(1)
    printed.unlink(missing_ok=True);runtime();sys.exit(0)
if args[0]=='print':
    if (case/'fault-path').exists() and 'retained-' in (case/'mode').read_text():
        runtime()
        if (case/'mode').read_text().endswith('stopped'):
            print('state = stopped\n'+next(line for line in (case/'daemon-definition').read_text().splitlines() if line.startswith('path = ')))
            sys.exit(0)
        print('Could not find service "'+args[-1].rsplit('/',1)[-1]+'" in domain for user gui',file=sys.stderr);sys.exit(1)
    if (case/'fault-path').exists() and not metadata()['serviceRunning']:
        mode = (case/'mode').read_text()
        if mode.endswith('foreign-path') or mode.endswith('missing-path'):
            if mode.endswith('foreign-path'):
                print('path = '+str(case/'old-home/Library/LaunchAgents/ai.openclaw.ocm.plist'))
            print('state = running\npid = '+os.environ['OCM_TEST_NATIVE_DAEMON_PID'])
            sys.exit(0)
    if ((case/'fault-path').exists() and (case/'mode').read_text().endswith('observation-fails')
            and not metadata()['serviceRunning']):
        print('forced manager observation failure', file=sys.stderr)
        sys.exit(1)
    runtime()
    if printed.exists():
        print(printed.read_text(),end='');sys.exit(0)
    print('Could not find service \"'+args[-1].rsplit('/',1)[-1]+'\" in domain for user gui',file=sys.stderr);sys.exit(1)
sys.exit(0)
