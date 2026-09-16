# Modeling quality tasks

`tasks.json` lists the versioned tasks; `targets/` holds their compact authored sources
(CC0-1.0, as are derived images). `reference-views/`, `fixture-report.json` and
`negative-control.json` are recorded fixture output (Mesa/EGL, 512 px); `baseline/` keeps
the pre-change prompt source and revision. `reliability/` holds archived sports-body
before/after and subdivision captures ([compare.html](reliability/compare.html)) and
scripted session transcripts.

```sh
# credential-free: compile/dimension/part/mesh-contract/export checks + matched renders
cargo run -p mogen --example quality_eval -- --out /tmp/mogen-quality [--no-render]

# opt-in live runs (spend API/subscription usage); use a new --out each time
cargo run -p mogen --example quality_eval -- --live --variant baseline|refined|guidance \
  --provider openai --model MODEL --repeats 3 --calls 12 --seconds 600 --out /tmp/run-a
python3 scripts/compare-quality.py /tmp/run-a /tmp/run-b --out /tmp/blinded

# scripted session smoke + resume (no credentials)
mogen session --prompt 'a box' --script benches/quality/reliability/scripted-responses.json --out-dir output/scripted --glb
mogen session --resume --script benches/quality/reliability/empty-responses.json --out-dir output/scripted --glb
```

Open `review.html` in the blinded output; keep `assignment-key.json` from reviewers until
judgments are saved. Optional task `candidate_source` renders another source with the
task's cameras (ignored in live mode). Fixtures prove the pipeline, not model quality.
