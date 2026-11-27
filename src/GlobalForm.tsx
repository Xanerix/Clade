import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AnimatePresence, motion } from "motion/react";
import {
  createDirectionalVariants,
  TRANSITION_INDICATOR,
  type SlideMotion,
} from "./motion";
import { ASSET_LABEL, SHARED_INFO, SHARED_KEYS, type AssetKind, type Global, type State } from "./types";
import {
  buildSettingSearchItems,
  matchSettingSearchItem,
  SETTINGS_GROUPS,
  type SettingsSec,
  type SettingsTarget,
} from "./settingsIndex";
import {
  ArrowLeftRightIcon,
  CheckIcon,
  FolderIcon,
  GlobeIcon,
  LayersIcon,
  MessageIcon,
  SearchIcon,
  ShieldIcon,
  SlidersIcon,
  WrenchIcon,
} from "./icons";
import { PromptPolicyControl, SettingRow, SwitchGroups, ToggleSwitch, TriState } from "./ui";

type Props = {
  state: State;
  focusTarget?: SettingsTarget | null;
  skipEnter?: boolean;
  getViewMotion: () => SlideMotion;
  onSaved: (text: string) => Promise<void>;
  onToast: (text: string, bad?: boolean) => void;
  onRefresh: () => Promise<State>;
  onDirtyChange: (dirty: boolean) => void;
};

const SETTINGS_SEC_ORDER: SettingsSec[] = SETTINGS_GROUPS.flatMap((g) => g.items.map((it) => it.id));

const ICONS: Record<SettingsSec | "search", React.ReactNode> = {
  general: <SlidersIcon />,
  assets: <LayersIcon />,
  shared: <FolderIcon />,
  prompts: <MessageIcon />,
  proxy: <GlobeIcon />,
  switches: <ShieldIcon />,
  tools: <WrenchIcon />,
  search: <SearchIcon />,
};

export default function GlobalForm({ state, focusTarget, skipEnter = false, getViewMotion, onSaved, onToast, onRefresh, onDirtyChange }: Props) {
  const initialJson = useMemo(() => JSON.stringify(state.global), [state.global]);
  const [form, setForm] = useState<Global>(structuredClone(state.global));
  const [sec, setSec] = useState<SettingsSec>(focusTarget?.sec ?? "general");
  const [hasSwitchedInner, setHasSwitchedInner] = useState(false);
  const innerMotionRef = useRef<SlideMotion | null>(null);
  const viewMotion = useMemo(() => createDirectionalVariants(getViewMotion), [getViewMotion]);
  const innerMotion = useMemo(
    () => createDirectionalVariants(() => innerMotionRef.current ?? getViewMotion()),
    [getViewMotion],
  );
  const switchSec = (nextSec: SettingsSec) => {
    if (nextSec === sec) return;
    innerMotionRef.current = {
      axis: "y",
      dir: SETTINGS_SEC_ORDER.indexOf(nextSec) > SETTINGS_SEC_ORDER.indexOf(sec) ? 1 : -1,
    };
    setHasSwitchedInner(true);
    setSec(nextSec);
  };
  const [search, setSearch] = useState("");
  const [pendingFocus, setPendingFocus] = useState<{ itemId: string; seq: number } | null>(
    focusTarget?.itemId ? { itemId: focusTarget.itemId, seq: focusTarget.seq } : null,
  );
  const mainRef = useRef<HTMLFormElement>(null);
  const highlightSeq = useRef(1);
  const highlightTimer = useRef<number | null>(null);
  const highlightedEl = useRef<HTMLElement | null>(null);

  const dirty = JSON.stringify(form) !== initialJson;

  useEffect(() => {
    onDirtyChange(dirty);
    return () => onDirtyChange(false);
  }, [dirty, onDirtyChange]);

  useEffect(() => {
    if (!focusTarget) return;
    if (focusTarget.sec !== sec) {
      innerMotionRef.current = {
        axis: "y",
        dir: SETTINGS_SEC_ORDER.indexOf(focusTarget.sec) > SETTINGS_SEC_ORDER.indexOf(sec) ? 1 : -1,
      };
      setHasSwitchedInner(true);
    }
    setSec(focusTarget.sec);
    if (focusTarget.itemId) {
      setPendingFocus({ itemId: focusTarget.itemId, seq: focusTarget.seq });
    }
  }, [focusTarget]);

  useEffect(() => {
    if (!pendingFocus) return;
    const { itemId } = pendingFocus;
    let cancelled = false;
    let tries = 0;
    let raf = 0;
    const locate = () => {
      if (cancelled) return;
      const root = mainRef.current;
      const target = root?.querySelector<HTMLElement>(`[data-setting-id="${CSS.escape(itemId)}"]`) ?? null;
      if (!target) {
        if (++tries < 60) raf = window.requestAnimationFrame(locate);
        return;
      }
      target.scrollIntoView({ behavior: "smooth", block: "center" });
      if (highlightedEl.current && highlightedEl.current !== target) {
        highlightedEl.current.classList.remove("setting-hit");
      }
      if (highlightTimer.current !== null) {
        window.clearTimeout(highlightTimer.current);
      }
      target.classList.remove("setting-hit");
      void target.offsetWidth;
      target.classList.add("setting-hit");
      highlightedEl.current = target;
      highlightTimer.current = window.setTimeout(() => {
        target.classList.remove("setting-hit");
        if (highlightedEl.current === target) highlightedEl.current = null;
        highlightTimer.current = null;
      }, 1700);
    };
    raf = window.requestAnimationFrame(locate);
    return () => {
      cancelled = true;
      window.cancelAnimationFrame(raf);
    };
  }, [pendingFocus, sec]);

  useEffect(() => {
    return () => {
      if (highlightTimer.current !== null) window.clearTimeout(highlightTimer.current);
    };
  }, []);

  const searchItems = useMemo(() => buildSettingSearchItems(state), [state]);
  const q = search.trim().toLowerCase();
  const matchedEntries = useMemo(
    () => (q ? searchItems.filter((it) => matchSettingSearchItem(it, q)) : []),
    [searchItems, q],
  );
  const allItems = SETTINGS_GROUPS.flatMap((g) => g.items);
  const cur = allItems.find((x) => x.id === sec) ?? allItems[0];

  const jumpToSetting = (targetSec: SettingsSec, itemId: string) => {
    switchSec(targetSec);
    setPendingFocus({ itemId, seq: ++highlightSeq.current });
  };

  const toggleAsset = (k: AssetKind, it: string) => {
    const xs = form.assets[k];
    setForm({ ...form, assets: { ...form.assets, [k]: xs.includes(it) ? xs.filter((x) => x !== it) : [...xs, it] } });
  };

  async function save() {
    try {
      await invoke("save_global", { global: form });
      onDirtyChange(false);
      await onSaved("已保存全局设置，下次启动各环境时生效");
    } catch (e) {
      onToast(String(e), true);
    }
  }

  async function runTool(fn: () => Promise<void>) {
    try {
      await fn();
    } catch (e) {
      onToast(String(e), true);
    }
  }

  return (
    <div className="settings-shell">
      <motion.nav
        className="settings-nav"
        variants={viewMotion.indexed}
        custom={{ forward: 0, backward: 2 }}
        initial={skipEnter ? false : undefined}
      >
        <div className="s-search">
          {ICONS.search}
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape" && search) {
                e.preventDefault();
                setSearch("");
              } else if (e.key === "Enter" && q && matchedEntries[0]) {
                e.preventDefault();
                jumpToSetting(matchedEntries[0].sec, matchedEntries[0].id);
              }
            }}
            placeholder="搜索设置项"
          />
        </div>
        {q ? (
          matchedEntries.length > 0 ? (
            <div className="s-group">
              <div className="s-group-lbl">匹配条目（{matchedEntries.length}）</div>
              {matchedEntries.map((it) => (
                <button
                  key={it.id}
                  type="button"
                  className={`s-nav-item s-hit-item ${pendingFocus?.itemId === it.id && sec === it.sec ? "on" : ""}`}
                  onClick={() => jumpToSetting(it.sec, it.id)}
                >
                  <span className="s-hit-title">{it.title}</span>
                  <span className="s-hit-sub">{it.secLabel}{it.sub ? ` · ${it.sub}` : ""}</span>
                </button>
              ))}
            </div>
          ) : (
            <div className="s-empty">没有匹配的设置项</div>
          )
        ) : (
          SETTINGS_GROUPS.map((g) => (
            <div key={g.group} className="s-group">
              <div className="s-group-lbl">{g.group}</div>
              {g.items.map((it) => (
                <button
                  key={it.id}
                  type="button"
                  className={`s-nav-item ${sec === it.id ? "on" : ""}`}
                  onClick={() => switchSec(it.id)}
                >
                  {sec === it.id && (
                    <motion.span className="s-nav-pill" layoutId="settings-nav-pill" transition={TRANSITION_INDICATOR} />
                  )}
                  <span className="s-nav-lbl">
                    {ICONS[it.id]}
                    <span>{it.label}</span>
                  </span>
                </button>
              ))}
            </div>
          ))
        )}
      </motion.nav>

      <form
        ref={mainRef}
        className="settings-main"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <div className="settings-content">
          <motion.div
            className="settings-head"
            variants={viewMotion.indexed}
            custom={{ forward: 1, backward: 0 }}
            initial={skipEnter ? false : undefined}
          >
            <h1 className="settings-h1">
              <span>{cur.label}</span>
              {dirty && <span className="dirty-mark" title="有未保存改动" />}
            </h1>
            {sec !== "tools" && (
              <button type="submit" className={`btn btn-sm ${dirty ? "btn-primary" : ""}`}>
                保存
              </button>
            )}
          </motion.div>

          <motion.div variants={viewMotion.block} custom={{ forward: 2, backward: 1 }}>
            <AnimatePresence mode="wait" initial={!skipEnter}>
              <motion.div
                key={sec}
                className="settings-sec-body"
                variants={innerMotion.subPage}
                custom={hasSwitchedInner ? "inner" : "view-enter"}
                initial="hidden"
                animate="show"
                exit="exit"
              >
          {sec === "general" && (
            <motion.div className="settings-group-block" variants={innerMotion.card}>
              <h3 className="settings-group-h">指令</h3>
              <div className="set-card">
                <SettingRow
                  itemId="general:inherit-claude-md"
                  title="默认加载 ~/.claude/CLAUDE.md"
                  tip="启动时复制全局指令到环境目录并自动排除原路径，各环境可单独覆盖"
                  control={
                    <ToggleSwitch
                      checked={form.inheritClaudeMd}
                      onChange={(v) => setForm({ ...form, inheritClaudeMd: v })}
                      label="默认加载全局 CLAUDE.md"
                    />
                  }
                />
              </div>
            </motion.div>
          )}

          {sec === "proxy" && (
            <motion.div className="settings-group-block" variants={innerMotion.card}>
              <h3 className="settings-group-h">代理策略</h3>
              <div className="set-card">
                <SettingRow
                  itemId="proxy:policy"
                  title="走系统代理"
                  tipCode="HTTP_PROXY / HTTPS_PROXY"
                  tip="「走」：补成注册表 ProxyServer 地址；「不走」：清除代理变量；「跟随默认」：保留终端原样。对会话、网关、额度查询与 OAuth 登录生效。"
                  control={<TriState value={form.proxy} onChange={(v) => setForm({ ...form, proxy: v })} labels={["走", "不走"]} />}
                />
                <SettingRow
                  itemId="proxy:current"
                  title="当前系统代理"
                  tip="读取自 HKCU\\...\\Internet Settings\\ProxyServer（不看 ProxyEnable，不支持 PAC）"
                  control={<span className="set-code">{state.systemProxy ?? "注册表未配置"}</span>}
                />
              </div>
            </motion.div>
          )}

          {sec === "assets" &&
            (["skills", "plugins", "mcp"] as AssetKind[]).map((k) => {
              const items = [...new Set([...state.library[k], ...form.assets[k]])].sort();
              return (
                <motion.div key={k} className="settings-group-block" variants={innerMotion.card}>
                  <h3 className="settings-group-h">{ASSET_LABEL[k]}</h3>
                  <div className="set-card" data-setting-id={`assets:${k}`}>
                    <div className="set-pad">
                      {items.length === 0 ? (
                        <span className="set-sub">素材库 _library\{k} 为空</span>
                      ) : (
                        <div className="choice-grid">
                          {items.map((it) => {
                            const on = form.assets[k].includes(it);
                            const missing = !state.library[k].includes(it);
                            return (
                              <button
                                key={it}
                                type="button"
                                className={`choice-tile ${on ? "on" : ""}`}
                                onClick={() => toggleAsset(k, it)}
                              >
                                <span className="c-box">
                                  <CheckIcon />
                                </span>
                                <span className="c-name">{it}</span>
                                {missing && <span className="c-missing">缺失</span>}
                              </button>
                            );
                          })}
                        </div>
                      )}
                    </div>
                  </div>
                </motion.div>
              );
            })}

          {sec === "shared" && (
            <motion.div className="settings-group-block" variants={innerMotion.card}>
              <h3 className="settings-group-h">
                <span className="inline-title">
                  <span>Junction</span>
                  <ArrowLeftRightIcon className="inline-arrow" />
                  <span>~/.claude</span>
                </span>
              </h3>
              <div className="set-card">
                {SHARED_KEYS.map((k) => {
                  const info = SHARED_INFO[k];
                  return (
                    <SettingRow
                      key={k}
                      itemId={`shared:${k}`}
                      title={info.dir}
                      monoTitle
                      tip={info.desc}
                      control={
                        <ToggleSwitch
                          checked={form.shared[k]}
                          onChange={(v) => setForm({ ...form, shared: { ...form.shared, [k]: v } })}
                          label={info.dir}
                        />
                      }
                    />
                  );
                })}
              </div>
            </motion.div>
          )}

          {sec === "prompts" && (
            <>
              <motion.div className="settings-group-block" variants={innerMotion.card}>
                <h3 className="settings-group-h">聚合模式默认改写</h3>
                <div className="set-card">
                  <PromptPolicyControl
                    itemId="prompts:system"
                    title="顶层 system"
                    hint="控制转发给上游的顶层 system 字段；剥离或自定义时保留 scratchpad 提示。跟随默认时：Antigravity 剥离，其余保留。"
                    value={form.systemPromptPolicy}
                    onChange={(v) => setForm({ ...form, systemPromptPolicy: v })}
                    placeholder="填写替换后的主对话系统提示词"
                  />
                  <SettingRow
                    itemId="prompts:memory"
                    title="保留 # Memory"
                    tip="剥离或自定义主对话 system 时保留持久记忆目录说明"
                    control={
                      <ToggleSwitch
                        checked={form.preserveMemoryPrompt}
                        onChange={(v) => setForm({ ...form, preserveMemoryPrompt: v })}
                        label="保留 # Memory"
                      />
                    }
                  />
                  <PromptPolicyControl
                    itemId="prompts:reminder"
                    title="system-reminder"
                    hint="控制 messages 中的 <system-reminder> 与 role: system 消息。跟随默认时默认保留。"
                    value={form.systemReminderPolicy}
                    onChange={(v) => setForm({ ...form, systemReminderPolicy: v })}
                    placeholder="填写替换后的 reminder 内容（会自动包裹 <system-reminder> 并置于首条用户消息前）"
                  />
                </div>
              </motion.div>

              <motion.div className="settings-group-block" variants={innerMotion.card}>
                <div className="settings-group-h">
                  <span>auto 分类器默认提示词</span>
                  <div style={{ display: "flex", gap: 6 }}>
                    {form.classifierPrompt && (
                      <button
                        type="button"
                        className="btn btn-ghost btn-sm"
                        onClick={() => setForm({ ...form, classifierPrompt: "" })}
                      >
                        清空
                      </button>
                    )}
                    <button
                      type="button"
                      className="btn btn-ghost btn-sm"
                      onClick={() => setForm({ ...form, classifierPrompt: state.defaultClassifierPrompt })}
                    >
                      填入模板
                    </button>
                  </div>
                </div>
                <div className="set-card" data-setting-id="prompts:classifier">
                  <div className="set-pad" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
                    <textarea
                      className="ed-textarea"
                      value={form.classifierPrompt}
                      onChange={(e) => setForm({ ...form, classifierPrompt: e.target.value })}
                      placeholder="留空不替换，保持 Claude Code 默认提示词…"
                    />
                    <span className="set-sub">
                      {form.classifierPrompt.trim()
                        ? `约 ${form.classifierPrompt.trim().length} 字符；须保留 <block>no</block> / <block>yes</block><category>...</category><reason>...</reason> 输出格式要求`
                        : "未启用替换（聚合模式下可替换为精简提示词以节省审查额度）"}
                    </span>
                  </div>
                </div>
              </motion.div>
            </>
          )}

          {sec === "switches" && (
            <SwitchGroups
              defs={state.switches.global}
              value={form.switches}
              onChange={(v) => setForm({ ...form, switches: v })}
              cardVariants={innerMotion.card}
            />
          )}

          {sec === "tools" && (
            <motion.div className="settings-group-block" variants={innerMotion.card}>
              <h3 className="settings-group-h">目录与集成</h3>
              <div className="set-card">
                <SettingRow
                  itemId="tools:import"
                  title="导入 ~/.claude 素材"
                  tip="同步 Skills、Plugins 与顶层 MCP 配置，已存在的不覆盖"
                  control={
                    <button
                      type="button"
                      className="btn btn-sm"
                      onClick={() =>
                        runTool(async () => {
                          const r = await invoke<string[]>("import_claude");
                          await onRefresh();
                          onToast(r.length ? `已导入：${r.join("，")}` : "没有新的素材可导入");
                        })
                      }
                    >
                      导入
                    </button>
                  }
                />
                <SettingRow
                  itemId="tools:path"
                  title="用户 PATH"
                  tip="将 clade.exe 所在目录写入 HKCU\Environment（新开终端或重启 IDE 后生效）"
                  control={
                    <button
                      type="button"
                      className="btn btn-sm"
                      onClick={() =>
                        runTool(async () => {
                          const [dir, added] = await invoke<[string, boolean]>("add_to_path");
                          onToast(
                            added
                              ? `已将 ${dir} 加入用户 PATH，请新开终端后使用`
                              : `${dir} 已在 PATH 中（若命令不可用，请重启终端或 IDE）`,
                          );
                        })
                      }
                    >
                      添加
                    </button>
                  }
                />
                <SettingRow
                  itemId="tools:root"
                  title="数据目录"
                  tipCode={state.root}
                  control={
                    <button
                      type="button"
                      className="btn btn-sm"
                      onClick={() =>
                        runTool(async () => {
                          await invoke("open_dir", { name: "" });
                          onToast("已打开数据目录");
                        })
                      }
                    >
                      打开
                    </button>
                  }
                />
                <SettingRow
                  itemId="tools:refresh"
                  title="刷新素材库与状态"
                  tip="重新读取 clade.json、_library 与系统环境"
                  control={
                    <button
                      type="button"
                      className="btn btn-sm"
                      onClick={() =>
                        runTool(async () => {
                          await onRefresh();
                          onToast("已刷新素材库与状态");
                        })
                      }
                    >
                      刷新
                    </button>
                  }
                />
              </div>
            </motion.div>
          )}
              </motion.div>
            </AnimatePresence>
          </motion.div>
        </div>
      </form>
    </div>
  );
}
