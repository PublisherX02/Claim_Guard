# Build ClaimGuard.exe: the console window as one Windows program (no Python needed on the target PC).
#   powershell -ExecutionPolicy Bypass -File desktop\build_windows.ps1
# The packaged app connects to a server (https://...). The local demo needs the source checkout and is not packed in.
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$work = Join-Path $here 'build'
uv venv "$work\venv" --python 3.12
uv pip install --python "$work\venv\Scripts\python.exe" -r "$here\requirements-desktop.txt" pyinstaller==6.16.0
& "$work\venv\Scripts\pyinstaller.exe" --noconfirm --onefile --windowed --name ClaimGuard `
    --distpath "$here\dist" --workpath "$work\pyi" --specpath $work "$here\claimguard_desktop.py"
Get-FileHash "$here\dist\ClaimGuard.exe" -Algorithm SHA256
