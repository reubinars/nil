# Builds nil (release) and refreshes a ready-to-use folder: by default a folder called
# `nil` next to this repo, or `dist\nil` inside it when the repo itself is the folder
# called nil. Only nil's own files there are overwritten; anything else in the folder
# is left alone.
param([string]$Dest)

$ErrorActionPreference = 'Stop'
$repo = $PSScriptRoot
if (-not $Dest) {
    $Dest = Join-Path (Split-Path $repo -Parent) 'nil'
    $full = { param($p) [IO.Path]::GetFullPath($p).TrimEnd('\') }
    if ((& $full $Dest) -eq (& $full $repo)) { $Dest = Join-Path $repo 'dist\nil' }
}
$src = Join-Path $repo 'packaging\nil'
Set-Location $repo

# Source paths end up in nil.exe's panic messages; replace this machine's folders
# (which name the user and the repo) with neutral ones. Unit-separated, so paths
# with spaces survive.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
$env:CARGO_ENCODED_RUSTFLAGS = @(
    "--remap-path-prefix=$cargoHome=cargo"
    "--remap-path-prefix=$repo=nil"
) -join [char]0x1f

cargo build --release -p bp-gui
if ($LASTEXITCODE -ne 0) { throw 'The build failed.' }

$licenses = Join-Path $Dest 'licenses'
New-Item -ItemType Directory -Force -Path $Dest, $licenses | Out-Null

try {
    Copy-Item (Join-Path $repo 'target\release\bp-gui.exe') (Join-Path $Dest 'nil.exe') -Force
} catch {
    throw "Couldn't replace nil.exe. Close nil and run this again."
}
Copy-Item (Join-Path $src 'README.txt'), (Join-Path $src 'RELEASE-NOTES.txt') $Dest -Force
$fonts = Join-Path $repo 'apps\bp-gui\assets\fonts'
Copy-Item (Join-Path $fonts 'OFL-Inter.txt') (Join-Path $licenses 'Inter-OFL.txt') -Force
Copy-Item (Join-Path $fonts 'OFL-JetBrainsMono.txt') (Join-Path $licenses 'JetBrainsMono-OFL.txt') -Force

# The libraries compiled into nil.exe: normal dependencies for this machine's target,
# minus nil's own crates (listed with their path in parentheses) and repeats ("(*)").
$crates = cargo tree -p bp-gui -e normal --prefix none --format '{p}|{l}' |
    Where-Object { $_ -and $_ -notmatch '\(' } |
    Sort-Object -Unique
if ($LASTEXITCODE -ne 0) { throw 'Listing the libraries failed.' }
$lines = @(
    'Third-party libraries in nil.exe'
    '================================'
    ''
    'nil is built with these open-source Rust libraries. Each is used under the'
    'licence shown (an "OR" means a choice of licences). The full licence texts'
    'are in each library''s source, published on https://crates.io.'
    ''
)
$lines += $crates | ForEach-Object {
    $name, $license = $_ -split '\|', 2
    if (-not $license) { $license = 'see the library''s source' }
    '{0,-48} {1}' -f $name, $license
}
$lines | Out-File -Encoding utf8 (Join-Path $licenses 'THIRD-PARTY.txt')

Write-Host ''
Write-Host "nil is ready in $Dest"
Invoke-Item $Dest
