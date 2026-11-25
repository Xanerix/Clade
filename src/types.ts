// 与 src-tauri/src/core.rs 的数据结构一一对应

export type Switch = "on" | "off";
export type Switches = Record<string, Switch>;

export type AssetKind = "skills" | "plugins" | "mcp";
export type Assets = Record<AssetKind, string[]>;
export type ListOverride = { add: string[]; exclude: string[] };

export type SharedKey = "projects" | "sessions" | "fileHistory" | "plugins" | "mods";
export type Shared = Record<SharedKey, boolean>;
export type SharedOverrides = Record<SharedKey, boolean | null>;

export type PromptPolicy = { mode: "keep" } | { mode: "strip" } | { mode: "custom"; text: string };

export const promptPolicyLabel = (p: PromptPolicy | null | undefined, fallback: string) => {
  if (!p) return fallback;
  if (p.mode === "keep") return "保留";
  if (p.mode === "strip") return "剥离";
  return "自定义";
};

export type Global = {
  assets: Assets;
  shared: Shared;
  switches: Switches;
  proxy: Switch | null;
  classifierPrompt: string;
  systemPromptPolicy?: PromptPolicy | null;
  preserveMemoryPrompt: boolean;
  systemReminderPolicy?: PromptPolicy | null;
  inheritClaudeMd: boolean;
};

export type Auth = "bearer" | "x-api-key";
export type ApiFormat = "anthropic" | "openai_chat" | "openai_responses" | "gemini_native";
export type CardKind = "api" | "official" | "openai_oauth" | "antigravity";
export type CardModel = { id: string; context: number };
export type Card = {
  id: string;
  name: string;
  kind: CardKind;
  protocol: ApiFormat;
  baseUrl: string;
  auth: Auth;
  modelList: CardModel[];
  stripSystemPrompt?: boolean | null;
  systemPromptPolicy?: PromptPolicy | null;
  preserveMemoryPrompt?: boolean | null;
  systemReminderPolicy?: PromptPolicy | null;
};

export type Mode = "direct" | "aggregate";
export type SlotKey = "main" | "sonnet" | "opus" | "fable" | "haiku";
/** 哪张卡的哪个模型；id 为空表示没选。直连模式下 card 不看 */
export type Pick = { card: string; id: string };

export type Profile = {
  name: string;
  mode: Mode;
  card: string;
  cards: string[];
  models: Record<SlotKey, Pick>;
  classifierModel: Pick;
  classifierPrompt: string | null;
  systemPromptPolicy?: PromptPolicy | null;
  preserveMemoryPrompt?: boolean | null;
  systemReminderPolicy?: PromptPolicy | null;
  compat: string[];
  assets: Record<AssetKind, ListOverride>;
  shared: SharedOverrides;
  switches: Switches;
  inheritSettings: boolean;
  writebackUiPrefs: boolean;
  inheritClaudeMd: boolean | null;
  claudeMdExcludes: string[];
  disableAutoMode: Switch | null;
  disableBypassMode: Switch | null;
  hideAttribution: Switch | null;
};

export type SwitchDef = { key: string; group: string; label: string; note: string };

export type GlobalPolicyState = {
  disableAutoMode: boolean;
  disableBypassMode: boolean;
  hideAttribution: boolean;
};

export type State = {
  root: string;
  global: Global;
  cards: Card[];
  profiles: Profile[];
  library: Assets;
  hasKey: Record<string, boolean>;
  conflictEnv: string[];
  switches: { global: SwitchDef[]; profile: SwitchDef[]; compat: SwitchDef[] };
  systemProxy: string | null;
  defaultClassifierPrompt: string;
  globalPolicy: GlobalPolicyState;
};

export type AuthStatus = { loggedIn: boolean; email: string; orgName: string; subscriptionType: string };
export type DeviceFlowStart = {
  deviceAuthId: string;
  userCode: string;
  verificationUri: string;
  expiresIn: number;
  interval: number;
};

export type DirCheck = { nestedClaudeMd: string[]; scratchpadSkipped: boolean };

export const DEFAULT_CONTEXT = 200000;
export const CONTEXT_1M = 1000000;

export const SLOT_KEYS: SlotKey[] = ["main", "sonnet", "opus", "fable", "haiku"];

export const ASSET_LABEL: Record<AssetKind, string> = { skills: "Skills", plugins: "插件", mcp: "MCP" };

export const SHARED_KEYS: SharedKey[] = ["projects", "sessions", "fileHistory", "plugins", "mods"];

export const SHARED_LABEL: Record<SharedKey, string> = {
  projects: "projects（会话记录、项目记忆）",
  sessions: "sessions（共享会话池，实验性）",
  fileHistory: "file-history（文件检查点）",
  plugins: "plugins（已安装插件缓存与状态栏）",
  mods: "mods（本地扩展模块）",
};

export const SHARED_INFO: Record<SharedKey, { dir: string; desc: string }> = {
  projects: { dir: "projects", desc: "会话记录、项目记忆" },
  sessions: { dir: "sessions", desc: "共享会话池（实验性）" },
  fileHistory: { dir: "file-history", desc: "文件检查点" },
  plugins: { dir: "plugins", desc: "已安装插件缓存与状态栏" },
  mods: { dir: "mods", desc: "本地扩展模块" },
};

export const API_FORMAT_LABEL: Record<ApiFormat, string> = {
  anthropic: "Anthropic",
  openai_chat: "OpenAI Chat",
  openai_responses: "OpenAI Responses",
  gemini_native: "Gemini Native",
};

export const isLoginCardKind = (k: CardKind) => k === "official" || k === "openai_oauth" || k === "antigravity";

export const blankCard = (): Card => ({
  id: "",
  name: "",
  kind: "api",
  protocol: "anthropic",
  baseUrl: "",
  auth: "bearer",
  modelList: [],
});

export const blankPick = (): Pick => ({ card: "", id: "" });

/** 当前模式下是否用到这张卡（和 core.rs 的 Profile::uses 一致） */
export const usesCard = (p: Profile, id: string) => (p.mode === "direct" ? p.card === id : p.cards.includes(id));

const blankOverride = (): ListOverride => ({ add: [], exclude: [] });

export const blankProfile = (): Profile => ({
  name: "",
  mode: "direct",
  card: "",
  cards: [],
  models: Object.fromEntries(SLOT_KEYS.map((k) => [k, blankPick()])) as Record<SlotKey, Pick>,
  classifierModel: blankPick(),
  classifierPrompt: null,
  systemPromptPolicy: null,
  preserveMemoryPrompt: null,
  systemReminderPolicy: null,
  compat: [],
  assets: { skills: blankOverride(), plugins: blankOverride(), mcp: blankOverride() },
  shared: { projects: null, sessions: null, fileHistory: null, plugins: null, mods: null },
  switches: {},
  inheritSettings: true,
  writebackUiPrefs: false,
  inheritClaudeMd: null,
  claudeMdExcludes: [],
  disableAutoMode: null,
  disableBypassMode: null,
  hideAttribution: null,
});

export type UsageWindow = { label: string; percent: number; resetsAt: string };
export type Usage = { windows: UsageWindow[]; fetchedAtMs: number; credentialExpiresAt: string };
