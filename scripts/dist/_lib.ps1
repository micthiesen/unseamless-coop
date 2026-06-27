# _lib.ps1 — shared helpers for the unseamless-coop friend installer (Install.ps1 / Uninstall.ps1).
#
# Dot-sourced by both scripts. Holds the bits that must agree between install and uninstall: where
# the game folder is, what surface we back up, and the bundle MANIFEST format. Mirrors the safety
# model of scripts/rig.sh (snapshot-once outside the game folder; restore is explicit only).

$ErrorActionPreference = 'Stop'

# ---- output ------------------------------------------------------------------------------------
function Write-Step($m) { Write-Host "==> $m" -ForegroundColor Cyan }
function Write-Ok($m)   { Write-Host "  + $m" -ForegroundColor Green }
function Write-Warn($m) { Write-Host "  ! $m" -ForegroundColor Yellow }
function Write-Err($m)  { Write-Host "ERROR: $m" -ForegroundColor Red }

# Flat game-folder files we back up and restore (not all are written on install: only dinput8.dll
# and start_protected_game.exe are; mod_loader_config.ini is captured defensively). The mods\ tree is
# handled separately in Install/Uninstall, not via this array. ERSC's own files (ersc.dll,
# SeamlessCoop\) are NOT here: like the rig, we never overwrite them (replacing the launcher just
# leaves ERSC dormant), so they need no restore; Install.ps1 still copies them as a safety snapshot.
$script:ManagedFiles = @('start_protected_game.exe', 'dinput8.dll', 'mod_loader_config.ini')

# ---- game-folder detection ---------------------------------------------------------------------
# Resolve the Steam install path from the registry (HKCU first, then HKLM 32-bit view).
function Get-SteamPath {
    foreach ($p in @(
        @{ Path = 'HKCU:\Software\Valve\Steam';                 Name = 'SteamPath' },
        @{ Path = 'HKLM:\SOFTWARE\WOW6432Node\Valve\Steam';     Name = 'InstallPath' },
        @{ Path = 'HKLM:\SOFTWARE\Valve\Steam';                 Name = 'InstallPath' }
    )) {
        try {
            $v = (Get-ItemProperty -Path $p.Path -Name $p.Name -ErrorAction Stop).$($p.Name)
            if ($v) { return $v -replace '/', '\' }
        } catch { }
    }
    return $null
}

# Parse libraryfolders.vdf for every Steam library root. The VDF stores Windows paths with the
# backslashes doubled ("D:\\SteamLibrary"), so unescape \\ -> \.
function Get-SteamLibraries([string]$SteamPath) {
    $libs = @()
    if (-not $SteamPath) { return $libs }   # no Steam in the registry -> let Find-GameDir fall back to the prompt
    $libs += $SteamPath
    $vdf = Join-Path $SteamPath 'steamapps\libraryfolders.vdf'
    if (Test-Path $vdf) {
        foreach ($m in [regex]::Matches((Get-Content -Raw $vdf), '"path"\s+"([^"]+)"')) {
            $libs += ($m.Groups[1].Value -replace '\\\\', '\')
        }
    }
    return $libs | Where-Object { $_ } | Select-Object -Unique
}

# Find "...\ELDEN RING\Game" (the folder with eldenring.exe). Order: explicit override, then every
# Steam library, then an interactive prompt. Throws if nothing valid is found.
function Find-GameDir([string]$Override) {
    if ($Override) {
        $g = $Override
        if (Test-Path (Join-Path $g 'eldenring.exe')) { return (Resolve-Path $g).Path }
        if (Test-Path (Join-Path $g 'Game\eldenring.exe')) { return (Resolve-Path (Join-Path $g 'Game')).Path }
        throw "No eldenring.exe under the path you gave: $g"
    }
    foreach ($lib in (Get-SteamLibraries (Get-SteamPath))) {
        $cand = Join-Path $lib 'steamapps\common\ELDEN RING\Game'
        if (Test-Path (Join-Path $cand 'eldenring.exe')) { return (Resolve-Path $cand).Path }
    }
    Write-Warn "Could not auto-detect your ELDEN RING\Game folder from Steam."
    Write-Host  "Paste the full path to it (the folder containing eldenring.exe), or press Enter to cancel:"
    $entered = Read-Host 'Game folder'
    if ($entered) {
        $entered = $entered.Trim('"')
        if (Test-Path (Join-Path $entered 'eldenring.exe')) { return (Resolve-Path $entered).Path }
        throw "No eldenring.exe under: $entered"
    }
    throw 'No game folder; cancelled.'
}

# ELDEN RING's per-user save root (%APPDATA%\EldenRing\<steamid>\ER0000.*). Used for the safety
# save backup; we never write here (we use our own save extension), so this is paranoia only.
function Get-EldenRingAppData { return (Join-Path $env:APPDATA 'EldenRing') }

# ---- bundle manifest -------------------------------------------------------------------------
# Read the MANIFEST.txt that ships in the bundle (written by scripts/rig.sh package): the build id
# and the expected sha256 of each shipped binary, for post-copy verification.
function Read-BundleManifest([string]$Path) {
    $info = @{ BuildId = '(unknown)'; Version = '(unknown)'; SaveExt = ''; Files = @{} }
    if (-not (Test-Path $Path)) { return $info }
    foreach ($line in Get-Content $Path) {
        if ($line -match '^\s*build_id:\s*(\S+)')       { $info.BuildId = $Matches[1]; continue }
        if ($line -match '^\s*version:\s*(\S+)')        { $info.Version = $Matches[1]; continue }
        if ($line -match '^\s*save_extension:\s*(\S+)') { $info.SaveExt = $Matches[1]; continue }
        if ($line -match '^\s*([0-9a-fA-F]{64})\s+(\S.*)$') { $info.Files[$Matches[2].Trim()] = $Matches[1].ToLower() }
    }
    return $info
}

# ---- co-op test save (isolated extension) ------------------------------------------------------
# The mod reads/writes ER0000.<ext> on an ISOLATED extension (never vanilla .sl2 or ERSC .co2), so your
# real save is never modified. To let you test co-op with your OWN character, Install seeds ER0000.<ext>
# from your newest ER0000.sl2, and Uninstall removes that copy again. These helpers are the single
# source of truth for that — shared by Install/Uninstall so they agree on the extension and the guards.

# Guard: a usable, isolated co-op extension — present, and NEVER the vanilla (.sl2) or ERSC (.co2) save.
# Everything below refuses to act unless this returns true, so we can only ever touch our own file.
function Test-CoopSaveExt([string]$Ext) {
    if (-not $Ext) { return $false }
    $e = $Ext.TrimStart('.').ToLower()
    return ($e -ne '' -and $e -ne 'sl2' -and $e -ne 'co2')
}

# Seed ER0000.<ext> from the newest ER0000.sl2 so you test co-op with your own character. Idempotent:
# if a co-op save already exists (e.g. a prior test, maybe with progress), it is KEPT, never clobbered.
function Initialize-CoopSave([string]$Ext) {
    if (-not (Test-CoopSaveExt $Ext)) {
        Write-Warn "co-op save extension '$Ext' is missing or unsafe (.sl2/.co2) - not seeding a co-op save"
        return
    }
    $e = $Ext.TrimStart('.').ToLower()
    $root = Get-EldenRingAppData
    if (-not (Test-Path $root)) { Write-Warn "no ELDEN RING save folder found - you'll start co-op as a new character"; return }
    # The active profile = the <steamid> folder whose ER0000.sl2 was played most recently.
    $prof = Get-ChildItem $root -Directory -ErrorAction SilentlyContinue |
        Where-Object { Test-Path (Join-Path $_.FullName 'ER0000.sl2') } |
        Sort-Object { (Get-Item (Join-Path $_.FullName 'ER0000.sl2')).LastWriteTime } -Descending |
        Select-Object -First 1
    if (-not $prof) { Write-Warn "no ER0000.sl2 found under $root - you'll start co-op as a new character"; return }
    $src = Join-Path $prof.FullName 'ER0000.sl2'
    $dst = Join-Path $prof.FullName "ER0000.$e"
    if (Test-Path $dst) { Write-Ok "co-op save ER0000.$e already exists ($($prof.Name)); kept your existing co-op character"; return }
    Copy-Item $src $dst -Force
    if (Test-Path "$src.bak") { Copy-Item "$src.bak" "$dst.bak" -Force }   # mirror the game's backup slot
    Write-Ok "seeded co-op save ER0000.$e from your character ($($prof.Name)); your real ER0000.sl2 is untouched"
}

# Remove the co-op test save(s) we created (ER0000.<ext> + its .bak) from every profile folder. Guarded
# so it can NEVER touch ER0000.sl2 / .co2. Used by Uninstall to leave the save folder as it was found.
function Remove-CoopSave([string]$Ext) {
    if (-not (Test-CoopSaveExt $Ext)) {
        Write-Warn "co-op save extension '$Ext' missing or unsafe - leaving all saves alone"
        return
    }
    $e = $Ext.TrimStart('.').ToLower()
    $root = Get-EldenRingAppData
    if (-not (Test-Path $root)) { return }
    $removed = 0
    foreach ($prof in (Get-ChildItem $root -Directory -ErrorAction SilentlyContinue)) {
        foreach ($name in @("ER0000.$e", "ER0000.$e.bak")) {
            $p = Join-Path $prof.FullName $name
            if (Test-Path $p) { Remove-Item $p -Force; $removed++ }
        }
    }
    if ($removed -gt 0) { Write-Ok "removed the co-op test save (ER0000.$e); your real ER0000.sl2 was never touched" }
    else { Write-Ok "no co-op test save to remove" }
}
