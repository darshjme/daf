"""Run real local CLI acceptance checks against target/debug/daf."""

from pathlib import Path
import os, subprocess, json, tempfile
binary=str(Path(__file__).resolve().parents[1] / 'target/debug/daf')
results={}
with tempfile.TemporaryDirectory(prefix='daf-acceptance-') as directory:
    root=Path(directory)
    def run(*args,env=None):return subprocess.run([binary,'--quiet',*args],cwd=root,env=env,capture_output=True,text=True,timeout=20)
    def mission(tasks):
        p=root/'mission.json'
        p.write_text(json.dumps({'mission':{'name':'acceptance','tasks':tasks}}))
        return str(p)
    def task(name,command,deps=()):return {'name':name,'agent':'local','depends_on':deps,'params':{'command':command}}
    p=mission([task('verify',['/bin/test','-f','created'],['create']),task('create',['/usr/bin/touch','created'])])
    r=run('--format','json','run',p,'--dry-run')
    preview=json.loads(r.stdout)
    assert r.returncode==0 and preview['status']=='planned' and preview['waves']==[['create'],['verify']]
    assert not (root/'created').exists() and preview['tasks'][1]['command']==['/usr/bin/touch','created']
    results['dry_run_plans_without_effects']=True
    r=run('run',p,'--dry-run')
    assert r.returncode==0 and 'Wave 1: create' in r.stdout and 'Wave 2: verify' in r.stdout and not (root/'created').exists()
    results['dry_run_text_is_actionable']=True
    r=run('run',p,'--dry-run','--yes')
    assert r.returncode!=0 and not (root/'created').exists()
    results['dry_run_conflicts_with_execution']=True
    r=subprocess.run([binary,'run',p,'--dry-run'],cwd=root,env={**os.environ,'NO_COLOR':'1'},capture_output=True,text=True,timeout=20)
    assert r.returncode==0 and '\x1b[' not in r.stdout+r.stderr and '██' not in r.stderr
    results['redirected_output_is_plain_and_compact']=True
    r=run('run',p,'--yes')
    assert r.returncode==0,r.stderr
    results['out_of_order_dependency_executes']=True
    p=mission([task('fail',['/usr/bin/false']),task('blocked',['/usr/bin/touch','must-not-exist'],['fail'])])
    r=run('run',p,'--yes')
    assert r.returncode!=0 and not (root/'must-not-exist').exists()
    results['failure_blocks_dependents']=True
    p=mission([task('cycle',['/usr/bin/touch','cycle-file'],['cycle'])])
    r=run('run',p,'--yes')
    assert r.returncode!=0 and not (root/'cycle-file').exists()
    results['cycle_rejected_before_effects']=True
    p=mission([task('timeout',['/bin/sleep','10'])])
    r=run('run',p,'--yes','--timeout','1')
    assert r.returncode!=0 and 'timed out' in r.stderr
    results['timeout_fails']=True
    env={**os.environ,'DAF_VAULT_PASSWORD':'acceptance-only-password','DAF_VAULT_DIR':str(root/'vault')}
    for args in [('vault','init'),('vault','set','fixture','one'),('vault','rotate','fixture','two')]:
        r=run(*args,env=env)
        assert r.returncode==0,r.stderr
    r=run('vault','get','fixture','--raw',env=env)
    assert r.returncode==0 and r.stdout=='two',repr(r.stdout)
    results['vault_persists_and_rotates']=True
    r=run('vault','get','fixture','--raw',env={**env,'DAF_VAULT_PASSWORD':'different-password'})
    assert r.returncode!=0
    results['vault_wrong_password_rejected']=True
    r=run('vault','init',env=env)
    assert r.returncode!=0
    results['vault_reinitialization_rejected']=True
    r=run('status')
    assert r.returncode!=0
    results['no_fabricated_cluster_status']=True
    # JSON output must remain parseable even when commands write stdout.
    p=mission([task('echo',['/bin/echo','fixture-output'])])
    r=run('--format','json','run',p,'--yes')
    report=json.loads(r.stdout)
    assert r.returncode==0 and report['tasks'][0]['status']=='passed' and 'fixture-output' in r.stderr
    results['json_results_isolate_child_output']=True
    p=mission([task('first',['/usr/bin/touch','preflight-file']),{**task('bad',['/usr/bin/true']),'params':{'command':['/usr/bin/true'],'cwd':'missing-directory'}}])
    r=run('run',p,'--yes')
    assert r.returncode!=0 and not (root/'preflight-file').exists()
    results['directories_preflight_before_effects']=True
    data={'mission':{'name':'typo','tasks':[task('a',['/usr/bin/true'])],'unexpected':True}}
    (root/'strict.json').write_text(json.dumps(data))
    r=run('run',str(root/'strict.json'),'--yes')
    assert r.returncode!=0
    results['unknown_fields_rejected']=True
    project=root/'project'
    project.mkdir()
    (project/'.gitignore').write_text('keep-me')
    r=run('init',str(project),'--yes')
    assert r.returncode!=0 and (project/'.gitignore').read_text()=='keep-me' and not (project/'daf.yml').exists()
    results['init_preserves_existing_files']=True
    fresh=root/'fresh'
    r=run('init',str(fresh),'--name','demo','--yes')
    assert r.returncode==0,r.stderr
    r=run('run',str(fresh/'mission.yml'),'--yes')
    assert r.returncode==0,r.stderr
    results['init_generates_runnable_mission']=True
print(json.dumps(results, indent=2))
