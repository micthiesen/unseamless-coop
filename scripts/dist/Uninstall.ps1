<#
.SYNOPSIS
  Restore your original ELDEN RING setup from the backup unseamless-coop's installer made.

.DESCRIPTION
  Puts back exactly what was there before you installed the test build: start_protected_game.exe,
  dinput8.dll (or removes ours if you had none), and your mods\ folder. Your REAL save (ER0000.sl2)
  and ERSC files were never modified, so they're left alone (the real save is only restored from the
  backup with -RestoreSaves). The one save we DID create — the isolated co-op test save ER0000.<ext>,
  seeded from your character at install — is removed by default (keep it with -KeepCoopSave). Your
  unseamless-coop\ logs folder is left in place; delete it by hand if you want it gone.

  Easiest: double-click Uninstall.cmd.

.PARAMETER GameDir
  Path to your ELDEN RING\Game folder. Skips auto-detection.

.PARAMETER RestoreSaves
  Also restore your real save folder from the backup. Off by default: we never write your real save,
  so your current one is the newest — restoring would roll it back. Only use this if something went wrong.

.PARAMETER KeepCoopSave
  Keep the isolated co-op test save (ER0000.<ext>) instead of removing it. Default: remove it (it's the
  test character we seeded at install; your real ER0000.sl2 is never touched either way).
#>
[CmdletBinding()]
param(
    [string]$GameDir,
    [switch]$RestoreSaves,
    [switch]$KeepCoopSave
)

. (Join-Path $PSScriptRoot '_lib.ps1')

try {
    $game = Find-GameDir $GameDir
    Write-Ok "game folder: $game"

    $elden     = Split-Path $game -Parent
    $backupDir = Join-Path $elden 'unseamless-coop-backup'
    $backupMan = Join-Path $backupDir 'BACKUP-MANIFEST.txt'
    $modDir    = Join-Path $game 'unseamless-coop'
    $marker    = Join-Path $modDir '.installed'

    if (-not (Test-Path $backupMan)) {
        throw "No backup at $backupDir, nothing to restore. (If you never ran Install.ps1, there's nothing to undo.)"
    }

    Write-Step "Restoring your original setup from $backupDir"

    # The backup manifest records, per managed file, whether the original existed: 'present' -> copy
    # it back; 'absent' -> we added one, so remove ours.
    foreach ($line in Get-Content $backupMan) {
        if ($line -match '^\s*(present|absent)\s+(\S.*)$') {
            $state = $Matches[1]; $name = $Matches[2].Trim()
            $dst = Join-Path $game $name
            if ($state -eq 'present') {
                $src = Join-Path $backupDir $name
                if (Test-Path $src) { Copy-Item $src $dst -Force; Write-Ok "restored $name" }
                else { Write-Warn "backup is missing $name; left your current $name in place (restore incomplete)" }
            } else {
                if (Test-Path $dst) { Remove-Item $dst -Force; Write-Ok "removed our $name (you had none)" }
            }
        }
    }

    # mods\: replace the current tree with the snapshot (exact original set). If the backup recorded
    # no mods\ tree, the player had none — clear ours.
    $gameMods = Join-Path $game 'mods'
    $bakMods  = Join-Path $backupDir 'mods'
    if (Test-Path $bakMods) {
        if (Test-Path $gameMods) { Remove-Item $gameMods -Recurse -Force }
        Copy-Item $bakMods $gameMods -Recurse -Force
        Write-Ok 'restored mods\ (exact original set)'
    } elseif (Test-Path $gameMods) {
        Remove-Item $gameMods -Recurse -Force
        Write-Ok 'removed mods\ (you had none originally)'
    }

    # Remove the isolated co-op test save we seeded at install (ER0000.<ext>), unless asked to keep it.
    # The extension comes from the install marker (records exactly what we created); fall back to this
    # bundle's MANIFEST. Read it before the marker is removed below. Your real ER0000.sl2 is never touched.
    $coopExt = ''
    if (Test-Path $marker) {
        foreach ($line in Get-Content $marker) {
            if ($line -match '^\s*save_extension:\s*(\S+)') { $coopExt = $Matches[1]; break }
        }
    }
    if (-not $coopExt) { $coopExt = (Read-BundleManifest (Join-Path $PSScriptRoot 'MANIFEST.txt')).SaveExt }
    if ($KeepCoopSave) {
        Write-Ok "kept the co-op test save ER0000.$coopExt (-KeepCoopSave)"
    } else {
        Remove-CoopSave $coopExt
    }

    if ($RestoreSaves) {
        $bakSaves = Join-Path $backupDir 'saves'
        if (Test-Path $bakSaves) {
            $dst = Get-EldenRingAppData
            Copy-Item (Join-Path $bakSaves '*') $dst -Recurse -Force
            Write-Ok 'restored saves (-RestoreSaves)'
        } else {
            Write-Warn 'no saves in the backup to restore'
        }
    }

    if (Test-Path $marker) { Remove-Item $marker -Force; Write-Ok 'removed our install marker' }

    # Consume the backup now that the original is restored. A later install must take a FRESH snapshot;
    # reusing this one would be stale and could wipe mods you add between sessions on the next uninstall.
    Remove-Item $backupDir -Recurse -Force
    Write-Ok 'removed the backup (a future install will snapshot again)'

    Write-Host ''
    Write-Step 'Done. Your original ELDEN RING setup is back.'
    Write-Host  "  Your real save (ER0000.sl2) was not touched$(if (-not $RestoreSaves) {'; use -RestoreSaves only if needed'})." -ForegroundColor Gray
    Write-Host  "  The unseamless-coop\ logs folder is left in place; delete it by hand if you like." -ForegroundColor Gray
}
catch {
    Write-Err $_.Exception.Message
    exit 1
}
