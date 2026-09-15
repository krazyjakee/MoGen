#!/usr/bin/env python3
"""Create a blinded, self-contained A/B review from two quality_eval artifacts."""
import argparse
import hashlib
import html
import json
from pathlib import Path
import shutil

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("left", type=Path)
parser.add_argument("right", type=Path)
parser.add_argument("--out", required=True, type=Path)
parser.add_argument("--seed", default="42")
args = parser.parse_args()
reports = [json.loads((root / "report.json").read_text()) for root in (args.left, args.right)]
runs = [{(r["task"], r["repeat"]): r for r in report["runs"]} for report in reports]
if runs[0].keys() != runs[1].keys():
    parser.error("Artifacts must contain the same tasks and repeat counts")
for key in runs[0]:
    a, b = (run[key] for run in runs)
    for field in ("mode", "provider", "model", "seed", "temperature", "thinking", "limits", "manifest_sha256"):
        if a[field] != b[field]:
            parser.error(f"{key}: unmatched {field}; use comparable settings/budgets")
args.out.mkdir(parents=True, exist_ok=False)
page = ["""<!doctype html><meta charset=utf-8><title>MoGen blinded comparison</title>
<style>body{font:16px system-ui;max-width:1000px;margin:30px auto}img{width:30%}section{border-top:1px solid #aaa;padding:16px}textarea{display:block;width:90%;height:70px}</style>
<h1>Blinded asset comparison</h1><p>Left: target. Center: A. Right: B. Evaluate silhouette, proportions, completeness, joints/negative space, finish, materials/UV scale and reference fidelity. Record uncertainty and disagreement. Do not open assignment-key.json until judgments are saved.</p>
<label>Evaluator <input id=evaluator></label><button onclick=save()>Download judgments</button>"""]
assignments = []
for task, repeat in sorted(runs[0]):
    identifier = f"{task}-{repeat}"
    swap = hashlib.sha256(f"{args.seed}-{identifier}".encode()).digest()[0] % 2
    assignments.append({"id": identifier, "A": str([args.left, args.right][swap]), "B": str([args.left, args.right][1-swap])})
    section = args.out / identifier
    section.mkdir()
    page.append(f"<section data-id='{html.escape(identifier)}'><h2>{html.escape(identifier)}</h2>")
    for view in ("front", "side", "back", "three_quarter", "presentation"):
        for label, root, kind in (("target", args.left, "reference"), ("A", [args.left, args.right][swap], "candidate"), ("B", [args.left, args.right][1-swap], "candidate")):
            src = root / identifier / f"{kind}-{view}.png"
            dest = section / f"{label}-{view}.png"
            if src.exists():
                shutil.copyfile(src, dest)
                page.append(f"<img alt='{label} {view}' src='{identifier}/{dest.name}'>")
            else:
                page.append(f"<span>Missing {label} {view}; record render/asset failure.</span>")
        page.append("<br>")
    page.append("<label>Preference <select><option value=''>Choose</option><option>A</option><option>B</option><option>Tie</option><option>Neither</option><option>Uncertain</option></select></label><textarea placeholder='Observations, defects, uncertainty and disagreement'></textarea></section>")
page.append("""<script>
function save(){const data={evaluator:document.querySelector('#evaluator').value,judgments:[...document.querySelectorAll('section')].map(s=>({id:s.dataset.id,preference:s.querySelector('select').value,notes:s.querySelector('textarea').value}))};const a=document.createElement('a');a.href=URL.createObjectURL(new Blob([JSON.stringify(data,null,2)],{type:'application/json'}));a.download='judgments.json';a.click();URL.revokeObjectURL(a.href)}
</script>""")
(args.out / "review.html").write_text("\n".join(page))
(args.out / "assignment-key.json").write_text(json.dumps(assignments, indent=2))
(args.out / "comparison-metadata.json").write_text(json.dumps({"seed": args.seed, "reports": reports}, indent=2))
print(args.out / "review.html")
