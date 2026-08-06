# 独立 target 目录运行 mining-app
# 此脚本设置独立的 CARGO_TARGET_DIR，防止与 operator_runtime_server 共享 operator_runtime.dll 导致冲突
$env:CARGO_TARGET_DIR = "$PSScriptRoot\target_app"
cargo run --package mining-app --bin mining-app
