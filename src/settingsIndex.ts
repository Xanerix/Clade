import { ASSET_LABEL, SHARED_INFO, SHARED_KEYS, type AssetKind, type State } from "./types";

export type SettingsSec = "general" | "assets" | "shared" | "prompts" | "proxy" | "switches" | "tools";

export type SettingsTarget = {
  sec: SettingsSec;
  itemId?: string;
  seq: number;
};

export type SettingSearchItem = {
  id: string;
  sec: SettingsSec;
  secLabel: string;
  title: string;
  sub?: string;
  keywords: string;
};

export const SETTINGS_GROUPS: { group: string; items: { id: SettingsSec; label: string; keywords: string }[] }[] = [
  {
    group: "工作区",
    items: [
      { id: "general", label: "常规", keywords: "claude.md 全局指令" },
      { id: "assets", label: "默认素材", keywords: "skills plugins mcp 插件 素材" },
      { id: "shared", label: "共享目录", keywords: "junction projects sessions file-history plugins mods" },
      { id: "prompts", label: "网关提示词", keywords: "system memory reminder 分类器 classifier prompt" },
    ],
  },
  {
    group: "系统",
    items: [
      { id: "proxy", label: "系统代理", keywords: "代理 proxy http_proxy https_proxy 网络" },
      { id: "switches", label: "全局开关", keywords: "disable telemetry autoupdater 隐私 更新 界面 环境变量" },
      { id: "tools", label: "环境工具", keywords: "导入 path 目录 刷新 ~/.clade" },
    ],
  },
];

export const SETTINGS_SEC_LABEL: Record<SettingsSec, string> = Object.fromEntries(
  SETTINGS_GROUPS.flatMap((g) => g.items.map((it) => [it.id, it.label])),
) as Record<SettingsSec, string>;

const ASSET_KINDS: AssetKind[] = ["skills", "plugins", "mcp"];

export function buildSettingSearchItems(state: State): SettingSearchItem[] {
  const items: SettingSearchItem[] = [
    {
      id: "general:inherit-claude-md",
      sec: "general",
      secLabel: SETTINGS_SEC_LABEL.general,
      title: "默认加载 ~/.claude/CLAUDE.md",
      keywords: "指令 全局指令 claude.md 复制 排除",
    },
    ...ASSET_KINDS.map((k) => {
      const names = [...new Set([...state.library[k], ...state.global.assets[k]])];
      return {
        id: `assets:${k}`,
        sec: "assets" as const,
        secLabel: SETTINGS_SEC_LABEL.assets,
        title: `默认素材 · ${ASSET_LABEL[k]}`,
        keywords: `${k} ${ASSET_LABEL[k]} 素材 ${names.join(" ")}`,
      };
    }),
    ...SHARED_KEYS.map((k) => {
      const info = SHARED_INFO[k];
      return {
        id: `shared:${k}`,
        sec: "shared" as const,
        secLabel: SETTINGS_SEC_LABEL.shared,
        title: info.dir,
        sub: info.desc,
        keywords: `junction ~/.claude 共享目录 ${info.dir} ${info.desc}`,
      };
    }),
    {
      id: "prompts:system",
      sec: "prompts",
      secLabel: SETTINGS_SEC_LABEL.prompts,
      title: "顶层 system",
      keywords: "聚合模式 网关提示词 system prompt 剥离 自定义 scratchpad antigravity",
    },
    {
      id: "prompts:memory",
      sec: "prompts",
      secLabel: SETTINGS_SEC_LABEL.prompts,
      title: "保留 # Memory",
      keywords: "聚合模式 网关提示词 memory 持久记忆 段落",
    },
    {
      id: "prompts:reminder",
      sec: "prompts",
      secLabel: SETTINGS_SEC_LABEL.prompts,
      title: "system-reminder",
      keywords: "聚合模式 网关提示词 reminder messages role system 隐藏块",
    },
    {
      id: "prompts:classifier",
      sec: "prompts",
      secLabel: SETTINGS_SEC_LABEL.prompts,
      title: "auto 分类器默认提示词",
      keywords: "auto 分类器 classifier prompt 安全审查 模板",
    },
    {
      id: "proxy:policy",
      sec: "proxy",
      secLabel: SETTINGS_SEC_LABEL.proxy,
      title: "走系统代理",
      sub: "HTTP_PROXY / HTTPS_PROXY",
      keywords: "代理策略 proxy http_proxy https_proxy proxyserver 网络 网关 oauth",
    },
    {
      id: "proxy:current",
      sec: "proxy",
      secLabel: SETTINGS_SEC_LABEL.proxy,
      title: "当前系统代理",
      keywords: "proxyserver 注册表 internet settings pac 系统代理",
    },
    ...state.switches.global.map((d) => ({
      id: `switch:${d.key}`,
      sec: "switches" as const,
      secLabel: SETTINGS_SEC_LABEL.switches,
      title: d.label,
      sub: d.key,
      keywords: `全局开关 ${d.group} ${d.key} ${d.note}`,
    })),
    {
      id: "tools:import",
      sec: "tools",
      secLabel: SETTINGS_SEC_LABEL.tools,
      title: "导入 ~/.claude 素材",
      keywords: "导入 skills plugins mcp 素材 环境工具",
    },
    {
      id: "tools:path",
      sec: "tools",
      secLabel: SETTINGS_SEC_LABEL.tools,
      title: "用户 PATH",
      keywords: "path clade.exe 环境变量 hkcu environment 环境工具",
    },
    {
      id: "tools:root",
      sec: "tools",
      secLabel: SETTINGS_SEC_LABEL.tools,
      title: "数据目录",
      sub: state.root,
      keywords: "数据目录 ~/.clade 打开 目录 环境工具",
    },
    {
      id: "tools:refresh",
      sec: "tools",
      secLabel: SETTINGS_SEC_LABEL.tools,
      title: "刷新素材库与状态",
      keywords: "刷新 clade.json _library 素材库 状态 环境工具",
    },
  ];
  return items;
}

export function matchSettingSearchItem(item: SettingSearchItem, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return false;
  const haystack = `${item.title} ${item.sub ?? ""} ${item.secLabel} ${item.keywords}`.toLowerCase();
  return q.split(/\s+/).every((part) => haystack.includes(part));
}
