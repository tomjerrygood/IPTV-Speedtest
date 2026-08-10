# ============================================================
# Windows 本地交叉编译 IPTV-Speedtest → S905 Android 32 位 (armv7)
#
# 前置要求:
#   1. Rust (rustup) 已安装
#   2. Android NDK r26d 已安装:
#        - Android Studio → SDK Manager → SDK Tools → NDK (Side by side)
#        或手动解压: https://dl.google.com/android/repository/android-ndk-r26d-windows.zip
#   3. (可选) 若 rustc 默认宿主工具链为 msvc 且报 link.exe 缺失，
#      安装 "Visual Studio Build Tools" (勾选 "使用 C++ 的桌面开发")。
#
# 用法:
#   PowerShell 执行:
#     .\build_android.ps1
#   可指定 NDK 路径(自动探测):
#     .\build_android.ps1 -NdkPath "D:\Android\ndk\26.3.11579264"
#
# 产物:
#   dist\iptv-speed-tester-armv7\iptv-speed-testHD  (可 push 到 S905 盒子)
# ============================================================

param(
    [string]$NdkPath = ""
)

$ErrorActionPreference = "Stop"

# ── 1. 探测 NDK ─────────────────────────────────────────────
if (-not $NdkPath) {
    $candidates = @(
        "$env:LOCALAPPDATA\Android\Sdk\ndk",
        "C:\Android\Sdk\ndk",
        "D:\Android\Sdk\ndk",
        "$env:ANDROID_HOME\ndk",
        "$env:ANDROID_SDK_ROOT\ndk"
    )
    foreach ($c in $candidates) {
        if ($c -and (Test-Path $c)) {
            $vers = Get-ChildItem $c -Directory -ErrorAction SilentlyContinue |
                Sort-Object Name -Descending
            if ($vers) {
                $NdkPath = $vers[0].FullName
                break
            }
        }
    }
}

if (-not $NdkPath -or -not (Test-Path $NdkPath)) {
    Write-Host "[错误] 未找到 Android NDK。请安装 NDK r26d 后重试，或用 -NdkPath 指定:" -ForegroundColor Red
    Write-Host "  1) Android Studio: SDK Manager -> SDK Tools -> NDK (Side by side) 勾选安装"
    Write-Host "  2) 手动下载解压: https://dl.google.com/android/repository/android-ndk-r26d-windows.zip"
    exit 1
}
Write-Host "[OK] NDK: $NdkPath" -ForegroundColor Green

# NDK 内工具链目录 (Windows 宿主)
$toolchainRoot = Join-Path $NdkPath "toolchains\llvm\prebuilt"
$ndkBin = $null
foreach ($hostDir in @("windows-x86_64", "windows")) {
    $p = Join-Path $toolchainRoot $hostDir "bin"
    if (Test-Path $p) {
        $ndkBin = $p
        break
    }
}
if (-not $ndkBin) {
    Write-Host "[错误] NDK 中未找到 windows-x86_64 工具链。" -ForegroundColor Red
    exit 1
}
Write-Host "[OK] NDK bin: $ndkBin" -ForegroundColor Green

$clang = Join-Path $ndkBin "armv7a-linux-androideabi24-clang.cmd"
$linker = Join-Path $ndkBin "armv7a-linux-androideabi24-clang.cmd"
$ar = Join-Path $ndkBin "llvm-ar.exe"
if (-not (Test-Path $clang)) {
    # 部分 NDK 版本无 .cmd 后缀
    $clang = Join-Path $ndkBin "armv7a-linux-androideabi24-clang"
    $linker = $clang
}
if (-not (Test-Path $ar)) { $ar = Join-Path $ndkBin "llvm-ar" }

Write-Host "[OK] clang: $clang"
Write-Host "[OK] ar   : $ar"

# ── 2. 安装 Rust android target ─────────────────────────────
Write-Host "[1/3] 安装 Rust android target (armv7-linux-androideabi)..."
rustup target add armv7-linux-androideabi
if ($LASTEXITCODE -ne 0) { exit 1 }

# ── 3. 设置交叉编译环境变量并构建 ────────────────────────────
Write-Host "[2/3] 设置交叉编译环境变量..."
$env:CC_armv7_linux_androideabi = $clang
$env:AR_armv7_linux_androideabi = $ar
# cargo 通过 PATH 查找 linker 名 (armv7a-linux-androideabi24-clang)
$env:PATH = "$ndkBin;$env:PATH"

Write-Host "[3/3] cargo build (release, android feature)..."
cargo build --release --no-default-features --features android `
    --target armv7-linux-androideabi
if ($LASTEXITCODE -ne 0) {
    Write-Host "[失败] 构建失败。若报 link.exe 缺失，请安装 Visual Studio Build Tools。" -ForegroundColor Red
    exit 1
}

$bin = "target\armv7-linux-androideabi\release\iptv-speed-tester.exe"
if (-not (Test-Path $bin)) { $bin = "target\armv7-linux-androideabi\release\iptv-speed-tester" }

# ── 4. 打包 ─────────────────────────────────────────────────
Write-Host "[完成] 组织发布包..."
$out = "dist\iptv-speed-tester-armv7"
New-Item -ItemType Directory -Force -Path $out | Out-Null
Copy-Item $bin (Join-Path $out "iptv-speed-testHD") -Force
Copy-Item README.md (Join-Path $out "README.md") -Force
if (Test-Path ANDROID-S905.md) { Copy-Item ANDROID-S905.md (Join-Path $out "ANDROID-S905.md") -Force }

$zip = "dist\iptv-speed-tester-armv7.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $out -DestinationPath $zip -Force

Write-Host ""
Write-Host "构建成功! 产物:" -ForegroundColor Green
Write-Host "  二进制: $out\iptv-speed-testHD"
Write-Host "  压缩包: $zip"
Write-Host ""
Write-Host "推送到盒子:" -ForegroundColor Cyan
Write-Host "  adb push $out\iptv-speed-testHD /data/local/tmp/iptv-speed-testHD"
Write-Host "  adb shell su -c 'chmod 755 /data/local/tmp/iptv-speed-testHD && /data/local/tmp/iptv-speed-testHD --port 3030'"
