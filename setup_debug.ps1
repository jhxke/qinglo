$ErrorActionPreference = "Continue"
$rootDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$targetDir = Join-Path $rootDir "target\debug"
$depsDir = Join-Path $targetDir "deps"

if (Test-Path $targetDir) {
    $dlls = @("operator_runtime.dll", "operator_runtime.pdb")
    foreach ($dll in $dlls) {
        $src = Join-Path $targetDir $dll
        $dst = Join-Path $depsDir $dll
        if ((Test-Path $src) -and (Test-Path $depsDir)) {
            try {
                Copy-Item $src $dst -Force -ErrorAction Stop
                Write-Host "[OK] Copied $dll -> deps\"
            } catch {
                Write-Host "[SKIP] $dll is locked, waiting 2s..."
                Start-Sleep -Seconds 2
                try {
                    Copy-Item $src $dst -Force -ErrorAction Stop
                    Write-Host "[OK] Copied $dll -> deps\"
                } catch {
                    Write-Host "[FAIL] Cannot copy $dll. Close all processes and try again."
                }
            }
        }
    }
    Write-Host "Done. You can now debug your tests."
} else {
    Write-Host "No target/debug directory found. Build the project first."
}