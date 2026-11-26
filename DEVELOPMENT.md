# 开发指南

本文档记录 `clade` 的本地开发、构建与测试方式。项目功能介绍与日常用法见 [README.md](README.md)。

## 开发环境要求

- **操作系统**：Windows 10 / 11
- **Rust 工具链**：[rustup](https://rustup.rs/)（2024 edition）
- **包管理与前端构建**：[Bun](https://bun.sh/)
- **PowerShell 7 (`pwsh`)**：用于运行开发脚本与拉起会话终端
- **Claude Code CLI**：已安装 `claude` 并可在 `PATH` 中调用

## 构建与运行

```powershell
# 安装前端依赖
bun install

# 开发运行（在 pwsh7 中执行：先构建 clade.exe，再启动 tauri dev）
.\dev.ps1

# 仅前端类型检查与打包
bun run build

# Release 构建（产出 NSIS / MSI 安装包，自动附带同目录 clade.exe；纯可执行文件可用 --no-bundle）
bun run tauri build
```

> `clade-gui.exe`（GUI，Windows 子系统）与 `clade.exe`（控制台启动器）**必须位于同一目录**。安装包会自动将两者装到同一目录；直接分发或使用便携版时也请保持两文件同目录存放。

## 测试

```powershell
cd src-tauri

# 后端单元测试（协议转换、OAuth、Antigravity 等，不碰本机数据）
cargo test --lib

# 聚合网关集成测试（本机假上游，不碰真实数据）
cargo test --test gateway

# 端到端 smoke 测试（会读写真实 ~/.clade 与凭据管理器，仅使用 zz- 前缀并自动还原/清理；
# 其中 usage_global 用例要求 ~/.claude 已登录官方订阅账号）
cargo test --test smoke -- --include-ignored --test-threads=1
```
