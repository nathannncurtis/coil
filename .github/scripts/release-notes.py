"""Prints release notes for a tag: its commit subjects since the previous tag, summarised by Workers AI.
Prints nothing when there is nothing to say or the call fails. Usage: release-notes.py TAG"""
import json, os, re, subprocess, sys, urllib.request

tag = sys.argv[1]
git = lambda *a: subprocess.run(["git", *a], capture_output=True, text=True, check=True).stdout
paperwork = re.compile(r"^(docs/|tests/|\.github/)|\.md$")

tags = sorted(git("tag", "--list", "v*").split(), key=lambda t: [int(x) for x in t[1:].split(".")])
prev = tags[tags.index(tag) - 1] if tag in tags and tags.index(tag) else None
changes = []
for block in git("log", "--no-merges", "--format=\x1e%s", "--name-only", f"{prev}..{tag}" if prev else tag).split("\x1e")[1:]:
    subject, *files = [l for l in block.split("\n") if l.strip()]
    if not (files and all(paperwork.search(f) for f in files)):
        changes.append(subject.strip())
if not changes:
    sys.exit()

prompt = ("Write release notes for Coil, a Python-to-Windows-executable compiler, from these commit subjects. Group them "
          "under short headings for the areas that changed (Compiler, Build and bundling, Windows launcher, Tooling), "
          "skipping areas with no changes. One short plain bullet per change, describing what changed for someone using "
          "Coil. No names, no attribution, no introduction, no closing line.\n\n"
          + "\n".join(f"- {c}" for c in changes))
req = urllib.request.Request(
    f"https://api.cloudflare.com/client/v4/accounts/{os.environ['CLOUDFLARE_ACCOUNT_ID']}/ai/run/@cf/meta/llama-3.3-70b-instruct-fp8-fast",
    json.dumps({"messages": [{"role": "user", "content": prompt}], "max_tokens": 1024}).encode(),
    {"Authorization": f"Bearer {os.environ['CLOUDFLARE_API_TOKEN']}", "Content-Type": "application/json"})
try:
    with urllib.request.urlopen(req, timeout=60) as r:
        text = (json.load(r).get("result") or {}).get("response") or ""
except Exception as e:
    print(f"::warning::Release notes not written: {e}", file=sys.stderr)
    sys.exit()
print(text.replace("@", "").strip())
