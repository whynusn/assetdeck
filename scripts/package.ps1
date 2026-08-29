<#
.SYNOPSIS
    统一发布打包流水线：一条命令产出便携版 + 安装版两种分发物。

.DESCRIPTION
    取代 scripts/package-portable.ps1 与 scripts/build-installer.ps1 的两段式流程，
    收敛为单一入口。流水线分两个阶段，顺序固定：

      portable（便携版）
        1. cargo build --release：主程序(app-ui→asset-manager)、worker(decode-worker)、
           sample-library、derive-thumbs
        2. 重建 dist/：拷贝 4 个 exe（不带任何素材库——8-29 覆盖事故的根治，D61）
        3. 校验必需文件齐全后打成 artifacts/素材管理器-便携版-<ver>.zip

      installer（安装版，依赖 portable 产出的 dist/）
        1. dist/ -> dist.tar.gz（临时中间文件，位于仓库根，gitignore 已排除）
        2. 校验 tar payload 完整
        3. 编译 installer/ 的 asset-installer（include_bytes! 内嵌 dist.tar.gz）
        4. 产出 artifacts/素材管理器-安装版-<ver>.exe

    stage=all（默认）串联两阶段。最终产物统一落在 artifacts/，不再散落项目根目录。
    收尾生成 artifacts/SHA256SUMS.txt（sha256sum 标准格式）——分发物未签名，校验和是
    完整性保障的最低限度，随产物一起上传并附进 GitHub Release。

    为什么 installer 是独立 workspace：其编译期 include_bytes!("../../dist.tar.gz")
    要求 tar 必须先于编译存在；若并入主 workspace，任何 cargo build --workspace
    都会在 dist.tar.gz 缺失或过期时用陈旧/空 payload 编译。独立 workspace 由
    本脚本严格保证「先 tar 后编译」的顺序，杜绝隐式依赖。

.PARAMETER Stage
    all（默认）| portable | installer

.PARAMETER SkipBuild
    跳过 cargo build，直接复用现有 target/release 产物重新打包。

.PARAMETER Version
    产物文件名中的版本号；缺省从 workspace Cargo.toml 的 [workspace.package] version 读取。

.PARAMETER OutDir
    最终产物目录，默认 <repo>/artifacts。

.EXAMPLE
    pwsh -NoProfile -File scripts/package.ps1
    pwsh -NoProfile -File scripts/package.ps1 -Stage portable
    pwsh -NoProfile -File scripts/package.ps1 -Stage all -SkipBuild
#>
[CmdletBinding()]
param(
    [ValidateSet("all", "portable", "installer")]
    [string]$Stage = "all",
    [switch]$SkipBuild,
    [string]$Version = "",
    [string]$OutDir = ""
)

$ErrorActionPreference = "Stop"

# ---- 分发产物静态链接 CRT（仅 MSVC 工具链）----
# MSVC 动态链接的 exe 依赖 VCRUNTIME140.dll（VC Redist 随附，非系统自带）——干净
# 系统上直接拒载（实测报「找不到 VCRUNTIME140.dll」）。CI 的 MSVC 分发构建一律
# +crt-static 自包含。windows-gnu 构建（本地）链接系统自带的 msvcrt.dll，本就自
# 包含；且 +crt-static 在 gnu 下改变链接行为（实测报找不到 -lshlwapi），故只对
# msvc host 生效。仅作用于本脚本（打包）；日常开发/CI 测试构建不受影响。
$hostTriple = (& rustc -vV 2>$null | Select-String '^host:').ToString()
if ($hostTriple -match 'msvc') {
    $env:RUSTFLAGS = "-C target-feature=+crt-static"
}

# ---- 路径（全部相对仓库根解析，脚本可从任意 cwd 调用）----
$RepoRoot = Split-Path -Parent $PSScriptRoot
$Dist = Join-Path $RepoRoot "dist"
$Payload = Join-Path $RepoRoot "dist.tar.gz"
$InstallerManifest = Join-Path $RepoRoot "installer\Cargo.toml"
$InstallerExe = Join-Path $RepoRoot "installer\target\release\asset-installer.exe"
if (-not $OutDir) { $OutDir = Join-Path $RepoRoot "artifacts" }

# ---- 版本号：默认取 workspace 级版本 ----
if (-not $Version) {
    $manifest = Get-Content -Raw (Join-Path $RepoRoot "Cargo.toml")
    if ($manifest -match '(?m)^version\s*=\s*"([^"]+)"') { $Version = $Matches[1] }
    else { $Version = "0.0.0" }
}

function Assert-LastExitCode {
    param([string]$What)
    if ($LASTEXITCODE -ne 0) { throw "$What 失败，退出码 $LASTEXITCODE" }
}

# ---------- 阶段一：便携版 ----------
function Step-Portable {
    Write-Host "==> [portable] 编译 release 产物（app-ui / worker / sample-library / derive-thumbs）"
    if (-not $SkipBuild) {
        Push-Location $RepoRoot
        try {
            & cargo build --release -p app-ui -p worker -p sample-library -p derive-thumbs
            Assert-LastExitCode "cargo build"
        } finally {
            Pop-Location
        }
    }

    Write-Host "==> [portable] 组装 dist/"
    if (Test-Path $Dist) { Remove-Item -Recurse -Force $Dist }
    New-Item -ItemType Directory -Force -Path $Dist | Out-Null

    $required = @("asset-manager.exe", "decode-worker.exe", "sample-library.exe", "derive-thumbs.exe")
    foreach ($name in $required) {
        $src = Join-Path $RepoRoot "target\release\$name"
        if (-not (Test-Path $src)) {
            throw "缺少必需产物: $src（先执行 cargo build --release）"
        }
        Copy-Item -Path $src -Destination $Dist -Force
    }

    # 8-29 事故根因（D61 收账）：payload 曾内嵌现场生成的示例库 dist/library，
    # 安装时 unpack 覆盖 exe 旁用户库的 meta.db——重装即清空用户索引（真机实测
    # 当天导入全灭）。且新版默认库根在 %LOCALAPPDATA%，exe 旁示例库本就是死重。
    # 分发包从此**不带任何素材库**：首启统一库为空属预期，旧版用户走应用内
    # 「设置 → 数据迁移」一键搬迁（D61）。sample-library.exe 保留：它是运行时
    # 导入子进程（D11），不是打包工具。

    Write-Host "==> [portable] 打 zip"
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    # 分发名一律 ASCII：中文文件名在 GitHub 资产链路被吞（实测「素材管理器-便携版-0.1.0」
    # 上传后变成「-.-0.1.0」）。应用内显示名不变，仅产物文件名用 ASCII。
    $zip = Join-Path $OutDir "assetdeck-portable-$Version.zip"
    $zipOk = $false
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            if (Test-Path $zip) { Remove-Item -Force $zip }
            # PS 5.1 的 Compress-Archive 遇到文件短暂占用（如 Defender 实时扫描刚拷贝的 exe）
            # 时会静默产出不完整 zip 而不报错，故必须显式校验产物非空
            Compress-Archive -Path (Join-Path $Dist "*") -DestinationPath $zip -CompressionLevel Optimal -ErrorAction Stop
            if (-not ((Test-Path $zip) -and ((Get-Item $zip).Length -gt 0))) { throw "zip 产物为空" }
            $zipOk = $true
            break
        } catch {
            Write-Warning "打 zip 第 $attempt 次尝试失败：$($_.Exception.Message)"
            if ($attempt -lt 3) { Start-Sleep -Seconds 2 }
        }
    }
    if (-not $zipOk) { throw "打 zip 连续失败：$zip" }
    Write-Host "    OK 便携版: $zip"
}

# ---------- 阶段二：安装版 ----------
function Step-Installer {
    if (-not (Test-Path (Join-Path $Dist "asset-manager.exe"))) {
        throw "dist/ 不完整，请先执行 portable 阶段（-Stage portable 或 -Stage all）"
    }

    Write-Host "==> [installer] 打 tar.gz payload"
    $tarOk = $false
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        try {
            if (Test-Path $Payload) { Remove-Item -Force $Payload }
            Push-Location $RepoRoot
            try {
                & tar.exe -a -c -f $Payload -C $Dist .
                if ($LASTEXITCODE -ne 0) { throw "tar 退出码 $LASTEXITCODE" }
            } finally {
                Pop-Location
            }
            if (-not ((Test-Path $Payload) -and ((Get-Item $Payload).Length -gt 0))) { throw "tar 产物为空" }
            $tarOk = $true
            break
        } catch {
            Write-Warning "打 tar 第 $attempt 次尝试失败：$($_.Exception.Message)"
            if ($attempt -lt 3) { Start-Sleep -Seconds 2 }
        }
    }
    if (-not $tarOk) { throw "打 tar 连续失败：$Payload" }

    # payload 完整性快检：4 个 exe 必须出现在归档根
    $entries = & tar.exe -tf $Payload
    Assert-LastExitCode "tar 校验"
    foreach ($name in @("asset-manager.exe", "decode-worker.exe", "sample-library.exe", "derive-thumbs.exe")) {
        if ($entries -notcontains "./$name") {
            throw "tar payload 缺少 $name"
        }
    }

    Write-Host "==> [installer] 编译 asset-installer（内嵌 payload）"
    & cargo build --manifest-path $InstallerManifest --release
    Assert-LastExitCode "cargo build installer"

    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    $installerOut = Join-Path $OutDir "assetdeck-installer-$Version.exe"
    Copy-Item -Path $InstallerExe -Destination $installerOut -Force

    # 兜底校验：安装包必须比 payload 大（防 include_bytes! 嵌进空/陈旧文件）
    $payloadLen = (Get-Item $Payload).Length
    $installerLen = (Get-Item $installerOut).Length
    if ($installerLen -le $payloadLen) {
        throw "安装包尺寸异常（$installerLen <= payload $payloadLen），请检查 installer 编译"
    }
    Write-Host "    OK 安装版: $installerOut"
}

# ---------- 主流程 ----------
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
switch ($Stage) {
    "portable"  { Step-Portable }
    "installer" { Step-Installer }
    "all"       { Step-Portable; Step-Installer }
}

# ---------- 校验和清单：未签名分发物的完整性最低保障 ----------
# 覆盖 OutDir 当前全部分发物，sha256sum 标准格式（哈希 + 两空格 + 文件名），Linux
# sha256sum -c 可直接校验。分阶段调用时以最后一次调用重刷的清单为准；CI 统一走 Stage=all。
$sumsPath = Join-Path $OutDir "SHA256SUMS.txt"
Get-ChildItem $OutDir -File |
    Where-Object { $_.Name -ne "SHA256SUMS.txt" } |
    ForEach-Object { "{0}  {1}" -f (Get-FileHash -Algorithm SHA256 $_.FullName).Hash.ToLowerInvariant(), $_.Name } |
    Set-Content -Path $sumsPath -Encoding ascii
Write-Host "    OK 校验和: $sumsPath"

Write-Host ""
Write-Host "打包完成（stage=$Stage, version=$Version, out=$OutDir）"
Get-ChildItem $OutDir | Select-Object Name, Length | Format-Table -AutoSize
