# 开发运行：先构建 clade.exe（GUI 启动 profile 时要用），再启动 tauri dev。
# 用法：在 pwsh7 里运行 .\dev.ps1
Set-Location $PSScriptRoot

cargo build --manifest-path src-tauri/Cargo.toml --bin clade
if ($LASTEXITCODE) { exit $LASTEXITCODE }

bun run tauri dev
