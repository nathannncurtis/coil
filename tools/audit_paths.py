"""Reproduce destructive output names and duplicate launcher identities in real builds.

Select baseline/current Coil through PYTHONPATH; pass a new output directory.
"""
import argparse
import os,json,subprocess,zipfile
from pathlib import Path
from coil.packager import package_bundled
parser=argparse.ArgumentParser(); parser.add_argument('--output',type=Path,required=True); out=parser.parse_args().output.resolve()
if out.exists(): parser.error('Use a new audit output directory')
out.mkdir(parents=True)
runtime=out/'runtime'
with zipfile.ZipFile(Path.home()/'.coil/cache/runtimes/python-3.12.10-embed-amd64.zip') as z:z.extractall(runtime)
p=out/'project'; p.mkdir(exist_ok=True)
(p/'main.py').write_text('print("HELLO",flush=True)')
victim=out/'victim'; victim.mkdir(exist_ok=True); (victim/'sentinel.txt').write_text('must survive')
rows=[]
try: package_bundled(p,out/'dist',runtime,['main.py'],'../victim','windows')
except Exception as e: rows.append({'case':'unsafe_name','error':str(e),'sentinel_survived':(victim/'sentinel.txt').exists()})
for folder in ['a','b']:
 (p/folder).mkdir(exist_ok=True); (p/folder/'main.py').write_text(f'print("ENTRY={folder}",flush=True)')
try:
 b=package_bundled(p,out/'dist',runtime,['a/main.py','b/main.py'],'Duplicate','windows')
 r=subprocess.run([str(b/'main.exe')],capture_output=True,text=True,timeout=10)
 rows.append({'case':'duplicate_entry_stems','executables':[x.name for x in b.glob('*.exe')],'stdout':r.stdout,'exit':r.returncode})
except Exception as error:
 rows.append({'case':'duplicate_entry_stems','error':str(error)})
(out/'results.json').write_text(json.dumps(rows,indent=2)); print(json.dumps(rows))
