# The Windows desktop app (ClaimGuard.exe): release notes

**Final release: `desktop-v1.0.0`** (30,218,381 bytes, SHA-256 `6A5F5D8C27082B7C39A75107EB1D67AEF5BC7654F5DF4A21A6F729CF7553757E`), <https://github.com/PublisherX02/Claim_Guard/releases/tag/desktop-v1.0.0>. Built from the Rust server of `main` at the release commit, with the UI/UX phase console ([docs/62](62_UI_UX_Phase.md)). Checked by launching `ClaimGuard.exe --local-demo` on the build PC: its bundled server answered `/healthz`, served the new console and listed the demo staff. Not code-signed; Windows warns on first run. What was tested for this release, and what was not, is in [docs/64](64_Final_Release_and_Audit.md). The sections below describe the earlier releases.

**Current release: `desktop-v0.3.0`** (29,042,447 bytes, SHA-256 `AB53BA400CFD8AB2556BEE29ABF6F1278D9E48997D8DFD0BF657BB747A361F06`), <https://github.com/PublisherX02/Claim_Guard/releases/tag/desktop-v0.3.0>. It is marked a pre-release: see "What is not tested" below. It replaces `desktop-v0.2.0` (built but never attached to its release page, so it could not be downloaded) and `desktop-v0.1.0` (28,165,007 bytes, SHA-256 `673BC26AF43CE4987EDEED8E8D41374C7AC09A0CDFC74914EE3DD369C23E0135`), which stays on the Releases page for reference; use v0.3.0.

**Latest release: `desktop-v0.4.0`** (29,260,113 bytes, SHA-256 `EB42D837E14EAC5B874C15B6FCBBD2C1C77148B85C72772D9BA9611220645934`), built from the Rust server of commit `2adf2b4` (every later commit changes documents, experiments and the embedding script only). New since v0.3.0: Arabic claim intake and the Arabic field dictionary; asynchronous batch intake with a progress display; the upgraded rulebook search (bge-m3 when an embedding service is configured, Arabic and French questions, refusal that names an out-of-scope topic). Still unsigned, Windows only, not run on a clean PC.

## What is new since v0.2.0
- **Reviewer copilot, what-if and recheck** (docs/51): deterministic correction suggestions, a dry run that shows how a proposed change would affect the findings, and "Correct and recheck" as a new claim version, all traced.
- **Fewer touches** (docs/50): auto-clear of green claims with a random audit sample and auto-request of missing information (both off by default), one-click and bulk confirm. Clearance is worded "pre-validation clear", never approval.
- **Feedback report by service** (docs/49 section 7): the dismissal counts per rule are also split by the service code and modifier of the line, shown in Queue overview.
- Sign-in hardening, backup and key rotation, and the explanation model served with vLLM in production (designed, not benchmarked on this PC).


| | |
|---|---|
| File | `ClaimGuard.exe`, one file, 28,654,298 bytes (about 27 MB) |
| SHA-256 | `4B7C00C840F290F59D74E7D3404D4E5A7EB248DFC00B93A556FA9EB9C471B753` |
| Built from | the Rust server and console of branch `rust-rewrite` at commit `8eb5cf8` (every later commit changes documents only) |
| Platform | Windows 10 or 11, 64-bit. Needs the Microsoft Edge WebView2 runtime, which Windows 11 includes and recent Windows 10 builds usually have |
| Signed | **No.** There is no code-signing certificate, so Windows shows a warning (below) |

## What is new since v0.1.0
The window and the bundled server now include everything built after the first release; all of it is in the same console, behind the same sign-in and permissions:

- **Import from files** (administrators): a claim can be imported from a PDF, a scan, a phone photo, FHIR, CSV or JSONL. Files pass the server's gate (type read from the bytes, size and picture caps, PDFs with active content or encryption refused, polyglot files and files with an executable appended refused). The text of a PDF or picture is read by an isolated reader service (a separate container, `deploy/ocr`; **in the desktop demo no reader is attached**, so PDF and picture import answers 503 `document_reader_not_configured` while FHIR, CSV and JSONL import works). The result is an editable draft shown beside the page, and a person confirms it before it is submitted. [docs/45 section 1](45_Document_Intake_Ledger_and_Streaming.md).
- **Payer rules** page: limits, provider lists, required documents, submission window and currency as versioned data; a change needs a reason and, with two administrators, a different administrator's approval with a fresh authenticator code. [docs/45 section 4](45_Document_Intake_Ledger_and_Streaming.md).
- **Ask the rulebook** page: quoted answers with their source, from the 15 rules and the payer policies in force; refuses what it cannot answer. [docs/45 section 5](45_Document_Intake_Ledger_and_Streaming.md).
- **Live updates:** the inbox and queue refresh within seconds of a change instead of on a 20-second poll.
- **Signed audit ledger:** checkpoints and inclusion proofs on the audit page. [docs/45 section 2](45_Document_Intake_Ledger_and_Streaming.md).
- Console fixes found in a real-browser review of every page for every role at desktop and phone width (a lost Send button, checkbox width, layout), and a content-security policy adjusted so the page preview can be shown.

## What it is
The reviewer console (the web page served by the server: inbox, claim review, queue, routing settings, users, audit and health panels, and the pages above) in its own window, so a reviewer does not need a browser tab. It is the same console, not a second program: everything the server shows or refuses, the window shows or refuses.

The package holds three things: the window (Python with `pywebview`, packed with PyInstaller), the Rust server `cg.exe`, and the rule files and sample claims for the demo. No Python, Rust or database has to be installed on the PC.

## Two ways to use it
1. **Connect to a real server** (the normal use): start it, enter the server's address (`https://...`) once; it is remembered in `~/.claimguard` and the window then opens that address. Sign-in is the server's own (badge number, password, authenticator code).
2. **Local demo:** `ClaimGuard.exe --local-demo` starts the bundled server on this PC with sample accounts and claims, and opens the window on it. In this mode only, the window can sign in as a demo account through a small bridge, so a demonstration does not need an authenticator app. That bridge does not exist in the connect-to-a-server mode.

## The Windows warning
The file is not signed, so on first run Windows SmartScreen says "Windows protected your PC". Check the hash first, then choose **More info, then Run anyway**.

```powershell
Get-FileHash .\ClaimGuard.exe -Algorithm SHA256
# must print 4B7C00C840F290F59D74E7D3404D4E5A7EB248DFC00B93A556FA9EB9C471B753
```

If the hash differs, do not run the file. A hash printed next to the file only protects you if you got the hash from a place the file did not come from: read it in this document in the repository, not from the release page alone.

## Building it yourself
```powershell
cd rust; cargo build --release -p cg-cli; cd ..
powershell -ExecutionPolicy Bypass -File desktop\build_windows.ps1   # prints the SHA-256
```
The build is not bit-for-bit reproducible (PyInstaller embeds timestamps), so your hash will differ from the one above; compare behaviour, not hashes, for a self-built file. Details of the script are in `desktop/build_windows.ps1`.

## What was tested
- The exe was **launched** in `--local-demo` mode on the development PC: the window process started, the bundled server came up on a local port and answered its health check with 200.
- The server and console it contains are covered by: 306 Rust tests (run against real MongoDB and Redis, with the database tests required rather than skipped), the real-Chrome review of every page (0 issues), and the black-box checks listed in [docs/45](45_Document_Intake_Ledger_and_Streaming.md) (14 ledger, 16 payer-rule, 21 live-stream, 8 rulebook-question, 9 import-flow). The 116-check penetration test ([docs/39](39_Rust_Security_Results.md)) was last run before the upload gate and the rulebook search were added; those two have their own tests.

## What is not tested
- The exe was **not launched on a clean PC** (one without this project's tools), and its window was not driven by hand after packaging; only the start-up and the server health check were verified.
- Not code-signed; Windows only; no automatic updates (download a new release).
- PDF and picture import needs the reader container, which the desktop demo does not include (see above); the reader image is built and checked separately ([docs/45](45_Document_Intake_Ledger_and_Streaming.md), "Reader image, built and checked").
- Local demo mode is for demonstrations: its accounts and secrets are generated on the PC and are not suitable for real claims.
- It has not been tested against the production Docker stack on the target server (nothing has: [docs/40](40_Rust_Operations_Runbook.md) section 9 is the checklist for that day).

## Related
Console and deployment overview: [docs/35](35_Console_Desktop_App_and_Deployment.md). The Rust server it bundles: [docs/38](38_Rust_Port.md). Security record: [docs/39](39_Rust_Security_Results.md). Operations: [docs/40](40_Rust_Operations_Runbook.md). Files, ledger, live updates, payer rules, rulebook search: [docs/45](45_Document_Intake_Ledger_and_Streaming.md). Test evaluation report: [docs/46](46_Rust_Test_Evaluation_Report.md).
