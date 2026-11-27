import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ask } from "@tauri-apps/plugin-dialog";
import { AnimatePresence, motion } from "motion/react";
import "./App.css";
import {
  createDirectionalVariants,
  GREETING_VARIANTS,
  MODAL_VARIANTS,
  OVERLAY_VARIANTS,
  STAGE_CONTAINER_VARIANTS,
  TOAST_VARIANTS,
  TRANSITION_FAST,
  TRANSITION_INDICATOR,
  type SlideMotion,
} from "./motion";
import {
  API_FORMAT_LABEL,
  isLoginCardKind,
  usesCard,
  type AuthStatus,
  type Card,
  type State,
  type Usage,
} from "./types";
import {
  AlertTriangleIcon,
  ArrowUpRightIcon,
  CloseIcon,
  CornerDownLeftIcon,
  FolderIcon,
  MoonIcon,
  PlusIcon,
  SearchIcon,
  SunIcon,
} from "./icons";
import { ModalDialog, UsageBars } from "./ui";
import { buildSettingSearchItems, matchSettingSearchItem, type SettingsTarget } from "./settingsIndex";
import ProfileForm from "./ProfileForm";
import CardForm from "./CardForm";
import GlobalForm from "./GlobalForm";
import Launcher from "./Launcher";
import PageBackdrop from "./PageBackdrop";

type TopView = "launch" | "profiles" | "cards" | "global";
const TOP_VIEW_INDEX: Record<TopView, number> = { launch: 0, profiles: 1, cards: 2, global: 3 };
type Theme = "light" | "dark";
type ToastItem = { id: number; text: string; bad?: boolean };

const GREETINGS = [
  // 克制日常
  "今天想先打开哪个工作区？",
  "思路就绪，挑个目录开工吧。",
  "新的一段对话，从这里开始。",
  "回到正题，选一个工作区。",
  "一切就位，随时可以出发。",
  // 工程极客
  "今天打算折腾哪个项目？",
  "少写一行是一行，先挑个仓库。",
  "编译通过之前，咖啡别凉。",
  "又有哪个 Bug 等着被清算？",
  "分支切好了吗，准备开工。",
  "重构一时爽，测试别忘了跑。",
  "先跑一遍测试，再谈理想。",
  "终端已经热好了，选个目录吧。",
  "今天是写新功能，还是还技术债？",
  // 诗意 / 文学感
  "光标闪烁之处，皆为起点。",
  "万行代码，始于一次回车。",
  "在逻辑的缝隙里，种下一点秩序。",
  "保持好奇，也保持克制。",
  "把复杂留给机器，把判断留给自己。",
  "安静下来，把一件事做透。",
  // 轻幽默 / 松弛感
  "先别急着重写整个项目。",
  "今天的 diff，尽量短一点。",
  "别怕，git 记得你做过的一切。",
  "希望今天遇到的都是清晰报错。",
  "喝口水，挑一个顺眼的目录。",
  "好消息：昨天留的坑还在等你。",
  // 干脆利落
  "选个环境，直接开干。",
  "少说废话，打开终端。",
  "从最难的那一块下手。",
  "下一步，交给键盘。",
];

const THEME_KEY = "clade-theme";

function readInitialTheme(): Theme {
  const saved = window.localStorage.getItem(THEME_KEY);
  if (saved === "light" || saved === "dark") return saved;
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function baseName(p: string): string {
  const clean = p.replace(/[\\/]+$/, "");
  const parts = clean.split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

export default function App() {
  const [state, setState] = useState<State | null>(null);
  const [bootError, setBootError] = useState<string | null>(null);
  const [view, setView] = useState<TopView>("launch");
  const [profileId, setProfileId] = useState<string | null>(null);
  const [modalCard, setModalCard] = useState<string | "__new__" | null>(null);
  const [warnOpen, setWarnOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [paletteQuery, setPaletteQuery] = useState("");
  const [paletteIdx, setPaletteIdx] = useState(0);
  const [launching, setLaunching] = useState<{ name: string; dir?: string } | null>(null);
  const [installingClaude, setInstallingClaude] = useState<"script" | "winget" | null>(null);
  const [settingsTarget, setSettingsTarget] = useState<SettingsTarget | null>(null);
  const settingsTargetSeq = useRef(1);
  const [rev, setRev] = useState(0);
  const [viewMountedRev, setViewMountedRev] = useState(0);
  const viewMotionRef = useRef<SlideMotion>({ axis: "y", dir: 1 });
  const getViewMotion = useCallback(() => viewMotionRef.current, []);
  const viewMotion = useMemo(() => createDirectionalVariants(getViewMotion), [getViewMotion]);
  const [eggOpen, setEggOpen] = useState(false);
  const wordmarkClicksRef = useRef<{ count: number; lastAt: number }>({ count: 0, lastAt: 0 });

  const [greetingIdx, setGreetingIdx] = useState(() => Math.floor(Math.random() * GREETINGS.length));
  const [theme, setTheme] = useState<Theme>(readInitialTheme);
  const [maximized, setMaximized] = useState(false);
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const toastSeq = useRef(1);

  const [recentMap, setRecentMap] = useState<Record<string, string[]>>({});
  const [authMap, setAuthMap] = useState<Record<string, AuthStatus>>({});
  const [usageMap, setUsageMap] = useState<Record<string, Usage>>({});
  const fetchedCardsRef = useRef<Set<string>>(new Set());
  const palInputRef = useRef<HTMLInputElement>(null);
  const palListRef = useRef<HTMLDivElement>(null);
  const dirtyRef = useRef(false);
  const setFormDirty = useCallback((d: boolean) => {
    dirtyRef.current = d;
  }, []);

  const toast = useCallback((text: string, bad?: boolean) => {
    const id = toastSeq.current++;
    setToasts((xs) => [...xs, { id, text, bad }]);
    window.setTimeout(() => {
      setToasts((xs) => xs.filter((x) => x.id !== id));
    }, bad ? 3600 : 2400);
  }, []);

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
  }, [theme]);

  useEffect(() => {
    const win = getCurrentWindow();
    let cancelled = false;
    const syncMaximized = () => {
      win.isMaximized().then((v) => {
        if (!cancelled) setMaximized(v);
      }, () => {});
    };
    syncMaximized();
    const unlistenPromise = win.onResized(syncMaximized);
    return () => {
      cancelled = true;
      void unlistenPromise.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    const mq = window.matchMedia?.("(prefers-color-scheme: dark)");
    if (!mq) return;
    const onChange = (e: MediaQueryListEvent) => {
      if (!window.localStorage.getItem(THEME_KEY)) {
        setTheme(e.matches ? "dark" : "light");
      }
    };
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  const themeTimerRef = useRef<number | null>(null);
  const toggleTheme = () => {
    const next: Theme = theme === "light" ? "dark" : "light";
    document.documentElement.classList.add("theme-switching");
    if (themeTimerRef.current !== null) window.clearTimeout(themeTimerRef.current);
    themeTimerRef.current = window.setTimeout(() => {
      document.documentElement.classList.remove("theme-switching");
      themeTimerRef.current = null;
    }, 260);
    setTheme(next);
    window.localStorage.setItem(THEME_KEY, next);
    toast(next === "light" ? "暖奶油亮色" : "暖炭暗色");
  };

  useEffect(() => {
    return () => {
      if (themeTimerRef.current !== null) window.clearTimeout(themeTimerRef.current);
    };
  }, []);

  const loadRecentForProfiles = useCallback(async (s: State) => {
    const entries = await Promise.all(
      s.profiles.map(async (p) => {
        try {
          const dirs = await invoke<string[]>("recent_dirs", { name: p.name });
          return [p.name, dirs] as const;
        } catch {
          return [p.name, []] as const;
        }
      }),
    );
    setRecentMap(Object.fromEntries(entries));
  }, []);

  const refresh = useCallback(async () => {
    const s = await invoke<State>("get_state");
    setState(s);
    void loadRecentForProfiles(s);
    return s;
  }, [loadRecentForProfiles]);

  useEffect(() => {
    refresh().then(
      (s) => {
        if (s.profiles[0]) setProfileId(s.profiles[0].name);
      },
      (e) => setBootError(String(e)),
    );
  }, [refresh]);

  useEffect(() => {
    if (view !== "cards" || !state) return;
    let cancelled = false;
    const loginCards = state.cards.filter((c) => isLoginCardKind(c.kind));
    for (const c of loginCards) {
      if (fetchedCardsRef.current.has(c.id)) continue;
      fetchedCardsRef.current.add(c.id);
      void (async () => {
        try {
          const st = await invoke<AuthStatus>("card_status", { id: c.id });
          if (cancelled) return;
          setAuthMap((m) => ({ ...m, [c.id]: st }));
          if (st.loggedIn) {
            try {
              const u = await invoke<Usage>("usage", { card: c.id });
              if (!cancelled) setUsageMap((m) => ({ ...m, [c.id]: u }));
            } catch {
              // 额度拉取失败静默保留，点开弹窗可看报错
            }
          }
        } catch {
          // 登录状态查询失败时不阻断画廊渲染
        }
      })();
    }
    return () => {
      cancelled = true;
    };
  }, [view, state]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteQuery("");
        setPaletteIdx(0);
        setPaletteOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    if (!eggOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopImmediatePropagation();
        setEggOpen(false);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [eggOpen]);

  const handleWordmarkClick = () => {
    const now = Date.now();
    const rec = wordmarkClicksRef.current;
    rec.count = now - rec.lastAt <= 600 ? rec.count + 1 : 1;
    rec.lastAt = now;
    if (rec.count >= 5) {
      rec.count = 0;
      setEggOpen(true);
      return;
    }
    void go("launch");
  };

  useEffect(() => {
    setPaletteIdx(0);
  }, [paletteQuery, paletteOpen]);

  useEffect(() => {
    if (!paletteOpen) return;
    const row = palListRef.current?.querySelector<HTMLElement>(`[data-pal-idx="${paletteIdx}"]`);
    row?.scrollIntoView({ block: "nearest" });
  }, [paletteIdx, paletteOpen]);

  const missingKeyCards = useMemo(() => {
    if (!state) return [];
    return state.cards.filter((c) => c.kind === "api" && !state.hasKey[c.id]);
  }, [state]);

  const settingItems = useMemo(() => (state ? buildSettingSearchItems(state) : []), [state]);

  const missingClaude = !!state && !state.claudePath;
  const warnCount = (missingClaude ? 1 : 0) + (state?.conflictEnv.length ? 1 : 0) + missingKeyCards.length;

  const runInstallClaude = async (method: "script" | "winget") => {
    if (installingClaude) return;
    setInstallingClaude(method);
    try {
      await invoke("install_claude", { method });
      const s = await refresh();
      toast(
        s.claudePath ? `已检测到 ${s.claudePath}` : "安装窗口已关闭，但仍未检测到 claude",
        !s.claudePath,
      );
    } catch (e) {
      toast(String(e), true);
    } finally {
      setInstallingClaude(null);
    }
  };

  const confirmLeave = async () => {
    if (!dirtyRef.current) return true;
    const ok = await ask("当前页面有未保存的改动，切换后将丢失。", {
      title: "放弃未保存的改动？",
      kind: "warning",
      okLabel: "放弃改动",
      cancelLabel: "取消",
    });
    if (ok) dirtyRef.current = false;
    return ok;
  };

  const selectProfile = async (nextId: string | null) => {
    if (nextId === profileId) return;
    if (!(await confirmLeave())) return;
    setProfileId(nextId);
  };

  const go = async (next: TopView, target?: string | null) => {
    const nextProfileId =
      next === "profiles"
        ? target !== undefined
          ? target
          : state && profileId === null && state.profiles[0]
            ? state.profiles[0].name
            : profileId
        : profileId;
    const leaving = next !== view || (next === "profiles" && nextProfileId !== profileId);
    if (leaving && !(await confirmLeave())) return;
    if (next !== view) {
      viewMotionRef.current = {
        axis: "x",
        dir: TOP_VIEW_INDEX[next] > TOP_VIEW_INDEX[view] ? 1 : -1,
      };
      setViewMountedRev(rev);
      setView(next);
    }
    if (next === "global") {
      setSettingsTarget(null);
    }
    if (next === "profiles") {
      setProfileId(nextProfileId);
    }
    if (next === "cards" && target) {
      setModalCard(target);
    }
  };

  const windowControls = (
    <div className="win-controls">
      <button
        type="button"
        className="win-btn"
        aria-label="最小化"
        title="最小化"
        onClick={() => void getCurrentWindow().minimize()}
      >
        <svg viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.2">
          <line x1="2" y1="6" x2="10" y2="6" />
        </svg>
      </button>
      <button
        type="button"
        className="win-btn"
        aria-label={maximized ? "还原" : "最大化"}
        title={maximized ? "还原" : "最大化"}
        onClick={() => void getCurrentWindow().toggleMaximize()}
      >
        {maximized ? (
          <svg viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.15">
            <rect x="2" y="3.5" width="6.5" height="6.5" rx="0.6" />
            <path d="M4 3.5V2.6c0-.33.27-.6.6-.6h5.4c.33 0 .6.27.6.6v5.4c0 .33-.27.6-.6.6H8.5" />
          </svg>
        ) : (
          <svg viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.15">
            <rect x="2" y="2" width="8" height="8" rx="0.6" />
          </svg>
        )}
      </button>
      <button
        type="button"
        className="win-btn close"
        aria-label="关闭"
        title="关闭"
        onClick={() => void getCurrentWindow().close()}
      >
        <svg viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round">
          <line x1="2.2" y1="2.2" x2="9.8" y2="9.8" />
          <line x1="9.8" y1="2.2" x2="2.2" y2="9.8" />
        </svg>
      </button>
    </div>
  );

  if (!state) {
    return (
      <div className="shell">
        <header className="mast" data-tauri-drag-region>
          <div className="mast-left" data-tauri-drag-region>
            <span className="wordmark" data-tauri-drag-region>
              Clade
            </span>
          </div>
          <div className="mast-right" data-tauri-drag-region>
            {windowControls}
          </div>
        </header>
        <div className="loading-screen">{bootError ?? "加载中…"}</div>
      </div>
    );
  }

  const cardById = (id: string) => state.cards.find((c) => c.id === id);

  const selectedProfile = profileId ? state.profiles.find((p) => p.name === profileId) ?? null : null;
  const editingCard = modalCard && modalCard !== "__new__" ? cardById(modalCard) ?? null : null;

  const palQ = paletteQuery.trim().toLowerCase();
  const matchedProfiles = state.profiles.filter((p) => !palQ || p.name.toLowerCase().includes(palQ));
  const matchedCards = state.cards.filter((c) => {
    if (!palQ) return true;
    const auth = authMap[c.id];
    return (
      (c.name || "").toLowerCase().includes(palQ) ||
      c.id.toLowerCase().includes(palQ) ||
      (auth?.email || "").toLowerCase().includes(palQ)
    );
  });
  const matchedSettings = palQ ? settingItems.filter((it) => matchSettingSearchItem(it, palQ)) : [];
  const paletteItems: (
    | { kind: "profile"; name: string }
    | { kind: "card"; id: string }
    | { kind: "setting"; sec: SettingsTarget["sec"]; itemId: string }
  )[] = [
    ...matchedProfiles.map((p) => ({ kind: "profile" as const, name: p.name })),
    ...matchedCards.map((c) => ({ kind: "card" as const, id: c.id })),
    ...matchedSettings.map((it) => ({ kind: "setting" as const, sec: it.sec, itemId: it.id })),
  ];
  const activePalIdx = paletteItems.length > 0 ? Math.min(paletteIdx, paletteItems.length - 1) : -1;
  const pickPaletteItem = (item: (typeof paletteItems)[number]) => {
    setPaletteOpen(false);
    if (item.kind === "profile") {
      void go("profiles", item.name);
    } else if (item.kind === "card") {
      setModalCard(item.id);
    } else {
      const target: SettingsTarget = { sec: item.sec, itemId: item.itemId, seq: ++settingsTargetSeq.current };
      if (view === "global") {
        setSettingsTarget(target);
      } else {
        void (async () => {
          if (!(await confirmLeave())) return;
          setSettingsTarget(target);
          viewMotionRef.current = {
            axis: "x",
            dir: TOP_VIEW_INDEX.global > TOP_VIEW_INDEX[view] ? 1 : -1,
          };
          setViewMountedRev(rev);
          setView("global");
        })();
      }
    }
  };

  const renderCardTile = (c: Card, index: number) => {
    const users = state.profiles.filter((p) => usesCard(p, c.id));
    const auth = authMap[c.id];
    const usage = usageMap[c.id];
    const loginCard = isLoginCardKind(c.kind);

    return (
      <motion.div key={c.id} className="card-motion-slot" variants={viewMotion.indexed} custom={index}>
        <button type="button" className="book-card" onClick={() => setModalCard(c.id)}>
          <div className="book-head">
            <div>
              <div className="book-title">{c.name || c.id}</div>
              <div className="book-id">{c.id}</div>
            </div>
          </div>

          {loginCard ? (
            usage && usage.windows.length > 0 ? (
              <UsageBars windows={usage.windows} max={2} />
            ) : (
              <div className="book-tags">
                {c.modelList.length > 0 ? (
                  c.modelList.slice(0, 4).map((m) => (
                    <span key={m.id} className="book-tag">
                      {m.id}
                    </span>
                  ))
                ) : (
                  <span className="set-sub" style={{ fontSize: 12 }}>
                    {auth === undefined ? "正在查询状态…" : auth.loggedIn ? "已登录" : "尚未登录"}
                  </span>
                )}
              </div>
            )
          ) : (
            <div className="book-tags">
              {c.modelList.length > 0 ? (
                c.modelList.slice(0, 6).map((m) => (
                  <span key={m.id} className="book-tag">
                    {m.id}
                  </span>
                ))
              ) : (
                <span className="set-sub" style={{ fontSize: 12 }}>
                  暂无模型
                </span>
              )}
            </div>
          )}

          <div className="book-foot">
            {loginCard ? (
              <div className="acct-line">
                <span className="acct-email">
                  {auth?.loggedIn
                    ? auth.email || auth.orgName || (c.kind === "antigravity" ? "Google 账号" : "已登录")
                    : c.kind === "official"
                      ? "Claude 官方"
                      : c.kind === "openai_oauth"
                        ? "OpenAI 登录"
                        : "Antigravity"}
                </span>
              </div>
            ) : (
              <span>{API_FORMAT_LABEL[c.protocol]}</span>
            )}
            <span className="mono" style={{ flexShrink: 0 }} title={`${users.length} 个环境使用`}>
              {users.length}
            </span>
          </div>
        </button>
      </motion.div>
    );
  };

  return (
    <div className="shell">
      {/* TOP NAV */}
      <header className="mast" data-tauri-drag-region>
        <div className="mast-left" data-tauri-drag-region>
          <button type="button" className="wordmark" onClick={handleWordmarkClick}>
            Clade
          </button>
          <nav className="top-nav">
            {(
              [
                ["launch", "启动台"],
                ["profiles", "环境"],
                ["cards", "模型卡"],
                ["global", "设置"],
              ] as [TopView, string][]
            ).map(([id, lbl]) => (
              <button key={id} type="button" className={`nav-link ${view === id ? "on" : ""}`} onClick={() => go(id)}>
                {view === id && (
                  <motion.span className="nav-pill" layoutId="top-nav-pill" transition={TRANSITION_INDICATOR} />
                )}
                <span className="nav-lbl">{lbl}</span>
              </button>
            ))}
          </nav>
        </div>

        <div className="mast-right" data-tauri-drag-region>
          <div className="mast-tools">
            <button
              type="button"
              className="search-btn"
              onClick={() => {
                setPaletteQuery("");
                setPaletteIdx(0);
                setPaletteOpen(true);
              }}
            >
              <span className="search-btn-lead">
                <SearchIcon className="ui-icon" />
                <span>搜索环境、模型卡或设置</span>
              </span>
              <span className="kbd">Ctrl K</span>
            </button>

            {warnCount > 0 && (
              <button
                type="button"
                className="btn-circle warn"
                title={`有 ${warnCount} 项提醒`}
                onClick={() => setWarnOpen(true)}
              >
                <AlertTriangleIcon />
                <span className="warn-dot" />
              </button>
            )}

            <button
              type="button"
              className="btn-circle"
              title={state.root}
              onClick={() =>
                invoke("open_dir", { name: "" }).then(
                  () => toast("已打开 ~/.clade"),
                  (e) => toast(String(e), true),
                )
              }
            >
              <FolderIcon />
            </button>

            <button type="button" className="btn-circle" title="切换明暗主题" onClick={toggleTheme}>
              <AnimatePresence mode="wait" initial={false}>
                <motion.span
                  key={theme}
                  className="icon-swap"
                  initial={{ opacity: 0, rotate: -25, scale: 0.85 }}
                  animate={{ opacity: 1, rotate: 0, scale: 1 }}
                  exit={{ opacity: 0, rotate: 25, scale: 0.85 }}
                  transition={TRANSITION_FAST}
                >
                  {theme === "dark" ? <MoonIcon /> : <SunIcon />}
                </motion.span>
              </AnimatePresence>
            </button>
          </div>

          <span className="mast-sep" aria-hidden="true" />
          {windowControls}
        </div>
      </header>

      {/* STAGE */}
      <div className="stage">
        <PageBackdrop view={view} />
        <AnimatePresence mode="wait">
          {view === "launch" && (
            <motion.div
              key="view-launch"
              className="page-scroll"
              variants={STAGE_CONTAINER_VARIANTS}
              initial="hidden"
              animate="show"
              exit="exit"
            >
              <div className="page-inner">
                <motion.div className="hero-bar" variants={viewMotion.indexed} custom={0}>
                  <h1
                    className="hero-h1"
                    title="点击换一句问候语"
                    onClick={() => setGreetingIdx((i) => (i + 1) % GREETINGS.length)}
                  >
                    <AnimatePresence mode="wait" initial={false}>
                      <motion.span
                        key={greetingIdx}
                        style={{ display: "inline-block" }}
                        variants={GREETING_VARIANTS}
                        initial="initial"
                        animate="animate"
                        exit="exit"
                      >
                        {GREETINGS[greetingIdx]}
                      </motion.span>
                    </AnimatePresence>
                  </h1>
                </motion.div>

                {missingClaude && (
                  <motion.div className="env-banner" variants={viewMotion.indexed} custom={1}>
                    <div className="env-banner-body">
                      <div className="env-banner-title">未检测到 Claude Code CLI（claude）</div>
                      <div className="set-sub">
                        启动任何环境前需要先安装 Claude Code，可选用官方脚本或 Winget 一键安装。
                      </div>
                    </div>
                    <div className="env-banner-actions">
                      <button
                        type="button"
                        className="btn btn-sm"
                        disabled={installingClaude !== null}
                        onClick={() =>
                          refresh().then(
                            (s) => toast(s.claudePath ? `已检测到 ${s.claudePath}` : "仍未检测到 claude", !s.claudePath),
                            (e) => toast(String(e), true),
                          )
                        }
                      >
                        重新检测
                      </button>
                      {state.hasWinget && (
                        <button
                          type="button"
                          className="btn btn-sm"
                          disabled={installingClaude !== null}
                          onClick={() => void runInstallClaude("winget")}
                        >
                          {installingClaude === "winget" ? "Winget 安装中…" : "Winget 安装"}
                        </button>
                      )}
                      <button
                        type="button"
                        className="btn btn-primary btn-sm"
                        disabled={installingClaude !== null}
                        onClick={() => void runInstallClaude("script")}
                      >
                        {installingClaude === "script" ? "脚本安装中…" : "脚本安装"}
                      </button>
                    </div>
                  </motion.div>
                )}

                {state.profiles.length === 0 ? (
                  <motion.div
                    className="empty-state"
                    variants={viewMotion.indexed}
                    custom={missingClaude ? 2 : 1}
                  >
                    <div>还没有任何环境，先创建一个环境吧。</div>
                    <button type="button" className="btn btn-primary btn-sm" onClick={() => go("profiles", null)}>
                      <PlusIcon className="ui-icon" strokeWidth={1.6} />
                      <span>新建环境</span>
                    </button>
                  </motion.div>
                ) : (
                  <div className="launch-grid">
                    {state.profiles.map((p, idx) => {
                      const nodes =
                        p.mode === "aggregate"
                          ? p.cards.map(cardById).filter((c): c is Card => !!c)
                          : [cardById(p.card)].filter((c): c is Card => !!c);
                      const dirs = (recentMap[p.name] ?? []).slice(0, 4);
                      const cardSlot = (missingClaude ? 2 : 1) + idx;
                      return (
                        <motion.div
                          key={p.name}
                          className="card-motion-slot"
                          variants={viewMotion.indexed}
                          custom={cardSlot}
                        >
                          <div className="launch-card">
                            <div className="lc-top">
                              <button type="button" className="lc-name" onClick={() => go("profiles", p.name)}>
                                {p.name}
                              </button>
                              <button
                                type="button"
                                className="btn btn-primary btn-sm"
                                onClick={() => setLaunching({ name: p.name })}
                              >
                                <span>启动</span>
                                <ArrowUpRightIcon className="ui-icon icon-launch" strokeWidth={1.6} />
                              </button>
                            </div>

                            <div className="card-pills">
                              {nodes.length > 0 ? (
                                nodes.map((c) => (
                                  <button
                                    key={c.id}
                                    type="button"
                                    className="badge-pill"
                                    onClick={() => setModalCard(c.id)}
                                  >
                                    {c.name || c.id}
                                  </button>
                                ))
                              ) : (
                                <span className="badge-pill">~/.claude</span>
                              )}
                            </div>

                            <div className="dir-list">
                              {dirs.length === 0 ? (
                                <div className="dir-empty">尚无最近目录，点右上角「启动」选择</div>
                              ) : (
                                dirs.map((d) => (
                                  <button
                                    key={d}
                                    type="button"
                                    className="dir-row"
                                    title={d}
                                    onClick={() => setLaunching({ name: p.name, dir: d })}
                                  >
                                    <span className="dir-row-main">
                                      <span className="dir-name">{baseName(d)}</span>
                                      <span className="dir-path">{d}</span>
                                    </span>
                                    <span className="dir-go">
                                      <span>打开</span>
                                      <CornerDownLeftIcon className="ui-icon-sm" />
                                    </span>
                                  </button>
                                ))
                              )}
                            </div>
                          </div>
                        </motion.div>
                      );
                    })}
                  </div>
                )}
              </div>
            </motion.div>
          )}

          {view === "profiles" && (
            <motion.div
              key="view-profiles"
              className="stage-pane"
              variants={STAGE_CONTAINER_VARIANTS}
              initial="hidden"
              animate="show"
              exit="exit"
            >
              <ProfileForm
                key={`profile#${rev}`}
                state={state}
                profile={selectedProfile}
                usageCache={usageMap}
                skipEnter={rev !== viewMountedRev}
                getViewMotion={getViewMotion}
                onUsageUpdated={(id, u) => setUsageMap((m) => ({ ...m, [id]: u }))}
                onSelect={(id) => void selectProfile(id)}
                onSaved={async (name, text) => {
                  const s = await refresh();
                  setProfileId(name ?? s.profiles[0]?.name ?? null);
                  setRev((r) => r + 1);
                  toast(text);
                }}
                onLaunch={(name) => setLaunching({ name })}
                onToast={toast}
                onDirtyChange={setFormDirty}
              />
            </motion.div>
          )}

          {view === "cards" && (
            <motion.div
              key="view-cards"
              className="cards-scroll"
              variants={STAGE_CONTAINER_VARIANTS}
              initial="hidden"
              animate="show"
              exit="exit"
            >
              <div className="cards-inner">
                <div className="book-grid">
                  {state.cards.map((c, idx) => renderCardTile(c, idx))}
                  <motion.div
                    className="card-motion-slot"
                    variants={viewMotion.indexed}
                    custom={state.cards.length}
                  >
                    <button type="button" className="book-card book-add" onClick={() => setModalCard("__new__")}>
                      <span className="book-add-label">
                        <PlusIcon className="book-add-icon" strokeWidth={1.35} />
                        <span>添加模型卡</span>
                      </span>
                    </button>
                  </motion.div>
                </div>
              </div>
            </motion.div>
          )}

          {view === "global" && (
            <motion.div
              key="view-global"
              className="stage-pane"
              variants={STAGE_CONTAINER_VARIANTS}
              initial="hidden"
              animate="show"
              exit="exit"
            >
              <GlobalForm
                key={`global#${rev}`}
                state={state}
                focusTarget={settingsTarget}
                skipEnter={rev !== viewMountedRev}
                getViewMotion={getViewMotion}
                onSaved={async (text) => {
                  await refresh();
                  setSettingsTarget((prev) => (prev ? { sec: prev.sec, seq: prev.seq } : null));
                  setRev((r) => r + 1);
                  toast(text);
                }}
                onToast={toast}
                onRefresh={refresh}
                onDirtyChange={setFormDirty}
              />
            </motion.div>
          )}
        </AnimatePresence>
      </div>

      {/* CARD EDITOR MODAL */}
      <AnimatePresence>
        {modalCard !== null && (
          <CardForm
            key={`card:${modalCard}#${rev}`}
            state={state}
            card={editingCard}
            cachedAuth={editingCard ? authMap[editingCard.id] : undefined}
            cachedUsage={editingCard ? usageMap[editingCard.id] : undefined}
            onAuthUpdated={(id, st) => setAuthMap((m) => ({ ...m, [id]: st }))}
            onUsageUpdated={(id, u) => setUsageMap((m) => ({ ...m, [id]: u }))}
            onSaved={async (id, text) => {
              if (id) fetchedCardsRef.current.delete(id);
              await refresh();
              setModalCard(null);
              toast(text);
            }}
            onClose={() => setModalCard(null)}
          />
        )}
      </AnimatePresence>

      {/* WARNINGS MODAL */}
      <AnimatePresence>
        {warnOpen && (
          <ModalDialog key="warn-modal" label="警告项目" onClose={() => setWarnOpen(false)}>
            <div className="modal-card narrow">
              <div className="mc-head flat">
                <div className="mc-top">
                  <h2 className="mc-title">警告项目</h2>
                  <button type="button" className="btn-circle" aria-label="关闭" onClick={() => setWarnOpen(false)}>
                    <CloseIcon />
                  </button>
                </div>
              </div>
              <div className="mc-body">
                {missingClaude && (
                  <div className="warn-item">
                    <div>
                      <div style={{ fontWeight: 500, color: "var(--ink)" }}>未检测到 Claude Code CLI</div>
                      <div className="set-sub" style={{ marginTop: 2 }}>
                        未在 PATH 或默认目录找到 <span className="mono">claude.exe</span> / <span className="mono">claude.cmd</span>，请先安装 Claude Code。
                      </div>
                    </div>
                    <div className="warn-actions">
                      {state.hasWinget && (
                        <button
                          type="button"
                          className="btn btn-sm"
                          disabled={installingClaude !== null}
                          onClick={() => void runInstallClaude("winget")}
                        >
                          {installingClaude === "winget" ? "安装中…" : "Winget"}
                        </button>
                      )}
                      <button
                        type="button"
                        className="btn btn-primary btn-sm"
                        disabled={installingClaude !== null}
                        onClick={() => void runInstallClaude("script")}
                      >
                        {installingClaude === "script" ? "安装中…" : "脚本安装"}
                      </button>
                    </div>
                  </div>
                )}

                {state.conflictEnv.length > 0 && (
                  <div className="warn-item">
                    <div>
                      <div style={{ fontWeight: 500, color: "var(--ink)" }}>检测到冲突环境变量</div>
                      <div className="set-sub" style={{ marginTop: 2 }}>
                        启动会话时将自动清除以下变量
                      </div>
                      <div className="warn-codes">
                        {state.conflictEnv.map((k) => (
                          <span key={k} className="warn-code">
                            {k}
                          </span>
                        ))}
                      </div>
                    </div>
                  </div>
                )}

                {missingKeyCards.map((c) => (
                  <div key={c.id} className="warn-item">
                    <div>
                      <div style={{ fontWeight: 500, color: "var(--ink)" }}>{c.name || c.id} 尚未设置 API Key</div>
                      <div className="set-sub mono" style={{ marginTop: 2 }}>
                        {c.id}
                      </div>
                    </div>
                    <button
                      type="button"
                      className="btn btn-sm"
                      onClick={() => {
                        setWarnOpen(false);
                        setModalCard(c.id);
                      }}
                    >
                      配置
                    </button>
                  </div>
                ))}

                {warnCount === 0 && <span className="set-sub">当前没有任何警告项目。</span>}
              </div>
              <div className="mc-foot">
                <span />
                <button type="button" className="btn btn-primary btn-sm" onClick={() => setWarnOpen(false)}>
                  关闭
                </button>
              </div>
            </div>
          </ModalDialog>
        )}
      </AnimatePresence>

      {/* SEARCH PALETTE */}
      <AnimatePresence>
        {paletteOpen && (
          <ModalDialog
            key="search-palette"
            label="搜索环境、模型卡或设置"
            overlayClass="top-align"
            variant="palette"
            initialFocusRef={palInputRef}
            onClose={() => setPaletteOpen(false)}
          >
            <div className="palette">
              <input
                ref={palInputRef}
                className="pal-in"
                value={paletteQuery}
                onChange={(e) => setPaletteQuery(e.target.value)}
                onKeyDown={(e) => {
                  if (e.nativeEvent.isComposing) return;
                  if (e.key === "ArrowDown") {
                    e.preventDefault();
                    if (paletteItems.length > 0) setPaletteIdx((i) => (i + 1) % paletteItems.length);
                  } else if (e.key === "ArrowUp") {
                    e.preventDefault();
                    if (paletteItems.length > 0) setPaletteIdx((i) => (i - 1 + paletteItems.length) % paletteItems.length);
                  } else if (e.key === "Home") {
                    if (paletteItems.length > 0) {
                      e.preventDefault();
                      setPaletteIdx(0);
                    }
                  } else if (e.key === "End") {
                    if (paletteItems.length > 0) {
                      e.preventDefault();
                      setPaletteIdx(paletteItems.length - 1);
                    }
                  } else if (e.key === "Enter") {
                    const target = activePalIdx >= 0 ? paletteItems[activePalIdx] : null;
                    if (target) {
                      e.preventDefault();
                      pickPaletteItem(target);
                    }
                  }
                }}
                placeholder="搜索环境、模型卡或设置…"
                role="combobox"
                aria-expanded={true}
                aria-controls="palette-listbox"
                aria-activedescendant={activePalIdx >= 0 ? `pal-opt-${activePalIdx}` : undefined}
              />
              <div ref={palListRef} id="palette-listbox" role="listbox" className="pal-list">
                {matchedProfiles.length > 0 && (
                  <>
                    <div className="pal-group">环境</div>
                    {matchedProfiles.map((p, idx) => (
                      <button
                        key={p.name}
                        id={`pal-opt-${idx}`}
                        data-pal-idx={idx}
                        type="button"
                        role="option"
                        aria-selected={activePalIdx === idx}
                        className={`pal-row ${activePalIdx === idx ? "active" : ""}`}
                        onMouseEnter={() => setPaletteIdx(idx)}
                        onClick={() => pickPaletteItem({ kind: "profile", name: p.name })}
                      >
                        <span className="pal-left">{p.name}</span>
                        <span className="pal-sub">
                          {p.mode === "aggregate" ? `${p.cards.length} 张卡` : p.card || "~/.claude"}
                        </span>
                      </button>
                    ))}
                  </>
                )}

                {matchedCards.length > 0 && (
                  <>
                    <div className="pal-group">模型卡</div>
                    {matchedCards.map((c, j) => {
                      const idx = matchedProfiles.length + j;
                      const auth = authMap[c.id];
                      const sub = auth?.loggedIn ? auth.email || auth.orgName || c.id : c.id;
                      return (
                        <button
                          key={c.id}
                          id={`pal-opt-${idx}`}
                          data-pal-idx={idx}
                          type="button"
                          role="option"
                          aria-selected={activePalIdx === idx}
                          className={`pal-row ${activePalIdx === idx ? "active" : ""}`}
                          onMouseEnter={() => setPaletteIdx(idx)}
                          onClick={() => pickPaletteItem({ kind: "card", id: c.id })}
                        >
                          <span className="pal-left">
                            <span>{c.name || c.id}</span>
                          </span>
                          <span className="pal-sub">{sub}</span>
                        </button>
                      );
                    })}
                  </>
                )}

                {matchedSettings.length > 0 && (
                  <>
                    <div className="pal-group">设置</div>
                    {matchedSettings.map((it, k) => {
                      const idx = matchedProfiles.length + matchedCards.length + k;
                      return (
                        <button
                          key={it.id}
                          id={`pal-opt-${idx}`}
                          data-pal-idx={idx}
                          type="button"
                          role="option"
                          aria-selected={activePalIdx === idx}
                          className={`pal-row ${activePalIdx === idx ? "active" : ""}`}
                          onMouseEnter={() => setPaletteIdx(idx)}
                          onClick={() => pickPaletteItem({ kind: "setting", sec: it.sec, itemId: it.id })}
                        >
                          <span className="pal-left">
                            <span>{it.title}</span>
                          </span>
                          <span className="pal-sub">{it.secLabel}</span>
                        </button>
                      );
                    })}
                  </>
                )}

                {matchedProfiles.length === 0 && matchedCards.length === 0 && matchedSettings.length === 0 && (
                  <div style={{ padding: 18, textAlign: "center", color: "var(--muted-soft)", fontSize: 13 }}>
                    没有匹配的环境、模型卡或设置
                  </div>
                )}
              </div>
            </div>
          </ModalDialog>
        )}
      </AnimatePresence>

      {/* LAUNCHER MODAL */}
      <AnimatePresence>
        {launching && (
          <Launcher
            key={`launch:${launching.name}:${launching.dir ?? ""}`}
            name={launching.name}
            initialDir={launching.dir}
            refresh={refresh}
            onClose={(text) => {
              setLaunching(null);
              void refresh();
              if (text) toast(text);
            }}
          />
        )}
      </AnimatePresence>

      {/* EASTER EGG: REVOKED NOTICE */}
      <AnimatePresence>
        {eggOpen && (
          <motion.div
            key="egg-revoked"
            className="egg-screen"
            role="dialog"
            aria-modal="true"
            aria-label="Account Notice"
            variants={OVERLAY_VARIANTS}
            initial="initial"
            animate="animate"
            exit="exit"
            onClick={() => setEggOpen(false)}
          >
            <motion.div
              className="egg-card"
              variants={MODAL_VARIANTS}
              initial="initial"
              animate="animate"
              exit="exit"
              onClick={(e) => e.stopPropagation()}
            >
              <div className="egg-brand">
                <svg className="egg-spark" viewBox="0 0 40 40" aria-hidden="true">
                  <g fill="currentColor" transform="translate(20 20)">
                    <path d="M-1.5 -3.6 L-1.9 -17 L1.9 -17 L1.5 -3.6 Z" transform="rotate(0)" />
                    <path d="M-1.3 -3.8 L-1.6 -15.2 L1.6 -15.2 L1.3 -3.8 Z" transform="rotate(30)" />
                    <path d="M-1.5 -3.6 L-1.9 -17 L1.9 -17 L1.5 -3.6 Z" transform="rotate(60)" />
                    <path d="M-1.3 -3.8 L-1.6 -15.2 L1.6 -15.2 L1.3 -3.8 Z" transform="rotate(90)" />
                    <path d="M-1.5 -3.6 L-1.9 -17 L1.9 -17 L1.5 -3.6 Z" transform="rotate(120)" />
                    <path d="M-1.3 -3.8 L-1.6 -15.2 L1.6 -15.2 L1.3 -3.8 Z" transform="rotate(150)" />
                    <path d="M-1.5 -3.6 L-1.9 -17 L1.9 -17 L1.5 -3.6 Z" transform="rotate(180)" />
                    <path d="M-1.3 -3.8 L-1.6 -15.2 L1.6 -15.2 L1.3 -3.8 Z" transform="rotate(210)" />
                    <path d="M-1.5 -3.6 L-1.9 -17 L1.9 -17 L1.5 -3.6 Z" transform="rotate(240)" />
                    <path d="M-1.3 -3.8 L-1.6 -15.2 L1.6 -15.2 L1.3 -3.8 Z" transform="rotate(270)" />
                    <path d="M-1.5 -3.6 L-1.9 -17 L1.9 -17 L1.5 -3.6 Z" transform="rotate(300)" />
                    <path d="M-1.3 -3.8 L-1.6 -15.2 L1.6 -15.2 L1.3 -3.8 Z" transform="rotate(330)" />
                    <circle r="3.4" />
                  </g>
                </svg>
                <span className="egg-wordmark">Clade</span>
              </div>

              <div className="egg-body">
                <p>Hello,</p>
                <p>
                  An internal investigation of suspicious signals associated with your account indicates you may be in
                  violation of our{" "}
                  <button
                    type="button"
                    className="egg-link"
                    onClick={() => toast("假的啦，你账号好好的")}
                  >
                    Supported Countries Policy
                  </button>
                  . As a result, we have revoked your access to Clade.
                </p>
                <p>
                  To appeal our decision, please fill out this{" "}
                  <button
                    type="button"
                    className="egg-link"
                    onClick={() => toast("申诉驳回：建议多点几下 Clade（不是）")}
                  >
                    form
                  </button>{" "}
                  or learn more about the appeals process{" "}
                  <button
                    type="button"
                    className="egg-link"
                    onClick={() => setEggOpen(false)}
                  >
                    here
                  </button>
                  .
                </p>
                <p className="egg-sign">Regards</p>
                <p className="egg-team">Clade&apos;s Safeguards Team</p>
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>

      {/* TOASTS */}
      <div className="toasts">
        <AnimatePresence initial={false}>
          {toasts.map((t) => (
            <motion.div
              key={t.id}
              layout
              className={`toast ${t.bad ? "bad" : ""}`}
              variants={TOAST_VARIANTS}
              initial="initial"
              animate="animate"
              exit="exit"
            >
              {t.text}
            </motion.div>
          ))}
        </AnimatePresence>
      </div>
    </div>
  );
}
