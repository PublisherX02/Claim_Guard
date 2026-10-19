"""Download the Laya English checkpoint (Apache 2.0) to a local folder, ready to upload as a Kaggle dataset.

Only the files training needs are fetched (about 850 MB): model.safetensors, rl_agent_config.json,
encoder/, tokenizer/ and the python helpers. The multilingual and typed-decisions sub-checkpoints
(~1.5 GB more) are skipped.

    pip install huggingface_hub
    python experiments/laya_kaggle/download_laya.py [--out laya_base]

Then, on kaggle.com: Datasets -> New Dataset -> drag the `laya_base` folder in -> name it `laya-base`.
In the notebook, "Add Input" -> that dataset; it appears under /kaggle/input/laya-base. (If the notebook has
Internet turned on, it can also download the model by itself; see the notebook's first cells.)
"""
import argparse
from pathlib import Path

from huggingface_hub import snapshot_download

ap = argparse.ArgumentParser()
ap.add_argument("--out", default="laya_base")
ap.add_argument("--repo", default="convaiinnovations/laya")
a = ap.parse_args()

path = snapshot_download(
    a.repo,
    local_dir=a.out,
    allow_patterns=["model.safetensors", "rl_agent_config.json", "config.json", "README.md", "*.py", "encoder/*", "tokenizer/*"],
)
files = sorted(p for p in Path(path).rglob("*") if p.is_file())
total = sum(p.stat().st_size for p in files)
print(f"downloaded {len(files)} files, {total / 1e6:.0f} MB to {path}")
for p in files:
    print(f"  {p.relative_to(path)}  {p.stat().st_size / 1e6:.1f} MB")
need = ["model.safetensors", "rl_agent_config.json"]
missing = [n for n in need if not (Path(path) / n).exists()] + [d for d in ["encoder", "tokenizer"] if not (Path(path) / d).is_dir()]
assert not missing, f"missing: {missing}"
print("ok")
