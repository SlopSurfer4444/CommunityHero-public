[CmdletBinding()]
param([int]$Port = 4186)
$ErrorActionPreference = 'Stop'
$repo = $PSScriptRoot
$binary = Join-Path $repo 'mvp/server/target/debug/communityhero-server.exe'
if (-not $IsWindows) { $binary = Join-Path $repo 'mvp/server/target/debug/communityhero-server' }
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) { throw 'Build first: cargo build --locked --manifest-path mvp/server/Cargo.toml --bin communityhero-server' }
$node = (Get-Command node -CommandType Application -ErrorAction Stop).Source
$saved = @{}
foreach ($entry in Get-ChildItem Env: | Where-Object { $_.Name -like 'COMMUNITYHERO_*' }) { $saved[$entry.Name] = $entry.Value }
try {
    foreach ($name in @($saved.Keys)) { [Environment]::SetEnvironmentVariable($name, $null, 'Process') }
    $env:COMMUNITYHERO_ACCOUNT = 'likeavto'
    $env:COMMUNITYHERO_NODE = $node
    $env:COMMUNITYHERO_PORT = [string]$Port
    $env:COMMUNITYHERO_DATA_DIR = Join-Path $repo 'mvp/data/public-dev'
    $env:COMMUNITYHERO_BRIDGE = Join-Path $repo 'project/portable-runtime/templates/fail-closed.mjs'
    $env:COMMUNITYHERO_BACKGROUND_DISABLED = '1'
    $env:COMMUNITYHERO_BACKGROUND_GENERATION_DISABLED = '1'
    $env:COMMUNITYHERO_EXTERNAL_WRITES = 'disabled'
    & $binary
    if ($LASTEXITCODE -ne 0) { throw "Development server exited: $LASTEXITCODE" }
} finally {
    foreach ($entry in Get-ChildItem Env: | Where-Object { $_.Name -like 'COMMUNITYHERO_*' }) { [Environment]::SetEnvironmentVariable($entry.Name, $null, 'Process') }
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
}
