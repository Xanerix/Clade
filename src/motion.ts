import type { Transition, Variants } from "motion/react";

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

/** 视图 / Tab 内容淡入微移 */
export const VIEW_VARIANTS: Variants = {
  initial: { opacity: 0, y: 6 },
  animate: { opacity: 1, y: 0, transition: TRANSITION_BASE },
  exit: { opacity: 0, y: -4, transition: { duration: 0.12, ease: EASE_EDITORIAL } },
};

/** 遮罩淡入淡出 */
export const OVERLAY_VARIANTS: Variants = {
  initial: { opacity: 0 },
  animate: { opacity: 1, transition: { duration: 0.16, ease: "easeOut" } },
  exit: { opacity: 0, transition: { duration: 0.14, ease: "easeIn" } },
};

/** 弹窗本体（轻微缩放 + 8px 上移） */
export const MODAL_VARIANTS: Variants = {
  initial: { opacity: 0, y: 8, scale: 0.985 },
  animate: { opacity: 1, y: 0, scale: 1, transition: { duration: 0.2, ease: EASE_EDITORIAL } },
  exit: { opacity: 0, y: 4, scale: 0.99, transition: { duration: 0.13, ease: "easeIn" } },
};

/** 搜索面板（顶部展开，轻位移） */
export const PALETTE_VARIANTS: Variants = {
  initial: { opacity: 0, y: -6, scale: 0.99 },
  animate: { opacity: 1, y: 0, scale: 1, transition: { duration: 0.18, ease: EASE_EDITORIAL } },
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
  animate: { opacity: 1, height: "auto", transition: { duration: 0.2, ease: EASE_EDITORIAL } },
  exit: { opacity: 0, height: 0, transition: { duration: 0.14, ease: "easeIn" } },
};

/** 问候语轻柔切换 */
export const GREETING_VARIANTS: Variants = {
  initial: { opacity: 0, y: 5 },
  animate: { opacity: 1, y: 0, transition: { duration: 0.22, ease: EASE_EDITORIAL } },
  exit: { opacity: 0, y: -5, transition: { duration: 0.12, ease: "easeIn" } },
};
