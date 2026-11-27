import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ask, message } from "@tauri-apps/plugin-dialog";
import { AnimatePresence, motion } from "motion/react";
import {
  createDirectionalVariants,
  TRANSITION_INDICATOR,
  type SlideMotion,
} from "./motion";
import {
  API_FORMAT_LABEL,
  ASSET_LABEL,
  blankPick,
  blankProfile,
  CONTEXT_1M,
  promptPolicyLabel,
  SHARED_INFO,
  SHARED_KEYS,
  SLOT_KEYS,
  type AssetKind,
  type Card,
  type Pick,
  type Profile,
  type SharedKey,
  type SlotKey,
  type State,
  type Usage,
} from "./types";
import {
  ArrowLeftRightIcon,
  ArrowRightIcon,
  ArrowUpRightIcon,
  CheckIcon,
  GatewayForkIcon,
  MinusIcon,
  PlusIcon,
} from "./icons";
import {
  CollapseBox,
  PromptPolicyControl,
  SegControl,
  SettingRow,
  SwitchGroups,
  ToggleSwitch,
  TriBool,
  TriState,
  UsagePanel,
} from "./ui";

type Props = {
  state: State;
  profile: Profile | null; // null = 新建
  usageCache: Record<string, Usage>;
  skipEnter?: boolean;
  getViewMotion: () => SlideMotion;
  onUsageUpdated: (cardId: string, u: Usage) => void;
  onSelect: (name: string | null) => void;
  onSaved: (name: string | null, text: string) => Promise<void>;
  onLaunch: (name: string) => void;
  onToast: (text: string, bad?: boolean) => void;
  onDirtyChange: (dirty: boolean) => void;
};

type ProfileTab = "route" | "caps" | "policy" | "switches";
const PROFILE_TAB_INDEX: Record<ProfileTab, number> = { route: 0, caps: 1, policy: 2, switches: 3 };

const SLOT_LABEL: Record<SlotKey, string> = {
  main: "主模型",
  sonnet: "Sonnet",
  opus: "Opus",
  fable: "Fable",
  haiku: "Haiku",
};

const uniq = (xs: string[]) => [...new Set(xs)];
const toggled = (xs: string[], x: string) => (xs.includes(x) ? xs.filter((y) => y !== x) : [...xs, x]);

export default function ProfileForm({
  state,
  profile,
  usageCache,
  skipEnter = false,
  getViewMotion,
  onUsageUpdated,
  onSelect,
  onSaved,
  onLaunch,
  onToast,
  onDirtyChange,
}: Props) {
  const isNew = profile === null;
  const profileKey = profile?.name ?? "__new__";
  const [initialJson, setInitialJson] = useState(() => JSON.stringify(profile ?? blankProfile()));
  const [form, setForm] = useState<Profile>(profile ? structuredClone(profile) : blankProfile());
  const [tab, setTab] = useState<ProfileTab>("route");
  const [hasSwitchedInner, setHasSwitchedInner] = useState(false);
  const [cloneName, setCloneName] = useState<string | null>(null);
  const [usageRefreshTick, setUsageRefreshTick] = useState(0);
  const [prevProfileKey, setPrevProfileKey] = useState(profileKey);
  const nameInputRef = useRef<HTMLInputElement>(null);
  const innerMotionRef = useRef<SlideMotion | null>(null);
  const viewMotion = useMemo(() => createDirectionalVariants(getViewMotion), [getViewMotion]);
  const innerMotion = useMemo(
    () =>
      createDirectionalVariants(() => innerMotionRef.current ?? getViewMotion(), {
        enterOffsetX: 28,
        exitOffsetX: 20,
      }),
    [getViewMotion],
  );

  const profileOrderIndex = (key: string) => {
    if (key === "__new__") return state.profiles.length;
    const idx = state.profiles.findIndex((x) => x.name === key);
    return idx === -1 ? state.profiles.length : idx;
  };

  if (prevProfileKey !== profileKey) {
    innerMotionRef.current = {
      axis: "y",
      dir: profileOrderIndex(profileKey) >= profileOrderIndex(prevProfileKey) ? 1 : -1,
    };
    setPrevProfileKey(profileKey);
    const next = profile ? structuredClone(profile) : blankProfile();
    setForm(next);
    setInitialJson(JSON.stringify(next));
    setTab("route");
    setHasSwitchedInner(true);
    setCloneName(null);
    setUsageRefreshTick(0);
  }

  const dirty = useMemo(() => JSON.stringify(form) !== initialJson, [form, initialJson]);

  useEffect(() => {
    const saved = JSON.stringify(profile ?? blankProfile());
    if (saved === initialJson) return;
    setInitialJson(saved);
    if (!dirty && profile) {
      setForm(structuredClone(profile));
    }
  }, [profile]);

  useEffect(() => {
    onDirtyChange(dirty);
    return () => onDirtyChange(false);
  }, [dirty, onDirtyChange]);
  const g = state.global;
  const direct = form.mode === "direct";
  const directCard = state.cards.find((c) => c.id === form.card);
  const usageCard = direct && (form.card === "" || directCard?.kind === "official") ? form.card : null;
  const aggCards = state.cards.filter((c) => c.kind === "api" || c.kind === "openai_oauth" || c.kind === "antigravity");

  const pool: Card[] = direct
    ? directCard?.kind === "api"
      ? [directCard]
      : []
    : form.cards.flatMap((id) => aggCards.filter((c) => c.id === id));
  const thirdParty = direct ? directCard?.kind === "api" : form.cards.length > 0;

  const set = <K extends keyof Profile>(k: K, v: Profile[K]) => setForm((f) => ({ ...f, [k]: v }));

  const classifierPromptMode = form.classifierPrompt === null ? "follow" : form.classifierPrompt === "" ? "off" : "custom";
  const setClassifierPromptMode = (mode: "follow" | "off" | "custom") => {
    if (mode === "follow") set("classifierPrompt", null);
    else if (mode === "off") set("classifierPrompt", "");
    else {
      const seed = g.classifierPrompt.trim() || state.defaultClassifierPrompt;
      set("classifierPrompt", form.classifierPrompt?.trim() ? form.classifierPrompt : seed);
    }
  };

  const pickKey = (p: Pick) => (p.id ? JSON.stringify([direct ? form.card : p.card, p.id]) : "");
  const inPool = (p: Pick) => pool.some((c) => (direct || c.id === p.card) && c.modelList.some((m) => m.id === p.id));
  const parsePick = (v: string): Pick => {
    if (!v) return blankPick();
    const [card, id] = JSON.parse(v) as [string, string];
    return { card, id };
  };

  const pickSelect = (p: Pick, empty: string, onChange: (p: Pick) => void) => (
    <select className="slot-select" value={pickKey(p)} onChange={(e) => onChange(parsePick(e.target.value))}>
      <option value="">{empty}</option>
      {p.id && !inPool(p) && <option value={pickKey(p)}>{p.id}（不在清单里，请重选）</option>}
      {pool.map((c) => (
        <optgroup key={c.id} label={c.name ? `${c.name}（${c.id}）` : c.id}>
          {c.modelList.map((m) => (
            <option key={m.id} value={JSON.stringify([c.id, m.id])}>
              {m.id}
              {m.context >= CONTEXT_1M ? " [1M]" : ""}
            </option>
          ))}
        </optgroup>
      ))}
    </select>
  );

  async function attempt(fn: () => Promise<unknown>) {
    try {
      await fn();
    } catch (e) {
      onToast(String(e), true);
    }
  }

  const save = () =>
    attempt(async () => {
      if (isNew && !form.name.trim()) {
        nameInputRef.current?.focus();
        onToast("请填写环境名称", true);
        return;
      }
      await invoke("save_profile", { profile: form, isNew });
      onDirtyChange(false);
      await onSaved(form.name, `已保存环境 ${form.name}`);
    });

  async function launch() {
    if (!dirty) {
      onLaunch(form.name);
      return;
    }
    const saveBtn = "保存并启动";
    const discardBtn = "放弃改动并启动";
    const choice = await message(`环境 ${form.name} 有未保存的改动，要如何启动？`, {
      title: "有未保存的改动",
      kind: "warning",
      buttons: { yes: saveBtn, no: discardBtn, cancel: "取消" },
    });
    if (choice === saveBtn) {
      let ok = false;
      await attempt(async () => {
        await invoke("save_profile", { profile: form, isNew });
        onDirtyChange(false);
        await onSaved(form.name, `已保存环境 ${form.name}`);
        ok = true;
      });
      if (ok) onLaunch(form.name);
    } else if (choice === discardBtn) {
      if (profile) {
        setForm(structuredClone(profile));
        setInitialJson(JSON.stringify(profile));
      }
      onDirtyChange(false);
      onLaunch(form.name);
    }
  }

  const doClone = () =>
    attempt(async () => {
      if (!cloneName?.trim()) return;
      await invoke("clone_profile", { src: form.name, name: cloneName.trim() });
      setCloneName(null);
      onDirtyChange(false);
      await onSaved(cloneName.trim(), `已克隆为 ${cloneName.trim()}`);
    });

  async function remove() {
    const dir = `${state.root}\\Agents\\${form.name}`;
    const ok = await ask(`只移除登记，目录 ${dir} 会保留（里面有历史记录和登录凭据）。`, {
      title: `移除环境 ${form.name}？`,
      kind: "warning",
      okLabel: "移除",
      cancelLabel: "取消",
    });
    if (!ok) return;
    await attempt(async () => {
      await invoke("delete_profile", { name: form.name });
      onDirtyChange(false);
      await onSaved(null, `已移除环境 ${form.name}（目录 ${dir} 保留）`);
    });
  }

  const cardMeta = (c: Card) => {
    if (c.kind === "official") return "官方账号";
    if (c.modelList.length > 0) return `${c.modelList.length} 个模型`;
    if (c.kind === "openai_oauth") return "OpenAI 登录";
    if (c.kind === "antigravity") return "Antigravity";
    return API_FORMAT_LABEL[c.protocol];
  };

  const renderAssetSection = (k: AssetKind) => {
    const o = form.assets[k];
    const items = uniq([...state.library[k], ...g.assets[k], ...o.add, ...o.exclude]).sort();
    const toggle = (it: string) => {
      const next = g.assets[k].includes(it)
        ? { ...o, exclude: toggled(o.exclude, it) }
        : { ...o, add: toggled(o.add, it) };
      set("assets", { ...form.assets, [k]: next });
    };
    return (
      <motion.section key={k} className="ed-sec" variants={innerMotion.card}>
        <h2 className="ed-sec-h">{ASSET_LABEL[k]}</h2>
        <div className="set-card">
          <div className="set-pad">
            {items.length === 0 ? (
              <span className="set-sub">素材库 _library\{k} 为空</span>
            ) : (
              <div className="choice-grid">
                {items.map((it) => {
                  const isG = g.assets[k].includes(it);
                  const isEx = o.exclude.includes(it);
                  const isAdd = o.add.includes(it);
                  const missing = !state.library[k].includes(it);
                  const cls = isG ? (isEx ? "excluded" : "inherited") : isAdd ? "on" : "";
                  return (
                    <button
                      key={it}
                      type="button"
                      className={`choice-tile ${cls}`}
                      title={isG ? (isEx ? "已从全局排除，点击恢复" : "继承自全局，点击排除") : "点击追加到此环境"}
                      onClick={() => toggle(it)}
                    >
                      <span className="c-box">{cls === "excluded" ? <MinusIcon /> : <CheckIcon />}</span>
                      <span className="c-name">{it}</span>
                      {missing && <span className="c-missing">缺失</span>}
                    </button>
                  );
                })}
              </div>
            )}
          </div>
        </div>
      </motion.section>
    );
  };

  const renderSharedRow = (k: SharedKey) => {
    const info = SHARED_INFO[k];
    return (
      <SettingRow
        key={k}
        title={info.dir}
        monoTitle
        tip={info.desc}
        control={
          <TriBool
            value={form.shared[k]}
            onChange={(next) => set("shared", { ...form.shared, [k]: next })}
            followLabel={`跟随全局（${g.shared[k] ? "共享" : "隔离"}）`}
            labels={["共享", "隔离"]}
          />
        }
      />
    );
  };

  return (
    <div className="studio">
      <motion.aside
        className="toc"
        variants={viewMotion.indexed}
        custom={{ forward: 0, backward: 2 }}
        initial={skipEnter ? false : undefined}
      >
        {state.profiles.map((x) => {
          const active = !isNew && x.name === form.name;
          const sub = x.mode === "aggregate" ? `${x.cards.length} 张卡` : x.card || "~/.claude";
          return (
            <button key={x.name} type="button" className={`toc-item ${active ? "on" : ""}`} onClick={() => onSelect(x.name)}>
              {active && <motion.span className="toc-pill" layoutId="profile-toc-pill" transition={TRANSITION_INDICATOR} />}
              <span className="toc-content">
                <span className="toc-name">{x.name}</span>
                <span className="toc-sub">{sub}</span>
              </span>
            </button>
          );
        })}
        <button
          type="button"
          className={`toc-item ${isNew ? "on" : ""}`}
          style={{ marginTop: 6, color: isNew ? "var(--ink)" : "var(--muted-soft)" }}
          onClick={() => onSelect(null)}
        >
          {isNew && <motion.span className="toc-pill" layoutId="profile-toc-pill" transition={TRANSITION_INDICATOR} />}
          <span className="toc-content">
            <span className="toc-name toc-name-row">
              <PlusIcon className="toc-icon" strokeWidth={1.45} />
              <span>新建</span>
            </span>
          </span>
        </button>
      </motion.aside>

      <form
        className="canvas-pane"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <motion.div
          className="canvas-head"
          variants={viewMotion.indexed}
          custom={{ forward: 1, backward: 0 }}
          initial={skipEnter ? false : undefined}
        >
          <div className="ch-top">
            {isNew ? (
              <div className="ch-title-edit">
                <input
                  ref={nameInputRef}
                  className="ch-title-input"
                  value={form.name}
                  placeholder="输入新环境名称（小写字母、数字和 -）"
                  aria-label="新环境名称（小写字母、数字和 -，创建后不可修改）"
                  title="小写字母、数字和 -，创建后不可修改"
                  autoFocus
                  onChange={(e) => set("name", e.target.value.toLowerCase())}
                />
                {dirty && <span className="dirty-mark" title="有未保存改动" />}
              </div>
            ) : (
              <h1 className="ch-title">
                <span>{form.name}</span>
                {dirty && <span className="dirty-mark" title="有未保存改动" />}
              </h1>
            )}
            <div className="ch-actions">
              {!isNew && (
                <button type="button" className="btn btn-primary btn-sm" onClick={() => void launch()}>
                  <span>启动</span>
                  <ArrowUpRightIcon className="ui-icon icon-launch" strokeWidth={1.6} />
                </button>
              )}
              <button type="submit" className={`btn btn-sm ${dirty || isNew ? "btn-primary" : ""}`}>
                保存
              </button>
              {!isNew && (
                <>
                  <button
                    type="button"
                    className="btn btn-ghost btn-sm"
                    onClick={() => setCloneName(cloneName === null ? `${form.name}-2` : null)}
                  >
                    克隆
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost btn-sm"
                    onClick={() => attempt(() => invoke("open_dir", { name: form.name }))}
                  >
                    目录
                  </button>
                  <button type="button" className="btn btn-ghost btn-danger btn-sm" onClick={remove}>
                    移除
                  </button>
                </>
              )}
            </div>
          </div>

          <div className="cat-tabs">
            {(
              [
                ["route", "路由"],
                ["caps", "能力"],
                ["policy", "策略"],
                ["switches", "开关"],
              ] as [ProfileTab, string][]
            ).map(([id, lbl]) => (
              <button
                key={id}
                type="button"
                className={`cat-tab ${tab === id ? "on" : ""}`}
                onClick={() => {
                  if (id !== tab) {
                    innerMotionRef.current = {
                      axis: "x",
                      dir: PROFILE_TAB_INDEX[id] > PROFILE_TAB_INDEX[tab] ? 1 : -1,
                    };
                    setHasSwitchedInner(true);
                    setTab(id);
                  }
                }}
              >
                {tab === id && (
                  <motion.span className="tab-pill" layoutId="profile-cat-tab" transition={TRANSITION_INDICATOR} />
                )}
                <span className="tab-lbl">{lbl}</span>
              </button>
            ))}
          </div>
        </motion.div>

        <div className="canvas-scroll">
          <motion.div className="canvas-stack" variants={viewMotion.block} custom={{ forward: 2, backward: 1 }}>
            <CollapseBox open={cloneName !== null && !isNew}>
              {cloneName !== null && !isNew && (
                <div className="clone-bar">
                  <input
                    value={cloneName}
                    onChange={(e) => setCloneName(e.target.value.toLowerCase())}
                    placeholder="新环境名称"
                  />
                  <button type="button" className="btn btn-primary btn-sm" onClick={doClone}>
                    确认克隆
                  </button>
                  <button type="button" className="btn btn-ghost btn-sm" onClick={() => setCloneName(null)}>
                    取消
                  </button>
                  <span className="set-sub">复制登记、settings.json、clade-settings.json 与 CLAUDE.md</span>
                </div>
              )}
            </CollapseBox>

            <AnimatePresence mode="wait" initial={!skipEnter}>
              <motion.div
                key={`${profileKey}:${tab}`}
                className="canvas-tab-body"
                variants={innerMotion.subPage}
                custom={hasSwitchedInner ? "inner" : "view-enter"}
                initial="hidden"
                animate="show"
                exit="exit"
              >
            {tab === "route" && (
              <>
                <motion.section className="ed-sec" variants={innerMotion.card}>
                  <h2 className="ed-sec-h">连接拓扑</h2>
                  <div className="topo-stack">
                    <div className="mode-row">
                      <button
                        type="button"
                        className={`mode-tile ${direct ? "on" : ""}`}
                        onClick={() => set("mode", "direct")}
                      >
                        <span>直连</span>
                        <ArrowRightIcon className="mode-icon" />
                      </button>
                      <button
                        type="button"
                        className={`mode-tile ${!direct ? "on" : ""}`}
                        onClick={() => set("mode", "aggregate")}
                      >
                        <span>聚合网关</span>
                        <GatewayForkIcon className="mode-icon" />
                      </button>
                    </div>

                    <div className="source-grid">
                      {direct && (
                        <button
                          type="button"
                          className={`sel-tile ${form.card === "" ? "on" : ""}`}
                          onClick={() => set("card", "")}
                        >
                          <div className="sel-tile-main">
                            <span className="sel-tile-name">~/.claude</span>
                            <span className="sel-tile-meta">全局官方</span>
                          </div>
                          <span className="ind radio" />
                        </button>
                      )}

                      {state.cards.map((c) => {
                        const disabled = !direct
                          ? c.kind === "official"
                          : c.kind === "openai_oauth" || c.kind === "antigravity" || (c.kind === "api" && c.protocol !== "anthropic");
                        const on = !direct ? form.cards.includes(c.id) : form.card === c.id;
                        return (
                          <button
                            key={c.id}
                            type="button"
                            className={`sel-tile ${on ? "on" : ""} ${!direct ? "agg" : ""}`}
                            disabled={disabled}
                            onClick={() =>
                              direct ? set("card", c.id) : set("cards", toggled(form.cards, c.id))
                            }
                          >
                            <div className="sel-tile-main">
                              <span className="sel-tile-name">{c.name || c.id}</span>
                              <span className="sel-tile-meta">{cardMeta(c)}</span>
                            </div>
                            <span className={`ind ${!direct ? "check" : "radio"}`}>{!direct && <CheckIcon />}</span>
                          </button>
                        );
                      })}

                      {!direct &&
                        form.cards
                          .filter((id) => !aggCards.some((c) => c.id === id))
                          .map((id) => (
                            <button
                              key={id}
                              type="button"
                              className="sel-tile on agg bad"
                              onClick={() => set("cards", toggled(form.cards, id))}
                            >
                              <div className="sel-tile-main">
                                <span className="sel-tile-name">{id}</span>
                                <span className="sel-tile-meta">已删除或不可聚合</span>
                              </div>
                              <span className="ind check">
                                <CheckIcon />
                              </span>
                            </button>
                          ))}
                    </div>
                  </div>
                  {direct && form.card && !directCard && (
                    <span className="set-sub bad">当前选中的模型卡 {form.card} 已不存在，请重新选择。</span>
                  )}
                  {direct && directCard && (directCard.kind === "openai_oauth" || directCard.kind === "antigravity" || (directCard.kind === "api" && directCard.protocol !== "anthropic")) && (
                    <span className="set-sub bad">该卡仅支持聚合模式，请切换到聚合网关或更换模型卡。</span>
                  )}
                </motion.section>

                {thirdParty && (
                  <motion.section className="ed-sec" variants={innerMotion.card}>
                    <h2 className="ed-sec-h">模型位</h2>
                    <div className="slot-bento">
                      {SLOT_KEYS.map((k) => (
                        <div key={k} className={`slot-card ${k === "main" ? "primary" : ""}`}>
                          <span className="slot-tag">{SLOT_LABEL[k]}</span>
                          {pickSelect(form.models[k], k === "main" ? "请选择主模型（必填）" : "跟随主模型", (p) =>
                            set("models", { ...form.models, [k]: p }),
                          )}
                        </div>
                      ))}
                      <div className="slot-card">
                        <span className="slot-tag">分类器</span>
                        {pickSelect(form.classifierModel, "跟随 Sonnet", (p) => set("classifierModel", p))}
                      </div>
                    </div>
                  </motion.section>
                )}

                {usageCard !== null && (
                  <motion.section className="ed-sec" variants={innerMotion.card}>
                    <div className="ed-sec-h">
                      <span>订阅额度（{usageCard ? `官方卡 ${usageCard}` : "~/.claude"}）</span>
                      <button
                        type="button"
                        className="btn btn-ghost btn-sm"
                        onClick={() => setUsageRefreshTick((n) => n + 1)}
                      >
                        刷新
                      </button>
                    </div>
                    <div className="set-card">
                      <div className="set-pad">
                        <UsagePanel
                          key={usageCard}
                          card={usageCard}
                          initialUsage={usageCache[usageCard]}
                          onUpdated={(u) => onUsageUpdated(usageCard, u)}
                          refreshSignal={usageRefreshTick}
                          hideFooterRefresh
                        />
                      </div>
                    </div>
                  </motion.section>
                )}

                {!direct && (
                  <motion.section className="ed-sec" variants={innerMotion.card}>
                    <h2 className="ed-sec-h">网关策略</h2>
                    <div className="set-card">
                      <PromptPolicyControl
                        title="顶层 system"
                        hint="覆盖全局设置，剥离或自定义时始终保留 scratchpad 路径提示。"
                        followLabel={`跟随全局（${promptPolicyLabel(g.systemPromptPolicy, "按卡默认")}）`}
                        value={form.systemPromptPolicy}
                        onChange={(v) => set("systemPromptPolicy", v)}
                        placeholder="填写替换后的主对话系统提示词"
                        copyGlobalText={g.systemPromptPolicy?.mode === "custom" ? g.systemPromptPolicy.text : undefined}
                      />
                      <SettingRow
                        title="保留 # Memory"
                        tip="剥离或自定义 system 时是否保留持久记忆段落"
                        control={
                          <TriBool
                            value={form.preserveMemoryPrompt}
                            onChange={(v) => set("preserveMemoryPrompt", v)}
                            followLabel={`跟随全局（${g.preserveMemoryPrompt ? "保留" : "不保留"}）`}
                            labels={["保留", "不保留"]}
                          />
                        }
                      />
                      <PromptPolicyControl
                        title="system-reminder"
                        hint="控制 messages 中的 <system-reminder> 与 role: system 消息。"
                        followLabel={`跟随全局（${promptPolicyLabel(g.systemReminderPolicy, "保留")}）`}
                        value={form.systemReminderPolicy}
                        onChange={(v) => set("systemReminderPolicy", v)}
                        placeholder="填写替换后的 reminder 内容（会自动包裹 <system-reminder>）"
                        copyGlobalText={g.systemReminderPolicy?.mode === "custom" ? g.systemReminderPolicy.text : undefined}
                      />
                      <SettingRow
                        title="auto 分类器提示词"
                        tip="替换客户端 auto 模式安全审查的默认 ~14 万字符提示词"
                        expanded={classifierPromptMode === "custom"}
                        control={
                          <SegControl<"follow" | "custom" | "off">
                            value={classifierPromptMode}
                            onChange={setClassifierPromptMode}
                            options={[
                              { value: "follow", label: `跟随全局（${g.classifierPrompt.trim() ? "已自定义" : "不替换"}）` },
                              { value: "custom", label: "自定义" },
                              { value: "off", label: "不替换" },
                            ]}
                          />
                        }
                      >
                        <CollapseBox open={classifierPromptMode === "custom"}>
                          {classifierPromptMode === "custom" && (
                            <>
                              <textarea
                                className="ed-textarea"
                                value={form.classifierPrompt ?? ""}
                                onChange={(e) => set("classifierPrompt", e.target.value)}
                                placeholder="填写自定义分类器系统提示词"
                              />
                              <div className="prompt-meta">
                                <button
                                  type="button"
                                  className="btn btn-sm"
                                  onClick={() => set("classifierPrompt", state.defaultClassifierPrompt)}
                                >
                                  填入精简模板
                                </button>
                                {g.classifierPrompt.trim() && (
                                  <button
                                    type="button"
                                    className="btn btn-sm"
                                    onClick={() => set("classifierPrompt", g.classifierPrompt)}
                                  >
                                    复制全局内容
                                  </button>
                                )}
                                <span className="set-sub">约 {(form.classifierPrompt ?? "").trim().length} 字符</span>
                              </div>
                            </>
                          )}
                        </CollapseBox>
                      </SettingRow>
                    </div>
                  </motion.section>
                )}
              </>
            )}

            {tab === "caps" && (
              <>
                {(["skills", "plugins", "mcp"] as AssetKind[]).map(renderAssetSection)}

                <motion.section className="ed-sec" variants={innerMotion.card}>
                  <h2 className="ed-sec-h">
                    <span className="inline-title">
                      <span>共享目录</span>
                      <ArrowLeftRightIcon className="inline-arrow" />
                      <span>~/.claude</span>
                    </span>
                  </h2>
                  <div className="set-card">{SHARED_KEYS.map(renderSharedRow)}</div>
                </motion.section>
              </>
            )}

            {tab === "policy" && (
              <>
                <motion.section className="ed-sec" variants={innerMotion.card}>
                  <h2 className="ed-sec-h">配置继承</h2>
                  <div className="set-card">
                    <SettingRow
                      title="继承 ~/.claude/settings.json"
                      tip={
                        form.inheritSettings
                          ? "每次启动用全局 settings.json 重新生成并同步界面偏好，再叠上 clade-settings.json"
                          : "全新模式：settings.json 由本环境自己维护，仅写入 clade 管理的键"
                      }
                      control={
                        <ToggleSwitch
                          checked={form.inheritSettings}
                          onChange={(v) => set("inheritSettings", v)}
                          label="继承 settings.json"
                        />
                      }
                    />
                    {form.inheritSettings && (
                      <SettingRow
                        title="退出时写回改动"
                        tip="正常退出后把通用设置与界面偏好改动增量写回 ~/.claude"
                        control={
                          <ToggleSwitch
                            checked={form.writebackUiPrefs}
                            onChange={(v) => set("writebackUiPrefs", v)}
                            label="退出时写回改动"
                          />
                        }
                      />
                    )}
                    <SettingRow
                      title="加载全局 ~/.claude/CLAUDE.md"
                      tip="无论加载与否，均自动排除原路径防止重复加载"
                      control={
                        <TriBool
                          value={form.inheritClaudeMd}
                          onChange={(v) => set("inheritClaudeMd", v)}
                          followLabel={`跟随全局（${g.inheritClaudeMd ? "加载" : "不加载"}）`}
                          labels={["加载", "不加载"]}
                        />
                      }
                    />
                  </div>
                </motion.section>

                <motion.section className="ed-sec" variants={innerMotion.card}>
                  <h2 className="ed-sec-h">权限与署名</h2>
                  <div className="set-card">
                    <SettingRow
                      title="禁用 auto 模式"
                      tipCode="disableAutoMode"
                      tip="「禁用」写进 settings.json 禁用 auto；「允许」移除继承来的禁用；「跟随全局」保持 ~/.claude/settings.json 的值"
                      control={
                        <TriState
                          value={form.disableAutoMode}
                          onChange={(v) => set("disableAutoMode", v)}
                          followLabel={`跟随全局（${state.globalPolicy.disableAutoMode ? "禁用" : "允许"}）`}
                          labels={["禁用", "允许"]}
                        />
                      }
                    />
                    <SettingRow
                      title="禁用 bypassPermissions"
                      tipCode="permissions.disableBypassPermissionsMode"
                      tip="「禁用」写进 settings.json 禁用 bypassPermissions；「允许」移除继承来的禁用；「跟随全局」保持 ~/.claude/settings.json 的值"
                      control={
                        <TriState
                          value={form.disableBypassMode}
                          onChange={(v) => set("disableBypassMode", v)}
                          followLabel={`跟随全局（${state.globalPolicy.disableBypassMode ? "禁用" : "允许"}）`}
                          labels={["禁用", "允许"]}
                        />
                      }
                    />
                    <SettingRow
                      title="去掉 Git 署名"
                      tipCode="attribution"
                      tip='「去掉」写入 {"commit": "", "pr": "", "sessionUrl": false}；「保留」删除 attribution 恢复默认署名；「跟随全局」保持 ~/.claude/settings.json 的值'
                      control={
                        <TriState
                          value={form.hideAttribution}
                          onChange={(v) => set("hideAttribution", v)}
                          followLabel={`跟随全局（${state.globalPolicy.hideAttribution ? "去掉" : "保留"}）`}
                          labels={["去掉", "保留"]}
                        />
                      }
                    />
                  </div>
                </motion.section>

                <motion.section className="ed-sec" variants={innerMotion.card}>
                  <h2 className="ed-sec-h">排除的 CLAUDE.md</h2>
                  <div className="set-card">
                    {form.claudeMdExcludes.length === 0 ? (
                      <div className="set-pad">
                        <span className="set-sub">暂无排除项。启动选工作目录时可勾选排除父目录里的 CLAUDE.md。</span>
                      </div>
                    ) : (
                      form.claudeMdExcludes.map((f) => (
                        <div key={f} className="set-row">
                          <span className="mono" style={{ fontSize: 12.5, color: "var(--muted)", wordBreak: "break-all" }}>
                            {f}
                          </span>
                          <button
                            type="button"
                            className="btn btn-ghost btn-danger btn-sm"
                            onClick={() => set("claudeMdExcludes", form.claudeMdExcludes.filter((x) => x !== f))}
                          >
                            移除
                          </button>
                        </div>
                      ))
                    )}
                  </div>
                </motion.section>
              </>
            )}

            {tab === "switches" && (
              <>
                {thirdParty && (
                  <motion.section className="ed-sec" variants={innerMotion.card}>
                    <h2 className="ed-sec-h">兼容</h2>
                    <div className="set-card">
                      {state.switches.compat.map((d) => {
                        const on = form.compat.includes(d.key);
                        return (
                          <SettingRow
                            key={d.key}
                            title={d.label}
                            tipCode={d.key}
                            tip={d.note || undefined}
                            control={
                              <ToggleSwitch
                                checked={on}
                                onChange={() => set("compat", toggled(form.compat, d.key))}
                                label={d.label}
                              />
                            }
                          />
                        );
                      })}
                    </div>
                  </motion.section>
                )}

                <SwitchGroups
                  defs={state.switches.profile}
                  value={form.switches}
                  onChange={(v) => set("switches", v)}
                  cardVariants={innerMotion.card}
                />
              </>
            )}
              </motion.div>
            </AnimatePresence>
          </motion.div>
        </div>
      </form>
    </div>
  );
}
