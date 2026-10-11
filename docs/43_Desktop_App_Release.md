# The Windows desktop app (ClaimGuard.exe): release notes

**Download:** the *Releases* page of the repository, release `desktop-v0.1.0`
(<https://github.com/PublisherX02/Claim_Guard/releases/tag/desktop-v0.1.0>). It is marked a pre-release: see "What is not tested" below.

| | |
|---|---|
| File | `ClaimGuard.exe`, one file, 28,165,007 bytes (about 27 MB) |
| SHA-256 | `673BC26AF43CE4987EDEED8E8D41374C7AC09A0CDFC74914EE3DD369C23E0135` |
| Built from | the Rust server and console as of commit `df3a770` (nothing under `rust/`, `src/` or `desktop/` has changed since) |
| Platform | Windows 10 or 11, 64-bit. Needs the Microsoft Edge WebView2 runtime, which Windows 11 includes and recent Windows 10 builds usually have |
| Signed | **No.** There is no code-signing certificate, so Windows shows a warning (below) |

## What it is
The reviewer console (the web page served by the server: inbox, claim review, queue, routing settings, users, audit and health panels) in its own window, so a reviewer does not need a browser tab. It is the same console, not a second program: everything the server shows or refuses, the window shows or refuses.

The package holds three things: the window (Python with `pywebview`, packed with PyInstaller), the Rust server `cg.exe`, and the rule files and 60 sample claims for the demo. No Python, Rust or database has to be installed on the PC.

## Two ways to use it
1. **Connect to a real server** (the normal use): start it, enter the server's address (`https://...`) once; it is remembered in `~/.claimguard` and the window then opens that address. Sign-in is the server's own (badge number, password, authenticator code).
2. **Local demo:** `ClaimGuard.exe --local-demo` starts the bundled server on this PC with sample accounts and claims, and opens the window on it. In this mode only, the window can sign in as a demo account through a small bridge, so a demonstration does not need an authenticator app. That bridge does not exist in the connect-to-a-server mode.

## The Windows warning
The file is not signed, so on first run Windows SmartScreen says "Windows protected your PC". Check the hash first, then choose **More info, then Run anyway**.

```powershell
Get-FileHash .\ClaimGuard.exe -Algorithm SHA256
# must print 673BC26AF43CE4987EDEED8E8D41374C7AC09A0CDFC74914EE3DD369C23E0135
```

If the hash differs, do not run the file. A hash printed next to the file only protects you if you got the hash from a place the file did not come from: read it in this document in the repository, not from the release page alone.

## Building it yourself
```powershell
cd rust; cargo build --release -p cg-cli; cd ..
powershell -ExecutionPolicy Bypass -File desktop\build_windows.ps1   # prints the SHA-256
```
The build is not bit-for-bit reproducible (PyInstaller embeds timestamps), so your hash will differ from the one above; compare behaviour, not hashes, for a self-built file. Details of the script are in `desktop/build_windows.ps1`.

## What is not tested
- The exe was built from the final code but **was not launched on a clean PC** (one without this project's tools) before release. The same console and server are covered by the 116-check penetration test, the browser XSS test and the 237 Rust tests (`docs/39`), but the packaged window itself has only been run on the development machine.
- Not code-signed; Windows only; no automatic updates (download a new release).
- Local demo mode is for demonstrations: its accounts and secrets are generated on the PC and are not suitable for real claims.
- It has not been tested against the production Docker stack on the target server (nothing has: `docs/40` section 9 is the checklist for that day).

## Related
Console and deployment overview: `docs/35`. The Rust server it bundles: `docs/38`. Security record: `docs/39`. Operations: `docs/40`.
