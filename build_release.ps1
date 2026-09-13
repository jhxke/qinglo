# ===========================================================================
# build_release.ps1 - Build a distributable release package of mining-app.
#
# Output:
#   release\mining-app-<version>-win-x64\         (staged, runnable folder)
#   release\mining-app-<version>-win-x64.zip      (zipped package)
#
# Package layout:
#   mining-app.exe                    GUI (statically linked, own target dir)
#   operator_runtime_server.exe       TCP runtime server (prefer-dynamic)
#   lib\public\operator_runtime.dll   shared native runtime, single copy;
#                                     the app prepends lib\public to PATH when
#                                     spawning the server, and the server also
#                                     registers it as a DLL search directory
#   lib\operator\<group>\<operator>\  operator dll + operator.json (same layout
#                                     as run_srv.ps1)
#   webview_plugins\                  external webview plugins (clock, ...)
#   operator_runtime\                 runtime crate SOURCE, required by the
#                                     in-app "compile custom operator" path
#                                     (generated project uses a path dep)
#   start.bat                         double-click launcher that pins CWD to
#                                     the package root and prepends
#                                     lib\public to PATH so the server resolves
#                                     .\lib\operator and find_runtime_path()
#                                     finds .\operator_runtime
#
# Independent CARGO_TARGET_DIRs (target_app / target_srv) mirror run_app.ps1
# and run_srv.ps1, preventing operator_runtime.dll artifact conflicts.
#
# Params:
#   -SkipBuild   stage/zip existing build outputs only (no cargo invocation)
#   -NoZip       produce the staged folder only
#
# This file is pure-ASCII on purpose: PowerShell 5.1 reads non-BOM files as
# the ANSI codepage (cp936 here) and would corrupt literal Chinese. Group and
# display directory names are built from [char] code points, exactly as in
# run_srv.ps1.
# ===========================================================================

param(
    [switch]$SkipBuild,
    [switch]$NoZip
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot

# cargo/rustc write progress to stderr. Under PowerShell 5.1 with
# $ErrorActionPreference="Stop" every stderr line from a native command is
# promoted to a terminating NativeCommandError, which would abort the build at
# the very first "Compiling ..." line. Temporarily switch to "Continue" around
# cargo invocations and rely on $LASTEXITCODE instead.
function Invoke-NativeCheck {
    param([Parameter(Mandatory=$true)][scriptblock]$Block, [string]$Label)
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        & $Block
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $prev
    }
    if ($code -ne 0) {
        Write-Host "$Label FAILED (exit code $code)!" -ForegroundColor Red
        exit 1
    }
}

# ---------------------------------------------------------------------------
# 1. Operator catalog (must stay in sync with run_srv.ps1).
#    Group = level-1 dir under lib\ ; Dir = operator leaf dir name.
# ---------------------------------------------------------------------------
$grpDataSource = [char]0x6570 + [char]0x636E + [char]0x6E90
$grpTechIndex  = [char]0x6280 + [char]0x672F + [char]0x6307 + [char]0x6807
$grpMathOp     = [char]0x6570 + [char]0x5B66 + [char]0x8FD0 + [char]0x7B97
$grpViz        = [char]0x53EF + [char]0x89C6 + [char]0x5316
$grpLLM        = [char]0x5927 + [char]0x6A21 + [char]0x578B

$operators = @(
    # data source
    @{ Group = $grpDataSource; Name = "datasource_operator"; Dir = [char]0x6570 + [char]0x636E + [char]0x6E90 + [char]0x8BFB + [char]0x53D6; Json = "operator\datasource_operator\operator.json" }

    # technical index
    @{ Group = $grpTechIndex; Name = "ma_operator";   Dir = "MA" + [char]0x7B97 + [char]0x5B50;   Json = "operator\ma_operator\operator.json" }
    @{ Group = $grpTechIndex; Name = "rsi_operator";  Dir = "RSI" + [char]0x7B97 + [char]0x5B50;  Json = "operator\rsi_operator\operator.json" }
    @{ Group = $grpTechIndex; Name = "macd_operator"; Dir = "MACD" + [char]0x7B97 + [char]0x5B50; Json = "operator\macd_operator\operator.json" }

    # math operation
    @{ Group = $grpMathOp; Name = "expression_operator"; Dir = [char]0x8868 + [char]0x8FBE + [char]0x5F0F + [char]0x7B97 + [char]0x5B50;             Json = "operator\expression_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "cumsum_operator";     Dir = [char]0x7D2F + [char]0x52A0 + [char]0x7B97 + [char]0x5B50;                         Json = "operator\cumsum_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "shift_add_operator";  Dir = [char]0x524D + [char]0x79FB + [char]0x52A0 + [char]0x7B97 + [char]0x5B50;           Json = "operator\shift_add_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "merge_operator";      Dir = [char]0x6570 + [char]0x636E + [char]0x5408 + [char]0x5E76 + [char]0x7B97 + [char]0x5B50; Json = "operator\merge_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "frame_index_operator"; Dir = [char]0x6570 + [char]0x7EC4 + [char]0x53D6 + [char]0x5E27 + [char]0x7B97 + [char]0x5B50; Json = "operator\frame_index_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "future_return_operator"; Dir = [char]0x672A + [char]0x6765 + [char]0x6536 + [char]0x76CA + [char]0x7B97 + [char]0x5B50; Json = "operator\future_return_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "daily_return_operator";  Dir = [char]0x5F53 + [char]0x65E5 + [char]0x6536 + [char]0x76CA + [char]0x7387 + [char]0x7B97 + [char]0x5B50; Json = "operator\daily_return_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "volatility_operator";   Dir = [char]0x6CE2 + [char]0x52A8 + [char]0x7387 + [char]0x7B97 + [char]0x5B50;                       Json = "operator\volatility_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "price_volume_factor_operator"; Dir = [char]0x91CF + [char]0x4EF7 + [char]0x56E0 + [char]0x5B50 + [char]0x7B97 + [char]0x5B50; Json = "operator\price_volume_factor_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "factor_vc_operator"; Dir = [char]0x91CF + [char]0x4EF7 + [char]0x80FD + [char]0x91CF + [char]0x56E0 + [char]0x5B50 + [char]0x7B97 + [char]0x5B50; Json = "operator\factor_vc_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "highest_operator"; Dir = [char]0x6700 + [char]0x9AD8 + [char]0x4EF7 + [char]0x7B97 + [char]0x5B50; Json = "operator\highest_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "return_histogram_operator"; Dir = [char]0x6536 + [char]0x76CA + [char]0x7387 + [char]0x76F4 + [char]0x65B9 + [char]0x56FE + [char]0x7B97 + [char]0x5B50; Json = "operator\return_histogram_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "factor_histogram_operator"; Dir = [char]0x56E0 + [char]0x5B50 + [char]0x76F4 + [char]0x65B9 + [char]0x56FE + [char]0x7B97 + [char]0x5B50; Json = "operator\factor_histogram_operator\operator.json" }
    @{ Group = $grpMathOp; Name = "column_stream_operator"; Dir = [char]0x5217 + [char]0x8F6C + [char]0x5B57 + [char]0x7B26 + [char]0x4E32 + [char]0x6D41 + [char]0x7B97 + [char]0x5B50; Json = "operator\column_stream_operator\operator.json" }

    # visualization
    @{ Group = $grpViz; Name = "kline_visualization_operator"; Dir = [char]0x53EF + [char]0x89C6 + [char]0x5316 + [char]0x7B97 + [char]0x5B50;                         Json = "operator\kline_visualization_operator\operator.json" }
    @{ Group = $grpViz; Name = "line_chart_operator";           Dir = [char]0x6298 + [char]0x7EBF + [char]0x53EF + [char]0x89C6 + [char]0x5316 + [char]0x7B97 + [char]0x5B50; Json = "operator\line_chart_operator\operator.json" }
    @{ Group = $grpViz; Name = "chat_visualization_operator";   Dir = "DSL" + [char]0x5BF9 + [char]0x8BDD + [char]0x5C55 + [char]0x793A + [char]0x7B97 + [char]0x5B50;        Json = "operator\chat_visualization_operator\operator.json" }
    @{ Group = $grpViz; Name = "histogram_visualization_operator"; Dir = [char]0x76F4 + [char]0x65B9 + [char]0x56FE + [char]0x5C55 + [char]0x793A + [char]0x7B97 + [char]0x5B50;     Json = "operator\histogram_visualization_operator\operator.json" }

    # large model
    @{ Group = $grpLLM; Name = "ollama_operator"; Dir = "Ollama" + [char]0x7B97 + [char]0x5B50; Json = "operator\ollama_operator\operator.json" }
)

# ---------------------------------------------------------------------------
# 2. Resolve version and target directories.
# ---------------------------------------------------------------------------
$appManifest = Get-Content "$root\mining-app\Cargo.toml" -Raw
$version = if ($appManifest -match '(?m)^version\s*=\s*"([^"]+)"') { $Matches[1] } else { "0.0.0" }

$pkgName    = "mining-app-$version-win-x64"
$releaseDir = Join-Path $root "release"
$stage      = Join-Path $releaseDir $pkgName
$zipPath    = Join-Path $releaseDir "$pkgName.zip"

$appTarget = Join-Path $root "target_app\release"
$srvTarget = Join-Path $root "target_srv\release"
$srvDeps   = Join-Path $srvTarget "deps"

Write-Host "===== mining-app release build: v$version =====" -ForegroundColor Green

# ---------------------------------------------------------------------------
# 3. Cargo builds.
# ---------------------------------------------------------------------------
if (-not $SkipBuild) {
    # 3a. GUI app: static (no prefer-dynamic), independent target dir.
    Write-Host "`n[1/2] Building mining-app (release, static)..." -ForegroundColor Cyan
    $env:CARGO_TARGET_DIR = Join-Path $root "target_app"
    Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue
    Invoke-NativeCheck -Label "mining-app build" -Block { cargo build --release --package mining-app }

    # 3b. Runtime server + all operators: prefer-dynamic so operators share
    #     one operator_runtime.dll with the server.
    Write-Host "`n[2/2] Building operator_runtime_server + operators (release, prefer-dynamic)..." -ForegroundColor Cyan
    $env:CARGO_TARGET_DIR = Join-Path $root "target_srv"
    $env:RUSTFLAGS = "-C prefer-dynamic"
    $pkgArgs = @("build", "--release", "-p", "operator_runtime_server")
    foreach ($op in $operators) { $pkgArgs += @("-p", $op.Name) }
    Invoke-NativeCheck -Label "runtime/operator build" -Block { cargo @pkgArgs }
}

# Restore a neutral environment for the staging phase.
Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue
Remove-Item Env:\CARGO_TARGET_DIR -ErrorAction SilentlyContinue

# ---------------------------------------------------------------------------
# 4. Verify primary build outputs exist.
# ---------------------------------------------------------------------------
$appExe = Join-Path $appTarget "mining-app.exe"
$srvExe = Join-Path $srvTarget "operator_runtime_server.exe"
$runtimeDll = Join-Path $srvDeps "operator_runtime.dll"

foreach ($f in @($appExe, $srvExe, $runtimeDll)) {
    if (-not (Test-Path $f)) {
        Write-Host "MISSING required build output: $f" -ForegroundColor Red
        Write-Host "Run the script without -SkipBuild first." -ForegroundColor Red
        exit 1
    }
}

# ---------------------------------------------------------------------------
# 5. Reset staging directory.
# ---------------------------------------------------------------------------
Write-Host "`n===== Staging package -> $stage =====" -ForegroundColor Green
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage -Force | Out-Null

# 5a. Binaries. operator_runtime.dll goes to lib\public\ below (single shared
#     copy), NOT next to the exes.
Copy-Item $appExe $stage
Copy-Item $srvExe $stage

# Ship wry/WebView2 native helper if cargo emitted one next to the app exe.
$appNative = Join-Path $appTarget "WebView2Loader.dll"
if (Test-Path $appNative) { Copy-Item $appNative $stage }

# Dynamic Rust runtime. The server + every operator are built with
# -C prefer-dynamic, so they import std-<hash>.dll from the active toolchain.
# Cargo does NOT copy it into target/ on Windows, so a package without it
# fails with "cannot find std-<hash>.dll" on double-click. Pull it from the
# sysroot (hash changes per toolchain, hence the glob), falling back to
# anything cargo emitted under the server target dir.
$stdDlls = @()
$prevEAP = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$sysroot = (& rustc --print sysroot | Out-String).Trim()
$ErrorActionPreference = $prevEAP
if ($sysroot -and (Test-Path (Join-Path $sysroot "bin"))) {
    $stdDlls += Get-ChildItem (Join-Path $sysroot "bin") -Filter "std-*.dll" -ErrorAction SilentlyContinue
}
$stdDlls += Get-ChildItem $srvTarget -Recurse -Filter "std-*.dll" -ErrorAction SilentlyContinue
$shippedStd = @{}
foreach ($dll in $stdDlls) {
    if (-not $shippedStd.ContainsKey($dll.Name)) {
        Copy-Item $dll.FullName $stage
        $shippedStd[$dll.Name] = $true
        Write-Host "  bundled runtime: $($dll.Name)" -ForegroundColor DarkYellow
    }
}
if ($shippedStd.Count -eq 0) {
    Write-Host "WARNING: no std-*.dll found; prefer-dynamic binaries will fail on clean machines!" -ForegroundColor Red
}

# VC++ runtime (vcruntime140.dll) imported by the server/operators.
# Win10/11 usually has it via existing redistributables, but ship it when
# locatable on PATH so the package works out of the box. It is an officially
# redistributable Microsoft binary.
$vcr = (Get-Command "vcruntime140.dll" -ErrorAction SilentlyContinue | Select-Object -First 1)
if ($vcr -and (Test-Path $vcr.Source)) {
    Copy-Item $vcr.Source $stage
    Write-Host "  bundled runtime: vcruntime140.dll" -ForegroundColor DarkYellow
} else {
    Write-Host "NOTE: vcruntime140.dll not found on PATH; target machine needs VC++ Redistributable x64." -ForegroundColor DarkYellow
}

# 5b. Shared native runtime: single copy under lib\public\. The GUI prepends
#     this directory to PATH when spawning the server (so the server's own
#     import of operator_runtime.dll resolves), and the server additionally
#     registers it via SetDllDirectory for operator DLL dependencies.
$publicDir = Join-Path $stage "lib\public"
New-Item -ItemType Directory -Path $publicDir -Force | Out-Null
Copy-Item $runtimeDll $publicDir
Write-Host "  lib\public\operator_runtime.dll" -ForegroundColor DarkGray

# 5c. Operator library tree: lib\operator\<group>\<dir>\{<name>.dll,
#     operator.json}. Mirrors run_srv.ps1 exactly.
$missing = 0
$grouped = [ordered]@{}
foreach ($op in $operators) {
    if (-not $grouped.Contains($op.Group)) { $grouped[$op.Group] = @() }
    $grouped[$op.Group] += $op
}

$operatorTree = Join-Path $stage "lib\operator"
foreach ($groupEntry in $grouped.GetEnumerator()) {
    foreach ($op in $groupEntry.Value) {
        $destDir = Join-Path $operatorTree ($groupEntry.Key + "\" + $op.Dir)
        New-Item -ItemType Directory -Path $destDir -Force | Out-Null

        $opDll  = Join-Path $srvDeps ($op.Name + ".dll")
        $opJson = Join-Path $root $op.Json
        if (-not (Test-Path $opDll))  { Write-Host "MISSING operator dll: $opDll" -ForegroundColor Red; $missing++; continue }
        if (-not (Test-Path $opJson)) { Write-Host "MISSING operator json: $opJson" -ForegroundColor Red; $missing++; continue }

        Copy-Item $opDll $destDir
        Copy-Item $opJson $destDir
        Write-Host ("  lib\operator\{0}\{1}" -f $groupEntry.Key, $op.Dir) -ForegroundColor DarkGray
    }
}
if ($missing -gt 0) { Write-Host "$missing operator(s) missing, abort." -ForegroundColor Red; exit 1 }

# 5d. External webview plugins (loaded from exe-dir\webview_plugins).
$pluginsSrc = Join-Path $root "webview_plugins"
if (Test-Path $pluginsSrc) {
    Copy-Item $pluginsSrc (Join-Path $stage "webview_plugins") -Recurse
}

# 5e. operator_runtime crate source: the in-app "compile and execute"
#     feature generates a cargo project with a path dependency on it.
#     Ship Cargo.toml + src only (no target/).
$rtSrc = Join-Path $root "operator_runtime"
$rtDst = Join-Path $stage "operator_runtime"
New-Item -ItemType Directory -Path (Join-Path $rtDst "src") -Force | Out-Null
Copy-Item (Join-Path $rtSrc "Cargo.toml") $rtDst
Copy-Item (Join-Path $rtSrc "src\*.rs") (Join-Path $rtDst "src")

# 5f. Launcher: pin CWD to the package root and prepend lib\public to PATH so
#     the auto-spawned server resolves its operator_runtime.dll import (and
#     .\lib\operator via the RUNTIME_LIB_DIR default), and find_runtime_path()
#     finds .\operator_runtime.
$bat = @(
    '@echo off',
    'rem mining-app release launcher - pin working directory to package root',
    'cd /d "%~dp0"',
    'set "PATH=%~dp0lib\public;%PATH%"',
    'start "mining-app" "%~dp0mining-app.exe"'
)
Set-Content -Path (Join-Path $stage "start.bat") -Value $bat -Encoding ASCII

# ---------------------------------------------------------------------------
# 6. Zip.
# ---------------------------------------------------------------------------
if (-not $NoZip) {
    Write-Host "`n===== Creating zip =====" -ForegroundColor Green
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
    Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zipPath -CompressionLevel Optimal
}

# ---------------------------------------------------------------------------
# 7. Summary.
# ---------------------------------------------------------------------------
$opCount = $operators.Count
$dirSize = (Get-ChildItem $stage -Recurse -File | Measure-Object Length -Sum).Sum
Write-Host "`n===== Done =====" -ForegroundColor Green
Write-Host ("Staged : {0}  ({1:N1} MB, {2} operators)" -f $stage, ($dirSize/1MB), $opCount)
if (-not $NoZip) {
    $zipSize = (Get-Item $zipPath).Length
    Write-Host ("Zip    : {0}  ({1:N1} MB)" -f $zipPath, ($zipSize/1MB))
}
Write-Host "Run    : double-click start.bat (or mining-app.exe) inside the package." -ForegroundColor Cyan
