# vendor-iroh.ps1 — 同步 iroh/iroh-relay 到 vendor/ 并去掉 cdylib crate-type（D80）
# 用法：powershell -File scripts\vendor-iroh.ps1 [-IrohVersion 1.1.0]
# 背景：iroh 1.1.0 起上游 [lib] crate-type = ["lib","cdylib"]，cargo 连带产出 FFI 动态库；
# windows-gnu ld 16 位导出序号上限被 6.8 万符号撑爆（export ordinal too large），MSVC 无此限。
# 本脚本从本机 cargo registry 缓存拷贝指定版本源码到 vendor/，仅改 crate-type 为 ["lib"]。
# 升级 iroh 时：cargo add iroh@<新版本> -p share-worker 后重跑本脚本，随后核对 vendor 内 Cargo.toml 无其他漂移。
param(
    [string]$IrohVersion = "1.1.0"
)

$ErrorActionPreference = "Stop"

$cargoHome = $env:CARGO_HOME
if (-not $cargoHome) { $cargoHome = Join-Path $env:USERPROFILE ".cargo" }
$srcRoot = Get-ChildItem -Directory (Join-Path $cargoHome "registry\src") | ForEach-Object {
    Join-Path $_.FullName "iroh-$IrohVersion"
} | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $srcRoot) { throw "registry 缓存里找不到 iroh-$IrohVersion（先 cargo fetch）" }
$relaySrc = Get-ChildItem -Directory (Join-Path $cargoHome "registry\src") | ForEach-Object {
    Join-Path $_.FullName "iroh-relay-$IrohVersion"
} | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $relaySrc) { throw "registry 缓存里找不到 iroh-relay-$IrohVersion" }

$repoRoot = Split-Path -Parent $PSScriptRoot
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Copy-Vendored($fromDir, $toName) {
    $to = Join-Path (Join-Path $repoRoot "vendor") $toName
    if (Test-Path $to) { Remove-Item -Recurse -Force $to }
    Copy-Item -Recurse -Force $fromDir $to
    Get-ChildItem -Recurse $to | ForEach-Object { $_.Attributes = 'Archive' }
    $manifest = Join-Path $to "Cargo.toml"
    $text = [IO.File]::ReadAllText($manifest, [Text.Encoding]::UTF8)
    # 只动 crate-type：把 ["lib","cdylib"] 数组形（含跨行）折叠为纯 lib
    $pattern = '(?s)crate-type\s*=\s*\[\s*"lib"\s*,\s*"cdylib"\s*,?\s*\]'
    $replacement = "crate-type = [`"lib`"]"
    if ($text -notmatch $pattern) { throw "$toName Cargo.toml 里没有预期的 crate-type 形态，人工核对" }
    $text = [regex]::Replace($text, $pattern, $replacement)
    [IO.File]::WriteAllText($manifest, $text, $utf8)
    Write-Host "vendored $toName -> $to"
}

Copy-Vendored $srcRoot "iroh"
Copy-Vendored $relaySrc "iroh-relay"
Write-Host "完成：根 Cargo.toml 的 [patch.crates-io] 指向 vendor/iroh 与 vendor/iroh-relay，无需改动。"
