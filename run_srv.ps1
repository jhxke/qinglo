# Independent target directory for operator_runtime_server
# Uses separate CARGO_TARGET_DIR to avoid operator_runtime.dll conflicts with mining-app
$env:CARGO_TARGET_DIR = "$PSScriptRoot\target_srv"

Write-Host "===== Building operators =====" -ForegroundColor Green

# Use prefer-dynamic so operator_runtime links as a separate DLL
$env:RUSTFLAGS = "-C prefer-dynamic"

# Build operator_runtime cdylib and all operators (debug mode, matching runtime_server)
cargo build -p datasource_operator -p indicator_operator -p expression_operator -p kline_visualization_operator -p line_chart_operator -p ollama_operator -p cumsum_operator -p shift_add_operator

if ($LASTEXITCODE -ne 0) {
    Write-Host "Operator build FAILED!" -ForegroundColor Red
    exit 1
}

# Cargo outputs go to deps directory
$depsDir = "$env:CARGO_TARGET_DIR\debug\deps"
$debugDir = "$env:CARGO_TARGET_DIR\debug"

# Operator DLL mapping: DLL name -> lib subdirectory -> operator.json path
$operatorMap = @(
    @{ Name = "datasource_operator"; Dir = [char]0x6570 + [char]0x636E + [char]0x6E90 + [char]0x8BFB + [char]0x53D6; Json = "operator\datasource_operator\operator.json" },
    @{ Name = "indicator_operator"; Dir = [char]0x6307 + [char]0x6807 + [char]0x7B97 + [char]0x5B50;   Json = "operator\indicator_operator\operator.json" },
    @{ Name = "expression_operator"; Dir = [char]0x8868 + [char]0x8FBE + [char]0x5F0F + [char]0x7B97 + [char]0x5B50; Json = "operator\expression_operator\operator.json" },
    @{ Name = "kline_visualization_operator"; Dir = [char]0x53EF + [char]0x89C6 + [char]0x5316 + [char]0x7B97 + [char]0x5B50; Json = "operator\kline_visualization_operator\operator.json" },
    @{ Name = "line_chart_operator"; Dir = [char]0x6298 + [char]0x7EBF + [char]0x53EF + [char]0x89C6 + [char]0x5316 + [char]0x7B97 + [char]0x5B50; Json = "operator\line_chart_operator\operator.json" },
    @{ Name = "ollama_operator"; Dir = "Ollama" + [char]0x7B97 + [char]0x5B50; Json = "operator\ollama_operator\operator.json" },
    @{ Name = "cumsum_operator"; Dir = [char]0x7D2F + [char]0x52A0 + [char]0x7B97 + [char]0x5B50; Json = "operator\cumsum_operator\operator.json" },
    @{ Name = "shift_add_operator"; Dir = [char]0x524D + [char]0x79FB + [char]0x52A0 + [char]0x7B97 + [char]0x5B50; Json = "operator\shift_add_operator\operator.json" }
)

# 1. Ensure operator_runtime.dll is next to the exe (Windows DLL search priority)
$runtimeDllSrc = "$depsDir\operator_runtime.dll"
$runtimeDllDst = "$debugDir\operator_runtime.dll"
if (Test-Path $runtimeDllSrc) {
    Copy-Item $runtimeDllSrc $runtimeDllDst -Force
    Write-Host "operator_runtime.dll copied to exe dir" -ForegroundColor Cyan
} else {
    Write-Host "WARNING: operator_runtime.dll not found at $runtimeDllSrc" -ForegroundColor Red
}

# 2. Deploy operator DLL + operator.json + operator_runtime.dll to lib subdirs
foreach ($op in $operatorMap) {
    $destDir = Join-Path $PSScriptRoot "lib\$($op.Dir)"
    Write-Host "Copying $($op.Name).dll -> lib\$($op.Dir)\" -ForegroundColor Yellow

    # Clean and recreate
    if (Test-Path $destDir) {
        Remove-Item $destDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $destDir -Force | Out-Null

    # Copy operator DLL
    Copy-Item "$depsDir\$($op.Name).dll" $destDir

    # Copy operator.json definition
    Copy-Item "$PSScriptRoot\$($op.Json)" $destDir

    # Also copy operator_runtime.dll so operator can find its dynamic dependency
    if (Test-Path $runtimeDllSrc) {
        Copy-Item $runtimeDllSrc $destDir
    }
}

Write-Host "===== Starting operator_runtime_server =====" -ForegroundColor Green
cargo run --package operator_runtime_server