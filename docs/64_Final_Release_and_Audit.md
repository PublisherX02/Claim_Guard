# 64. The final release (v1.0.0): what was audited, what was run, what is left

This is the closing record of the project. It says what the last release contains, what was run against it before it was published, what was found and fixed on the way, and what is deliberately not done. As everywhere else in this repository, a claim here is either backed by a command you can run or is marked as not checked.

All data in the system is synthetic. A passed pre-check is not a payer approval.

## 1. What the release is

| | |
|---|---|
| Version | `desktop-v1.0.0`, marked the final release |
| Code | the `main` branch at the commit named on the release page |
| Server | the Rust build (`cg serve`), which embeds the console |
| Console | the UI/UX phase ([docs/62](62_UI_UX_Phase.md)): a workspace per role, the demo staff cards, the notification bell, Customize |
| Model | the requirements specification and its UML model are current as of 2026-10-29 ([docs/63](63_UML_Update_and_Specification_Addendum.md), [SPECIFICATION.md](../SPECIFICATION.md) section 10) |
| Installer | `ClaimGuard.exe` (30,218,381 bytes, SHA-256 `6A5F5D8C27082B7C39A75107EB1D67AEF5BC7654F5DF4A21A6F729CF7553757E`), one file, Windows 10 or 11, not code-signed (Windows warns on first run; the SHA-256 is on the release page and in [docs/43](43_Desktop_App_Release.md)) |

## 2. What was run before the release

| Check | Result |
|---|---|
| Python test suite (`pytest tests`) | 1,527 passed, 0 failed, 112 skipped in the final run (the skipped ones need a service that is not running on the build PC). Three failures in the first run were fixed, see section 3 |
| Rust test suite (`cargo test --workspace`, 2 jobs) | 457 tests passed, 0 failed |
| `cargo audit` | no vulnerabilities; one warning that `bincode` is unmaintained, accepted |
| Dependency alerts on GitHub | 110 open on 2026-10-29 (pillow, pypdf, transformers, sentence-transformers, jsonwebtoken); every one has its package raised past the fixed version, see [docs/32](32_Privacy_and_Security_Note.md) section 6.1 |
| Embedding service after the upgrade | the same bge-m3 weights give the same vectors on the old stack (transformers 4.47.1, torch 2.5.1) and the new one (transformers 5.10.4, torch 2.14.1): cosine similarity 1.000000 on ten texts in English, French and Arabic, so the 0.485 refusal threshold is unchanged. The Docker image itself was not built |
| Document reader after the upgrade | 18 of 18 checks of `rust/tools/ocr_service_test.py` on Python 3.12 with pillow 13.0.0 and pypdf 6.21.4 |
| The built `ClaimGuard.exe` | started with `--local-demo`: the bundled server answered `/healthz`, served the console with the final stylesheet and listed the demo staff. The window itself (the WebView2 shell) was not opened on screen |
| Console review script (`rust/tools/console_review.py`) | every role (viewer, reviewer, senior, administrator), every page, desktop and phone width: 0 issues on the final server. On the previous build it found the sideways scroll of section 3 |
| Console, in Chrome | every page of the reviewer and the supervisor workspace was opened on a freshly started server and used: sign-in with the staff cards, inbox, a claim with the reviewer copilot, a what-if, a correction saved as a new version, the notification that follows, rulebook search, Customize, queue overview, import, submit, batches, routing settings, payer rules, system health, audit and the signed ledger (all checkpoints verified), users, brand. No console errors |

## 3. What the audit found, and what was done

| Finding | Fix |
|---|---|
| The lines table of "Correct and recheck" could push the page 3 pixels wider than the window, which shows a page-level horizontal scrollbar | `html, body { overflow-x: clip }` in `style.css` |
| `test_the_console_holds_no_secret_or_remote_address` read every file in the console folder as text; a tool had left a binary file there | the stray file was removed; it was never tracked |
| `test_a_crashing_rule_becomes_unable_to_assess...` passes alone and failed in the full run, because another test file leaves logging switched off | the test now switches logging back on before it asserts on the warning |
| `test_a_fresh_system_is_not_down...` failed because the build PC's disk had 2% free and the health check correctly reported that the audit logs could not be written | not a code defect; it passes with free disk space. The check is doing what it is for |
| Two docs were both numbered 62 | the UML note is now 63 |
| transformers 5 does not import on torch 2.5.1 | the embedding image moves to torch 2.14.1 together with it |

## 4. Known limits (unchanged, stated again)

- Everything is synthetic. Real hospital paper, handwriting, stamps and Arabic documents are untested.
- The Windows app is not code-signed.
- The pen test, fuzz and mutation results in [docs/46](46_Rust_Test_Evaluation_Report.md) are from earlier builds of the same server; they were not re-run for this release.
- The embedding Docker image, the production deployment on a target server and a real vLLM model were not run on the build PC.
- The audit log is tamper-evident and signed, not write-once storage ([docs/58](58_Privacy_Erasure_Retention_Immutability.md)).
- `POST /claims/{id}/recheck` still answers 501; the working route is `/work/claims/{id}/recheck`.
- The browser inspection covered the reviewer and supervisor workspaces. The viewer and senior-reviewer workspaces, and the phone layout, were not walked through again in this pass (they are covered by [docs/62](62_UI_UX_Phase.md) and its screenshots).

## 5. Ideas for a later version (not in this release)

Nothing below is built. They are recorded so the next person does not have to rediscover them.

1. Sign the Windows app and publish it through a store or an update channel.
2. Run the embedding image build and the pen test again in CI on every release.
3. Run `rust/tools/console_review.py` (every role, desktop and phone width) in CI on each release.
4. Real operator questions for the rulebook search, reviewed by native Arabic and French speakers.
5. Move the demo-only routes behind a compile-time feature so a production binary cannot contain them.

## 6. How to reproduce

```
cd rust && cargo build --release -p cg-cli
rust/target/release/cg serve --demo --port 8765 --data-dir <empty folder>     # console at http://127.0.0.1:8765/ui/
python -m venv venv && venv/Scripts/pip install -r requirements-dev.txt pytest && venv/Scripts/python -m pytest tests
cd rust && cargo test --workspace -j 2 && cargo audit
```

## 7. After this release

`desktop-v1.1.0` (2026-10-31) added the administrator data center, the daily JSON report and the movable claim panels; see [docs/67](67_Admin_Data_Center_and_Movable_Panels.md) for what was tested and not tested. The audit above describes v1.0.0. For v1.1.0 the console review (every role, desktop and phone width) and the black-box pen test (125 checks, 0 failed) were re-run on the new build, and CI is green; the fuzz, mutation and Docker-stack tests were not repeated (docs/67 section 5). CI also began failing on four new `pyasn1` advisories; `pyasn1` 0.6.5 fixes them.
