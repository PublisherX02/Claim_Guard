# Language benchmark

Five candidate languages (Rust, Go, C#/.NET, Java, TypeScript on Node) against the current Python, on the same 20,000 generated claims.
The write-up, graphs and decision are in [docs/36_Language_Choice.md](../../docs/36_Language_Choice.md).

| Folder | What |
|---|---|
| `gen_corpus.py` | writes `corpus/claims.jsonl`, `corpus/expected.txt` (statuses from `tests/oracle.py`) and `corpus/pack/` (not committed: regenerate) |
| `rust/ go/ csharp/ java/ node/` | one port each of the 15 rules plus an HTTP endpoint with the same contract (`POST /evaluate`, `GET /healthz`) |
| `python/` | the control: the oracle on the same corpus, and a FastAPI/uvicorn server |
| `loadgen/` | a Go load generator and a probe of 22 hostile requests; every answer is checked against `expected.txt` |
| `run_bench.py` | builds everything, runs the batch and HTTP passes, writes `results/results.json` |
| `make_report.py`, `judged.json` | figures into `docs/figures/languages`, `results/summary.md`; the two judged criteria with their reasons |
| `results/` | `results.json` (final, 3 HTTP passes), `results_single_pass.json`, `results_first_pass.json` (before the null and idle-connection fixes) |

## Tool setup (Windows, what was used)

* Go 1.27 (zip unpacked to `C:\Users\moham\tools\go`), Rust stable with the **gnu** target (`rustup toolchain install stable-x86_64-pc-windows-gnu`, MSYS2 ucrt64 gcc first on PATH; no Visual Studio needed), .NET SDK 10, a JDK (27), Node 24.
* Java needs three jars in `java/lib/` (not committed): `jackson-core`, `jackson-databind`, `jackson-annotations` 2.18.2 from Maven Central.
* `node/`: `npm install` (decimal.js 10.4.3). `go/` and `loadgen/`: `go build`. `rust/`: `cargo +stable-x86_64-pc-windows-gnu build --release`.

## Run

```
python benchmarks/languages/gen_corpus.py
python benchmarks/languages/run_bench.py --batch-runs 5 --load-seconds 8 --http-reps 3   # about 40 minutes, keep the machine quiet
python benchmarks/languages/make_report.py
```

The machine matters more than the code: on one laptop the HTTP numbers varied up to 2x between passes. For a decision that depends on a gap smaller than that, run it on a quiet Linux server with the load generator on a second machine.
