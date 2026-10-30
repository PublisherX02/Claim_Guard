# Server-side experiments

Scripts that measure ClaimGuard AI on the target server (Linux, RTX 3080 10 GB, 12 GB RAM) and produce the figures and the results chapter. The plan, the targets and the reasons are in [docs/65](../../docs/65_Server_Experiment_Plan.md); the generated results go to [docs/66](../../docs/66_Server_Experiment_Results.md).

| File | Experiment |
|---|---|
| `run_all.py` | runs the experiments in order with the sampler beside them, then builds the report |
| `sampler.py` | S0: GPU, container memory and CPU, host memory every few seconds |
| `s01_model.py` | S1: explanation model on the GPU, requests per minute and latency by concurrency |
| `s02_pipeline.py` | S2: claims per minute through the whole pipeline, and an optional soak |
| `s03_search.py` | S3: "Ask the rulebook" quality and latency |
| `s04_documents.py` | S4: documents to claims, time and accuracy |
| `s05_resilience.py` | S5: restart each service under load (interrupts the server; needs `--yes-restart`) |
| `s06_exposure.py` | S6: what the network sees (run from another machine) |
| `s07_integrity.py` | S7: health panel, audit chains, ledger, backup |
| `pack_results.py` | one zip of a run to send back, after a check that it holds no secret |
| `make_server_report.py` | figures to `docs/figures/server/` and the markdown report |
| `common.py` | sign-in with a one-time code, statistics, helpers (standard library only) |
| `mock_vllm.py` | a fake model server, to rehearse the scripts without a GPU |

```
export CG_URL=https://<server> CG_BADGE=<benchmark admin> CG_PASSWORD='...' CG_TOTP_SEED=<base32 seed>
export CG_VIEW_BADGE=<benchmark viewer, level 1> CG_VIEW_PASSWORD='...' CG_VIEW_TOTP_SEED=<base32 seed>
python3 experiments/server/run_all.py --run server_$(date +%Y%m%d) --docs /srv/docs_test --soak-minutes 30 --yes-restart
```

Results land in `outputs/server/<run>/`. Use a stack with no real data: the experiments add thousands of synthetic claims. Rehearse on a laptop with `--dry` (every file and the report are then marked as a rehearsal).
