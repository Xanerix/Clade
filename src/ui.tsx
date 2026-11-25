import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { AnimatePresence, motion } from "motion/react";
import {
  COLLAPSE_VARIANTS,
  EASE_EDITORIAL,
  MODAL_VARIANTS,
  OVERLAY_VARIANTS,
  PALETTE_VARIANTS,
  TIP_VARIANTS,
  TRANSITION_INDICATOR,
} from "./motion";
import type { CardKind, PromptPolicy, Switch, SwitchDef, Switches, Usage, UsageWindow } from "./types";

export type Msg = { text: string; bad?: boolean } | null;

// ===== 统一弹窗栈（Esc 只关最顶层 + 焦点锁 + 还原焦点） =====
type ModalEntry = {
  id: number;
  container: HTMLElement;
  onEscape: () => void;
};

let modalSeq = 1;
const modalStack: ModalEntry[] = [];
let modalListenerInstalled = false;

const FOCUSABLE_SEL =
  'a[href], button:not(:disabled), input:not(:disabled):not([type="hidden"]), select:not(:disabled), textarea:not(:disabled), summary, [tabindex]:not([tabindex="-1"])';

function getFocusable(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SEL)).filter(
    (el) => el.offsetParent !== null || el === document.activeElement,
  );
}

function handleGlobalModalKeyDown(e: KeyboardEvent) {
  const top = modalStack[modalStack.length - 1];
  if (!top) return;

  if (e.key === "Escape") {
    e.preventDefault();
    e.stopImmediatePropagation();
    top.onEscape();
    return;
  }

  if (e.key === "Tab") {
    const items = getFocusable(top.container);
    if (items.length === 0) {
      e.preventDefault();
      top.container.focus();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (e.shiftKey) {
      if (!active || !top.container.contains(active) || active === first) {
        e.preventDefault();
        last.focus();
      }
    } else if (!active || !top.container.contains(active) || active === last) {
      e.preventDefault();
      first.focus();
    }
  }
}

function ensureModalListener() {
  if (modalListenerInstalled) return;
  modalListenerInstalled = true;
  window.addEventListener("keydown", handleGlobalModalKeyDown, true);
}

/** 通用弹窗遮罩：注册到弹窗栈，提供 role="dialog"、aria-modal、Esc 只关顶层与焦点锁。 */
export function ModalDialog({
  onClose,
  canClose = true,
  overlayClass = "",
  variant = "modal",
  label,
  initialFocusRef,
  children,
}: {
  onClose: () => void;
  canClose?: boolean;
  overlayClass?: string;
  variant?: "modal" | "palette";
  label: string;
  initialFocusRef?: React.RefObject<HTMLElement | null>;
  children: ReactNode;
}) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  const canCloseRef = useRef(canClose);
  onCloseRef.current = onClose;
  canCloseRef.current = canClose;

  useEffect(() => {
    const el = dialogRef.current;
    if (!el) return;
    ensureModalListener();
    const prevFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const id = modalSeq++;
    const entry: ModalEntry = {
      id,
      container: el,
      onEscape: () => {
        if (canCloseRef.current) onCloseRef.current();
      },
    };
    modalStack.push(entry);

    const target = initialFocusRef?.current ?? getFocusable(el)[0] ?? el;
    target.focus();

    return () => {
      const idx = modalStack.findIndex((x) => x.id === id);
      if (idx !== -1) modalStack.splice(idx, 1);
      if (prevFocus && prevFocus.isConnected) prevFocus.focus();
    };
  }, [initialFocusRef]);

  return (
    <motion.div
      className={`overlay ${overlayClass}`.trim()}
      variants={OVERLAY_VARIANTS}
      initial="initial"
      animate="animate"
      exit="exit"
      onClick={(e) => {
        if (e.target === e.currentTarget && canClose) onClose();
      }}
    >
      <motion.div
        ref={dialogRef}
        className="dialog-host"
        role="dialog"
        aria-modal="true"
        aria-label={label}
        tabIndex={-1}
        variants={variant === "palette" ? PALETTE_VARIANTS : MODAL_VARIANTS}
      >
        {children}
      </motion.div>
    </motion.div>
  );
}

export const CheckIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round">
    <polyline points="20 6 9 17 4 12" />
  </svg>
);

export const MinusIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round">
    <line x1="5" y1="12" x2="19" y2="12" />
  </svg>
);

const pad = (n: number) => String(n).padStart(2, "0");
export const fmtTime = (d: Date) =>
  `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;

export const fmtResetShort = (iso: string) => {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return fmtTime(d).slice(5);
};

/** 带有悬浮 Tooltip 的单行设置项：行内仅显示标题和控件，环境变量名和说明在窗口左下角显示。 */
export function SettingRow({
  itemId,
  title,
  monoTitle = false,
  tipCode,
  tip,
  control,
  expanded,
  children,
}: {
  itemId?: string;
  title: string;
  monoTitle?: boolean;
  tipCode?: string;
  tip?: string;
  control: ReactNode;
  /** 当 children 使用 CollapseBox 保持挂载以支持退场动效时，用于同步展开态的间距 */
  expanded?: boolean;
  children?: ReactNode;
}) {
  const [hovered, setHovered] = useState(false);
  const hasTip = !!(tipCode || tip);
  const hasBody = children !== undefined && children !== null && children !== false;
  const isOpen = expanded ?? hasBody;

  return (
    <div
      data-setting-id={itemId}
      className={`set-row ${hasBody ? "col" : ""} ${hasBody && !isOpen ? "collapsed" : ""} ${hasTip ? "has-tip" : ""}`}
      onMouseEnter={hasTip && !hasBody ? () => setHovered(true) : undefined}
      onMouseLeave={hasTip && !hasBody ? () => setHovered(false) : undefined}
    >
      {hasBody ? (
        <>
          <div
            className="set-row-head"
            onMouseEnter={hasTip ? () => setHovered(true) : undefined}
            onMouseLeave={hasTip ? () => setHovered(false) : undefined}
          >
            <div className="set-info">
              <span className={`set-title ${monoTitle ? "mono" : ""}`}>{title}</span>
            </div>
            {control}
          </div>
          {children}
        </>
      ) : (
        <>
          <div className="set-info">
            <span className={`set-title ${monoTitle ? "mono" : ""}`}>{title}</span>
          </div>
          {control}
        </>
      )}

      {hasTip &&
        createPortal(
          <AnimatePresence>
            {hovered && (
              <motion.div className="tip-bubble" variants={TIP_VARIANTS} initial="initial" animate="animate" exit="exit">
                {tipCode && <span className="tip-code">{tipCode}</span>}
                {tip && <span className="tip-text">{tip}</span>}
              </motion.div>
            )}
          </AnimatePresence>,
          document.body,
        )}
    </div>
  );
}

export function SegControl<T>({
  options,
  value,
  onChange,
}: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (v: T) => void;
}) {
  const layoutId = useId();
  return (
    <div className="seg">
      {options.map((opt) => {
        const active = value === opt.value;
        return (
          <button
            key={opt.label}
            type="button"
            className={active ? "on" : ""}
            onClick={() => onChange(opt.value)}
          >
            {active && <motion.span className="seg-pill" layoutId={layoutId} transition={TRANSITION_INDICATOR} />}
            <span className="seg-lbl">{opt.label}</span>
          </button>
        );
      })}
    </div>
  );
}

/** 可平滑展开收起的内容块（克隆栏、自定义提示词框、设备码框等）。 */
export function CollapseBox({ open, children }: { open: boolean; children: ReactNode }) {
  return (
    <AnimatePresence initial={false}>
      {open && (
        <motion.div
          className="collapse-box"
          variants={COLLAPSE_VARIANTS}
          initial="initial"
          animate="animate"
          exit="exit"
        >
          <div className="collapse-inner">{children}</div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}

export function ToggleSwitch({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label?: string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      className={`sw ${checked ? "on" : ""}`}
      onClick={() => onChange(!checked)}
    />
  );
}

/** 提示词改写策略选择器（跟随默认 / 保留 / 剥离 / 自定义 + 文本框）。 */
export function PromptPolicyControl({
  itemId,
  title,
  hint,
  followLabel = "跟随默认",
  value,
  onChange,
  placeholder,
  copyGlobalText,
}: {
  itemId?: string;
  title: string;
  hint?: string;
  followLabel?: string;
  value: PromptPolicy | null | undefined;
  onChange: (v: PromptPolicy | null) => void;
  placeholder: string;
  copyGlobalText?: string;
}) {
  const mode = !value ? "follow" : value.mode;
  const setMode = (m: "follow" | "keep" | "strip" | "custom") => {
    if (m === "follow") onChange(null);
    else if (m === "keep") onChange({ mode: "keep" });
    else if (m === "strip") onChange({ mode: "strip" });
    else {
      const prev = value?.mode === "custom" ? value.text : copyGlobalText ?? "";
      onChange({ mode: "custom", text: prev });
    }
  };
  return (
    <SettingRow
      itemId={itemId}
      title={title}
      tip={hint}
      expanded={value?.mode === "custom"}
      control={
        <SegControl
          value={mode}
          onChange={setMode}
          options={[
            { value: "follow", label: followLabel },
            { value: "keep", label: "保留" },
            { value: "strip", label: "剥离" },
            { value: "custom", label: "自定义" },
          ]}
        />
      }
    >
      <CollapseBox open={value?.mode === "custom"}>
        {value?.mode === "custom" && (
          <>
            <textarea
              className="ed-textarea"
              value={value.text}
              onChange={(e) => onChange({ mode: "custom", text: e.target.value })}
              placeholder={placeholder}
            />
            <div className="prompt-meta">
              {copyGlobalText && copyGlobalText !== value.text && (
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => onChange({ mode: "custom", text: copyGlobalText })}
                >
                  复制全局内容
                </button>
              )}
              <span className="set-sub">
                {value.text.trim() ? `约 ${value.text.trim().length} 字符` : "内容为空时等价于剥离"}
              </span>
            </div>
          </>
        )}
      </CollapseBox>
    </SettingRow>
  );
}

/** 三态开关：跟随默认 / 开 / 关。 */
export function TriState({
  value,
  onChange,
  followLabel = "跟随默认",
  labels = ["开", "关"],
}: {
  value: Switch | null;
  onChange: (v: Switch | null) => void;
  followLabel?: string;
  labels?: [string, string];
}) {
  return (
    <SegControl<Switch | null>
      value={value}
      onChange={onChange}
      options={[
        { value: null, label: followLabel },
        { value: "on", label: labels[0] },
        { value: "off", label: labels[1] },
      ]}
    />
  );
}

/** 三态布尔开关：跟随默认 / 开 / 关。 */
export function TriBool({
  value,
  onChange,
  followLabel = "跟随默认",
  labels = ["开", "关"],
}: {
  value: boolean | null | undefined;
  onChange: (v: boolean | null) => void;
  followLabel?: string;
  labels?: [string, string];
}) {
  return (
    <SegControl<boolean | null>
      value={value ?? null}
      onChange={onChange}
      options={[
        { value: null, label: followLabel },
        { value: true, label: labels[0] },
        { value: false, label: labels[1] },
      ]}
    />
  );
}

/** 按 group 分组显示的开关卡片列表（全部展开，以小标题分组）。 */
export function SwitchGroups({
  defs,
  value,
  onChange,
}: {
  defs: SwitchDef[];
  value: Switches;
  onChange: (v: Switches) => void;
}) {
  const groups = [...new Set(defs.map((d) => d.group))];
  const set = (key: string, v: Switch | null) => {
    const next = { ...value };
    if (v === null) delete next[key];
    else next[key] = v;
    onChange(next);
  };

  return (
    <>
      {groups.map((g) => {
        const items = defs.filter((d) => d.group === g);
        return (
          <div key={g} className="settings-group-block">
            <h3 className="settings-group-h">{g}</h3>
            <div className="set-card">
              {items.map((d) => (
                <SettingRow
                  key={d.key}
                  itemId={`switch:${d.key}`}
                  title={d.label}
                  tipCode={d.key}
                  tip={d.note || undefined}
                  control={<TriState value={value[d.key] ?? null} onChange={(v) => set(d.key, v)} />}
                />
              ))}
            </div>
          </div>
        );
      })}
    </>
  );
}

export function UsageBars({ windows, max }: { windows: UsageWindow[]; max?: number }) {
  const list = max ? windows.slice(0, max) : windows;
  return (
    <div className="q-rows">
      {list.map((w) => {
        const pct = Math.round(w.percent);
        const isFull = pct >= 100;
        const isHigh = pct >= 75;
        const reset = fmtResetShort(w.resetsAt);
        return (
          <div key={w.label} className="q-item">
            <div className="q-top">
              <span>{w.label}</span>
              <span>
                {reset && (
                  <span className="mono" style={{ fontSize: 11, color: "var(--muted-soft)", marginRight: 10 }}>
                    {reset}
                  </span>
                )}
                <span className={`q-pct ${isFull ? "full" : isHigh ? "high" : ""}`}>{pct}%</span>
              </span>
            </div>
            <div className="q-track">
              <motion.div
                className={`q-fill ${isFull ? "full" : isHigh ? "primary" : ""}`}
                initial={{ width: 0 }}
                animate={{ width: `${Math.min(pct, 100)}%` }}
                transition={{ duration: 0.38, ease: EASE_EDITORIAL }}
              />
            </div>
          </div>
        );
      })}
    </div>
  );
}

/** 官方账号 / OpenAI / Antigravity 登录卡的额度和登录到期时间。card 为空时查 ~/.claude 的登录。 */
export function UsagePanel({
  card,
  kind,
  initialUsage,
  onUpdated,
  refreshSignal = 0,
  hideFooterRefresh = false,
}: {
  card: string;
  kind?: CardKind;
  initialUsage?: Usage | null;
  onUpdated?: (u: Usage) => void;
  refreshSignal?: number;
  hideFooterRefresh?: boolean;
}) {
  const [usage, setUsage] = useState<Usage | "loading" | null>(initialUsage ?? null);
  const [error, setError] = useState("");
  const isOpenAi = kind === "openai_oauth";
  const isAntigravity = kind === "antigravity";

  async function refresh() {
    setUsage("loading");
    setError("");
    try {
      const next = await invoke<Usage>("usage", { card });
      setUsage(next);
      onUpdated?.(next);
    } catch (e) {
      setUsage(null);
      setError(String(e));
    }
  }

  useEffect(() => {
    if (initialUsage && refreshSignal === 0) {
      setUsage(initialUsage);
      return;
    }
    refresh();
  }, [card, refreshSignal]);

  const expires = usage && usage !== "loading" && usage.credentialExpiresAt ? new Date(usage.credentialExpiresAt) : null;
  const daysLeft = expires ? (expires.getTime() - Date.now()) / 86400000 : Infinity;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      {usage === "loading" ? (
        <span className="set-sub">正在查询额度…</span>
      ) : usage && usage.windows.length > 0 ? (
        <UsageBars windows={usage.windows} />
      ) : usage ? (
        <span className="set-sub">
          {isOpenAi
            ? "ChatGPT 没有返回额度信息（可能不是 Plus/Pro 订阅账号）"
            : isAntigravity
              ? "Antigravity 没有返回额度信息"
              : "claude 没有返回额度（可能没登录，或者不是订阅账号）"}
        </span>
      ) : null}

      {expires && (
        <span className={`set-sub ${daysLeft < 7 ? "bad" : ""}`}>
          登录到期：{fmtTime(expires)}
          {daysLeft < 7 && "（刷新 token 不会延长，到期前需重新登录）"}
        </span>
      )}

      {error && <p className="msg-inline bad">{error}</p>}

      {!hideFooterRefresh && (
        <div className="prompt-meta" style={{ justifyContent: "space-between" }}>
          <span className="set-sub">
            {usage && usage !== "loading" && usage.fetchedAtMs > 0 && `数据时间 ${fmtTime(new Date(usage.fetchedAtMs))} · `}
            {isOpenAi
              ? "ChatGPT 订阅接口，不花额度"
              : isAntigravity
                ? "Antigravity 接口，不花额度"
                : "claude -p /usage，不花 token"}
          </span>
          <button type="button" className="btn btn-ghost btn-sm" disabled={usage === "loading"} onClick={refresh}>
            {usage === "loading" ? "查询中…" : "刷新"}
          </button>
        </div>
      )}
    </div>
  );
}

export function MsgLine({ msg }: { msg: Msg }) {
  const ref = useRef<HTMLParagraphElement>(null);
  useEffect(() => {
    if (msg?.bad) {
      ref.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });
    }
  }, [msg]);
  return msg ? (
    <p ref={ref} role={msg.bad ? "alert" : "status"} className={`msg-inline ${msg.bad ? "bad" : ""}`}>
      {msg.text}
    </p>
  ) : null;
}
