# 独立 target 目录运行 mining-app，避免与 operator_runtime_server 抢占 operator_runtime.dll
$env:CARGO_TARGET_DIR = "$PSScriptRoot\target\app"
$env:PATH = "$PSScriptRoot\target\app\debug;$PSScriptRoot\target\debug;$env:PATH"
cargo run --package mining-app --bin mining-app @args
