import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ask } from "@tauri-apps/plugin-dialog";
import { AnimatePresence, motion } from "motion/react";
import { TRANSITION_INDICATOR, VIEW_VARIANTS } from "./motion";
import {
  API_FORMAT_LABEL,
  blankCard,
  CONTEXT_1M,
  DEFAULT_CONTEXT,
  isLoginCardKind,
  usesCard,
  type ApiFormat,
  type AuthStatus,
  type Card,
  type CardModel,
  type DeviceFlowStart,
  type State,
  type Usage,
} from "./types";
import { CheckIcon, CollapseBox, ModalDialog, MsgLine, UsagePanel, type Msg } from "./ui";

type Props = {
  state: State;
  card: Card | null; // null = 新建
  cachedAuth?: AuthStatus | null;
  cachedUsage?: Usage | null;
  onAuthUpdated?: (id: string, st: AuthStatus) => void;
  onUsageUpdated?: (id: string, u: Usage) => void;
  onSaved: (id: string | null, text: string) => Promise<void>;
  onClose: () => void;
};

type ModalTab = "base" | "models";

export default function CardForm({
  state,
  card,
  cachedAuth,
  cachedUsage,
  onAuthUpdated,
  onUsageUpdated,
  onSaved,
  onClose,
}: Props) {
  const isNew = card === null;
  const initialJson = useMemo(() => JSON.stringify(card ?? blankCard()), [card]);
  const [form, setForm] = useState<Card>(card ? structuredClone(card) : blankCard());
  const [tab, setTab] = useState<ModalTab>("base");
  const [key, setKey] = useState("");
  const [fetched, setFetched] = useState<string[]>([]);
  const [manual, setManual] = useState("");
  const [fetching, setFetching] = useState(false);
  const [auth, setAuth] = useState<AuthStatus | "loading" | null>(cachedAuth ?? null);
  const [browserWaiting, setBrowserWaiting] = useState(false);
  const [device, setDevice] = useState<DeviceFlowStart | null>(null);
  const [msg, setMsg] = useState<Msg>(null);
  const closingRef = useRef(false);
  const idInputRef = useRef<HTMLInputElement>(null);
  const baseUrlInputRef = useRef<HTMLInputElement>(null);

  const official = form.kind === "official";
  const openaiOauth = form.kind === "openai_oauth";
  const antigravity = form.kind === "antigravity";
  const isApi = form.kind === "api";
  const isLoginCard = isLoginCardKind(form.kind);
  const isAnthropic = form.protocol === "anthropic";
  const hasKey = card ? state.hasKey[card.id] : false;
  const users = card ? state.profiles.filter((p) => usesCard(p, card.id)).map((p) => p.name) : [];
  const dirty = JSON.stringify(form) !== initialJson || (isApi && key !== "");

  const requestClose = useCallback(async () => {
    if (!dirty) {
      onClose();
      return;
    }
    if (closingRef.current) return;
    closingRef.current = true;
    try {
      const ok = await ask("当前模型卡有未保存的改动，关闭后将丢失。", {
        title: "放弃未保存的改动？",
        kind: "warning",
        okLabel: "放弃改动",
        cancelLabel: "取消",
      });
      if (ok) onClose();
    } finally {
      closingRef.current = false;
    }
  }, [dirty, onClose]);

  const set = <K extends keyof Card>(k: K, v: Card[K]) => setForm((f) => ({ ...f, [k]: v }));
  const inList = (id: string) => form.modelList.some((m) => m.id === id);
  const addModel = (id: string) =>
    setForm((f) => (f.modelList.some((m) => m.id === id) ? f : { ...f, modelList: [...f.modelList, { id, context: DEFAULT_CONTEXT }] }));
  const removeModel = (id: string) => setForm((f) => ({ ...f, modelList: f.modelList.filter((m) => m.id !== id) }));
  const setContext = (id: string, context: number) =>
    setForm((f) => ({ ...f, modelList: f.modelList.map((m): CardModel => (m.id === id ? { ...m, context } : m)) }));
  const fail = (e: unknown) => setMsg({ text: String(e), bad: true });

  async function refreshAuth() {
    if (!card) return;
    setAuth("loading");
    try {
      const st = await invoke<AuthStatus>("card_status", { id: card.id });
      setAuth(st);
      onAuthUpdated?.(card.id, st);
    } catch (e) {
      setAuth(null);
      fail(e);
    }
  }

  useEffect(() => {
    if (card && isLoginCardKind(card.kind) && !cachedAuth) {
      refreshAuth();
    }
    return () => {
      if (card?.kind === "openai_oauth") {
        void invoke("openai_cancel_login");
      } else if (card?.kind === "antigravity") {
        void invoke("antigravity_cancel_login");
      }
    };
  }, []);

  // 设备码登录轮询
  useEffect(() => {
    if (!device || !card) return;
    let cancelled = false;
    const intervalMs = Math.max(device.interval, 5) * 1000;
    const timer = window.setInterval(async () => {
      if (cancelled) return;
      try {
        const res = await invoke<AuthStatus | null>("openai_device_poll", {
          id: card.id,
          deviceAuthId: device.deviceAuthId,
          userCode: device.userCode,
        });
        if (cancelled) return;
        if (res) {
          setDevice(null);
          setAuth(res);
          onAuthUpdated?.(card.id, res);
          setMsg({ text: `登录成功：${res.email || res.orgName || card.id}` });
        }
      } catch (e) {
        if (cancelled) return;
        setDevice(null);
        fail(e);
      }
    }, intervalMs);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [device, card]);

  async function save(keyArg: string | null) {
    if (isNew && !form.id.trim()) {
      setTab("base");
      window.setTimeout(() => idInputRef.current?.focus(), 0);
      fail("请填写模型卡 ID");
      return;
    }
    if (isApi && !form.baseUrl.trim()) {
      setTab("base");
      window.setTimeout(() => baseUrlInputRef.current?.focus(), 0);
      fail("请填写 Base URL");
      return;
    }
    try {
      await invoke("save_card", { card: form, key: keyArg, isNew });
      await onSaved(form.id, keyArg === "" ? `已清除 ${form.id} 的 API key` : `已保存模型卡 ${form.id}`);
    } catch (e) {
      fail(e);
    }
  }

  async function clearSavedKey() {
    const ok = await ask(`确定要从 Windows 凭据管理器中清除模型卡 ${form.id} 已保存的 API Key 吗？`, {
      title: "清除已保存 Key？",
      kind: "warning",
      okLabel: "清除",
      cancelLabel: "取消",
    });
    if (!ok) return;
    await save("");
  }

  async function fetchModels() {
    setFetching(true);
    try {
      const list = await invoke<string[]>("fetch_models", {
        baseUrl: form.baseUrl,
        auth: isAnthropic ? form.auth : "bearer",
        key,
        cardId: form.id,
      });
      setFetched(list);
      setMsg({ text: list.length ? `拉到 ${list.length} 个模型，点击勾选加入清单` : "服务商没有返回任何模型" });
    } catch (e) {
      fail(e);
    } finally {
      setFetching(false);
    }
  }

  async function login() {
    setAuth("loading");
    setMsg({ text: "已在新窗口里运行登录，在浏览器里完成授权；窗口关掉后自动刷新" });
    try {
      const st = await invoke<AuthStatus>("card_login", { id: form.id });
      setAuth(st);
      onAuthUpdated?.(form.id, st);
      setMsg(st.loggedIn ? { text: `登录成功：${st.email}` } : { text: "登录没有成功", bad: true });
    } catch (e) {
      setAuth(null);
      fail(e);
    }
  }

  async function openaiBrowserLogin() {
    setDevice(null);
    setBrowserWaiting(true);
    setAuth("loading");
    setMsg({ text: "已打开浏览器，请完成 ChatGPT 登录授权（回调监听 localhost:1455）" });
    try {
      const st = await invoke<AuthStatus>("openai_browser_login", { id: form.id });
      setAuth(st);
      onAuthUpdated?.(form.id, st);
      setMsg(st.loggedIn ? { text: `登录成功：${st.email || st.orgName}` } : { text: "登录没有成功", bad: true });
    } catch (e) {
      await refreshAuth();
      fail(e);
    } finally {
      setBrowserWaiting(false);
    }
  }

  async function antigravityBrowserLogin() {
    setBrowserWaiting(true);
    setAuth("loading");
    setMsg({ text: "已打开浏览器，请完成 Google 账号 Antigravity 登录授权" });
    try {
      const st = await invoke<AuthStatus>("antigravity_browser_login", { id: form.id });
      setAuth(st);
      onAuthUpdated?.(form.id, st);
      setMsg(st.loggedIn ? { text: `登录成功：${st.email || st.orgName}` } : { text: "登录没有成功", bad: true });
    } catch (e) {
      await refreshAuth();
      fail(e);
    } finally {
      setBrowserWaiting(false);
    }
  }

  async function openaiDeviceLogin() {
    setBrowserWaiting(false);
    setMsg(null);
    try {
      const started = await invoke<DeviceFlowStart>("openai_device_start");
      setDevice(started);
      setMsg({ text: `请打开 ${started.verificationUri} 并输入设备码 ${started.userCode}，授权完成后自动刷新` });
    } catch (e) {
      fail(e);
    }
  }

  async function logout() {
    const ok = await ask("用这张卡的所有环境都会变成未登录。", {
      title: `退出 ${form.id} 的登录？`,
      kind: "warning",
      okLabel: "退出登录",
      cancelLabel: "取消",
    });
    if (!ok) return;
    try {
      await invoke("card_logout", { id: form.id });
      await refreshAuth();
    } catch (e) {
      fail(e);
    }
  }

  async function remove() {
    if (!card) return;
    const what = official
      ? `如果还登录着，会先退出登录（服务器作废这次登录的 token），再删掉卡目录 ${state.root}\\Cards\\${card.id}。退出失败就不删。`
      : openaiOauth
        ? "会退出登录并从 Windows 凭据管理器里删掉这张卡的 OpenAI 登录凭据。"
        : antigravity
          ? "会尝试撤销服务器端授权，并从 Windows 凭据管理器里删掉这张卡的 Antigravity 登录凭据。"
          : "会同时从凭据管理器里删掉这张卡的 API key。";
    const ok = await ask(what, { title: `删除模型卡 ${card.id}？`, kind: "warning", okLabel: "删除", cancelLabel: "取消" });
    if (!ok) return;
    try {
      await invoke("delete_card", { id: card.id });
      await onSaved(null, `已删除模型卡 ${card.id}`);
    } catch (e) {
      fail(e);
    }
  }

  const loggedIn = auth !== "loading" && !!auth?.loggedIn;
  const tabs: [ModalTab, string][] = official
    ? [["base", "基础与登录"]]
    : [
        ["base", isApi ? "基础与认证" : "基础与登录"],
        ["models", `模型 (${form.modelList.length})`],
      ];
  const activeTab: ModalTab = tabs.some(([id]) => id === tab) ? tab : "base";

  return (
    <ModalDialog label={isNew ? "新建模型卡" : `编辑模型卡 ${form.name || form.id}`} onClose={() => void requestClose()}>
      <form
        className="modal-card"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          save(isApi && key ? key : null);
        }}
      >
        <div className={`mc-head ${tabs.length <= 1 ? "flat" : ""}`}>
          <div className="mc-top">
            <div>
              <h2 className="mc-title">{isNew ? "新建模型卡" : form.name || form.id}</h2>
              {!isNew && <div className="mc-sub">{form.id}</div>}
            </div>
            <button type="button" className="btn-circle" aria-label="关闭" onClick={() => void requestClose()}>
              ✕
            </button>
          </div>
          {tabs.length > 1 && (
            <div className="cat-tabs" style={{ paddingBottom: 12 }}>
              {tabs.map(([id, lbl]) => (
                <button
                  key={id}
                  type="button"
                  className={`cat-tab ${activeTab === id ? "on" : ""}`}
                  onClick={() => setTab(id)}
                >
                  {activeTab === id && (
                    <motion.span className="tab-pill" layoutId="card-modal-tab" transition={TRANSITION_INDICATOR} />
                  )}
                  <span className="tab-lbl">{lbl}</span>
                </button>
              ))}
            </div>
          )}
        </div>

        <div className="mc-body">
          <AnimatePresence mode="wait" initial={false}>
            <motion.div
              key={activeTab}
              className="mc-tab-body"
              variants={VIEW_VARIANTS}
              initial="initial"
              animate="animate"
              exit="exit"
            >
          {activeTab === "base" ? (
            <>
              <div className="field-2">
                {isNew ? (
                  <div className="field">
                    <label>类型</label>
                    <select value={form.kind} onChange={(e) => set("kind", e.target.value as Card["kind"])}>
                      <option value="api">第三方 API</option>
                      <option value="official">Claude 官方</option>
                      <option value="openai_oauth">OpenAI 登录</option>
                      <option value="antigravity">Antigravity 登录</option>
                    </select>
                  </div>
                ) : (
                  <div className="field">
                    <label>显示名</label>
                    <input
                      value={form.name}
                      placeholder={official ? "Claude Max" : openaiOauth ? "ChatGPT Plus" : antigravity ? "Antigravity" : "DeepSeek"}
                      onChange={(e) => set("name", e.target.value)}
                    />
                  </div>
                )}
                <div className="field">
                  <label>ID</label>
                  <input
                    ref={idInputRef}
                    className="mono"
                    value={form.id}
                    disabled={!isNew}
                    placeholder={official ? "claude-max" : openaiOauth ? "chatgpt-pro" : antigravity ? "antigravity" : "deepseek"}
                    onChange={(e) => set("id", e.target.value.toLowerCase())}
                  />
                </div>
              </div>

              {isNew && (
                <div className="field">
                  <label>显示名</label>
                  <input
                    value={form.name}
                    placeholder={official ? "例如 Claude 备用" : openaiOauth ? "例如 ChatGPT 主号" : antigravity ? "例如 Antigravity 主号" : "例如 DeepSeek"}
                    onChange={(e) => set("name", e.target.value)}
                  />
                </div>
              )}

              {isApi ? (
                <>
                  <div className="field-2">
                    <div className="field">
                      <label>协议</label>
                      <select
                        value={form.protocol}
                        onChange={(e) => {
                          const next = e.target.value as ApiFormat;
                          setForm((f) => ({ ...f, protocol: next, auth: next === "anthropic" ? f.auth : "bearer" }));
                        }}
                      >
                        <option value="anthropic">{API_FORMAT_LABEL.anthropic}</option>
                        <option value="openai_chat">{API_FORMAT_LABEL.openai_chat}</option>
                        <option value="openai_responses">{API_FORMAT_LABEL.openai_responses}</option>
                        <option value="gemini_native">{API_FORMAT_LABEL.gemini_native}</option>
                      </select>
                    </div>
                    <div className="field">
                      <label>认证</label>
                      <select
                        value={isAnthropic ? form.auth : form.protocol === "gemini_native" ? "x-api-key" : "bearer"}
                        disabled={!isAnthropic}
                        onChange={(e) => set("auth", e.target.value as Card["auth"])}
                      >
                        {isAnthropic ? (
                          <>
                            <option value="bearer">Bearer</option>
                            <option value="x-api-key">x-api-key</option>
                          </>
                        ) : form.protocol === "gemini_native" ? (
                          <option value="x-api-key">x-goog-api-key（固定）</option>
                        ) : (
                          <option value="bearer">Bearer（固定）</option>
                        )}
                      </select>
                    </div>
                  </div>

                  <div className="field">
                    <label>Base URL</label>
                    <input
                      ref={baseUrlInputRef}
                      className="mono"
                      value={form.baseUrl}
                      placeholder={
                        isAnthropic
                          ? "https://api.example.com/anthropic"
                          : form.protocol === "gemini_native"
                            ? "https://generativelanguage.googleapis.com"
                            : "https://api.openai.com/v1"
                      }
                      onChange={(e) => set("baseUrl", e.target.value.trim())}
                    />
                  </div>

                  <div className="field">
                    <div className="field-head">
                      <label htmlFor="card-api-key-input">API Key</label>
                      {hasKey && (
                        <button
                          type="button"
                          className="btn btn-ghost btn-danger btn-sm"
                          style={{ height: 26, padding: "0 10px" }}
                          onClick={clearSavedKey}
                        >
                          清除已保存 Key
                        </button>
                      )}
                    </div>
                    <input
                      id="card-api-key-input"
                      type="password"
                      value={key}
                      placeholder={hasKey ? "••••••••（已保存，留空不改）" : "粘贴 Key"}
                      onChange={(e) => setKey(e.target.value)}
                      autoComplete="off"
                    />
                    <span className="set-sub">
                      存在 Windows 凭据管理器中。{!isAnthropic && "非 Anthropic 协议仅用于聚合模式。"}
                    </span>
                  </div>
                </>
              ) : (
                <div className="set-card">
                  <div className="set-row">
                    {isNew ? (
                      <span className="set-sub">请先保存这张卡，再进行账号登录。</span>
                    ) : (
                      <>
                        <div className="acct-line">
                          {auth === "loading" ? (
                            <span className="set-sub">
                              {openaiOauth && browserWaiting
                                ? "等待浏览器回调 (localhost:1455)…"
                                : antigravity && browserWaiting
                                  ? "等待 Google 授权回调…"
                                  : "查询登录状态…"}
                            </span>
                          ) : auth?.loggedIn ? (
                            <span className="acct-email" style={{ fontSize: 13, color: "var(--ink)" }}>
                              {auth.email || auth.orgName || (antigravity ? "Google 账号" : "已登录")}
                            </span>
                          ) : (
                            <span className="set-sub">未登录</span>
                          )}
                        </div>
                        <div style={{ display: "flex", gap: 6, flexShrink: 0, flexWrap: "wrap" }}>
                          {official && !loggedIn && (
                            <button type="button" className="btn btn-primary btn-sm" disabled={auth === "loading"} onClick={login}>
                              登录
                            </button>
                          )}
                          {official && (
                            <button type="button" className="btn btn-sm" disabled={auth === "loading"} onClick={refreshAuth}>
                              刷新
                            </button>
                          )}
                          {openaiOauth && !loggedIn && (
                            <>
                              <button
                                type="button"
                                className="btn btn-primary btn-sm"
                                disabled={browserWaiting || device !== null}
                                onClick={openaiBrowserLogin}
                              >
                                登录
                              </button>
                              <button
                                type="button"
                                className="btn btn-sm"
                                disabled={browserWaiting || device !== null}
                                onClick={openaiDeviceLogin}
                              >
                                设备码
                              </button>
                            </>
                          )}
                          {openaiOauth && browserWaiting && (
                            <button type="button" className="btn btn-sm" onClick={() => void invoke("openai_cancel_login")}>
                              取消
                            </button>
                          )}
                          {antigravity && !loggedIn && (
                            <button
                              type="button"
                              className="btn btn-primary btn-sm"
                              disabled={browserWaiting}
                              onClick={antigravityBrowserLogin}
                            >
                              登录
                            </button>
                          )}
                          {antigravity && browserWaiting && (
                            <button type="button" className="btn btn-sm" onClick={() => void invoke("antigravity_cancel_login")}>
                              取消
                            </button>
                          )}
                          {loggedIn && (
                            <button
                              type="button"
                              className="btn btn-ghost btn-danger btn-sm"
                              disabled={browserWaiting}
                              onClick={logout}
                            >
                              退出
                            </button>
                          )}
                        </div>
                      </>
                    )}
                  </div>

                  <CollapseBox open={!isNew && !!device}>
                    {!isNew && device && (
                      <div className="set-pad">
                        <div className="device-box">
                          <div className="set-title">
                            设备码：<span className="mono">{device.userCode}</span>
                          </div>
                          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                            <button
                              type="button"
                              className="btn btn-primary btn-sm"
                              onClick={() => void invoke("open_url", { url: device.verificationUri })}
                            >
                              打开验证页面
                            </button>
                            <button
                              type="button"
                              className="btn btn-sm"
                              onClick={() => {
                                navigator.clipboard?.writeText(device.userCode);
                                setMsg({ text: `已复制设备码 ${device.userCode}` });
                              }}
                            >
                              复制设备码
                            </button>
                            <button type="button" className="btn btn-ghost btn-sm" onClick={() => setDevice(null)}>
                              取消
                            </button>
                          </div>
                        </div>
                      </div>
                    )}
                  </CollapseBox>

                  {!isNew && isLoginCard && loggedIn && (
                    <div className="set-pad">
                      <UsagePanel
                        card={form.id}
                        kind={form.kind}
                        initialUsage={cachedUsage}
                        onUpdated={(u) => onUsageUpdated?.(form.id, u)}
                      />
                    </div>
                  )}
                </div>
              )}

              {!isNew && (
                <span className="set-sub">
                  {users.length ? `正在使用的环境：${users.join("、")}` : "当前没有环境使用这张卡"}
                </span>
              )}
            </>
          ) : (
            <>
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 12 }}>
                <span style={{ fontWeight: 500, color: "var(--ink)" }}>模型清单</span>
                <button
                  type="button"
                  className="btn btn-sm"
                  disabled={fetching || (isApi ? !form.baseUrl : isNew || !loggedIn)}
                  onClick={fetchModels}
                >
                  {fetching ? "正在拉取…" : "从上游拉取"}
                </button>
              </div>

              <div className="set-card">
                <div className="set-pad" style={{ paddingTop: 6, paddingBottom: 6 }}>
                  {form.modelList.length === 0 ? (
                    <div className="set-sub" style={{ padding: "10px 0" }}>
                      还没有模型。点右上角「从上游拉取」，或在下方手动添加。
                    </div>
                  ) : (
                    form.modelList.map((m) => (
                      <div key={m.id} className="m-row">
                        <span className="m-id" title={m.id}>
                          {m.id}
                        </span>
                        <input
                          type="number"
                          className="ctx-input"
                          min={1}
                          value={m.context}
                          onChange={(e) => setContext(m.id, Number(e.target.value) || DEFAULT_CONTEXT)}
                          title="上下文窗口（token）"
                        />
                        <button
                          type="button"
                          className={`ctx-btn ${m.context === DEFAULT_CONTEXT ? "active" : ""}`}
                          onClick={() => setContext(m.id, DEFAULT_CONTEXT)}
                        >
                          200K
                        </button>
                        <button
                          type="button"
                          className={`ctx-btn ${m.context >= CONTEXT_1M ? "active" : ""}`}
                          onClick={() => setContext(m.id, CONTEXT_1M)}
                        >
                          1M
                        </button>
                        <button
                          type="button"
                          className="btn btn-ghost btn-danger btn-sm"
                          style={{ padding: "0 8px", height: 30 }}
                          onClick={() => removeModel(m.id)}
                        >
                          ✕
                        </button>
                      </div>
                    ))
                  )}
                </div>
              </div>

              <div style={{ display: "flex", gap: 8 }}>
                <input
                  className="manual-model-input"
                  value={manual}
                  onChange={(e) => setManual(e.target.value.trim())}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      if (manual) addModel(manual);
                      setManual("");
                    }
                  }}
                  placeholder="手动输入模型 ID"
                />
                <button
                  type="button"
                  className="btn btn-sm"
                  disabled={!manual}
                  onClick={() => {
                    addModel(manual);
                    setManual("");
                  }}
                >
                  添加
                </button>
              </div>

              {fetched.length > 0 && (
                <div className="fetched-box">
                  {fetched.map((id) => {
                    const checked = inList(id);
                    return (
                      <button
                        key={id}
                        type="button"
                        className={`choice-tile ${checked ? "on" : ""}`}
                        style={{ height: 34, fontSize: 12.5 }}
                        onClick={() => (checked ? removeModel(id) : addModel(id))}
                      >
                        <span className="c-box">
                          <CheckIcon />
                        </span>
                        <span className="c-name mono">{id}</span>
                      </button>
                    );
                  })}
                </div>
              )}

              <span className="set-sub">
                窗口默认 200K；聚合模式下 ≥ 1M 的模型会自动附带 <span className="mono">[1m]</span> 后缀。
              </span>
            </>
          )}
            </motion.div>
          </AnimatePresence>

          <MsgLine msg={msg} />
        </div>

        <div className="mc-foot">
          {!isNew ? (
            <button type="button" className="btn btn-ghost btn-danger btn-sm" onClick={remove}>
              删除
            </button>
          ) : (
            <span />
          )}
          <div style={{ display: "flex", gap: 8 }}>
            <button type="button" className="btn btn-sm" onClick={() => void requestClose()}>
              取消
            </button>
            <button type="submit" className="btn btn-primary btn-sm">
              保存
            </button>
          </div>
        </div>
      </form>
    </ModalDialog>
  );
}
