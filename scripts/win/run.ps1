# Wrapper that the scheduled task runs INSIDE the interactive desktop session. A DXGI swapchain needs a
# real window station / desktop, which an SSH (non-interactive) session lacks — CreateSwapChainForHwnd
# there returns DXGI_ERROR_NOT_CURRENTLY_AVAILABLE (0x887A0022). So win.sh's `run` drives the harness
# via an Interactive-principal scheduled task that lands in the logged-on desktop, and that task runs
# THIS. Pushed into the staging dir by win.sh; it reads its own dir from $PSScriptRoot so it tracks
# WIN_REMOTE_DIR. Per-run env knobs come from knobs.txt (KEY=VALUE lines) the host writes before each
# run; stdout/err + exit code land next to the harness log for the host to pull.
$dir = $PSScriptRoot
$env:DX12_HARNESS_LOG = Join-Path $dir 'dx12-harness.log'
$kf = Join-Path $dir 'knobs.txt'
if (Test-Path $kf) {
  Get-Content $kf | ForEach-Object {
    if ($_ -match '^\s*([A-Za-z0-9_]+)\s*=\s*(.*)$') { Set-Item -Path ("Env:" + $matches[1]) -Value $matches[2] }
  }
}
& (Join-Path $dir 'dx12-harness.exe') *> (Join-Path $dir 'run-out.txt')
"exit=$LASTEXITCODE" | Out-File -FilePath (Join-Path $dir 'exitcode.txt') -Encoding ascii
