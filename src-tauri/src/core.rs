//! clade 核心逻辑：存储、目录同步、密钥、启动命令。不依赖 tauri，GUI 与 clade 共用。

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

// ---------- 数据结构 ----------

/// clade.json 的格式版本。没有 version 字段的是第一版，不兼容，读到就删掉。
pub const STORE_VERSION: u64 = 2;

/// 开关的显式取值；没写进表里的开关就是「跟随默认」，clade 不管。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Switch {
    On,
    Off,
}

pub type Switches = BTreeMap<String, Switch>;

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Assets {
    pub skills: Vec<String>,
    pub plugins: Vec<String>,
    pub mcp: Vec<String>,
}

/// profile 在全局列表基础上追加和排除。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct ListOverride {
    pub add: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct AssetOverrides {
    pub skills: ListOverride,
    pub plugins: ListOverride,
    pub mcp: ListOverride,
}

/// 可共享的目录：true 表示 junction 到 ~/.claude 下的同名目录。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Shared {
    pub projects: bool,
    pub sessions: bool,
    pub file_history: bool,
    pub plugins: bool,
    pub mods: bool,
}

/// None 表示跟随全局。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SharedOverrides {
    pub projects: Option<bool>,
    pub sessions: Option<bool>,
    pub file_history: Option<bool>,
    pub plugins: Option<bool>,
    pub mods: Option<bool>,
}

/// 网关对主对话顶层 system 或 messages 里 `<system-reminder>` / `role: "system"` 的重写策略。
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum PromptPolicy {
    /// 保持原样（不改动）
    Keep,
    /// 剥离（顶层 system 仍可保留 scratchpad / # Memory；messages 中移除对应隐藏块）
    Strip,
    /// 替换为自定义文本
    Custom { text: String },
}

/// 对所有 profile 生效的设置。开关只认 GLOBAL_SWITCHES 里的。
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Global {
    pub assets: Assets,
    pub shared: Shared,
    pub switches: Switches,
    /// 子 claude 走不走系统代理：None 跟随默认（不动环境）/ On 走（环境里没有代理变量就用系统代理补上）/ Off 不走（删掉代理变量）
    pub proxy: Option<Switch>,
    /// 聚合模式本地网关拦截 auto 模式分类器请求时替换用的自定义提示词（空表示不替换）
    pub classifier_prompt: String,
    /// 聚合模式本地网关对主对话顶层 system 的默认处理策略（None 表示按卡类型默认：Antigravity 剥离，其余保留）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_policy: Option<PromptPolicy>,
    /// 剥离或自定义替换顶层 system 时，是否保留原提示词中的 `# Memory` 段落（默认 true）
    #[serde(default = "default_true")]
    pub preserve_memory_prompt: bool,
    /// 聚合模式本地网关对 messages 中 `<system-reminder>` 与 `role: "system"` 块的默认处理策略（None 表示保留）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_reminder_policy: Option<PromptPolicy>,
    /// 默认是否加载全局 ~/.claude/CLAUDE.md（复制到 profile 目录；默认 true）。profile 可单独覆盖。
    #[serde(default = "default_true")]
    pub inherit_claude_md: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Global {
    fn default() -> Self {
        Self {
            assets: Assets::default(),
            shared: Shared::default(),
            // 所有 profile 共用同一个 claude.exe，clade 依赖的若干内部行为是在固定版本上实测的
            switches: Switches::from([("DISABLE_AUTOUPDATER".into(), Switch::On)]),
            proxy: None,
            classifier_prompt: String::new(),
            system_prompt_policy: None,
            preserve_memory_prompt: true,
            system_reminder_policy: None,
            inherit_claude_md: true,
        }
    }
}

/// 推荐的精简 auto 模式分类器提示词，供界面「填入精简模板」使用。
/// 保留 Claude Code 客户端分类器要求的 XML 输出格式（<block>/<category>/<reason>），将 ~14 万字符压缩到几百字。
pub const DEFAULT_CLASSIFIER_PROMPT: &str = "\
You are a security classifier for an autonomous coding agent.
Evaluate ONLY the last tool call in <transcript> based on the user's explicit request and project scope.

Allow routine development actions within the project workspace: reading/editing project files, running builds/tests/linters, git status/diff/add/commit on local branches, installing project dependencies, and actions explicitly requested by the user.

Block dangerous or unauthorized actions unless explicitly requested by the user:
- Destructive operations: deleting/overwriting files outside the project, wiping databases/storage, `git push --force`, `git reset --hard`, destructive system commands.
- Data exfiltration & secret leakage: reading/sending credentials, tokens, private keys, `.env` secrets, or sensitive files to external endpoints.
- Unauthorized remote/production changes: deploying to production, publishing packages/artifacts, modifying shared cloud/CI/DNS/IAM resources, merging/pushing without consent.
- Privilege/persistence/bypass: modifying system/user auth or security settings, installing persistence, bypassing deny rules or safety checks.

Output Format:
- If the action should be allowed, output ONLY:
<block>no</block>
- If the action should be blocked, output ONLY:
<block>yes</block><category>Rule Name</category><reason>[Rule Name] one short sentence</reason>";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Auth {
    /// Authorization: Bearer，注入 ANTHROPIC_AUTH_TOKEN
    #[default]
    Bearer,
    /// x-api-key，注入 ANTHROPIC_API_KEY
    XApiKey,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum ApiFormat {
    /// Anthropic Messages API（默认）
    #[default]
    #[serde(rename = "anthropic")]
    Anthropic,
    /// OpenAI Chat Completions API（只允许用于聚合模式）
    #[serde(rename = "openai_chat")]
    OpenAiChat,
    /// OpenAI Responses API（只允许用于聚合模式）
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    /// Google Gemini Native generateContent API（只允许用于聚合模式）
    #[serde(rename = "gemini_native")]
    GeminiNative,
}

impl ApiFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiFormat::Anthropic => "anthropic",
            ApiFormat::OpenAiChat => "openai_chat",
            ApiFormat::OpenAiResponses => "openai_responses",
            ApiFormat::GeminiNative => "gemini_native",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CardKind {
    /// 第三方服务商（Anthropic / OpenAI / Gemini 协议），key 存凭据管理器
    #[default]
    #[serde(alias = "direct")]
    Api,
    /// Claude 官方账号：登录凭据存在卡自己的目录（~/.clade/Cards/<id>），用这张卡的 profile 共用一份登录
    Official,
    /// OpenAI (ChatGPT Plus/Pro Codex) OAuth 登录卡：凭据存在 Windows 凭据管理器，只能用于聚合模式
    #[serde(rename = "openai_oauth")]
    OpenAiOAuth,
    /// Antigravity (Google Cloud Code Assist) OAuth 登录卡：凭据存在 Windows 凭据管理器，只能用于聚合模式
    #[serde(rename = "antigravity")]
    Antigravity,
}

pub const DEFAULT_CONTEXT: u64 = 200_000;
/// 窗口达到这个值的模型，聚合时在模型 ID 后面加 [1m]
pub const CONTEXT_1M: u64 = 1_000_000;

/// 卡的模型清单里的一项
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct CardModel {
    pub id: String,
    /// 上下文窗口（token）
    pub context: u64,
}

impl Default for CardModel {
    fn default() -> Self {
        Self { id: String::new(), context: DEFAULT_CONTEXT }
    }
}

/// 模型卡：一个账号 + 可用模型清单。第三方卡的 key 存在凭据管理器；官方卡的登录凭据存在卡目录。
/// 官方卡只用 id 和 name，其余字段忽略。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Card {
    /// 规则同 profile 名，创建后不能改
    pub id: String,
    pub name: String,
    pub kind: CardKind,
    /// 接口协议（第三方卡使用，官方卡忽略）
    #[serde(default)]
    pub protocol: ApiFormat,
    pub base_url: String,
    pub auth: Auth,
    /// 可用模型清单，profile 的模型位只能从这里选
    pub model_list: Vec<CardModel>,
    /// 旧字段兼容：是否剥离 Claude Code 自带系统提示词（仅在 system_prompt_policy 为 None 时兜底）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_system_prompt: Option<bool>,
    /// 网关转发时对主对话顶层 system 的处理策略（None 跟随全局；全局也未设时按旧字段/卡类型默认）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_policy: Option<PromptPolicy>,
    /// 剥离或自定义替换顶层 system 时是否保留 `# Memory` 段落（None 跟随全局）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_memory_prompt: Option<bool>,
    /// 网关转发时对 messages 中 `<system-reminder>` 与 `role: "system"` 块的处理策略（None 跟随全局）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_reminder_policy: Option<PromptPolicy>,
}

impl Card {
    pub fn should_strip_system_prompt(&self) -> bool {
        self.strip_system_prompt.unwrap_or(self.kind == CardKind::Antigravity)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// 选一张卡：第三方卡写进 settings.json 直连，官方卡或不选卡用官方登录
    #[default]
    Direct,
    /// 选多张第三方卡，经本地网关转发，所有卡的模型都能在 /model 里选
    Aggregate,
}

/// 模型位选中的模型：哪张卡的哪个模型。id 为空表示没选（main 以外的跟随 main）。
/// 直连模式下 card 不看，用 profile 选的那张卡。
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(default)]
pub struct Pick {
    pub card: String,
    pub id: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Picks {
    pub main: Pick,
    pub sonnet: Pick,
    pub opus: Pick,
    pub fable: Pick,
    pub haiku: Pick,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub name: String,
    pub mode: Mode,
    /// 直连模式的模型卡 id，为空表示用 ~/.claude 的官方登录（直接运行 claude 时的那个账号）
    pub card: String,
    /// 聚合模式的模型卡 id（只能是第三方卡）
    pub cards: Vec<String>,
    /// 用第三方卡时的 5 个模型位：main 必填，其余为空跟随 main
    pub models: Picks,
    /// auto 模式的分类器模型（CLAUDE_CODE_AUTO_MODE_MODEL），为空跟随 sonnet 模型位
    pub classifier_model: Pick,
    /// 聚合模式本地网关替换 auto 模式分类器提示词的覆盖：None 跟随全局，Some("") 不替换，Some(非空) 用自定义内容
    pub classifier_prompt: Option<String>,
    /// 聚合模式本地网关对主对话顶层 system 的覆盖策略（优先级：Profile > 模型卡 > 全局 > 卡类型默认）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_policy: Option<PromptPolicy>,
    /// 剥离或自定义替换顶层 system 时是否保留 `# Memory` 段落（优先级：Profile > 模型卡 > 全局）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_memory_prompt: Option<bool>,
    /// 聚合模式本地网关对 messages 中 `<system-reminder>` / `role: "system"` 块的覆盖策略（优先级：Profile > 模型卡 > 全局 > 保留）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_reminder_policy: Option<PromptPolicy>,
    /// 用第三方卡时启用了 COMPAT_SWITCHES 里的哪几个
    pub compat: Vec<String>,
    pub assets: AssetOverrides,
    pub shared: SharedOverrides,
    /// 只认 PROFILE_SWITCHES 里的
    pub switches: Switches,
    /// true：每次启动用 ~/.claude/settings.json 重新生成本 profile 的 settings.json；
    /// false（全新）：settings.json 由 profile 自己维护，clade 只改自己管理的键
    pub inherit_settings: bool,
    /// true（且开启 inherit_settings）：会话退出时把本次改动的 settings.json 通用设置与 .claude.json 界面偏好写回 ~/.claude
    pub writeback_ui_prefs: bool,
    /// 是否加载全局 ~/.claude/CLAUDE.md（复制到 profile 目录）：None 跟随全局，Some(true) 加载，Some(false) 不加载。
    /// 无论开关如何，原路径 ~/.claude/CLAUDE.md 都会自动排除，避免被当作父目录项目指令重复加载或泄漏。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherit_claude_md: Option<bool>,
    /// 不加载的 CLAUDE.md（绝对路径），启动时用 --settings 传给 claude
    pub claude_md_excludes: Vec<String>,
    /// 禁用 auto 模式：On 写 disableAutoMode = "disable"，Off 删掉，None 跟随默认（不动继承来的值）
    pub disable_auto_mode: Option<Switch>,
    /// 禁用 bypassPermissions 模式：写 permissions.disableBypassPermissionsMode，取值含义同上
    pub disable_bypass_mode: Option<Switch>,
    /// 去掉 commit / PR 署名：On 写 attribution = {commit:"", pr:"", sessionUrl:false}，
    /// Off 删掉 attribution 和已废弃的 includeCoAuthoredBy（用 claude 默认的署名），None 跟随默认（不动继承来的值）
    pub hide_attribution: Option<Switch>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: String::new(),
            mode: Mode::Direct,
            card: String::new(),
            cards: Vec::new(),
            models: Picks::default(),
            classifier_model: Pick::default(),
            classifier_prompt: None,
            system_prompt_policy: None,
            preserve_memory_prompt: None,
            system_reminder_policy: None,
            compat: Vec::new(),
            assets: AssetOverrides::default(),
            shared: SharedOverrides::default(),
            switches: Switches::new(),
            inherit_settings: true,
            writeback_ui_prefs: false,
            inherit_claude_md: None,
            claude_md_excludes: Vec::new(),
            disable_auto_mode: None,
            disable_bypass_mode: None,
            hide_attribution: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub version: u64,
    pub global: Global,
    pub cards: Vec<Card>,
    pub profiles: Vec<Profile>,
}

impl Default for Store {
    fn default() -> Self {
        Self { version: STORE_VERSION, global: Global::default(), cards: Vec::new(), profiles: Vec::new() }
    }
}

#[derive(Serialize)]
pub struct Library {
    pub skills: Vec<String>,
    pub plugins: Vec<String>,
    pub mcp: Vec<String>,
}

// ---------- 开关清单 ----------

#[derive(Serialize)]
pub struct SwitchDef {
    pub key: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    /// 风险或副作用提示，没有就是空串
    pub note: &'static str,
}

const fn sw(key: &'static str, group: &'static str, label: &'static str, note: &'static str) -> SwitchDef {
    SwitchDef { key, group, label, note }
}

/// 全局开关：隐私和流量、更新和安装、界面。profile 不能覆盖。
pub const GLOBAL_SWITCHES: &[SwitchDef] = &[
    sw("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "隐私和流量", "禁用非必要网络流量", "总开关：同时关掉自动更新、遥测、错误上报、/feedback、更新说明等。会连带停掉 feature flag，Remote Control 可能失效，官方 profile 原生的 scratchpad 可能消失。"),
    sw("DISABLE_TELEMETRY", "隐私和流量", "禁用遥测", "会连带停掉 feature flag，影响同上。"),
    sw("DISABLE_ERROR_REPORTING", "隐私和流量", "禁用错误上报", ""),
    sw("CLAUDE_CODE_DISABLE_FEATURE_FLAGS", "隐私和流量", "禁用 feature flag", "Remote Control 等依赖 feature flag 的功能会失效，官方 profile 原生的 scratchpad 可能消失。"),
    sw("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY", "隐私和流量", "禁用会话质量问卷", ""),
    sw("DISABLE_FEEDBACK_COMMAND", "隐私和流量", "隐藏 /feedback、/bug、/share", ""),
    sw("DISABLE_AUTOUPDATER", "更新和安装", "禁用自动更新", "所有 profile 共用同一个 claude.exe，一处更新全部跟着变。手动 claude update 仍然可用。"),
    sw("DISABLE_UPDATES", "更新和安装", "禁止一切更新", "连手动 claude update 和 claude install 也禁止。"),
    sw("DISABLE_INSTALLATION_CHECKS", "更新和安装", "不显示安装位置警告", ""),
    sw("CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL", "更新和安装", "不自动注册官方插件市场", "一旦生效就是永久的，之后改回来也不会撤销。"),
    sw("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN", "界面", "禁用全屏渲染", "改用经典渲染，对话留在终端自己的滚动区里。"),
    sw("CLAUDE_CODE_DISABLE_MOUSE", "界面", "禁用鼠标", ""),
    sw("CLAUDE_CODE_DISABLE_MOUSE_CLICKS", "界面", "禁用鼠标点击（保留滚轮）", ""),
    sw("CLAUDE_CODE_DISABLE_VIRTUAL_SCROLL", "界面", "禁用虚拟滚动", ""),
    sw("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "界面", "不自动修改终端标题", "同时省掉生成会话标题的那次小模型请求。"),
    sw("DISABLE_COST_WARNINGS", "界面", "不显示费用警告", ""),
    sw("CLAUDE_CODE_DISABLE_NOTIFICATION_PRESENCE_CHECK", "界面", "通知不检查是否正在看终端", ""),
];

/// profile 开关：功能开关、隐藏命令。
pub const PROFILE_SWITCHES: &[SwitchDef] = &[
    sw("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "功能", "禁用自动记忆", ""),
    sw("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "功能", "不加载任何 CLAUDE.md", "包括用户级、项目级和自动记忆。"),
    sw("CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS", "功能", "去掉内置 git 指引和 git status 快照", ""),
    sw("CLAUDE_CODE_DISABLE_ATTACHMENTS", "功能", "@ 文件不展开成内容", ""),
    sw("CLAUDE_CODE_DISABLE_FILE_MENTIONS", "功能", "@ 文件只给路径", ""),
    sw("DISABLE_AUTO_COMPACT", "功能", "禁用自动压缩", ""),
    sw("DISABLE_COMPACT", "功能", "禁用全部压缩", "连手动 /compact 也禁止。"),
    sw("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "功能", "禁用后台任务", ""),
    sw("CLAUDE_CODE_DISABLE_AGENT_VIEW", "功能", "禁用后台代理和 agent view", ""),
    sw("CLAUDE_CODE_DISABLE_ARTIFACT", "功能", "禁用 Artifact 工具", ""),
    sw("CLAUDE_CODE_DISABLE_ADVISOR_TOOL", "功能", "禁用 advisor 工具", ""),
    sw("CLAUDE_CODE_DISABLE_CRON", "功能", "禁用定时任务（/loop）", ""),
    sw("CLAUDE_CODE_DISABLE_WORKFLOWS", "功能", "禁用 workflows", ""),
    sw("CLAUDE_CODE_DISABLE_EXPLORE_PLAN_AGENTS", "功能", "禁用内置 Explore 和 Plan 子代理", ""),
    sw("CLAUDE_CODE_DISABLE_BUNDLED_SKILLS", "功能", "禁用自带的 skills", ""),
    sw("CLAUDE_CODE_DISABLE_WEB_FETCH", "功能", "禁用 WebFetch", ""),
    sw("CLAUDE_CODE_DISABLE_MCP_TASK_BACKGROUND", "功能", "MCP 任务等结果，不转后台", ""),
    sw("CLAUDE_CODE_DISABLE_FAST_MODE", "功能", "禁用 fast mode", ""),
    sw("CLAUDE_CODE_DISABLE_ULTRATHINK", "功能", "禁用 ultrathink 关键词", ""),
    sw("CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING", "功能", "禁用自适应思考", "只对 Opus 4.6 和 Sonnet 4.6 生效。"),
    sw("CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK", "功能", "安全分类器拦截后不自动换模型", ""),
    sw("CLAUDE_CODE_DISABLE_FILE_CHECKPOINTING", "功能", "禁用文件检查点", "/rewind 将不能还原代码。"),
    sw("DISABLE_DOCTOR_COMMAND", "隐藏命令", "隐藏 /doctor", ""),
    sw("DISABLE_EXTRA_USAGE_COMMAND", "隐藏命令", "隐藏 /usage-credits", ""),
    sw("DISABLE_LOGIN_COMMAND", "隐藏命令", "隐藏 /login", ""),
    sw("DISABLE_LOGOUT_COMMAND", "隐藏命令", "隐藏 /logout", ""),
    sw("DISABLE_UPGRADE_COMMAND", "隐藏命令", "隐藏 /upgrade", ""),
];

/// 兼容开关：第三方服务商不认某些字段时用。用第三方卡的 profile 勾了就写 1，没勾就删掉。
pub const COMPAT_SWITCHES: &[SwitchDef] = &[
    sw("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS", "兼容", "去掉 beta 请求头和对应字段", "网关报 Unexpected value(s) 或 Extra inputs are not permitted 时用。"),
    sw("CLAUDE_CODE_DISABLE_STRUCTURED_OUTPUTS", "兼容", "不发结构化输出字段", ""),
    sw("CLAUDE_CODE_DISABLE_THINKING", "兼容", "不发 thinking 参数", ""),
    sw("DISABLE_INTERLEAVED_THINKING", "兼容", "不发交错思考的 beta 头", ""),
    sw("CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK", "兼容", "流式失败时不退回非流式", "网关导致工具被重复执行时用。"),
    sw("DISABLE_PROMPT_CACHING", "兼容", "关闭全部提示词缓存", ""),
    sw("DISABLE_PROMPT_CACHING_FABLE", "兼容", "关闭 Fable 档的提示词缓存", ""),
    sw("DISABLE_PROMPT_CACHING_OPUS", "兼容", "关闭 Opus 档的提示词缓存", ""),
    sw("DISABLE_PROMPT_CACHING_SONNET", "兼容", "关闭 Sonnet 档的提示词缓存", ""),
    sw("DISABLE_PROMPT_CACHING_HAIKU", "兼容", "关闭 Haiku 档的提示词缓存", ""),
];

/// 这个 profile 实际生效的开关（全局 + profile 自己的），只保留清单里有的。
pub fn effective_switches(global: &Global, p: &Profile) -> Vec<(&'static str, Switch)> {
    let pick = |defs: &[SwitchDef], map: &Switches| -> Vec<(&'static str, Switch)> {
        defs.iter().filter_map(|d| map.get(d.key).map(|s| (d.key, *s))).collect()
    };
    let mut out = pick(GLOBAL_SWITCHES, &global.switches);
    out.extend(pick(PROFILE_SWITCHES, &p.switches));
    out
}

// ---------- 路径和存储 ----------

fn home() -> PathBuf {
    dirs::home_dir().expect("找不到用户主目录")
}

/// 数据目录：clade.json 和 _library 在这里，各 profile 在 Agents/ 下
pub fn root() -> PathBuf {
    home().join(".clade")
}

pub fn lib_dir(kind: &str) -> PathBuf {
    root().join("_library").join(kind)
}

pub fn profile_dir(name: &str) -> PathBuf {
    root().join("Agents").join(name)
}

/// 官方卡的目录：登录凭据（.credentials.json）和账号信息都在这里
pub fn card_dir(id: &str) -> PathBuf {
    root().join("Cards").join(id)
}

fn store_path() -> PathBuf {
    root().join("clade.json")
}

/// 第一版的 clade.json（没有 version）不兼容，直接删掉、从空的开始；旧的 profile 目录不动。
pub fn load() -> Result<Store> {
    let path = store_path();
    if !path.exists() {
        return Ok(Store::default());
    }
    let v: Value = serde_json::from_str(&fs::read_to_string(&path)?).with_context(|| format!("解析 {} 失败", path.display()))?;
    match v.get("version").and_then(Value::as_u64) {
        None => {
            fs::remove_file(&path)?;
            Ok(Store::default())
        }
        Some(STORE_VERSION) => serde_json::from_value(v).with_context(|| format!("解析 {} 失败", path.display())),
        Some(n) => bail!("{} 是更新版本的格式（version {n}），请升级 clade", path.display()),
    }
}

pub fn save(store: &Store) -> Result<()> {
    fs::create_dir_all(root())?;
    fs::write(store_path(), serde_json::to_string_pretty(store)? + "\n")?;
    Ok(())
}

pub fn find<'a>(store: &'a Store, name: &str) -> Result<&'a Profile> {
    store
        .profiles
        .iter()
        .find(|p| p.name == name)
        .with_context(|| format!("没有名为 {name} 的 profile"))
}

fn find_card<'a>(store: &'a Store, p: &Profile, id: &str) -> Result<&'a Card> {
    store
        .cards
        .iter()
        .find(|c| c.id == id)
        .with_context(|| format!("{} 用的模型卡 {id} 不存在", p.name))
}

/// 模型位解析出来的结果：哪张卡的哪个模型
pub type ModelRef<'a> = (&'a Card, &'a CardModel);

pub struct Slots<'a> {
    pub main: ModelRef<'a>,
    /// sonnet、opus、fable、haiku，None 跟随 main
    pub tiers: [Option<ModelRef<'a>>; 4],
    pub classifier: Option<ModelRef<'a>>,
}

/// profile 实际用哪种连接。
pub enum Conn<'a> {
    /// 不选卡：~/.claude 的官方登录
    Global,
    Official(&'a Card),
    Direct(&'a Card, Slots<'a>),
    Aggregate(Vec<&'a Card>, Slots<'a>),
}

impl Conn<'_> {
    /// 用的是第三方服务商（要补 scratchpad、写兼容开关）
    pub fn third_party(&self) -> bool {
        matches!(self, Conn::Direct(..) | Conn::Aggregate(..))
    }
}

/// 模型位只能从卡的清单里选。直连模式只看 pick.id，聚合模式还要求 pick.card 在所选的卡里。
fn resolve_slots<'a>(p: &Profile, cards: &[&'a Card], direct: bool) -> Result<Slots<'a>> {
    let one = |label: &str, pick: &Pick| -> Result<Option<ModelRef<'a>>> {
        if pick.id.is_empty() {
            return Ok(None);
        }
        let card = if direct {
            cards[0]
        } else {
            *cards
                .iter()
                .find(|c| c.id == pick.card)
                .with_context(|| format!("{} 的{label}用的卡 {} 不在所选的卡里", p.name, pick.card))?
        };
        let model = card
            .model_list
            .iter()
            .find(|m| m.id == pick.id)
            .with_context(|| format!("{} 的{label} {} 不在卡 {} 的模型清单里", p.name, pick.id, card.id))?;
        Ok(Some((card, model)))
    };
    let m = &p.models;
    Ok(Slots {
        main: one("主模型", &m.main)?.with_context(|| format!("{} 没有选主模型", p.name))?,
        tiers: [
            one("Sonnet 模型位", &m.sonnet)?,
            one("Opus 模型位", &m.opus)?,
            one("Fable 模型位", &m.fable)?,
            one("Haiku 模型位", &m.haiku)?,
        ],
        classifier: one("分类器模型", &p.classifier_model)?,
    })
}

/// 检查 profile 引用的卡和模型，得到它的连接方式。
pub fn resolve<'a>(store: &'a Store, p: &Profile) -> Result<Conn<'a>> {
    match p.mode {
        Mode::Direct if p.card.is_empty() => Ok(Conn::Global),
        Mode::Direct => {
            let card = find_card(store, p, &p.card)?;
            if card.kind == CardKind::Official {
                return Ok(Conn::Official(card));
            }
            if card.kind == CardKind::OpenAiOAuth {
                bail!("OpenAI 登录卡只能用于聚合模式");
            }
            if card.kind == CardKind::Antigravity {
                bail!("Antigravity 登录卡只能用于聚合模式");
            }
            if card.protocol != ApiFormat::Anthropic {
                bail!("非 Anthropic 协议的卡只能用于聚合模式");
            }
            Ok(Conn::Direct(card, resolve_slots(p, &[card], true)?))
        }
        Mode::Aggregate => {
            if p.cards.is_empty() {
                bail!("{} 是聚合模式，至少要选一张卡", p.name);
            }
            let mut cards = Vec::new();
            for id in &p.cards {
                let c = find_card(store, p, id)?;
                if c.kind == CardKind::Official {
                    bail!("官方卡 {id} 不能用于聚合模式");
                }
                if cards.iter().any(|x: &&Card| x.id == c.id) {
                    bail!("卡 {id} 选了两次");
                }
                cards.push(c);
            }
            let slots = resolve_slots(p, &cards, false)?;
            Ok(Conn::Aggregate(cards, slots))
        }
    }
}

impl Profile {
    /// 当前模式下是否用到这张卡
    pub fn uses(&self, card: &str) -> bool {
        match self.mode {
            Mode::Direct => self.card == card,
            Mode::Aggregate => self.cards.iter().any(|c| c == card),
        }
    }
}

/// 名字会成为目录名和 clade 的命令行参数，所以限制得严一点。模型卡 id 也用这条规则。
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.chars().next().unwrap().is_ascii_alphanumeric()
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        bail!("名字只能用小写字母、数字和 -，且以字母或数字开头");
    }
    Ok(())
}

pub fn validate_card(c: &Card) -> Result<()> {
    validate_name(&c.id)?;
    if c.kind == CardKind::Official {
        return Ok(());
    }
    if c.kind == CardKind::Api && !(c.base_url.starts_with("https://") || c.base_url.starts_with("http://")) {
        bail!("Base URL 要以 http:// 或 https:// 开头");
    }
    for (i, m) in c.model_list.iter().enumerate() {
        if m.id.is_empty() {
            bail!("模型清单里有空的模型名");
        }
        if m.context == 0 {
            bail!("{} 的上下文窗口要大于 0", m.id);
        }
        if c.model_list[..i].iter().any(|x| x.id == m.id) {
            bail!("模型 {} 重复了", m.id);
        }
    }
    Ok(())
}

// ---------- 素材库 ----------

fn list(dir: &Path, want_dir: bool, ext: Option<&str>) -> Result<Vec<String>> {
    fs::create_dir_all(dir)?;
    let mut out = Vec::new();
    for e in fs::read_dir(dir)? {
        let path = e?.path();
        if path.is_dir() != want_dir {
            continue;
        }
        if let Some(ext) = ext
            && path.extension().and_then(|s| s.to_str()) != Some(ext)
        {
            continue;
        }
        let name = if ext.is_some() { path.file_stem() } else { path.file_name() };
        out.push(name.unwrap().to_string_lossy().into_owned());
    }
    out.sort();
    Ok(out)
}

pub fn library() -> Result<Library> {
    Ok(Library {
        skills: list(&lib_dir("skills"), true, None)?,
        plugins: list(&lib_dir("plugins"), true, None)?,
        mcp: list(&lib_dir("mcp"), false, Some("json"))?,
    })
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            fs::copy(e.path(), to)?;
        }
    }
    Ok(())
}

/// 去掉路径非法字符，避免 MCP 名或插件名拼进文件名/目录名时失败或穿越路径。
fn safe_asset_name(raw: &str) -> Option<String> {
    let name: String = raw
        .trim()
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let name = name.trim_matches(['.', ' ']).to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// 把 ~/.claude 里的 skills、已安装插件和 ~/.claude.json 里的全局 mcpServers 导入到指定素材库目录，已存在的不覆盖。
pub fn import_from_claude_into(claude_home: &Path, lib_root: &Path) -> Result<Vec<String>> {
    let mut done = Vec::new();
    let claude_dir = claude_home.join(".claude");

    // 1. skills: ~/.claude/skills/<name>/ -> _library/skills/<name>/
    let skills = claude_dir.join("skills");
    if skills.is_dir() {
        let dst_root = lib_root.join("skills");
        for name in list(&skills, true, None)? {
            let to = dst_root.join(&name);
            if !to.exists() {
                copy_dir(&skills.join(&name), &to)?;
                done.push(format!("skill: {name}"));
            }
        }
    }

    // 2. plugins: ~/.claude/plugins/installed_plugins.json -> _library/plugins/<name>/
    let installed_path = claude_dir.join("plugins").join("installed_plugins.json");
    if installed_path.is_file() {
        let root = read_json_object(&installed_path)?;
        if let Some(Value::Object(plugins)) = root.get("plugins") {
            let dst_root = lib_root.join("plugins");
            for (key, entries) in plugins {
                let Some(arr) = entries.as_array() else { continue };
                let Some(entry) = arr.iter().find(|e| e.get("scope").and_then(Value::as_str) == Some("user")).or_else(|| arr.first()) else {
                    continue;
                };
                let Some(install_path) = entry.get("installPath").and_then(Value::as_str).map(PathBuf::from) else {
                    continue;
                };
                if !install_path.is_dir() {
                    continue;
                }
                let raw_name = key.split('@').next().unwrap_or(key);
                let Some(name) = safe_asset_name(raw_name) else { continue };
                let to = dst_root.join(&name);
                if !to.exists() {
                    copy_dir(&install_path, &to)?;
                    done.push(format!("插件: {name}"));
                }
            }
        }
    }

    // 3. mcp: ~/.claude.json 顶层 mcpServers -> _library/mcp/<name>.json
    let claude_json = claude_home.join(".claude.json");
    if claude_json.is_file() {
        let root = read_json_object(&claude_json)?;
        if let Some(Value::Object(servers)) = root.get("mcpServers") {
            let dst_root = lib_root.join("mcp");
            fs::create_dir_all(&dst_root)?;
            for (raw_name, cfg) in servers {
                if !cfg.is_object() {
                    continue;
                }
                let Some(name) = safe_asset_name(raw_name) else { continue };
                let to = dst_root.join(format!("{name}.json"));
                if !to.exists() {
                    fs::write(&to, serde_json::to_string_pretty(cfg)? + "\n")?;
                    done.push(format!("MCP: {name}"));
                }
            }
        }
    }

    Ok(done)
}

/// 把 ~/.claude 里的 skills、已安装插件和 ~/.claude.json 的全局 MCP 导入素材库，已存在的不覆盖。返回导入了什么。
pub fn import_from_claude() -> Result<Vec<String>> {
    import_from_claude_into(&home(), &root().join("_library"))
}

/// 全局列表加上 profile 追加的，去掉 profile 排除的。
pub fn effective_list(global: &[String], o: &ListOverride) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for x in global.iter().chain(&o.add) {
        if !o.exclude.contains(x) && !out.contains(x) {
            out.push(x.clone());
        }
    }
    out
}

pub fn effective_assets(global: &Global, p: &Profile) -> Assets {
    Assets {
        skills: effective_list(&global.assets.skills, &p.assets.skills),
        plugins: effective_list(&global.assets.plugins, &p.assets.plugins),
        mcp: effective_list(&global.assets.mcp, &p.assets.mcp),
    }
}

/// 可共享的目录名和这个 profile 实际是否共享。
pub fn effective_shared(global: &Global, p: &Profile) -> [(&'static str, bool); 5] {
    let (g, o) = (&global.shared, &p.shared);
    [
        ("projects", o.projects.unwrap_or(g.projects)),
        ("sessions", o.sessions.unwrap_or(g.sessions)),
        ("file-history", o.file_history.unwrap_or(g.file_history)),
        ("plugins", o.plugins.unwrap_or(g.plugins)),
        ("mods", o.mods.unwrap_or(g.mods)),
    ]
}

/// 聚合模式本地网关实际生效的自定义分类器提示词：
/// profile 为 None 时跟随全局，Some("")（或全空白）强制不替换，Some(非空) 用 profile 的自定义提示词。
pub fn effective_classifier_prompt(global: &Global, p: &Profile) -> Option<String> {
    let raw = match &p.classifier_prompt {
        Some(s) => s.as_str(),
        None => global.classifier_prompt.as_str(),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

/// 聚合模式本地网关实际生效的主对话顶层 system 提示词策略：
/// 优先级：Profile > 模型卡（新字段 `system_prompt_policy` > 旧字段 `strip_system_prompt`）> 全局 > 卡类型默认（Antigravity 剥离，其余保留）。
pub fn effective_system_prompt_policy(global: &Global, p: &Profile, card: &Card) -> PromptPolicy {
    if let Some(pol) = &p.system_prompt_policy {
        return pol.clone();
    }
    if let Some(pol) = &card.system_prompt_policy {
        return pol.clone();
    }
    if let Some(strip) = card.strip_system_prompt {
        return if strip { PromptPolicy::Strip } else { PromptPolicy::Keep };
    }
    if let Some(pol) = &global.system_prompt_policy {
        return pol.clone();
    }
    if card.kind == CardKind::Antigravity {
        PromptPolicy::Strip
    } else {
        PromptPolicy::Keep
    }
}

/// 剥离或自定义替换顶层 system 时，是否保留原提示词中的 `# Memory` 段落（Profile > 模型卡 > 全局）。
pub fn effective_preserve_memory_prompt(global: &Global, p: &Profile, card: &Card) -> bool {
    p.preserve_memory_prompt
        .or(card.preserve_memory_prompt)
        .unwrap_or(global.preserve_memory_prompt)
}

/// 聚合模式本地网关实际生效的 messages 中 `<system-reminder>` / `role: "system"` 块处理策略：
/// 优先级：Profile > 模型卡 > 全局 > 默认保留。
pub fn effective_system_reminder_policy(global: &Global, p: &Profile, card: &Card) -> PromptPolicy {
    p.system_reminder_policy
        .clone()
        .or_else(|| card.system_reminder_policy.clone())
        .or_else(|| global.system_reminder_policy.clone())
        .unwrap_or(PromptPolicy::Keep)
}

// ---------- 同步 profile 目录 ----------

pub fn apply(store: &Store, p: &Profile) -> Result<()> {
    let conn = resolve(store, p)?;
    let dir = profile_dir(&p.name);
    fs::create_dir_all(&dir)?;
    write_settings(&dir, &store.global, p, &conn)?;

    // 新配置目录会触发首次引导（选主题等），预置跳过；继承模式下顺便把 ~/.claude.json 的全局界面偏好同步进来。
    sync_claude_state(&dir, p.inherit_settings)?;

    let assets = effective_assets(&store.global, p);
    sync_junctions(&dir.join("skills"), &lib_dir("skills"), &assets.skills)?;

    if p.inherit_claude_md.unwrap_or(store.global.inherit_claude_md) {
        let src = global_claude_md();
        if src.is_file() {
            fs::copy(&src, dir.join("CLAUDE.md")).with_context(|| format!("复制 {} 失败", src.display()))?;
        }
    }

    let mut servers = Map::new();
    for name in &assets.mcp {
        let path = lib_dir("mcp").join(format!("{name}.json"));
        let text = fs::read_to_string(&path).with_context(|| format!("读取 {} 失败", path.display()))?;
        let v: Value = serde_json::from_str(&text).with_context(|| format!("{} 不是合法 JSON", path.display()))?;
        servers.insert(name.clone(), v);
    }
    let mcp = serde_json::json!({ "mcpServers": servers });
    fs::write(dir.join("clade-mcp.json"), serde_json::to_string_pretty(&mcp)? + "\n")?;

    for (name, shared) in effective_shared(&store.global, p) {
        sync_shared(&dir, name, shared)?;
    }
    Ok(())
}

/// ~/.claude.json 里属于全局用户/界面偏好的键（不含账号、凭据、mcpServers、projects 与运行时缓存）。
/// 对照 Claude Code 2.1.330 内置全局配置键清单与 /config 面板实际写入的键整理。
const CLAUDE_STATE_PREF_KEYS: &[&str] = &[
    "theme",
    "verbose",
    "preferredNotifChannel",
    "shiftEnterKeyBindingInstalled",
    "optionAsMetaKeyInstalled",
    "editorMode",
    "autoCompactEnabled",
    "autoScrollEnabled",
    "showTurnDuration",
    "externalEditorContext",
    "showMessageTimestamps",
    "diffTool",
    "todoFeatureEnabled",
    "showExpandedTodos",
    "briefTranscript",
    "diffSidebarOpen",
    "messageIdleNotifThresholdMs",
    "autoConnectIde",
    "autoInstallIdeExtension",
    "fileCheckpointingEnabled",
    "terminalProgressBarEnabled",
    "showStatusInTerminalTab",
    "taskCompleteNotifEnabled",
    "inputNeededNotifEnabled",
    "agentPushNotifEnabled",
    "respectGitignore",
    "claudeInChromeDefaultEnabled",
    "lspRecommendationDisabled",
    "lspRecommendationNeverPlugins",
    "copyFullResponse",
    "copyOnSelect",
    "leftArrowOpensAgents",
    "defaultToAgentsView",
    "prStatusFooterEnabled",
    "remoteControlAtStartup",
    "autoUploadSessions",
    "autoAddRemoteControlDaemonWorker",
    "workflowSizeGuideline",
    "teammateMode",
];

/// 把全局 ~/.claude.json 里的用户界面偏好同步到目标 .claude.json，保留目标文件里的其余运行时状态。
/// 返回是否写回了文件。
pub fn sync_claude_state_from(global_state: &Path, dst_state: &Path, inherit: bool) -> Result<bool> {
    let mut dst = if dst_state.exists() {
        read_json_object(dst_state)?
    } else {
        let mut m = Map::new();
        m.insert("hasCompletedOnboarding".into(), Value::Bool(true));
        m
    };

    let mut changed = !dst_state.exists();
    if inherit && global_state.is_file() {
        let global = read_json_object(global_state)?;
        for &k in CLAUDE_STATE_PREF_KEYS {
            if let Some(v) = global.get(k)
                && dst.get(k) != Some(v)
            {
                dst.insert(k.into(), v.clone());
                changed = true;
            }
        }
    }

    if changed {
        write_json_atomic(dst_state, &dst)?;
    }
    Ok(changed)
}

fn write_json_atomic(path: &Path, obj: &Map<String, Value>) -> Result<()> {
    let tmp = path.with_extension(format!("json.clade-{}.tmp", uuid::Uuid::new_v4().simple()));
    fs::write(&tmp, serde_json::to_string_pretty(obj)? + "\n")?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

fn sync_claude_state(dir: &Path, inherit: bool) -> Result<()> {
    sync_claude_state_from(&home().join(".claude.json"), &dir.join(".claude.json"), inherit)?;
    Ok(())
}

/// 抓取 .claude.json 里白名单界面偏好的快照，用于会话退出后做 diff 写回。
pub fn snapshot_ui_prefs_from(state_path: &Path) -> Result<Map<String, Value>> {
    let root = read_json_object(state_path)?;
    let mut out = Map::new();
    for &k in CLAUDE_STATE_PREF_KEYS {
        if let Some(v) = root.get(k) {
            out.insert(k.into(), v.clone());
        }
    }
    Ok(out)
}

/// 跨进程独占文件锁，避免多个会话同时写回 ~/.claude.json 时互相覆盖。
fn acquire_writeback_lock() -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};
    const ERROR_SHARING_VIOLATION: i32 = 32;

    let path = std::env::temp_dir().join("clade-state-writeback.lock");
    let start = Instant::now();
    loop {
        match fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path)
        {
            Ok(f) => return Ok(f),
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => {
                if start.elapsed() >= Duration::from_secs(5) {
                    bail!("等待 ~/.claude.json 写回锁超时");
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(e).with_context(|| format!("创建写回锁 {} 失败", path.display())),
        }
    }
}

/// 对比 profile 退出后的 .claude.json 与启动前快照，只把白名单内实际发生变化的键合并写回全局 .claude.json。
/// 返回实际写回的键名列表（无变化时返回空且不碰磁盘）。
pub fn writeback_ui_prefs_between(
    before: &Map<String, Value>,
    profile_state: &Path,
    global_state: &Path,
) -> Result<Vec<String>> {
    if !profile_state.is_file() {
        return Ok(Vec::new());
    }
    let after = snapshot_ui_prefs_from(profile_state)?;
    let mut diff: Vec<(&'static str, Option<Value>)> = Vec::new();
    for &k in CLAUDE_STATE_PREF_KEYS {
        let b = before.get(k);
        let a = after.get(k);
        if a != b {
            diff.push((k, a.cloned()));
        }
    }
    if diff.is_empty() {
        return Ok(Vec::new());
    }

    let _lock = acquire_writeback_lock()?;
    let mut global = read_json_object(global_state)?;
    let mut written = Vec::new();
    for (k, val) in diff {
        match val {
            Some(v) => {
                if global.get(k) != Some(&v) {
                    global.insert(k.into(), v);
                    written.push(k.to_string());
                }
            }
            None => {
                if global.remove(k).is_some() {
                    written.push(k.to_string());
                }
            }
        }
    }
    if !written.is_empty() {
        write_json_atomic(global_state, &global)?;
    }
    Ok(written)
}

/// 会话启动前抓拍的写回基线（仅包含需要做 diff 的两份配置）。
pub struct WritebackSnapshot {
    pub ui_prefs: Map<String, Value>,
    pub settings: Map<String, Value>,
}

/// 退出写回结果：分别记录写回 ~/.claude.json 和 ~/.claude/settings.json 的键路径。
#[derive(Default)]
pub struct WritebackResult {
    pub ui_prefs: Vec<String>,
    pub settings: Vec<String>,
}

fn is_clade_managed_env(key: &str) -> bool {
    is_provider_env(key)
        || CARD_ENV.contains(&key)
        || GLOBAL_SWITCHES.iter().any(|d| d.key == key)
        || PROFILE_SWITCHES.iter().any(|d| d.key == key)
        || COMPAT_SWITCHES.iter().any(|d| d.key == key)
}

fn is_blocked_settings_path(path: &[String], p: &Profile) -> bool {
    match path {
        [k] => {
            PROVIDER_TOP_KEYS.contains(&k.as_str())
                || (p.disable_auto_mode.is_some() && k == "disableAutoMode")
                || (p.hide_attribution.is_some() && (k == "attribution" || k == "includeCoAuthoredBy"))
        }
        [a, b] if a == "permissions" => {
            (p.disable_auto_mode.is_some() && b == "disableAutoMode")
                || (p.disable_bypass_mode.is_some() && b == "disableBypassPermissionsMode")
        }
        [a, b] if a == "env" => is_clade_managed_env(b),
        _ => false,
    }
}

/// 递归计算 before -> after 的 settings 增量补丁，跳过被禁止回写或由 clade-settings.json 覆盖的路径。
fn diff_settings_map(
    prefix: &mut Vec<String>,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
    overrides: Option<&Map<String, Value>>,
    p: &Profile,
    ops: &mut Vec<(Vec<String>, Option<Value>)>,
) {
    let mut keys: Vec<&String> = before.keys().chain(after.keys()).collect();
    keys.sort();
    keys.dedup();
    let empty = Map::new();

    for k in keys {
        prefix.push(k.clone());
        let ov = overrides.and_then(|m| m.get(k));
        let blocked = is_blocked_settings_path(prefix, p) || matches!(ov, Some(v) if !v.is_object());
        if !blocked {
            let b = before.get(k);
            let a = after.get(k);
            if a != b {
                let ov_obj = ov.and_then(Value::as_object);
                match (b, a) {
                    (Some(Value::Object(bm)), Some(Value::Object(am))) => {
                        diff_settings_map(prefix, bm, am, ov_obj, p, ops);
                    }
                    (None, Some(Value::Object(am))) => {
                        diff_settings_map(prefix, &empty, am, ov_obj, p, ops);
                    }
                    (Some(Value::Object(bm)), None) => {
                        diff_settings_map(prefix, bm, &empty, ov_obj, p, ops);
                    }
                    (_, Some(v)) => {
                        // 数组、标量，或对象与非对象互换：整体替换（若新值是对象且旧值非对象，这里先过滤一层不允许的子键）
                        if let Value::Object(am) = v {
                            diff_settings_map(prefix, &empty, am, ov_obj, p, ops);
                        } else {
                            ops.push((prefix.clone(), Some(v.clone())));
                        }
                    }
                    (_, None) => {
                        ops.push((prefix.clone(), None));
                    }
                }
            }
        }
        prefix.pop();
    }
}

fn set_json_path(obj: &mut Map<String, Value>, path: &[String], val: Value) -> bool {
    let Some((head, tail)) = path.split_first() else { return false };
    if tail.is_empty() {
        if obj.get(head) == Some(&val) {
            return false;
        }
        obj.insert(head.clone(), val);
        return true;
    }
    let child = obj.entry(head.clone()).or_insert_with(|| Value::Object(Map::new()));
    if !child.is_object() {
        *child = Value::Object(Map::new());
    }
    set_json_path(child.as_object_mut().unwrap(), tail, val)
}

fn remove_json_path(obj: &mut Map<String, Value>, path: &[String]) -> bool {
    let Some((head, tail)) = path.split_first() else { return false };
    if tail.is_empty() {
        return obj.remove(head).is_some();
    }
    let Some(Value::Object(child)) = obj.get_mut(head) else { return false };
    let removed = remove_json_path(child, tail);
    if removed && child.is_empty() {
        obj.remove(head);
    }
    removed
}

/// 对比 profile 退出后的 settings.json 与启动前快照，只把允许回写的增量补丁应用到全局 settings.json。
/// 返回实际写回的键路径列表（无变化时返回空且不碰磁盘）。
pub fn writeback_settings_between(
    before: &Map<String, Value>,
    profile_settings: &Path,
    overrides_path: &Path,
    global_settings: &Path,
    p: &Profile,
) -> Result<Vec<String>> {
    if !profile_settings.is_file() {
        return Ok(Vec::new());
    }
    let after = read_json_object(profile_settings)?;
    let overrides = read_json_object(overrides_path)?;
    let mut ops = Vec::new();
    let mut prefix = Vec::new();
    diff_settings_map(&mut prefix, before, &after, Some(&overrides), p, &mut ops);
    if ops.is_empty() {
        return Ok(Vec::new());
    }

    let _lock = acquire_writeback_lock()?;
    let mut global = read_json_object(global_settings)?;
    let mut written = Vec::new();
    for (path, val) in ops {
        let changed = match val {
            Some(v) => set_json_path(&mut global, &path, v),
            None => remove_json_path(&mut global, &path),
        };
        if changed {
            written.push(path.join("."));
        }
    }
    if !written.is_empty() {
        if let Some(parent) = global_settings.parent() {
            fs::create_dir_all(parent)?;
        }
        write_json_atomic(global_settings, &global)?;
    }
    Ok(written)
}

/// 若该 profile 开启了继承且启用了写回，返回启动前快照；否则返回 None。
pub fn snapshot_profile_writeback(name: &str) -> Result<Option<WritebackSnapshot>> {
    let store = load()?;
    let p = find(&store, name)?;
    if !p.inherit_settings || !p.writeback_ui_prefs {
        return Ok(None);
    }
    let dir = profile_dir(name);
    Ok(Some(WritebackSnapshot {
        ui_prefs: snapshot_ui_prefs_from(&dir.join(".claude.json"))?,
        settings: read_json_object(&dir.join("settings.json"))?,
    }))
}

/// 把会话期间修改过的界面偏好与通用 settings 增量写回 ~/.claude。
pub fn writeback_profile(name: &str, before: &WritebackSnapshot) -> Result<WritebackResult> {
    let store = load()?;
    let p = find(&store, name)?;
    let dir = profile_dir(name);
    let ui_prefs = writeback_ui_prefs_between(&before.ui_prefs, &dir.join(".claude.json"), &home().join(".claude.json"))?;
    let settings = writeback_settings_between(
        &before.settings,
        &dir.join("settings.json"),
        &dir.join("clade-settings.json"),
        &home().join(".claude").join("settings.json"),
        p,
    )?;
    Ok(WritebackResult { ui_prefs, settings })
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    serde_json::from_str(&fs::read_to_string(path)?).with_context(|| format!("{} 不是合法 JSON 对象", path.display()))
}

// 继承全局设置时要去掉的服务商相关键，照搬 cc-switch 的 floor 清单（src-tauri/src/live/floor.rs）
const PROVIDER_ENV_PREFIXES: &[&str] = &["ANTHROPIC_", "AWS_", "VERTEX_REGION_"];
const PROVIDER_ENV_KEYS: &[&str] = &[
    "CLAUDE_CODE_SUBAGENT_MODEL",
    "CLOUD_ML_REGION",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "CLAUDE_CODE_API_KEY_HELPER_TTL_MS",
];
const PROVIDER_TOP_KEYS: &[&str] = &[
    "apiKeyHelper",
    "apiBaseUrl",
    "primaryModel",
    "smallFastModel",
    "apiKey",
    "model",
    "fallbackModel",
    "modelOverrides",
    "modelPicker",
    "advisorModel",
    "awsAuthRefresh",
    "awsCredentialExport",
    "gcpAuthRefresh",
];

fn is_provider_env(key: &str) -> bool {
    PROVIDER_ENV_PREFIXES.iter().any(|p| key.starts_with(p))
        || PROVIDER_ENV_KEYS.contains(&key)
        || CONFLICT_ENV.contains(&key)
        || (key.starts_with("CLAUDE_CODE_SKIP_") && key.ends_with("_AUTH"))
}

/// 全局 settings.json 去掉服务商相关的键。
fn strip_provider_keys(root: &mut Map<String, Value>) {
    for k in PROVIDER_TOP_KEYS {
        root.remove(*k);
    }
    if let Some(Value::Object(env)) = root.get_mut("env") {
        env.retain(|k, _| !is_provider_env(k));
    }
}

/// 把 over 叠到 base 上：对象逐层合并、over 优先；数组和标量整个替换；null 表示删除该键。
fn merge_settings(base: &mut Map<String, Value>, over: &Map<String, Value>) {
    for (k, v) in over {
        match v {
            Value::Null => {
                base.remove(k);
            }
            Value::Object(o) => {
                let entry = base.entry(k.clone()).or_insert_with(|| Value::Object(Map::new()));
                if !entry.is_object() {
                    *entry = Value::Object(Map::new());
                }
                merge_settings(entry.as_object_mut().unwrap(), o);
            }
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

/// 继承模式的设置内容：全局设置去掉服务商相关键，再叠上 profile 的覆盖项（覆盖项不过滤）。
pub fn inherited_settings(mut global: Map<String, Value>, overrides: &Map<String, Value>) -> Map<String, Value> {
    strip_provider_keys(&mut global);
    merge_settings(&mut global, overrides);
    global
}

/// 第三方卡对应的 env：地址、5 个模型位（空的跟随 main）、窗口、auto 模式的分类器。
/// 直连时写进 settings.json；聚合时从 settings.json 删掉，由启动器注入进程环境（settings 的 env 会压过进程 env）。
const CARD_ENV: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
    "CLAUDE_CODE_AUTO_MODE_SERVER",
    "CLAUDE_CODE_AUTO_MODE_MODEL",
];

/// 模型位的 env（不含地址）。name 决定写进去的模型 ID：直连用原名，聚合用网关发布的 ID。
fn slot_env(s: &Slots, name: impl Fn(ModelRef) -> String, max_context: Option<u64>) -> Vec<(&'static str, String)> {
    let main = name(s.main);
    let mut out = vec![(CARD_ENV[1], main.clone())];
    for (i, t) in s.tiers.iter().enumerate() {
        out.push((CARD_ENV[2 + i], t.map(&name).unwrap_or_else(|| main.clone())));
    }
    // 第三方 claude 不认识，窗口按这个变量算（也能消掉「isn't recognized」警告）
    if let Some(n) = max_context {
        out.push((CARD_ENV[6], n.to_string()));
    }
    // 第三方不转发服务端的安全审查，不设 0 时 auto 模式会拒绝所有要检查的操作（2.1.330 -p 实测）。
    // 设 0 后 claude 自己发分类器请求，走同一个 BASE_URL。
    out.push((CARD_ENV[7], "0".into()));
    // 留空就不写，claude 按文档用 ANTHROPIC_DEFAULT_SONNET_MODEL，也就是 sonnet 模型位
    if let Some(c) = s.classifier {
        out.push((CARD_ENV[8], name(c)));
    }
    out
}

/// 直连：写进 settings.json 的 env，窗口取主模型的。
fn direct_env(card: &Card, s: &Slots) -> Vec<(&'static str, String)> {
    let mut out = vec![(CARD_ENV[0], card.base_url.clone())];
    out.extend(slot_env(s, |(_, m)| m.id.clone(), Some(s.main.1.context)));
    out
}

/// 聚合：注入进程环境的模型位 env（地址和 token 由启动器另外设）。
/// 窗口达到 1M 的模型 ID 带 [1m]，claude 固定按 1M 算；其余的按 CLAUDE_CODE_MAX_CONTEXT_TOKENS，
/// 取主模型的窗口，主模型是 1M 时取其余不带 [1m] 的模型里最小的。
pub fn aggregate_env(cards: &[&Card], s: &Slots) -> Vec<(&'static str, String)> {
    let main = s.main.1.context;
    let max = if main < CONTEXT_1M {
        Some(main)
    } else {
        cards.iter().flat_map(|c| &c.model_list).map(|m| m.context).filter(|&n| n < CONTEXT_1M).min()
    };
    let mut out = slot_env(s, |(c, m)| crate::gateway::model_id(&c.id, m), max);
    // 不设的话 claude 不请求 /v1/models（2.1.330 实测）。/model 的行由 model_picker 给，
    // 但 claude 判断一行能不能选时会参考发现到的模型，所以照样要开
    out.push(("CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY", "1".into()));
    out
}

/// 聚合时 /model 只列所选各卡的模型：replaceBuiltInOptions 去掉内置的 Opus/Sonnet/Fable/Haiku 等行，只留 Default。
/// claude 只认 managed、--settings 和用户级设置里的 modelPicker，取优先级最高的那份，不合并。
pub fn model_picker(cards: &[&Card]) -> Value {
    let options: Vec<Value> = cards
        .iter()
        .flat_map(|c| {
            c.model_list.iter().map(|m| {
                serde_json::json!({
                    "model": crate::gateway::model_id(&c.id, m),
                    "label": m.id,
                    "description": crate::gateway::model_description(c, m),
                })
            })
        })
        .collect();
    serde_json::json!({ "options": options, "replaceBuiltInOptions": true })
}

/// 把 settings 里 obj[key] 设成 "disable"（Switch::On）或删掉（Switch::Off）。
fn set_lock(obj: &mut Map<String, Value>, key: &str, s: Switch) {
    match s {
        Switch::On => obj.insert(key.into(), "disable".into()),
        Switch::Off => obj.remove(key),
    };
}

/// 从 ~/.claude/settings.json 读出的权限与署名当前有效状态（供界面展示「跟随全局（…）」）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalPolicyState {
    pub disable_auto_mode: bool,
    pub disable_bypass_mode: bool,
    pub hide_attribution: bool,
}

/// 读取 ~/.claude/settings.json 中关于 auto / bypass 锁和 Git 署名的实际状态。
/// 文件不存在或解析失败时返回全 false（与 claude 默认行为一致）。
pub fn global_policy_state() -> GlobalPolicyState {
    let root = read_json_object(&home().join(".claude").join("settings.json")).unwrap_or_default();
    let perms = root.get("permissions").and_then(Value::as_object);
    let is_disabled = |v: Option<&Value>| v.and_then(Value::as_str) == Some("disable");

    let disable_auto_mode =
        is_disabled(root.get("disableAutoMode")) || is_disabled(perms.and_then(|m| m.get("disableAutoMode")));
    let disable_bypass_mode = is_disabled(perms.and_then(|m| m.get("disableBypassPermissionsMode")));

    let hide_attribution = match root.get("attribution") {
        Some(Value::Bool(false)) => true,
        Some(Value::Object(m)) => {
            m.get("commit").and_then(Value::as_str) == Some("")
                && m.get("pr").and_then(Value::as_str) == Some("")
        }
        _ => root.get("includeCoAuthoredBy").and_then(Value::as_bool) == Some(false),
    };

    GlobalPolicyState { disable_auto_mode, disable_bypass_mode, hide_attribution }
}

/// 禁用 auto / bypass 模式。没设置（跟随默认）就不动继承来的值。
fn write_mode_locks(root: &mut Map<String, Value>, p: &Profile) {
    if p.disable_auto_mode.is_none() && p.disable_bypass_mode.is_none() {
        return;
    }
    let mut perms = match root.remove("permissions") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    if let Some(s) = p.disable_auto_mode {
        // 顶层和 permissions 下两种写法 claude 都认，统一只留顶层
        perms.remove("disableAutoMode");
        set_lock(root, "disableAutoMode", s);
    }
    if let Some(s) = p.disable_bypass_mode {
        set_lock(&mut perms, "disableBypassPermissionsMode", s);
    }
    if !perms.is_empty() {
        root.insert("permissions".into(), Value::Object(perms));
    }
}

/// commit / PR 署名。没设置（跟随默认）就不动继承来的值。
fn write_attribution(root: &mut Map<String, Value>, p: &Profile) {
    match p.hide_attribution {
        None => {}
        Some(Switch::On) => {
            root.insert("attribution".into(), serde_json::json!({"commit": "", "pr": "", "sessionUrl": false}));
        }
        Some(Switch::Off) => {
            root.remove("attribution");
            root.remove("includeCoAuthoredBy");
        }
    }
}

/// 继承：用 ~/.claude/settings.json（去掉服务商相关键）+ <profile>/clade-settings.json 重新生成；
/// 全新：保留 settings.json 原有内容。两种情况最后都写入 clade 管理的 env 键：
/// 第三方卡的地址、模型位和分类器（只有直连第三方卡时写，其余情况去掉）、兼容开关（用第三方卡时管，官方登录时不动），
/// 以及显式设置过的开关（开写 1，关删掉）。最后处理禁用 auto / bypass 模式的两把锁和署名。
fn write_settings(dir: &Path, global: &Global, p: &Profile, conn: &Conn) -> Result<()> {
    let path = dir.join("settings.json");
    let mut root = if p.inherit_settings {
        inherited_settings(
            read_json_object(&home().join(".claude").join("settings.json"))?,
            &read_json_object(&dir.join("clade-settings.json"))?,
        )
    } else {
        read_json_object(&path)?
    };
    let mut env = match root.remove("env") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    for k in CARD_ENV {
        env.remove(*k);
    }
    if let Conn::Direct(card, slots) = conn {
        for (k, v) in direct_env(card, slots) {
            env.insert(k.into(), Value::String(v));
        }
    }
    if conn.third_party() {
        for d in COMPAT_SWITCHES {
            if p.compat.iter().any(|x| x == d.key) {
                env.insert(d.key.into(), "1".into());
            } else {
                env.remove(d.key);
            }
        }
    }
    for (k, s) in effective_switches(global, p) {
        match s {
            Switch::On => env.insert(k.into(), "1".into()),
            Switch::Off => env.remove(k),
        };
    }
    if !env.is_empty() {
        root.insert("env".into(), Value::Object(env));
    }
    write_mode_locks(&mut root, p);
    write_attribution(&mut root, p);
    fs::write(&path, serde_json::to_string_pretty(&root)? + "\n")?;
    Ok(())
}

/// dir 下：选中的建 junction 指向 src/<name>；指向 src 但未选中的删掉。用户自己放的真实目录不动。
fn sync_junctions(dir: &Path, src: &Path, selected: &[String]) -> Result<()> {
    fs::create_dir_all(dir)?;
    for e in fs::read_dir(dir)? {
        let path = e?.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if junction::exists(&path).unwrap_or(false)
            && junction::get_target(&path).map(|t| t.starts_with(src)).unwrap_or(false)
            && !selected.contains(&name)
        {
            fs::remove_dir(&path)?; // 只删链接本身
        }
    }
    for name in selected {
        let link = dir.join(name);
        if !link.exists() {
            let target = src.join(name);
            if !target.is_dir() {
                bail!("素材库里没有 {}", target.display());
            }
            junction::create(&target, &link)?;
        }
    }
    Ok(())
}

/// 共享：<profile>/<name> junction 到 ~/.claude/<name>。原来的真实目录空的就删掉，有内容就改名成 _<name>。
/// 隔离：删掉 junction，有 _<name> 就改回原名。有内容的目录永远不删。
fn sync_shared(dir: &Path, name: &str, shared: bool) -> Result<()> {
    let link = dir.join(name);
    let backup = dir.join(format!("_{name}"));
    let is_junction = junction::exists(&link).unwrap_or(false);
    if shared {
        if is_junction {
            return Ok(());
        }
        if link.exists() {
            if fs::read_dir(&link)?.next().is_none() {
                fs::remove_dir(&link)?;
            } else if backup.exists() {
                bail!("{} 和 {} 都有内容，请先手动处理其中一个再共享", link.display(), backup.display());
            } else {
                fs::rename(&link, &backup).with_context(|| format!("把 {} 改名为 _{name} 失败", link.display()))?;
            }
        }
        let target = home().join(".claude").join(name);
        fs::create_dir_all(&target)?;
        junction::create(&target, &link)?;
    } else {
        if is_junction {
            fs::remove_dir(&link)?; // 只删链接本身
        }
        if !link.exists() && backup.exists() {
            fs::rename(&backup, &link).with_context(|| format!("把 {} 改回 {name} 失败", backup.display()))?;
        }
    }
    Ok(())
}

/// 克隆 profile：复制登记（换个名字）和 settings.json、clade-settings.json、CLAUDE.md。
/// 不复制历史记录和登录凭据；模型卡的 key 跟着卡走，不用重新填。
pub fn clone_profile(store: &Store, src: &str, name: &str) -> Result<Profile> {
    validate_name(name)?;
    if store.profiles.iter().any(|p| p.name == name) {
        bail!("{name} 已存在");
    }
    let dst = profile_dir(name);
    if dst.exists() {
        bail!("目录 {} 已存在，换个名字", dst.display());
    }
    let mut p = find(store, src)?.clone();
    p.name = name.into();
    fs::create_dir_all(&dst)?;
    for f in ["settings.json", "clade-settings.json", "CLAUDE.md"] {
        let from = profile_dir(src).join(f);
        if from.is_file() {
            fs::copy(&from, dst.join(f))?;
        }
    }
    Ok(p)
}

// ---------- 密钥（Windows 凭据管理器，按模型卡存） ----------

fn entry(card: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new("clade", &format!("card:{card}"))?)
}

pub fn get_key(card: &str) -> Result<Option<String>> {
    match entry(card)?.get_password() {
        Ok(k) => Ok(Some(k)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// 空字符串表示删除。
pub fn set_key(card: &str, key: &str) -> Result<()> {
    let e = entry(card)?;
    if key.is_empty() {
        match e.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    } else {
        Ok(e.set_password(key)?)
    }
}

/// 还有 profile 在用的卡不让删；删卡时一并退出登录并删除凭据与卡目录。
pub fn delete_card(store: &mut Store, id: &str) -> Result<()> {
    let users: Vec<&str> = store.profiles.iter().filter(|p| p.uses(id)).map(|p| p.name.as_str()).collect();
    if !users.is_empty() {
        bail!("这些 profile 还在用 {id}：{}", users.join("、"));
    }
    let dir = card_dir(id);
    let kind = store.cards.iter().find(|c| c.id == id).map(|c| c.kind);
    match kind {
        Some(CardKind::Official) => {
            // 还登录着就先退出，让服务器作废 token；退出失败就不删，免得留下一个再也作废不了的 token
            if dir.exists() && card_auth_status(id)?.logged_in {
                card_logout(id).context("退出登录失败，卡没有删除")?;
            }
        }
        Some(CardKind::OpenAiOAuth) => {
            crate::openai_oauth::logout(id)?;
        }
        Some(CardKind::Antigravity) => {
            crate::antigravity::logout(id, &store.global)?;
        }
        _ => {}
    }
    store.cards.retain(|c| c.id != id);
    set_key(id, "")?;
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("删除 {} 失败", dir.display()))?;
    }
    Ok(())
}

// ---------- 官方卡的登录 ----------

/// 在官方卡目录里运行 claude（配置目录和凭据目录都指向卡目录），用于登录、退出、查状态。
pub fn card_claude(id: &str) -> Result<Command> {
    let dir = card_dir(id);
    fs::create_dir_all(&dir)?;
    let mut cmd = Command::new("claude");
    for k in CONFLICT_ENV {
        cmd.env_remove(k);
    }
    apply_proxy(&mut cmd, &load()?.global);
    cmd.env("CLAUDE_CONFIG_DIR", &dir).env("CLAUDE_SECURESTORAGE_CONFIG_DIR", &dir);
    Ok(cmd)
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub logged_in: bool,
    pub email: String,
    pub org_name: String,
    pub subscription_type: String,
}

/// 不弹控制台窗口地运行。
fn run_hidden(mut cmd: Command) -> Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW).output().context("运行 claude 失败")
}

/// claude auth status（JSON）、OpenAI OAuth 或 Antigravity OAuth 凭据状态。
pub fn card_auth_status(id: &str) -> Result<AuthStatus> {
    let store = load()?;
    if let Some(c) = store.cards.iter().find(|c| c.id == id) {
        match c.kind {
            CardKind::OpenAiOAuth => return crate::openai_oauth::auth_status(id),
            CardKind::Antigravity => return crate::antigravity::auth_status(id),
            CardKind::Api => bail!("第三方 API 卡 {id} 不支持查询登录状态"),
            CardKind::Official => {}
        }
    }
    let mut cmd = card_claude(id)?;
    cmd.args(["auth", "status"]);
    // 未登录时退出码不一定是 0，只看输出
    let text = String::from_utf8_lossy(&run_hidden(cmd)?.stdout).into_owned();
    let v: Value = serde_json::from_str(&text).with_context(|| format!("claude auth status 的输出不是 JSON：{text}"))?;
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    Ok(AuthStatus {
        logged_in: v["loggedIn"].as_bool().unwrap_or(false),
        email: s("email"),
        org_name: s("orgName"),
        subscription_type: s("subscriptionType"),
    })
}

pub fn card_logout(id: &str) -> Result<()> {
    let store = load()?;
    if let Some(c) = store.cards.iter().find(|c| c.id == id) {
        match c.kind {
            CardKind::OpenAiOAuth => return crate::openai_oauth::logout(id),
            CardKind::Antigravity => return crate::antigravity::logout(id, &store.global),
            CardKind::Api => bail!("第三方 API 卡 {id} 不支持退出登录"),
            CardKind::Official => {}
        }
    }
    let mut cmd = card_claude(id)?;
    cmd.args(["auth", "logout"]);
    let out = run_hidden(cmd)?;
    if !out.status.success() {
        bail!("claude auth logout 失败：{}{}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
    }
    Ok(())
}

// ---------- 官方账号的额度 ----------

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub label: String,
    pub percent: f64,
    /// ISO 8601；没有就是空
    pub resets_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub windows: Vec<UsageWindow>,
    /// 这份额度是 claude 什么时候查到的（毫秒时间戳）；没有缓存是 0
    pub fetched_at_ms: i64,
    /// 登录到期时间（ISO 8601，UTC），刷新 token 不会推迟它；没有就是空
    pub credential_expires_at: String,
}

/// 查官方账号的额度。card 为空时查 ~/.claude 的登录（不选卡的 profile 用的那个）。
/// 跑一次 claude -p /usage（本地命令，不花 token），让 claude 自己刷新 token、查额度、写进
/// <配置目录>/.claude.json 的 cachedUsageUtilization（和 /api/oauth/usage 的返回同一个结构），再读这份缓存。
/// clade 不碰 token。做法参照 cc-switch。
pub fn usage(card: &str) -> Result<Usage> {
    if !card.is_empty() {
        let store = load()?;
        if let Some(c) = store.cards.iter().find(|c| c.id == card) {
            match c.kind {
                CardKind::OpenAiOAuth => return crate::openai_oauth::fetch_usage_blocking(card, &store.global),
                CardKind::Antigravity => return crate::antigravity::fetch_usage_blocking(card, &store.global),
                _ => {}
            }
        }
    }
    let (mut cmd, config) = if card.is_empty() {
        let mut cmd = Command::new("claude");
        for k in CONFLICT_ENV {
            cmd.env_remove(k);
        }
        cmd.env_remove("CLAUDE_CONFIG_DIR");
        apply_proxy(&mut cmd, &load()?.global);
        (cmd, home().join(".claude.json"))
    } else {
        (card_claude(card)?, card_dir(card).join(".claude.json"))
    };
    // 不加载任何设置、工具和 MCP，不留会话；关掉 auto memory 才不会在 projects 里给工作目录建记忆目录
    cmd.args(["-p", "/usage", "--output-format", "json", "--tools", "", "--strict-mcp-config"])
        .args(["--setting-sources", "", "--no-session-persistence"])
        // 设了它 /usage 就不联网，只打印别处留下的旧读数
        .env_remove("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .env("DISABLE_TELEMETRY", "1")
        .env("DISABLE_ERROR_REPORTING", "1")
        .env("DISABLE_AUTOUPDATER", "1")
        .current_dir(home());
    let out = run_hidden_timeout(cmd, std::time::Duration::from_secs(45))?;
    let expires = serde_json::from_slice::<Value>(&out)
        .ok()
        .and_then(|v| v["credential_expires_at"].as_str().map(String::from))
        .unwrap_or_default();
    let doc: Value = fs::read(&config).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let cached = &doc["cachedUsageUtilization"];
    // 缓存跟账号走，只认当前登录账号的（claude 自己也是这样）
    let uuid = doc["oauthAccount"]["accountUuid"].as_str();
    let mine = uuid.is_some() && cached["accountUuid"].as_str() == uuid;
    Ok(Usage {
        windows: if mine { parse_usage(&cached["utilization"]) } else { vec![] },
        fetched_at_ms: if mine { cached["fetchedAtMs"].as_f64().unwrap_or(0.0) as i64 } else { 0 },
        credential_expires_at: expires,
    })
}

/// 不弹窗口地运行，超时就杀掉，返回 stdout。
fn run_hidden_timeout(mut cmd: Command, timeout: std::time::Duration) -> Result<Vec<u8>> {
    use std::{io::Read, os::windows::process::CommandExt, process::Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = cmd
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("运行 claude 失败")?;
    // 另起线程读 stdout，免得输出塞满管道把 claude 卡住
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let start = std::time::Instant::now();
    while child.try_wait()?.is_none() {
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("claude 超过 {} 秒没有结束，已结束进程", timeout.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(reader.join().unwrap_or_default())
}

/// 从 /api/oauth/usage 结构里取额度窗口，文案自己拼。
/// 认识的旧式窗口按固定顺序取；limits 里按模型分的周额度（weekly_scoped）覆盖同名的旧式窗口。
pub fn parse_usage(u: &Value) -> Vec<UsageWindow> {
    const KNOWN: [(&str, &str); 5] = [
        ("five_hour", "当前会话（5 小时）"),
        ("seven_day", "本周（所有模型）"),
        ("seven_day_opus", "本周（仅 Opus）"),
        ("seven_day_sonnet", "本周（仅 Sonnet）"),
        ("seven_day_fable", "本周（仅 Fable）"),
    ];
    let resets = |v: &Value| v["resets_at"].as_str().unwrap_or_default().to_string();
    let mut out: Vec<UsageWindow> = KNOWN
        .iter()
        .filter_map(|(k, label)| {
            let w = &u[*k];
            Some(UsageWindow { label: label.to_string(), percent: w["utilization"].as_f64()?, resets_at: resets(w) })
        })
        .collect();
    for l in u["limits"].as_array().into_iter().flatten() {
        // 只要整个模型的周额度，不要特定使用场景的子额度
        if l["kind"] != "weekly_scoped" || l["group"] != "weekly" || !l["scope"]["surface"].is_null() {
            continue;
        }
        let (Some(model), Some(percent)) = (l["scope"]["model"]["display_name"].as_str(), l["percent"].as_f64()) else {
            continue;
        };
        let w = UsageWindow { label: format!("本周（仅 {model}）"), percent, resets_at: resets(l) };
        match out.iter_mut().find(|x| x.label == w.label) {
            Some(x) => *x = w,
            None => out.push(w),
        }
    }
    out
}

// ---------- 拉取模型列表 ----------

/// GET <base>/v1/models?limit=1000（和 claude 自己请求的一样），返回模型 id。
pub fn fetch_models(base_url: &str, auth: Auth, key: &str) -> Result<Vec<String>> {
    let url = format!("{}/v1/models?limit=1000", base_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    let req = client.get(&url).header("anthropic-version", "2023-06-01");
    let req = match auth {
        Auth::Bearer => req.bearer_auth(key),
        Auth::XApiKey => req.header("x-api-key", key),
    };
    let resp = req.send().with_context(|| format!("请求 {url} 失败"))?;
    let status = resp.status();
    let text = resp.text()?;
    if !status.is_success() {
        bail!("{url} 返回 {status}：{}", text.chars().take(300).collect::<String>());
    }
    let v: Value = serde_json::from_str(&text).with_context(|| format!("{url} 返回的不是 JSON"))?;
    let ids: Vec<String> = v["data"]
        .as_array()
        .with_context(|| format!("{url} 返回的 JSON 里没有 data 数组"))?
        .iter()
        .filter_map(|m| m["id"].as_str().map(String::from))
        .collect();
    Ok(ids)
}

// ---------- 冲突的环境变量 ----------

/// 启动时从继承的环境里清除这些变量；GUI 和 clade 也用这份清单检测冲突。
pub const CONFLICT_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_OAUTH_SCOPES",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_GATEWAY",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
    "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    "CLAUDE_CODE_REMOTE_MEMORY_DIR",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
];

/// 当前进程环境里存在的冲突变量（只返回名字）。
pub fn conflict_env() -> Vec<&'static str> {
    CONFLICT_ENV.iter().copied().filter(|k| env::var_os(k).is_some()).collect()
}

// ---------- 代理 ----------

/// claude 认的代理变量。Windows 的环境变量不分大小写，http_proxy 和 HTTP_PROXY 是同一个。
pub const PROXY_ENV: [&str; 2] = ["HTTP_PROXY", "HTTPS_PROXY"];

/// Windows 系统代理（HKCU Internet Settings 的 ProxyServer），不看 ProxyEnable。PAC 不支持。
pub fn system_proxy() -> Option<String> {
    use winreg::{enums::*, RegKey};
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings").ok()?;
    parse_proxy_server(&key.get_value::<String, _>("ProxyServer").ok()?)
}

/// ProxyServer 有两种写法：host:port，或 http=host:port;https=host:port;socks=...（取 https，没有就取 http）。
/// 没写协议的补 http://。
pub fn parse_proxy_server(s: &str) -> Option<String> {
    let s = s.trim();
    let addr = if s.contains('=') {
        let find = |p: &str| s.split(';').find_map(|e| e.trim().strip_prefix(p).map(str::trim));
        find("https=").or_else(|| find("http="))?
    } else {
        s
    };
    if addr.is_empty() {
        return None;
    }
    Some(if addr.contains("://") { addr.to_string() } else { format!("http://{addr}") })
}

/// 两个代理地址是否相同：忽略大小写、http:// 前缀和末尾的 /。
fn same_proxy(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.trim().trim_end_matches('/').to_lowercase();
        s.strip_prefix("http://").map(String::from).unwrap_or(s)
    };
    norm(a) == norm(b)
}

/// 当前进程环境里的代理变量（空值不算）。
fn env_proxies() -> Vec<(&'static str, String)> {
    PROXY_ENV.iter().filter_map(|k| env::var(k).ok().filter(|v| !v.trim().is_empty()).map(|v| (*k, v))).collect()
}

/// 全局设成「走」时，环境里已有、但和系统代理不一致的代理变量：(变量名, 环境里的值, 系统代理)。
/// 用于启动前警告；没有系统代理就没法比较，返回 None。
pub fn proxy_mismatch(global: &Global) -> Option<(&'static str, String, String)> {
    if global.proxy != Some(Switch::On) {
        return None;
    }
    let sys = system_proxy()?;
    env_proxies().into_iter().find(|(_, v)| !same_proxy(v, &sys)).map(|(k, v)| (k, v, sys))
}

/// 按全局代理开关解析当前进程应使用的代理配置（网关与 OAuth HTTP 客户端共用）。
pub fn resolve_proxy(global: &Global) -> crate::gateway::Proxy {
    use crate::gateway::Proxy;
    match global.proxy {
        None => Proxy::Env,
        Some(Switch::Off) => Proxy::Off,
        Some(Switch::On) => {
            let Some(sys) = system_proxy() else { return Proxy::Env };
            let have = env_proxies();
            let pick = |k: &str| {
                have.iter()
                    .find(|(h, _)| *h == k)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| sys.clone())
            };
            Proxy::Set { http: pick("HTTP_PROXY"), https: pick("HTTPS_PROXY") }
        }
    }
}

/// 按全局的代理开关改 claude 子进程的代理变量，返回网关访问上游该用的代理（和 claude 一致）。
/// 「走」时环境里已有的变量保留（不一致由调用方先警告），缺的用系统代理补上；没有系统代理就不动。
fn apply_proxy(cmd: &mut Command, global: &Global) -> crate::gateway::Proxy {
    use crate::gateway::Proxy;
    let proxy = resolve_proxy(global);
    match &proxy {
        Proxy::Env => {}
        Proxy::Off => {
            for k in PROXY_ENV {
                cmd.env_remove(k);
            }
        }
        Proxy::Set { http, https } => {
            let have = env_proxies();
            if !have.iter().any(|(h, _)| *h == "HTTP_PROXY") {
                cmd.env("HTTP_PROXY", http);
            }
            if !have.iter().any(|(h, _)| *h == "HTTPS_PROXY") {
                cmd.env("HTTPS_PROXY", https);
            }
        }
    }
    proxy
}

// ---------- 工作目录检查 ----------

pub fn global_claude_md() -> PathBuf {
    home().join(".claude").join("CLAUDE.md")
}

fn norm_claude_md_path(s: &str) -> String {
    s.replace('\\', "/").to_lowercase()
}

/// 工作目录的严格父目录里会被 claude 当作项目指令加载的 CLAUDE.md。
/// `~/.claude/CLAUDE.md` 由全局/profile 开关单独控制且启动时始终自动排除，这里不再提示。
pub fn nested_claude_md(cwd: &Path) -> Vec<PathBuf> {
    let global_norm = norm_claude_md_path(&global_claude_md().display().to_string());
    let mut out = Vec::new();
    for dir in cwd.ancestors().skip(1) {
        for f in [dir.join("CLAUDE.md"), dir.join(".claude").join("CLAUDE.md")] {
            if f.is_file() && norm_claude_md_path(&f.display().to_string()) != global_norm {
                out.push(f);
            }
        }
    }
    out
}

/// claude 的 scratchpad 目录名：cwd 里非字母数字的字符（按 UTF-16 单元计）换成 '-'，
/// 转小写后 sha256 取前 16 位十六进制。替换后超过 200 个字符时 claude 另有算法，返回 None。
pub fn scratchpad_hash(cwd: &Path) -> Option<String> {
    let mut s = String::new();
    for c in cwd.to_string_lossy().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else {
            s.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    if s.len() > 200 {
        return None;
    }
    let hex: String = Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    Some(hex[..16].to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirCheck {
    pub nested_claude_md: Vec<String>,
    /// 第三方 profile 因路径太长，本次不补 scratchpad
    pub scratchpad_skipped: bool,
}

/// profile 已经排除的不再提醒。
pub fn check_dir(name: &str, cwd: &Path) -> Result<DirCheck> {
    let store = load()?;
    let p = find(&store, name)?;
    let excluded: Vec<String> = p.claude_md_excludes.iter().map(|f| norm_claude_md_path(f)).collect();
    Ok(DirCheck {
        nested_claude_md: nested_claude_md(cwd)
            .iter()
            .map(|f| f.display().to_string())
            .filter(|f| !excluded.contains(&norm_claude_md_path(f)))
            .collect(),
        scratchpad_skipped: resolve(&store, p)?.third_party() && scratchpad_hash(cwd).is_none(),
    })
}

// ---------- 最近用过的工作目录 ----------

const RECENT_MAX: usize = 8;

fn recent_path(name: &str) -> PathBuf {
    profile_dir(name).join("clade-recent.json")
}

/// 最近用过的工作目录，新的在前，已不存在的不返回。
pub fn recent_dirs(name: &str) -> Vec<String> {
    let list: Vec<String> = fs::read_to_string(recent_path(name))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    list.into_iter().filter(|d| Path::new(d).is_dir()).collect()
}

fn touch_recent(name: &str, dir: &Path) -> Result<()> {
    let dir = dir.display().to_string();
    let mut list = recent_dirs(name);
    list.retain(|d| !d.eq_ignore_ascii_case(&dir));
    list.insert(0, dir);
    list.truncate(RECENT_MAX);
    fs::write(recent_path(name), serde_json::to_string_pretty(&list)? + "\n")?;
    Ok(())
}

// ---------- 启动 ----------

/// 返回 scratchpad 用的会话 id。用户自己指定了 --session-id 就用它；
/// 恢复会话（--resume/--continue）时只生成 id、不传给 claude；其余情况生成并用 --session-id 传入。
fn session_for_scratchpad(cmd: &mut Command, extra: &[String]) -> String {
    if let Some(i) = extra.iter().position(|a| a == "--session-id")
        && let Some(id) = extra.get(i + 1)
    {
        return id.clone();
    }
    if let Some(id) = extra.iter().find_map(|a| a.strip_prefix("--session-id=")) {
        return id.to_string();
    }
    let id = uuid::Uuid::new_v4().to_string();
    let resuming = extra
        .iter()
        .any(|a| matches!(a.as_str(), "-r" | "--resume" | "-c" | "--continue") || a.starts_with("--resume="));
    if !resuming {
        cmd.arg("--session-id").arg(&id);
    }
    id
}

/// 权限规则里的绝对路径：Windows 路径先转成 POSIX 形式，`C:\a\b` 写成 `//c/a/b`。
pub fn rule_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    match s.as_bytes() {
        [d, b':', ..] if d.is_ascii_alphabetic() => format!("//{}{}", d.to_ascii_lowercase() as char, &s[2..]),
        _ => s,
    }
}

/// clade 注入的 scratchpad 系统提示词首尾标记（网关剥离 Claude Code 系统提示词时据此保留该段）。
pub const SCRATCHPAD_PROMPT_PREFIX: &str = "Scratchpad directory: ";
pub const SCRATCHPAD_PROMPT_SUFFIX: &str = "Only use /tmp if the user explicitly asks.";

/// 第三方 profile 拿不到原生的 scratchpad，按同样的路径自己建好，再通过提示词告诉模型。
/// 写入许可用权限规则给，不用 --add-dir：后者每次启动都会弹信任确认（路径里的会话 UUID 每次都不同）。
/// 路径太长时返回 false，本次不补。
fn add_scratchpad(cmd: &mut Command, cwd: &Path, extra: &[String]) -> Result<bool> {
    let Some(hash) = scratchpad_hash(cwd) else {
        return Ok(false);
    };
    let session = session_for_scratchpad(cmd, extra);
    let dir = env::temp_dir().join("claude").join(hash).join(session).join("scratchpad");
    fs::create_dir_all(&dir).with_context(|| format!("创建 {} 失败", dir.display()))?;
    let prompt = format!(
        "{SCRATCHPAD_PROMPT_PREFIX}{}\n\
         Always use this directory for temporary files (intermediate results, scripts, outputs that don't belong in the project) \
         instead of /tmp or other system temp directories. It is session-specific and isolated from the project. \
         {SCRATCHPAD_PROMPT_SUFFIX}",
        dir.display()
    );
    // Edit 规则同时管 Write；Read 规则同时管 Glob/Grep
    let pattern = format!("{}/**", rule_path(&dir));
    cmd.arg("--append-system-prompt")
        .arg(prompt)
        .arg(format!("--allowedTools=Read({pattern}),Edit({pattern})"));
    Ok(true)
}

/// claude 的 --mcp-config、--allowedTools 等是可变参数，分开写会把后面的提示词等位置参数一起吞掉；
/// 写成 --flag=值 只取这一个值。
fn variadic_arg(flag: &str, path: &Path) -> std::ffi::OsString {
    let mut s = std::ffi::OsString::from(format!("{flag}="));
    s.push(path);
    s
}


fn key_var(auth: Auth) -> &'static str {
    match auth {
        Auth::Bearer => "ANTHROPIC_AUTH_TOKEN",
        Auth::XApiKey => "ANTHROPIC_API_KEY",
    }
}

/// 同步目录后构造 claude 命令：独立配置目录 + 清理冲突变量 + 注入 key + 插件 + MCP + scratchpad + CLAUDE.md 排除。
/// 聚合模式会在本进程里起网关（随机端口，进程退出就停），所以只能由启动 claude 的那个进程调用。
pub fn command(name: &str, cwd: &Path, extra: &[String]) -> Result<Command> {
    let store = load()?;
    let p = find(&store, name)?;
    let conn = resolve(&store, p)?;
    apply(&store, p)?;
    let dir = profile_dir(name);
    touch_recent(name, cwd)?;

    let mut cmd = Command::new("claude");
    cmd.current_dir(cwd);
    for k in CONFLICT_ENV {
        cmd.env_remove(k);
    }
    // 显式关掉的开关，继承来的进程环境里也不能留
    for (k, s) in effective_switches(&store.global, p) {
        if s == Switch::Off {
            cmd.env_remove(k);
        }
    }
    let proxy = apply_proxy(&mut cmd, &store.global);
    cmd.env("CLAUDE_CONFIG_DIR", &dir);
    // 共享 projects 时，<profile>/projects 是指向 ~/.claude/projects 的 junction；
    // 让自动记忆直接走真实目录 ~/.claude，避免写 memory 时因经过 junction 触发 symlink 确认弹窗。
    // CLAUDE_CODE_REMOTE_MEMORY_DIR 是未公开的变量，2.1.330 实测有效。
    if p.shared.projects.unwrap_or(store.global.shared.projects) {
        cmd.env("CLAUDE_CODE_REMOTE_MEMORY_DIR", home().join(".claude"));
    }
    // 官方登录：选了官方卡就用卡目录的凭据，没选卡就用 ~/.claude 的（直接运行 claude 时的那个账号）。
    // CLAUDE_SECURESTORAGE_CONFIG_DIR 是未公开的变量，2.1.330 实测有效。
    match &conn {
        Conn::Global => {
            cmd.env("CLAUDE_SECURESTORAGE_CONFIG_DIR", home().join(".claude"));
        }
        Conn::Official(c) => {
            cmd.env("CLAUDE_SECURESTORAGE_CONFIG_DIR", card_dir(&c.id));
        }
        Conn::Direct(c, _) => {
            if let Some(key) = get_key(&c.id)? {
                cmd.env(key_var(c.auth), key);
            }
        }
        Conn::Aggregate(cards, slots) => {
            // claude 只拿到一个随机 token，网关核对后换成各卡的真 key / OAuth token 发给上游
            let token = uuid::Uuid::new_v4().simple().to_string();
            let mut routes = Vec::new();
            for c in cards {
                let auth = match c.kind {
                    CardKind::OpenAiOAuth => {
                        let creds = crate::openai_oauth::load_creds(&c.id)?
                            .with_context(|| format!("OpenAI 登录卡 {} 还没有登录", c.id))?;
                        crate::gateway::RouteAuth::OpenAiOAuth(std::sync::Arc::new(tokio::sync::Mutex::new(creds)))
                    }
                    CardKind::Antigravity => {
                        let creds = crate::antigravity::load_creds(&c.id)?
                            .with_context(|| format!("Antigravity 登录卡 {} 还没有登录", c.id))?;
                        crate::gateway::RouteAuth::Antigravity(std::sync::Arc::new(tokio::sync::Mutex::new(creds)))
                    }
                    _ => {
                        let key = get_key(&c.id)?.with_context(|| format!("模型卡 {} 还没有 API key", c.id))?;
                        crate::gateway::RouteAuth::ApiKey(key)
                    }
                };
                let mut card_resolved = (*c).clone();
                card_resolved.system_prompt_policy = Some(effective_system_prompt_policy(&store.global, p, c));
                card_resolved.preserve_memory_prompt = Some(effective_preserve_memory_prompt(&store.global, p, c));
                card_resolved.system_reminder_policy = Some(effective_system_reminder_policy(&store.global, p, c));
                routes.push(crate::gateway::Route { card: card_resolved, auth, protocol: c.protocol });
            }
            let port = crate::gateway::start(crate::gateway::Config {
                routes,
                token: token.clone(),
                log: Some(dir.join("clade-gateway.log")),
                proxy,
                classifier_prompt: effective_classifier_prompt(&store.global, p),
            })?;
            cmd.env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{port}"));
            cmd.env("ANTHROPIC_AUTH_TOKEN", token);
            // 环境里设了 http_proxy 时 claude 连本机网关也走代理，代理没开就一直卡住（2.1.330 实测）。
            // Windows 的环境变量不分大小写，NO_PROXY 和 no_proxy 是同一个。
            let no_proxy = match env::var("NO_PROXY") {
                Ok(v) if !v.is_empty() => format!("{v},127.0.0.1"),
                _ => "127.0.0.1".into(),
            };
            cmd.env("NO_PROXY", no_proxy);
            for (k, v) in aggregate_env(cards, slots) {
                cmd.env(k, v);
            }
        }
    }
    let assets = effective_assets(&store.global, p);
    for plugin in &assets.plugins {
        cmd.arg("--plugin-dir").arg(lib_dir("plugins").join(plugin));
    }
    if !assets.mcp.is_empty() {
        cmd.arg(variadic_arg("--mcp-config", &dir.join("clade-mcp.json")));
    }
    if conn.third_party() {
        add_scratchpad(&mut cmd, cwd, extra)?;
    }
    let mut cli_settings = Map::new();
    // 和 settings.json 里的 claudeMdExcludes 合并（2.1.330 实测），不会互相覆盖；反斜杠路径不生效。
    // 原路径 ~/.claude/CLAUDE.md 始终排除：开启「加载全局 CLAUDE.md」时已复制到 profile 目录（避免重复加载），关闭时避免泄漏。
    let mut exclude_paths: Vec<String> = p.claude_md_excludes.iter().map(|f| f.replace('\\', "/")).collect();
    let global_md = global_claude_md().display().to_string().replace('\\', "/");
    if !exclude_paths.iter().any(|f| f.eq_ignore_ascii_case(&global_md)) {
        exclude_paths.push(global_md);
    }
    cli_settings.insert("claudeMdExcludes".into(), serde_json::json!(exclude_paths));
    if let Conn::Aggregate(cards, _) = &conn {
        cli_settings.insert("modelPicker".into(), model_picker(cards));
    }
    if !cli_settings.is_empty() {
        cmd.arg(format!("--settings={}", Value::Object(cli_settings)));
    }
    cmd.args(extra);
    Ok(cmd)
}

// ---------- pwsh7 ----------

fn find_in_path(exe: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?).map(|d| d.join(exe)).find(|f| f.is_file())
}

/// 找 pwsh7。刚用 winget 装完时本进程的 PATH 还是旧的，所以再查一下默认安装位置。
pub fn find_pwsh() -> Option<PathBuf> {
    find_in_path("pwsh.exe").or_else(|| {
        let f = PathBuf::from(env::var_os("ProgramFiles")?).join("PowerShell").join("7").join("pwsh.exe");
        f.is_file().then_some(f)
    })
}

pub fn has_winget() -> bool {
    find_in_path("winget.exe").is_some()
}

/// 在新控制台窗口里运行 cmdline（第一个词按 PATH 查找），wait 为 true 时等它退出。
///
/// 不用 std 的 Command：它总会设置 STARTF_USESTDHANDLES，而 GUI 进程没有标准句柄，
/// 子进程拿到的是空句柄。pwsh 自己靠控制台 API 还能显示，但它再启动的 clade/claude
/// 没有可用的输入输出，会直接退出。这里不传句柄，让新控制台自己分配。
pub fn spawn_console(cmdline: &str, cwd: Option<&Path>, wait: bool) -> Result<()> {
    use std::{mem::zeroed, os::windows::ffi::OsStrExt, ptr::null};
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{CreateProcessW, WaitForSingleObject, CREATE_NEW_CONSOLE, INFINITE, PROCESS_INFORMATION, STARTUPINFOW},
    };
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();
    let mut cmd = wide(cmdline.as_ref());
    let cwd = cwd.map(|d| wide(d.as_os_str()));
    unsafe {
        let mut si: STARTUPINFOW = zeroed();
        si.cb = size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = zeroed();
        let ok = CreateProcessW(
            null(),
            cmd.as_mut_ptr(),
            null(),
            null(),
            0,
            CREATE_NEW_CONSOLE,
            null(),
            cwd.as_ref().map_or(null(), |d| d.as_ptr()),
            &si,
            &mut pi,
        );
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| format!("运行 {cmdline} 失败"));
        }
        if wait {
            WaitForSingleObject(pi.hProcess, INFINITE);
        }
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }
    Ok(())
}

// ---------- 用户级 PATH ----------

/// PATH 值（; 分隔）里是否已有 dir。忽略大小写和末尾的斜杠。
pub fn path_contains(path: &str, dir: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches(['\\', '/']).to_lowercase();
    path.split(';').any(|e| norm(e) == norm(dir))
}

/// 把 dir 加进 HKCU\Environment 的 Path，不需要管理员。已经有了返回 false。
pub fn add_to_user_path(dir: &Path) -> Result<bool> {
    use winreg::{enums::*, RegKey, RegValue};
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)?;
    let (old, vtype) = match key.get_raw_value("Path") {
        Ok(v) => {
            let units: Vec<u16> = v.bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            (String::from_utf16_lossy(&units).trim_end_matches('\0').to_string(), v.vtype)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), REG_EXPAND_SZ),
        Err(e) => return Err(e.into()),
    };
    let dir = dir.display().to_string();
    if path_contains(&old, &dir) {
        return Ok(false);
    }
    let new = if old.is_empty() || old.ends_with(';') { format!("{old}{dir}") } else { format!("{old};{dir}") };
    let bytes = new.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
    key.set_raw_value("Path", &RegValue { bytes, vtype })?;
    broadcast_env_change();
    Ok(true)
}

/// 通知资源管理器等进程「环境变量变了」，之后新开的终端才能拿到新 PATH。
fn broadcast_env_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };
    let name: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            name.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            std::ptr::null_mut(),
        );
    }
}

