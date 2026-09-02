# 发版序一键化（D72）：scripts/release.ps1 <version> [-Subject 一句话主题] [-DryRun]
# 与 scripts/package.ps1（一条命令打包）同构；发版概念与 CI 门禁见 DECISIONS.md D57/D72。
# 本文件必须保持 UTF-8 **带 BOM**：Windows PowerShell 5.1 对无 BOM 文件按 ANSI(GBK) 解码，
# 中文串会全部乱码。用 Write/Edit 工具编辑（勿用 Set-Content 重写本文件）。
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Subject = "",
    [string]$Remote = "origin",
    [switch]$DryRun
)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

function Fail([string]$msg) {
    Write-Host "[release] 失败：$msg" -ForegroundColor Red
    exit 1
}

# ── 校验段（任何修改发生前全部跑完）──────────────────────────────────────────
if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    Fail "版本号须为 x.y.z 三段数字，得到: '$Version'"
}

$dirty = git status --porcelain
if ($dirty) { Fail "工作树不干净，先提交或暂存：`n$dirty" }

$branch = (git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -ne "main") { Fail "发版只能在 main 上进行（当前分支: $branch）" }

$localTag = git tag -l "v$Version"
if ($localTag) { Fail "本地 tag v$Version 已存在" }
$remoteTag = git ls-remote --tags $Remote "refs/tags/v$Version"
if ($remoteTag) { Fail "远程 tag v$Version 已存在" }

# 根 Cargo.toml 的第一条 ^version 行即 [workspace.package] 的版本（成员 crate 的
# version 在各自文件里），与 package.ps1 的读取假设同源——两处假设必须同步改。
$manifestPath = Join-Path (Get-Location) "Cargo.toml"
$text = [IO.File]::ReadAllText($manifestPath)
$rx = [regex]'(?m)^version\s*=\s*"([^"]+)"'
$m = $rx.Match($text)
if (-not $m.Success) { Fail "Cargo.toml 未找到顶层 version 行" }
$old = $m.Groups[1].Value
if ($old -eq $Version) { Fail "Cargo.toml 已是 $Version，无可升版（要发新版本请换号）" }

Write-Host "[release] 校验全过：版本 $old → $Version"

# ── DryRun：到此为止，只打印计划 ────────────────────────────────────────────
if ($DryRun) {
    Write-Host "[DryRun] 将执行："
    Write-Host "  1. 写 Cargo.toml: version = `"$Version`"（UTF-8 无 BOM，LF 原样保留）"
    Write-Host "  2. cargo update --workspace（Cargo.lock 成员版本派生）"
    Write-Host "  3. git add Cargo.toml Cargo.lock && git commit -m `"chore(release): 升版 v$Version`""
    Write-Host "  4. git push $Remote main"
    $tagMsg = if ($Subject) { "v$Version：$Subject" } else { "v$Version" }
    Write-Host "  5. git tag -a v$Version -m `"$tagMsg`""
    Write-Host "  6. git push $Remote v$Version"
    Write-Host "  7. 提示盯 CI / gh release view v$Version"
    exit 0
}

# ── 执行段 ───────────────────────────────────────────────────────────────────
[IO.File]::WriteAllText($manifestPath, $rx.Replace($text, ('version = "' + $Version + '"'), 1),
    [Text.UTF8Encoding]::new($false))
Write-Host "[release] Cargo.toml 已写 $Version"

cargo update --workspace
if ($LASTEXITCODE -ne 0) { Fail "cargo update --workspace 失败（exit $LASTEXITCODE）；Cargo.toml 已改动，请 git checkout 恢复后排查" }

git add Cargo.toml Cargo.lock
git commit -m "chore(release): 升版 v$Version——scripts/release.ps1 一键发版"
if ($LASTEXITCODE -ne 0) { Fail "git commit 失败（锁文件无变化？）" }

git push $Remote main
if ($LASTEXITCODE -ne 0) { Fail "git push main 失败（网络/代理？见 skill proxy-control）" }

$tagMsg = if ($Subject) { "v$Version：$Subject" } else { "v$Version" }
git tag -a "v$Version" -m $tagMsg
if ($LASTEXITCODE -ne 0) { Fail "git tag 失败" }

git push $Remote "v$Version"
if ($LASTEXITCODE -ne 0) { Fail "git push tag 失败——如远程已半落，删后重试: git push $Remote :refs/tags/v$Version" }

Write-Host "[release] 完成：tag v$Version 已推送，tag 触发的 CI/Release 流水线开始跑。"
Write-Host "  盯流水线: gh run list -R whynusn/assetdeck --limit 3（需挂 HTTPS_PROXY，见 skill proxy-control）"
Write-Host "  验发布:   gh release view v$Version -R whynusn/assetdeck"
