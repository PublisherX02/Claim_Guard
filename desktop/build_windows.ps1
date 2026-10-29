# Build ClaimGuard.exe: the console window as one Windows program (no Python, Rust or database needed on the target PC).
#   powershell -ExecutionPolicy Bypass -File desktop\build_windows.ps1
# The package holds the window plus the Rust server (cg.exe), the rule files and the sample claims, so the "local demo" runs from the
# exe alone; connecting to a real server (https://...) works as before. Build the server first: cd rust; cargo build --release -p cg-cli
param([string]$Out = 'dist')   # output folder under desktop\ (use dist_new to leave a running dist\ClaimGuard.exe alone)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Split-Path -Parent $here
$work = Join-Path $here 'build'
$cg = Join-Path $repo 'rust\target\release\cg.exe'
if (-not (Test-Path $cg)) { throw "build the Rust server first (cd rust; cargo build --release -p cg-cli): $cg is missing" }
$engine = Join-Path $work 'engine'
if (Test-Path $engine) { Remove-Item -Recurse -Force $engine }
New-Item -ItemType Directory -Force "$engine\data\development" | Out-Null
Copy-Item $cg "$engine\cg.exe"
Copy-Item -Recurse (Join-Path $repo 'rules') "$engine\rules"
Copy-Item (Join-Path $repo 'data\development\claims.jsonl') "$engine\data\development\claims.jsonl"
$uv = Get-Command uv -ErrorAction SilentlyContinue   # uv when installed, otherwise Python 3.12's own venv and pip
if (-not (Test-Path "$work\venv\Scripts\python.exe")) { if ($uv) { uv venv "$work\venv" --python 3.12 } else { py -3.12 -m venv "$work\venv" } }
if ($uv) { uv pip install --python "$work\venv\Scripts\python.exe" -r "$here\requirements-desktop.txt" pyinstaller==6.16.0 }
else { & "$work\venv\Scripts\python.exe" -m pip install -q -r "$here\requirements-desktop.txt" pyinstaller==6.16.0 }
if ($LASTEXITCODE) { throw "installing the build requirements failed" }
& "$work\venv\Scripts\pyinstaller.exe" --noconfirm --onefile --windowed --name ClaimGuard `
    --add-data "$engine;engine" `
    --distpath "$here\$Out" --workpath "$work\pyi" --specpath $work "$here\claimguard_desktop.py"
Get-FileHash "$here\$Out\ClaimGuard.exe" -Algorithm SHA256
