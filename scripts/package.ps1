<#
.SYNOPSIS
    统一发布打包流水线：一条命令产出便携版 + 安装版两种分发物。

.DESCRIPTION
    取代 scripts/package-portable.ps1 与 scripts/build-installer.ps1 的两段式流程，
    收敛为单一入口。流水线分两个阶段，顺序固定：

      portable（便携版）
        1. cargo build --release：主程序(app-ui→asset-manager)、worker(decode-worker)、
           sample-library、derive-thumbs
        2. 重建 dist/：拷贝 4 个 exe + 现场生成示例库 samples/inbox -> dist/library
        3. 校验必需文件齐全后打成 artifacts/素材管理器-便携版-<ver>.zip

      installer（安装版，依赖 portable 产出的 dist/）
        1. dist/ -> dist.tar.gz（临时中间文件，位于仓库根，gitignore 已排除）
        2. 校验 tar payload 完整
        3. 编译 installer/ 的 asset-installer（include_bytes! 内嵌 dist.tar.gz）
        4. 产出 artifacts/素材管理器-安装版-<ver>.exe

    stage=all（默认）串联两阶段。最终产物统一落在 artifacts/，不再散落项目根目录。

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

    # 示例库是生成物、不进 git：每次打包现场用 sample-library 从 samples/inbox
    # 生成到 dist/library（开箱即有带缩略图的演示库）。旧写法依赖作者机器上
    # 残留的 samples/library 目录，CI 上必然缺失而失败。
    $sampleExe = Join-Path $RepoRoot "target\release\sample-library.exe"
    & $sampleExe (Join-Path $RepoRoot "samples\inbox") (Join-Path $Dist "library")
    Assert-LastExitCode "生成示例库"
    # 校验：示例库元数据必须随包（安装后开箱即有预览）
    if (-not (Test-Path (Join-Path $Dist "library\meta.db"))) {
        throw "dist/library 缺少 meta.db，示例库生成失败"
    }

    Write-Host "==> [portable] 打 zip"
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    $zip = Join-Path $OutDir "素材管理器-便携版-$Version.zip"
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
    $installerOut = Join-Path $OutDir "素材管理器-安装版-$Version.exe"
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

Write-Host ""
Write-Host "打包完成（stage=$Stage, version=$Version, out=$OutDir）"
Get-ChildItem $OutDir | Select-Object Name, Length | Format-Table -AutoSize
