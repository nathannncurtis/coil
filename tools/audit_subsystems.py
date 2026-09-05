"""Build real multi-entry exes and inspect their GUI/console headers. Select Coil via PYTHONPATH."""
import argparse
import json,os,subprocess,sys
from pathlib import Path
import pefile
parser=argparse.ArgumentParser(); parser.add_argument('--output',type=Path,required=True); out=parser.parse_args().output.resolve()
if out.exists(): parser.error('Use a new output directory')
out.mkdir(parents=True)
p=out/'project';p.mkdir(exist_ok=True)
(p/'main.py').write_text('print("main")')
(p/'gui.py').write_text('if False: import tkinter\nprint("gui")')
(p/'coil.toml').write_text('[build.dependencies]\nauto=false\n')
rows=[]
for label,flags in [('auto',[]),('force_console',['--console'])]:
 cmd=[sys.executable,'-m','coil','build',str(p),'--mode','bundled','--entry','main.py','--entry','gui.py','--python','3.12.10','--name','Probe','--output',str(out/label),*flags]
 r=subprocess.run(cmd,capture_output=True,text=True)
 exes={}
 for exe in (out/label/'Probe').glob('*.exe'):
  pe=pefile.PE(str(exe));exes[exe.name]=pe.OPTIONAL_HEADER.Subsystem;pe.close()
 rows.append(dict(case=label,exit=r.returncode,exes=exes))
(out/'results.json').write_text(json.dumps(rows,indent=2));print(json.dumps(rows))
