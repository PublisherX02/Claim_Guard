# Laya fine-tune on ClaimGuard rule checks (Kaggle, 2x T4)

Proof of concept: fine-tune the open model **Laya** (`convaiinnovations/laya`, Apache 2.0, 421M parameters, ModernBERT-large + decision head) on **real labelled claims**. No simulated reviewers.

## What the model learns

One case = one claim + one payer rule. Answer = `PASS` / `FAIL` / `UNABLE_TO_ASSESS` / `NOT_APPLICABLE`, with a calibrated probability.
The label is the organizers' answer key (`data/*/expected_results.jsonl`). **These are rule results, not reviewer decisions**, and the deterministic engine already reproduces all of them (docs/46). So this tests whether a small model can learn the rulebook and say how sure it is. It is not a replacement for the engine, and it says nothing yet about reviewer behaviour (that needs real reviewer decisions).

| Split | Source | Cases | Notes |
|---|---|---|---|
| train | development, 400 claims | 2,486 | all FAIL / UNABLE / NOT_APPLICABLE kept; PASS capped at 1,500 of 5,014 |
| test | validation, 150 claims x 15 rules | 2,250 | complete, never trained on |
| stress | stress, 50 claims x 15 rules | 750 | complete, adversarial/edge cases |

Class mix in test: PASS 83.5%, so **the majority-class baseline is 0.835 accuracy**. Judge the model by macro-F1 and by FAIL recall, not accuracy alone. About 3% of training cases are cut by the 768-token state budget (the longest claims lose their tail).

## Files

| File | What |
|---|---|
| `kaggle_upload.zip` | the Kaggle **data** dataset: `train/test/stress.jsonl`, `summary.json`, `train_ddp.py`, `eval_laya.py`, `build_dataset.py` |
| `laya_base/` (run `download_laya.py`) | the Laya checkpoint (about 850 MB) for a **model** dataset |
| `laya_claimguard_2xT4_kaggle.ipynb` | the notebook: GPU check, build items, zero-shot baseline, 2-GPU training, evaluation, results zip |
| `build_dataset.py` | rebuilds the JSONL from `data/` and `rules/rules.json` (`python experiments/laya_kaggle/build_dataset.py`) |
| `train_ddp.py`, `eval_laya.py` | training (torchrun, DDP) and evaluation |

## Run it on Kaggle

1. kaggle.com -> **Datasets -> New Dataset**: upload `kaggle_upload.zip` (name it `claimguard-laya-data`). Upload the `laya_base` folder as a second dataset (`laya-base`); skip this if you will turn Internet on in the notebook instead.
2. **Code -> New Notebook -> File -> Import Notebook**: `laya_claimguard_2xT4_kaggle.ipynb`.
3. Notebook options: **Accelerator = GPU T4 x2**, add both datasets under **Add Input**, Internet on only if you skipped the model dataset.
4. Run all. Training takes roughly 30 to 60 minutes (a checkpoint is saved every epoch). Download `laya_claimguard_result.zip` and `eval_*.json` from the Output tab.

## What to look at in the result

- `eval_zero_shot.json` vs `eval_finetuned.json`: accuracy, macro-F1, FAIL recall, ECE.
- The **auto-accept table**: at confidence >= t, how many cases a person would not need to touch, how many of those are wrong, and how many real FAILs would be waved through as PASS. That last number is the price of any shortcut.
- Honest limits (from the model card): base Laya is near chance zero-shot and ships over-confident, so the temperatures fitted in training matter; Laya cannot see past its token budget; its confidence is not a safety guarantee.

## Result of the first run (Kaggle, 2x T4, 2026-10-15, notebook version 1)

One run, one seed, 6 epochs, about 41 minutes end to end (training 6 x ~5 min). Trained on 2,238 cases (248 held out for calibration), tested on claims it never saw.

| Split | Model | Accuracy | Macro-F1 | Always-PASS baseline | ECE (lower is better) |
|---|---|---|---|---|---|
| test (150 claims, 2,250 cases) | base Laya, zero-shot | 0.631 | 0.289 | 0.835 | 0.276 |
| test | **fine-tuned** | **0.971** | **0.915** | 0.835 | **0.027** |
| stress (50 claims, 750 cases) | base Laya, zero-shot | 0.595 | 0.316 | 0.799 | 0.242 |
| stress | **fine-tuned** | **0.973** | **0.907** | 0.799 | **0.035** |

Per class, test: PASS recall 0.990 / precision 0.976; NOT_APPLICABLE 0.978 / 1.000; UNABLE_TO_ASSESS 0.986 / 0.935; **FAIL recall 0.641 / precision 0.843** (75 of 117 real FAILs found; all 42 missed ones were called PASS).

Auto-accept table, test (answers at or above a calibrated confidence, accepted without a person):

| Min confidence | Cases accepted | Share | Wrong | Real FAILs accepted as PASS |
|---|---|---|---|---|
| 0 (everything) | 2,250 | 100% | 65 | 42 |
| 0.90 | 2,117 | 94.1% | 37 | 26 |
| 0.95 | 1,356 | 60.3% | 2 | 1 |
| 0.99 | 256 | 11.4% | 0 | 0 |

Weakest rules by accuracy on test: R007 0.887, R006 0.933, R014 0.933, R013 0.947, R012 0.953. Latency about 55 ms per case on a T4.

What this does and does not show:
- It does show a 421M-parameter open model learns the fictional rulebook from 2,486 real labelled cases, and that its calibration fits well (ECE 0.027).
- It does **not** beat the engine: the engine is exact on these labels, the model is not. As a stand-alone checker it is unsafe, because 36% of real FAILs are called PASS. Only the high-confidence slice is clean: at 0.95 it keeps 60% of cases with 2 wrong.
- The labels are rule results, not reviewer decisions, so nothing here says how reviewers behave.
- One seed and 150 test claims: the numbers carry real noise (FAIL has only 117 test cases). The 0.95 row rests on 2 errors.
