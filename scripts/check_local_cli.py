from pathlib import Path
import os, subprocess, json, tempfile
binary=str(Path(__file__).resolve().parents[1] / 'target/debug/daf')
results={}
with tempfile.TemporaryDirectory(prefix='daf-acceptance-') as directory:
 root=Path(directory)
 def run(*args,env=None):return subprocess.run([binary,'--quiet',*args],cwd=root,env=env,capture_output=True,text=True,timeout=20)
 def mission(tasks):
  p=root/'mission.json';p.write_text(json.dumps({'mission':{'name':'acceptance','tasks':tasks}}));return str(p)
 def task(name,command,deps=[]):return {'name':name,'agent':'local','depends_on':deps,'params':{'command':command}}
 p=mission([task('verify',['/bin/test','-f','created'],['create']),task('create',['/usr/bin/touch','created'])]);r=run('run',p,'--yes');assert r.returncode==0,r.stderr;results['out_of_order_dependency_executes']=True
 p=mission([task('fail',['/usr/bin/false']),task('blocked',['/usr/bin/touch','must-not-exist'],['fail'])]);r=run('run',p,'--yes');assert r.returncode!=0 and not (root/'must-not-exist').exists();results['failure_blocks_dependents']=True
 p=mission([task('cycle',['/usr/bin/touch','cycle-file'],['cycle'])]);r=run('run',p,'--yes');assert r.returncode!=0 and not (root/'cycle-file').exists();results['cycle_rejected_before_effects']=True
 p=mission([task('timeout',['/bin/sleep','10'])]);r=run('run',p,'--yes','--timeout','1');assert r.returncode!=0 and 'timed out' in r.stderr;results['timeout_fails']=True
 env={**os.environ,'DAF_VAULT_PASSWORD':'acceptance-only-password','DAF_VAULT_DIR':str(root/'vault')}
 for args in [('vault','init'),('vault','set','fixture','one'),('vault','rotate','fixture','two')]:
  r=run(*args,env=env);assert r.returncode==0,r.stderr
 r=run('vault','get','fixture','--raw',env=env);assert r.returncode==0 and r.stdout=='two',repr(r.stdout);results['vault_persists_and_rotates']=True
 r=run('vault','get','fixture','--raw',env={**env,'DAF_VAULT_PASSWORD':'different-password'});assert r.returncode!=0;results['vault_wrong_password_rejected']=True
 r=run('vault','init',env=env);assert r.returncode!=0;results['vault_reinitialization_rejected']=True
 r=run('status');assert r.returncode!=0;results['no_fabricated_cluster_status']=True
print(json.dumps(results))
