# clade

Windows 专用的 [Claude Code](https://docs.anthropic.com/en/docs/claude-code) 多环境（profile）管理器，基于 **Tauri 2 + React 19 + Rust** 构建。

核心隔离方式：**每个环境对应一个独立的 `CLAUDE_CONFIG_DIR`**（`~/.clade/Agents/<name>`），避免反复改写全局 `~/.claude/settings.json`，支持多账号 / 多协议模型并行运行，并内置本地聚合网关与可视化管理界面。

> [!WARNING]
> 目前无法保证Antigravity服务不会封禁，项目作者目前已稳定使用3天

## 功能特性

- **多类型模型卡（多环境共用）**
  - **第三方 API 卡**：支持 Anthropic、OpenAI Chat Completions、OpenAI Responses、Google Gemini Native 四种协议；支持从 `/v1/models` 拉取模型或手填，按模型配置上下文窗口（默认 200K，≥ 1M 自动标记 `[1m]`）。
  - **Claude 官方卡**：每个官方卡对应独立凭据目录（`~/.clade/Cards/<id>`），一次 `claude auth login --claudeai` 即可供多个环境共用，也支持不选卡直接沿用全局 `~/.claude` 登录。
  - **OpenAI 登录卡（Codex OAuth）**：ChatGPT Plus/Pro 订阅账号，支持浏览器 PKCE 登录与设备码登录、模型拉取与额度查询。
  - **Antigravity 登录卡（Cloud Code Assist）**：Google Antigravity 账号，支持浏览器 PKCE 登录、模型拉取、额度查询与退出撤销；流式长思考期间支持抢先开启 `thinking` 块与 SSE `ping` 保活。
  - **额度与过期提示**：官方账号与 OAuth 登录卡可在界面查看额度进度条与登录到期时间（到期前 7 天标红）。
- **直连与本地聚合网关**
  - **直连模式**：绑定单张 Anthropic 协议第三方卡、官方卡或全局 `~/.claude` 登录。
  - **聚合模式**：组合多张非官方卡（第三方 4 协议 / OpenAI OAuth / Antigravity OAuth），由 `clade.exe` 在本机随机端口启动临时 HTTP 网关（随会话退出自动关闭），实现跨卡分配 5 个模型位与 `/model` 实时切换；Claude 进程只持有一次性随机 token，真实密钥与 OAuth 凭据均不出网关。
  - **网关提示词改写**（聚合模式，优先级：`环境 > 模型卡 > 全局 > 卡类型默认`）：支持替换 auto 模式分类器提示词（附精简模板）、控制主对话顶层 `system`（保留 / 剥离 / 自定义，自动保留 scratchpad 并可选保留 `# Memory`）以及 `messages` 中的 `<system-reminder>` / `role: "system"` 块。
- **配置继承、素材库与共享目录**
  - **设置继承与写回**：默认每次启动从 `~/.claude/settings.json` 剔除服务商键后叠上 `<profile>/clade-settings.json` 生成环境配置，并同步 `~/.claude.json` 界面偏好；可选「退出时将改动增量写回 `~/.claude`」，或切为全新模式独立维护。
  - **全局 `CLAUDE.md` 控制**：支持全局默认 + 单环境三态覆写，且始终自动排除原路径 `~/.claude/CLAUDE.md` 防止重复加载。
  - **统一素材库**（`_library/`）：管理 `skills`（junction 同步）、`plugins`（`--plugin-dir`）与 `mcp`（`--mcp-config`），支持全局默认 + 单环境追加/排除，以及从 `~/.claude` 一键导入。
  - **按需共享目录**：`projects`、`sessions`（实验性跨环境共享会话池）、`file-history`、`plugins`、`mods` 可通过 NTFS junction 与 `~/.claude` 互通，切换时自动备份旧目录，永不删除非空目录。
- **启动保护与日常体验**
  - 自动清除继承来的冲突环境变量（`ANTHROPIC_*`、OAuth token、Bedrock/Vertex 开关等），GUI 与终端均会提示。
  - 全局三态系统代理开关（跟随默认 / 走 / 不走），统一作用于会话、网关、OAuth 与额度查询。
  - 第三方环境自动补齐 scratchpad 目录与读写权限；启动前检测父目录 `CLAUDE.md` 并支持按环境永久排除。
  - 全部 API Key 与 OAuth 凭据保存在 Windows 凭据管理器（服务名 `clade`），不落明文文件；OAuth 刷新带跨进程文件锁防并发失效。
  - 桌面端提供启动台（最近目录一键直达）、环境/模型卡/全局设置管理、明暗主题切换与 `Ctrl+K` 全局搜索。

## 运行环境要求

- **操作系统**：Windows 10 / 11
- **Claude Code CLI**：已安装 `claude` 并可在 `PATH` 中调用
- **PowerShell 7 (`pwsh`)**：GUI 启动会话仅支持 PowerShell 7（未安装时会在界面引导通过 `winget` 安装或前往下载）

## 安装与命令行用法

可从 Releases 下载安装版（NSIS / MSI）或便携版 zip。`clade-gui.exe`（GUI）与 `clade.exe`（控制台启动器）**必须位于同一目录**（安装包会自动处理，便携版解压后保持同目录即可）。

在 GUI 的「设置 → 环境工具」里点击「添加到 PATH」，新开终端后即可直接使用：

```powershell
clade                            # 打开 GUI
clade <profile> [claude 参数...]  # 在当前工作目录启动指定环境
```

## 数据目录结构

```text
~/.clade/
├─ clade.json                 全局设置、模型卡、环境清单（第二版格式）
├─ Cards/<卡 id>/             官方卡的登录凭据目录
├─ _library/
│  ├─ skills/<name>/          通过 junction 同步到各环境的 skills/
│  ├─ plugins/<name>/         启动时通过 --plugin-dir 加载
│  └─ mcp/<name>.json         单个 MCP server 配置，如 {"command":"npx","args":[...]}
└─ Agents/
   └─ <profile>/              独立的 CLAUDE_CONFIG_DIR
      ├─ clade-settings.json  继承模式下叠在全局设置上的手写覆盖项（可选）
      ├─ clade-recent.json    最近使用过的工作目录（最多 8 条）
      ├─ clade-gateway.log    聚合模式网关日志（多会话并行时依次写 -2、-3……）
      └─ _projects/ 等        切为共享前原非空目录的自动备份（切回隔离时恢复）
```

密钥与 OAuth 凭据全部存于 Windows 凭据管理器（服务名 `clade`）：

- 第三方 API Key：`card:<卡 id>`
- OpenAI / Antigravity OAuth 凭据：`oauth:<卡 id>`（超长自动分片为 `oauth:<卡 id>:<idx>`）

## 注意事项与免责声明

- 本项目部分能力依赖 Claude Code 的**未公开内部环境变量或本地缓存结构**（例如 `CLAUDE_SECURESTORAGE_CONFIG_DIR`、`CLAUDE_CODE_REMOTE_MEMORY_DIR`、`.claude.json` 中的 `cachedUsageUtilization`、`sessions/` 跨会话池等），均在实测版本上验证通过，但随 Claude Code 升级可能发生变更或失效。
- OpenAI 登录卡与 Antigravity 登录卡通过对应客户端的公开 OAuth 流程与接口工作，请遵守各服务商的使用条款，自行承担使用风险。
- 安装包未做代码签名，Windows SmartScreen 首次运行可能弹窗提醒；也可参考 [DEVELOPMENT.md](DEVELOPMENT.md) 从源码自行构建。
