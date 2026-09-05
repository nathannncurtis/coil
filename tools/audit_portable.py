"""Reproduce published portable failures in real executables. Select Coil via PYTHONPATH."""
import argparse
from pathlib import Path
import os, sys, zipfile, subprocess, json, struct, shutil, io
parser=argparse.ArgumentParser(); parser.add_argument('--output',type=Path,required=True); ROOT=parser.parse_args().output.resolve()
if ROOT.exists(): parser.error('Use a new output directory')
ROOT.mkdir(parents=True)
os.environ['TEMP']=os.environ['TMP']=str(ROOT/'temp'); Path(os.environ['TEMP']).mkdir(exist_ok=True)
os.environ['LOCALAPPDATA']=str(ROOT/'local'); Path(os.environ['LOCALAPPDATA']).mkdir(exist_ok=True)
from coil.packager import package_portable
runtime=ROOT/'runtime'; runtime.mkdir(exist_ok=True)
with zipfile.ZipFile(Path.home()/'.coil/cache/runtimes/python-3.12.10-embed-amd64.zip') as z:z.extractall(runtime)
project=ROOT/'project';project.mkdir(exist_ok=True)
(project/'main.py').write_text('import os, sys, json\nfrom pathlib import Path\np=Path(__file__).parent.parent.parent / "assets" / "café-猫.txt"\nr={"argv":sys.argv,"executable":sys.executable,"asset_exists":p.exists(),"assets":[x.name for x in p.parent.iterdir()] if p.parent.exists() else []}\nPath(os.environ["COIL_REPORT"]).write_text(json.dumps(r))\nprint("STDOUT_SENTINEL",flush=True)\nprint("STDERR_SENTINEL",file=sys.stderr,flush=True)\n',encoding='utf-8')
(project/'assets').mkdir(exist_ok=True);(project/'assets'/'café-猫.txt').write_text('data',encoding='utf-8')
exe=package_portable(project,ROOT/'baseline',runtime,['main.py'],'PortableProbe','windows')[0]
def run(label,path,local=None):
 env=os.environ.copy();env['COIL_REPORT']=str(ROOT/f'{label}.json');env['LOCALAPPDATA']=str(local or ROOT/'local')
 r=subprocess.run([str(path)],env=env,capture_output=True,timeout=40)
 print(label, r.returncode, repr(r.stdout),repr(r.stderr),flush=True)
 return {'exit':r.returncode,'stdout':r.stdout.decode(errors='replace'),'stderr':r.stderr.decode(errors='replace'),'report':json.loads(Path(env['COIL_REPORT']).read_text()) if Path(env['COIL_REPORT']).exists() else None}
results={'fresh':run('fresh',exe)}
renamed=exe.with_name('Renamed.exe');shutil.copyfile(exe,renamed);results['renamed']=run('renamed',renamed)
data=bytearray(exe.read_bytes());offset,hash_,magic=struct.unpack_from('<III',data,len(data)-12)
cache=next((ROOT/'local/coil/PortableProbe').glob('*/.coil_ready')).parent
cache.resolve().relative_to(ROOT.resolve())
# Simulate lost inner executable but intact marker, confined to own output.
(cache/'PortableProbe.exe').unlink();results['partial_ready']=run('partial_ready',exe)
# Syntactically valid certificate table append (the exact EOF/layout effect of Authenticode).
pe=struct.unpack_from('<I',data,0x3c)[0];security=pe+24+112+4*8
pad=(-len(data))%8; cert_at=len(data)+pad;cert=struct.pack('<IHH',16,0x200,2)+b'CERTTEST'
data.extend(b'\0'*pad+cert);struct.pack_into('<II',data,security,cert_at,len(cert))
signed=exe.with_name('CertificateAppended.exe');signed.write_bytes(data);results['certificate_appended']=run('certificate_appended',signed)
(ROOT/'results.json').write_text(json.dumps(results,indent=2))
print(json.dumps(results,indent=2))
