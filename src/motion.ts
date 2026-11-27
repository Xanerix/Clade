import type { Transition, Variants } from "motion/react";

/**
 * 差速过渡曲线（移植自 bdtv-web Axis 动效体系，扩展支持方向感知的水平 / 垂直轴向）：
 * - 水平标签页切换：向右切（dir = +1）旧内容向左错峰退场、新内容从右侧错峰滑入；
 *   向左切（dir = -1）旧内容向右错峰退场、新内容从左侧错峰滑入。
 * - 垂直侧栏切换：向下切（dir = +1）旧内容向上退场、新内容从下方上浮入场；向上切反向。
 */
export const EASE_OUT = [0.16, 1, 0.3, 1] as const;

/** 克制编辑风统一缓动曲线（柔和减速，无弹跳） */
export const EASE_EDITORIAL = [0.22, 1, 0.36, 1] as const;

export const TRANSITION_FAST: Transition = {
  duration: 0.14,
  ease: EASE_EDITORIAL,
};

export const TRANSITION_BASE: Transition = {
  duration: 0.2,
  ease: EASE_EDITORIAL,
};

/** 共享滑动指示器（导航、Tab、分段控件）的克制临界阻尼弹簧 */
export const TRANSITION_INDICATOR: Transition = {
  type: "spring",
  stiffness: 520,
  damping: 42,
  mass: 0.85,
};

export type SlideAxis = "x" | "y";
export type SlideDir = -1 | 1;
export type SlideMotion = { axis: SlideAxis; dir: SlideDir };
export type SlotSpec = number | { forward: number; backward: number };

type DirectionalOpts = {
  enterOffsetX?: number;
  exitOffsetX?: number;
  enterOffsetY?: number;
  exitOffsetY?: number;
  enterDuration?: number;
  exitDuration?: number;
  enterDelay?: number;
  enterStagger?: number;
  exitStagger?: number;
  enterCap?: number;
  exitCap?: number;
};

function resolveSlot(spec: SlotSpec | undefined, motion: SlideMotion): number {
  if (typeof spec === "number") return spec;
  if (!spec) return 0;
  return motion.axis === "x" && motion.dir < 0 ? spec.backward : spec.forward;
}

function offsetPos(motion: SlideMotion, distX: number, distY: number, sign: 1 | -1) {
  return motion.axis === "x"
    ? { x: sign * motion.dir * distX, y: 0 }
    : { x: 0, y: sign * motion.dir * distY };
}

/** 顶层舞台编排容器：不做整块硬切，只向内部区块/卡片传递 hidden / show / exit 状态 */
export const STAGE_CONTAINER_VARIANTS: Variants = {
  hidden: {},
  show: {},
  exit: {},
};

/**
 * 构造支持实时方向感知的差速过渡变体组。
 * 在 hidden / show / exit 触发瞬间动态读取 getMotion()，确保 AnimatePresence 退场旧节点
 * 始终沿最新切换方向离开，不会拿到入场时的过期方向。
 */
export function createDirectionalVariants(
  getMotion: () => SlideMotion,
  opts: DirectionalOpts = {},
): {
  card: Variants;
  indexed: Variants;
  block: Variants;
  subPage: Variants;
} {
  const {
    enterOffsetX = 32,
    exitOffsetX = 24,
    enterOffsetY = 20,
    exitOffsetY = 16,
    enterDuration = 0.34,
    exitDuration = 0.18,
    enterDelay = 0.02,
    enterStagger = 0.045,
    exitStagger = 0.03,
    enterCap = 0.24,
    exitCap = 0.12,
  } = opts;

  const card: Variants = {
    hidden: () => ({
      opacity: 0,
      ...offsetPos(getMotion(), enterOffsetX, enterOffsetY, 1),
    }),
    show: {
      opacity: 1,
      x: 0,
      y: 0,
      transition: { duration: enterDuration, ease: EASE_OUT },
    },
    exit: () => ({
      opacity: 0,
      ...offsetPos(getMotion(), exitOffsetX, exitOffsetY, -1),
      transition: { duration: exitDuration, ease: "easeIn" },
    }),
  };

  const indexed: Variants = {
    hidden: () => ({
      opacity: 0,
      ...offsetPos(getMotion(), enterOffsetX, enterOffsetY, 1),
    }),
    show: (spec?: SlotSpec) => {
      const slot = resolveSlot(spec, getMotion());
      return {
        opacity: 1,
        x: 0,
        y: 0,
        transition: {
          delay: enterDelay + Math.min(slot * enterStagger, enterCap),
          duration: enterDuration,
          ease: EASE_OUT,
        },
      };
    },
    exit: (spec?: SlotSpec) => {
      const m = getMotion();
      const slot = resolveSlot(spec, m);
      return {
        opacity: 0,
        ...offsetPos(m, exitOffsetX, exitOffsetY, -1),
        transition: {
          delay: Math.min(slot * exitStagger, exitCap),
          duration: exitDuration,
          ease: "easeIn",
        },
      };
    },
  };

  const block: Variants = {
    hidden: { opacity: 1, x: 0, y: 0 },
    show: { opacity: 1, x: 0, y: 0 },
    exit: (spec?: SlotSpec) => {
      const m = getMotion();
      const slot = resolveSlot(spec, m);
      return {
        opacity: 0,
        ...offsetPos(m, exitOffsetX, exitOffsetY, -1),
        transition: {
          delay: Math.min(slot * exitStagger, exitCap),
          duration: exitDuration,
          ease: "easeIn",
        },
      };
    },
  };

  const subPage: Variants = {
    hidden: {},
    show: (mode: "inner" | "view-enter" = "inner") => {
      const m = getMotion();
      const extra =
        mode === "inner"
          ? 0
          : m.axis === "x" && m.dir < 0
            ? enterStagger
            : enterStagger * 2;
      return {
        transition: { staggerChildren: enterStagger, delayChildren: enterDelay + extra },
      };
    },
    exit: { transition: { staggerChildren: exitStagger } },
  };

  return { card, indexed, block, subPage };
}

/** 默认向上浮入卡片变体（用于独立/无方向上下文的回退） */
export const CARD_VARIANTS: Variants = createDirectionalVariants(() => ({ axis: "y", dir: 1 })).card;

/** 遮罩淡入淡出 */
export const OVERLAY_VARIANTS: Variants = {
  initial: { opacity: 0 },
  animate: { opacity: 1, transition: { duration: 0.16, ease: "easeOut" } },
  exit: { opacity: 0, transition: { duration: 0.14, ease: "easeIn" } },
};

/** 弹窗本体（轻微缩放 + 上移） */
export const MODAL_VARIANTS: Variants = {
  initial: { opacity: 0, y: 10, scale: 0.985 },
  animate: { opacity: 1, y: 0, scale: 1, transition: { duration: 0.22, ease: EASE_OUT } },
  exit: { opacity: 0, y: -8, scale: 0.99, transition: { duration: 0.15, ease: "easeIn" } },
};

/** 搜索面板（顶部展开，轻位移） */
export const PALETTE_VARIANTS: Variants = {
  initial: { opacity: 0, y: -6, scale: 0.99 },
  animate: { opacity: 1, y: 0, scale: 1, transition: { duration: 0.18, ease: EASE_OUT } },
  exit: { opacity: 0, y: -4, scale: 0.99, transition: { duration: 0.12, ease: "easeIn" } },
};

/** Toast 滑入滑出 */
export const TOAST_VARIANTS: Variants = {
  initial: { opacity: 0, y: 10, scale: 0.98 },
  animate: { opacity: 1, y: 0, scale: 1, transition: TRANSITION_BASE },
  exit: { opacity: 0, y: 6, scale: 0.98, transition: { duration: 0.14, ease: "easeIn" } },
};

/** 左下角 Tooltip */
export const TIP_VARIANTS: Variants = {
  initial: { opacity: 0, y: 5 },
  animate: { opacity: 1, y: 0, transition: TRANSITION_FAST },
  exit: { opacity: 0, y: 3, transition: { duration: 0.1, ease: "easeIn" } },
};

/** 可展开块（克隆栏、自定义提示词框、设备码框等） */
export const COLLAPSE_VARIANTS: Variants = {
  initial: { opacity: 0, height: 0 },
  animate: { opacity: 1, height: "auto", transition: { duration: 0.22, ease: EASE_OUT } },
  exit: { opacity: 0, height: 0, transition: { duration: 0.14, ease: "easeIn" } },
};

/** 问候语轻柔切换 */
export const GREETING_VARIANTS: Variants = {
  initial: { opacity: 0, y: 6 },
  animate: { opacity: 1, y: 0, transition: { duration: 0.26, ease: EASE_OUT } },
  exit: { opacity: 0, y: -6, transition: { duration: 0.14, ease: "easeIn" } },
};
