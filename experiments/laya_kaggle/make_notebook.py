"""Writes laya_claimguard_2xT4_kaggle.ipynb and the upload folder kaggle_upload/ (+ zip).
Run from experiments/laya_kaggle:  python make_notebook.py
"""
import json
import shutil
import zipfile
from pathlib import Path

here = Path(__file__).parent

cells = []


def md(s):
    cells.append({"cell_type": "markdown", "metadata": {}, "source": s.strip("\n").splitlines(True)})


def code(s):
    cells.append({"cell_type": "code", "metadata": {}, "execution_count": None, "outputs": [], "source": s.strip("\n").splitlines(True)})


md("""
# Laya fine-tune on ClaimGuard rule checks (Kaggle, 2x T4)

**What this is.** A proof of concept: fine-tune the open model Laya (Apache 2.0) on *real labelled claims* (no simulated reviewers).
Each case is one claim plus one payer rule; the answer is the rule result (`PASS` / `FAIL` / `UNABLE_TO_ASSESS` / `NOT_APPLICABLE`).
The labels are the organizers' answer key, **not reviewer decisions**, and our deterministic engine already gets them all right, so the question
here is whether a small model can learn the rulebook and report an honest confidence. It is not a replacement for the engine.

**Setup (once).** Notebook options -> Accelerator **GPU T4 x2**. Add as inputs: the data dataset (`claimguard-laya-data`, made from
`kaggle_upload.zip`) and, optionally, the model dataset (`laya-base`, from `download_laya.py`). With Internet on, the model is downloaded for you if the dataset is missing.
""")

code("""
import os, glob, subprocess, torch
n_gpu = torch.cuda.device_count()
print("visible GPUs:", n_gpu)
for i in range(n_gpu):
    p = torch.cuda.get_device_properties(i)
    print(f"  GPU {i}: {p.name} ({p.total_memory/1e9:.1f} GB)")
assert n_gpu >= 2, "Switch the accelerator to GPU T4 x2 (Notebook options -> Accelerator)."
os.environ["PYTORCH_CUDA_ALLOC_CONF"] = "expandable_segments:True"
""")

code("""
!pip install -q -U "laya>=0.4.8" "transformers>=4.48.0" safetensors huggingface_hub pandas scipy accelerate tabulate
import laya, transformers
print("laya", laya.__version__, "| transformers", transformers.__version__, "| torch", torch.__version__)
""")

code("""
# Locate the data folder and the Laya checkpoint.
def find_one(pattern):
    hits = glob.glob(pattern, recursive=True)
    return hits[0] if hits else None

train_jsonl = find_one("/kaggle/input/**/train.jsonl")
assert train_jsonl, "Add the claimguard-laya-data dataset as an input (Add Input -> your dataset)."
DATA_DIR = os.path.dirname(train_jsonl)
SCRIPT_DIR = os.path.dirname(find_one("/kaggle/input/**/train_ddp.py"))
print("data:", DATA_DIR, "| scripts:", SCRIPT_DIR)

weights = find_one("/kaggle/input/**/model.safetensors")
if weights:
    MODEL_DIR = os.path.dirname(weights)
else:  # needs Internet on
    from huggingface_hub import snapshot_download
    MODEL_DIR = snapshot_download("convaiinnovations/laya", local_dir="/kaggle/working/laya_base",
        allow_patterns=["model.safetensors", "rl_agent_config.json", "config.json", "*.py", "encoder/*", "tokenizer/*"])
print("model:", MODEL_DIR)
assert os.path.isdir(os.path.join(MODEL_DIR, "encoder")) and os.path.isdir(os.path.join(MODEL_DIR, "tokenizer"))
print(open(os.path.join(DATA_DIR, "summary.json")).read())
""")

code("""
# Turn the training cases into Laya training items; report how many claims are cut by the token budget.
import json, numpy as np
from transformers import AutoTokenizer
from laya.agent import _fix_tokenizer_config
from laya.train import items_from_rows, encode_state

_fix_tokenizer_config(MODEL_DIR)
tok = AutoTokenizer.from_pretrained(os.path.join(MODEL_DIR, "tokenizer"))
cfg = json.load(open(os.path.join(MODEL_DIR, "rl_agent_config.json")))
MAX_LEN, HEAD_MAX_LEN = 1024, 256

rows = [json.loads(l) for l in open(os.path.join(DATA_DIR, "train.jsonl"), encoding="utf-8") if l.strip()]
lens = [len(encode_state(tok, r["state"], 10**6)) for r in rows]
budget = MAX_LEN - HEAD_MAX_LEN
print(f"state tokens: median {np.median(lens):.0f}, p95 {np.percentile(lens,95):.0f}, max {max(lens)} | budget {budget} | cut: {np.mean([l>budget for l in lens])*100:.1f}% of cases")

items, skipped = items_from_rows(tok, rows, MAX_LEN, HEAD_MAX_LEN)
print(f"{len(items)} training items, skipped: {skipped}")
torch.save(items, "/kaggle/working/train_items.pt")
""")

md("## 1. Zero-shot baseline (base Laya, before any training)\nThe model card says base Laya is near chance on new tasks; this measures it on ours so the gain is not guessed.")

code("""
!python {SCRIPT_DIR}/eval_laya.py {MODEL_DIR} {DATA_DIR} /kaggle/working/eval_zero_shot.json --device cuda:0
""")

md("## 2. Fine-tune on both GPUs (DDP)\nDefaults: 6 epochs, effective batch 32, calibration temperatures fitted on 10% of the training items that are never trained on. Roughly 30-60 minutes on 2x T4; a checkpoint is saved after every epoch.")

code("""
OUTPUT_DIR = "/kaggle/working/laya_claimguard"
cmd = f"torchrun --standalone --nproc_per_node=2 {SCRIPT_DIR}/train_ddp.py {MODEL_DIR} {OUTPUT_DIR} /kaggle/working/train_items.pt"
print(cmd)
!{cmd}
""")

md("## 3. Evaluate the fine-tuned model\nOn the validation claims (`test`, 150 claims x 15 rules) and the stress claims (`stress`, 50 claims). No claim in these splits was seen in training.")

code("""
!python {SCRIPT_DIR}/eval_laya.py {OUTPUT_DIR} {DATA_DIR} /kaggle/working/eval_finetuned.json --device cuda:0
""")

code("""
import json, pandas as pd
z, f = json.load(open("/kaggle/working/eval_zero_shot.json")), json.load(open("/kaggle/working/eval_finetuned.json"))
rows = []
for split in ["test", "stress"]:
    for name, r in [("base (zero-shot)", z), ("fine-tuned", f)]:
        s = r[split]
        rows.append({"split": split, "model": name, "accuracy": s["accuracy"], "macro_f1": s["macro_f1"],
                     "majority_baseline": s["majority_class"]["accuracy"], "ECE": s["ece"], "p50_ms": s["latency_ms_p50"]})
print(pd.DataFrame(rows).to_markdown(index=False))
print("\\nPer-class recall/precision, fine-tuned, test split:")
print(pd.DataFrame(f["test"]["per_class"]).T.to_markdown())
print("\\nAuto-accept table (fine-tuned, test): answers at or above a confidence, accepted without a person")
print(pd.DataFrame(f["test"]["auto_accept_if_confidence_at_least"]).to_markdown(index=False))
print("\\nAccuracy by rule (fine-tuned, test):", f["test"]["accuracy_by_rule"])
""")

code("""
# Bundle everything worth downloading: weights, config, reports, predictions.
import shutil
shutil.make_archive("/kaggle/working/laya_claimguard_result", "zip", "/kaggle/working", "laya_claimguard")
print(os.listdir("/kaggle/working"))
""")

code("""
# Optional: publish the weights to your own Hugging Face repo (add HF_TOKEN under Add-ons -> Secrets first).
# from kaggle_secrets import UserSecretsClient
# from huggingface_hub import HfApi
# api = HfApi(token=UserSecretsClient().get_secret("HF_TOKEN"))
# api.create_repo("YOUR_USER/laya-claimguard", private=True, exist_ok=True)
# api.upload_folder(folder_path=OUTPUT_DIR, repo_id="YOUR_USER/laya-claimguard")
""")

nb = {"cells": cells, "metadata": {"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
                                   "language_info": {"name": "python"}}, "nbformat": 4, "nbformat_minor": 5}
(here / "laya_claimguard_2xT4_kaggle.ipynb").write_text(json.dumps(nb, indent=1), encoding="utf-8")

up = here / "kaggle_upload"
if up.exists():
    shutil.rmtree(up)
up.mkdir()
for n in ["train.jsonl", "test.jsonl", "stress.jsonl", "summary.json"]:
    shutil.copy(here / "dataset" / n, up / n)
for n in ["train_ddp.py", "eval_laya.py", "build_dataset.py"]:
    shutil.copy(here / n, up / n)
with zipfile.ZipFile(here / "kaggle_upload.zip", "w", zipfile.ZIP_DEFLATED) as z:
    for p in sorted(up.iterdir()):
        z.write(p, p.name)
print("notebook + kaggle_upload.zip written")
