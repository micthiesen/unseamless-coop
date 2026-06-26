<#
.SYNOPSIS
  Install the unseamless-coop test build into ELDEN RING, after snapshotting whatever is there now.

.DESCRIPTION
  For testing the mod with friends. Run it from inside the extracted bundle folder (it installs the
  dinput8.dll + start_protected_game.exe + shared config that sit next to it). It:
    1. Finds your ELDEN RING\Game folder (auto-detect via Steam, or -GameDir).
    2. Backs up your current setup ONCE to ELDEN RING\unseamless-coop-backup (start_protected_game.exe,
       dinput8.dll, mods\, your ERSC files, and your saves) — outside Game\, so a Steam "verify" or a
       game update can't wipe it. Idempotent: re-running never overwrites an existing backup.
    3. Installs our files and the shared config (your group's password), then verifies the copied
       binaries match the bundle's published hashes.
  Nothing here is permanent: Uninstall.ps1 puts your original setup back.

  Easiest launch: double-click Install.cmd (handles PowerShell's execution policy). If a file gets
  quarantined or blocked, see README-FRIENDS.txt ("If Windows blocks it").

.PARAMETER GameDir
  Path to your ELDEN RING\Game folder (or the ELDEN RING folder). Skips auto-detection.

.PARAMETER KeepMods
  Leave your existing mods\ folder in place. Default: install with an empty mods\ (your mods are
  still backed up and restored on uninstall) so co-op tests run a clean, reproducible load.

.PARAMETER KeepConfig
  Don't overwrite an existing unseamless_coop.toml. Default: install the bundle's shared config
  (so everyone's password matches), backing up any existing one to unseamless_coop.toml.bak.

.PARAMETER NoSaveBackup
  Skip copying your ELDEN RING saves into the backup. Default: copy them (belt-and-suspenders; we
  never write your saves — co-op uses its own save extension).
#>
[CmdletBinding()]
param(
    [string]$GameDir,
    [switch]$KeepMods,
    [switch]$KeepConfig,
    [switch]$NoSaveBackup
)

. (Join-Path $PSScriptRoot '_lib.ps1')

try {
    $bundle = $PSScriptRoot
    $manifest = Read-BundleManifest (Join-Path $bundle 'MANIFEST.txt')
    Write-Step "unseamless-coop installer, build $($manifest.BuildId) (v$($manifest.Version))"

    # Our shipped files must be here next to the script.
    foreach ($f in @('dinput8.dll', 'start_protected_game.exe')) {
        if (-not (Test-Path (Join-Path $bundle $f))) {
            throw "Bundle is incomplete: $f is missing next to this script. Re-extract the zip."
        }
    }

    $game = Find-GameDir $GameDir
    Write-Ok "game folder: $game"

    $elden     = Split-Path $game -Parent              # ...\ELDEN RING
    $backupDir = Join-Path $elden 'unseamless-coop-backup'
    $backupMan = Join-Path $backupDir 'BACKUP-MANIFEST.txt'
    $modDir    = Join-Path $game 'unseamless-coop'
    $marker    = Join-Path $modDir '.installed'

    # ---- backup once (guarded) ----------------------------------------------------------------
    if (Test-Path $backupMan) {
        Write-Ok "backup already present ($backupDir); leaving your original snapshot untouched"
    }
    elseif (Test-Path $marker) {
        throw @"
Our mod looks already installed (marker at $marker) but there's no backup at $backupDir.
Refusing to snapshot. It would record OUR files as your original. If you're sure the game folder
is back to your normal setup, delete that marker file and re-run; otherwise run Uninstall.ps1.
"@
    }
    else {
        Write-Step "Backing up your current setup -> $backupDir"
        # A prior attempt that failed partway never wrote $backupMan (it is written last), but may have
        # left partial copies behind; clear them so the recursive copies below can't nest (mods\mods).
        if (Test-Path $backupDir) { Remove-Item $backupDir -Recurse -Force }
        New-Item -ItemType Directory -Force -Path $backupDir | Out-Null
        $lines = @(
            '# unseamless-coop friend backup of your original ELDEN RING setup.',
            "date: $(Get-Date -Format o)",
            "source: $game",
            'files:'
        )
        foreach ($f in $script:ManagedFiles) {
            $src = Join-Path $game $f
            if (Test-Path $src) {
                Copy-Item $src (Join-Path $backupDir $f) -Force
                $lines += "  present  $f"
                Write-Ok "saved $f"
            } else {
                $lines += "  absent   $f"   # restore deletes ours instead of putting one back
            }
        }
        if (Test-Path (Join-Path $game 'mods')) {
            Copy-Item (Join-Path $game 'mods') (Join-Path $backupDir 'mods') -Recurse -Force
            $lines += '  tree     mods\'
            Write-Ok 'saved mods\'
        }
        # ERSC bits: pure safety copy (we never overwrite them, so uninstall won't touch them).
        $erscDir = Join-Path $backupDir 'ersc-safety'
        foreach ($e in @('ersc.dll', 'SeamlessCoop')) {
            $src = Join-Path $game $e
            if (Test-Path $src) {
                New-Item -ItemType Directory -Force -Path $erscDir | Out-Null
                Copy-Item $src $erscDir -Recurse -Force
                Write-Ok "saved $e (safety copy)"
            }
        }
        if (-not $NoSaveBackup) {
            $saves = Get-EldenRingAppData
            if (Test-Path $saves) {
                Copy-Item $saves (Join-Path $backupDir 'saves') -Recurse -Force
                Write-Ok 'saved your ELDEN RING save folder (safety copy; never modified by us)'
            }
        }
        $lines | Set-Content -Path $backupMan -Encoding UTF8
        Write-Ok "snapshot complete; this is your rollback point"
    }

    # ---- install ------------------------------------------------------------------------------
    Write-Step 'Installing the mod'
    Copy-Item (Join-Path $bundle 'dinput8.dll')              (Join-Path $game 'dinput8.dll') -Force
    Copy-Item (Join-Path $bundle 'start_protected_game.exe') (Join-Path $game 'start_protected_game.exe') -Force
    Write-Ok 'copied dinput8.dll + start_protected_game.exe'

    $gameMods = Join-Path $game 'mods'
    if ($KeepMods) {
        New-Item -ItemType Directory -Force -Path $gameMods | Out-Null
        Write-Ok 'kept your existing mods\ (-KeepMods)'
    } else {
        if (Test-Path $gameMods) { Remove-Item $gameMods -Recurse -Force }
        New-Item -ItemType Directory -Force -Path $gameMods | Out-Null
        Write-Ok 'mods\ left empty for a clean co-op load (restored on uninstall)'
    }

    New-Item -ItemType Directory -Force -Path $modDir | Out-Null
    $cfgDst = Join-Path $modDir 'unseamless_coop.toml'
    $cfgSrc = Join-Path $bundle 'unseamless_coop.toml'
    if ($KeepConfig -and (Test-Path $cfgDst)) {
        Write-Ok 'kept your existing unseamless_coop.toml (-KeepConfig)'
    } elseif (Test-Path $cfgSrc) {
        if (Test-Path $cfgDst) { Copy-Item $cfgDst "$cfgDst.bak" -Force; Write-Ok 'backed up your old config -> unseamless_coop.toml.bak' }
        Copy-Item $cfgSrc $cfgDst -Force
        Write-Ok 'installed the shared config (your group password)'
    }

    # ---- verify the copied binaries match the bundle's published hashes ------------------------
    # This is a copy-integrity check (did the bytes land intact), not an authenticity guarantee: the
    # hashes ship inside the same zip. Done before recording the install, so the marker never claims a
    # corrupt one. A mismatch throws -> the outer catch reports it and exits non-zero (Install.cmd pauses).
    Write-Step 'Verifying installed files'
    $allOk = $true
    foreach ($f in @('dinput8.dll', 'start_protected_game.exe')) {
        $want = $manifest.Files[$f]
        $got  = (Get-FileHash -Algorithm SHA256 (Join-Path $game $f)).Hash.ToLower()
        if (-not $want) { Write-Warn "$f has no hash in MANIFEST to check against"; continue }
        if ($got -eq $want) { Write-Ok "$f sha256 OK" }
        else { Write-Err "$f sha256 MISMATCH (expected $want, got $got)"; $allOk = $false }
    }
    if (-not $allOk) {
        throw "A copied file's sha256 didn't match the bundle MANIFEST. Re-extract the zip (Unblock it first) and run Install again before playing."
    }

    # Record the install only after verification passes.
    "build_id: $($manifest.BuildId)`nversion: $($manifest.Version)`ndate: $(Get-Date -Format o)" |
        Set-Content -Path $marker -Encoding UTF8

    Write-Host ''
    Write-Step "Done. Build $($manifest.BuildId) installed."
    Write-Host  "  Launch ELDEN RING from Steam as usual (Play)." -ForegroundColor Gray
    Write-Host  "  To undo everything: run Uninstall.cmd." -ForegroundColor Gray
    if ($manifest.BuildId -match '-dirty') {
        Write-Warn "This is a -dirty (uncommitted) test build (expected during testing)."
    }
}
catch {
    Write-Err $_.Exception.Message
    exit 1
}
