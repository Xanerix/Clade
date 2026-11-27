import { memo, useEffect, useId, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { AnimatePresence, usePresence, useReducedMotion } from "motion/react";

export type BackdropView = "launch" | "profiles" | "cards" | "global";

function useWindowActive(): boolean {
  const [active, setActive] = useState(
    () => typeof document === "undefined" || (!document.hidden && document.hasFocus()),
  );

  useEffect(() => {
    let focused = document.hasFocus();
    const sync = () => setActive(!document.hidden && focused);

    const onFocus = () => {
      focused = true;
      sync();
    };
    const onBlur = () => {
      focused = false;
      sync();
    };
    const onVis = () => sync();

    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);
    document.addEventListener("visibilitychange", onVis);

    let unlisten: (() => void) | null = null;
    let disposed = false;
    void (async () => {
      try {
        const win = getCurrentWindow();
        const isFocused = await win.isFocused();
        if (disposed) return;
        focused = isFocused;
        sync();
        const off = await win.onFocusChanged(({ payload }) => {
          focused = payload;
          sync();
        });
        if (disposed) {
          off();
        } else {
          unlisten = off;
        }
      } catch {
        // 非 Tauri 环境回退到 window focus/blur 与 visibilitychange
      }
    })();

    return () => {
      disposed = true;
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("blur", onBlur);
      document.removeEventListener("visibilitychange", onVis);
      unlisten?.();
    };
  }, []);

  return active;
}

interface PetalSpec {
  k: number;
  ang: number;
  d: string;
}

/**
 * Claude 星芒花瓣几何（原 viewBox 256×257）：
 * 以 12 道内谷收敛中心 (136.89, 133.71) 为坐标原点 (0, 0)，
 * 仅保留右下角构图 (viewBox 180×180, hub 162,162) 实际伸出中心圆 (r=30.2) 外的 6 片可见花瓣，
 * 按右→左逆时针展开顺次排列；其余 6 片在视口内完全落于中心圆内，不产生越界杂瓣。
 */
const LAUNCH_CORNER_PETALS: readonly PetalSpec[] = [
  {
    k: 4,
    ang: 36.34,
    d: "M0 0 L10.55 -43.43 L12.82 -43.43 L15.41 -46.02 L25.91 -59.95 L43.54 -81.99 L51.32 -90.74 L60.39 -100.4 L66.23 -105 L77.24 -105 L85.34 -92.94 L81.72 -80.5 L70.37 -66.11 L60.98 -53.93 L47.5 -35.78 L39.07 -21.26 Z",
  },
  {
    k: 3,
    ang: 5.63,
    d: "M0 0 L-6.36 -35.97 L-6.36 -37.53 L-5 -55.74 L-2.47 -78.1 L-0.01 -106.88 L0.83 -114.98 L4.85 -124.7 L12.82 -129.95 L19.04 -126.97 L24.16 -119.64 L23.45 -114.91 L20.4 -95.15 L14.44 -64.17 L10.55 -43.43 Z",
  },
  {
    k: 2,
    ang: -27.84,
    d: "M0 0 L-36.63 -39.67 L-36.43 -40.45 L-38.18 -43.36 L-52.64 -69.48 L-68.06 -96.05 L-74.93 -107.07 L-76.75 -113.68 C-77.39 -116.41 -77.85 -118.67 -77.85 -121.46 L-69.88 -132.28 L-65.47 -133.71 L-54.84 -132.28 L-50.37 -128.39 L-43.76 -113.29 L-33.06 -89.51 L-16.47 -57.17 L-11.61 -47.58 L-9.02 -38.7 L-8.05 -35.97 L-6.36 -35.97 Z",
  },
  {
    k: 1,
    ang: -56.37,
    d: "M0 0 L-34.88 -9.6 L-36.3 -10.63 L-37.41 -11.67 L-62.55 -28.72 L-89.77 -46.73 L-104.03 -57.1 L-111.74 -62.35 L-115.63 -67.28 L-117.32 -78.04 L-110.32 -85.75 L-100.92 -85.1 L-98.52 -84.45 L-88.99 -77.13 L-68.64 -61.38 L-42.07 -41.81 L-38.18 -38.57 L-36.63 -39.67 Z",
  },
  {
    k: 0,
    ang: -86.69,
    d: "M0 0 L-35.46 5.89 L-36.3 4.53 L-38.77 4.53 L-47.19 4.01 L-75.97 3.24 L-100.92 2.2 L-125.09 0.9 L-131.19 -0.39 L-136.89 -7.91 L-136.31 -11.67 L-131.19 -15.11 L-123.86 -14.46 L-107.66 -13.36 L-83.36 -11.67 L-65.73 -10.63 L-39.61 -7.91 L-35.46 -7.91 L-34.88 -9.6 Z",
  },
  {
    k: 11,
    ang: -119.04,
    d: "M0 0 L-24.57 20.47 L-25.16 20.47 L-92.82 64.42 L-104.87 65.97 L-110.06 61.11 L-109.41 53.14 L-106.95 50.55 L-86.6 36.55 L-36.3 8.36 L-35.46 5.89 Z",
  },
];

const LAUNCH_EXTRA_EXIT_PETALS: readonly PetalSpec[] = [
  {
    k: 5,
    ang: 78.17,
    d: "M0 0 L39.07 -21.26 L39.85 -20.1 L41.86 -20.29 L72.32 -26.77 L88.78 -29.75 L108.42 -33.12 L117.3 -28.98 L118.27 -24.76 L114.77 -16.14 L93.77 -10.96 L69.14 -6.03 L32.46 2.65 L32.01 2.98 Z",
  },
  {
    k: 6,
    ang: 102.18,
    d: "M0 0 L32.01 2.98 L32.52 3.62 L49.05 5.18 L56.12 5.57 L73.42 5.57 L105.63 7.97 L114.06 13.54 L119.11 20.35 L118.27 25.53 L105.31 32.14 L87.81 27.99 L46.98 18.27 L32.98 14.77 L31.03 14.77 Z",
  },
  {
    k: 7,
    ang: 132.92,
    d: "M0 0 L31.03 14.77 L31.03 15.94 L42.7 27.34 L64.09 46.66 L90.85 71.55 L92.21 77.7 L88.78 82.56 L85.15 82.04 L61.62 64.35 L52.55 56.38 L32.01 39.08 L30.65 39.08 Z",
  },
  {
    k: 8,
    ang: 149.27,
    d: "M0 0 L30.65 39.08 L30.65 40.89 L35.38 47.82 L60.39 85.41 L61.69 96.95 L59.87 100.71 L53.39 102.98 L46.26 101.68 L31.62 81.14 L16.52 58 L4.33 37.26 Z",
  },
  {
    k: 9,
    ang: -172.79,
    d: "M0 0 L4.33 37.26 L2.84 38.1 L-4.35 115.55 L-7.72 119.5 L-15.5 122.49 L-21.98 117.56 L-25.42 109.59 L-21.98 93.84 L-17.83 73.3 L-14.46 56.96 L-11.42 36.68 L-9.6 29.94 L-9.73 29.48 Z",
  },
  {
    k: 10,
    ang: -141.14,
    d: "M0 0 L-9.73 29.48 L-11.22 29.68 L-26.52 50.68 L-49.78 82.11 L-68.19 101.81 L-72.6 103.56 L-80.25 99.61 L-79.53 92.54 L-75.26 86.26 L-49.79 53.85 L-34.43 33.76 L-24.51 22.16 L-24.57 20.47 Z",
  },
];

const LAUNCH_ORIGIN_ANG = 38;
const LAUNCH_HUB_BASE = 162;
const LAUNCH_EXIT_ROT_DEG = -64;
const LAUNCH_ENTER_DURATION_MS = 680;
const LAUNCH_EXIT_DURATION_MS = 300;
const TAB_SWITCH_ENTER_DELAY_MS = 140;

function cubicBezier(x1: number, y1: number, x2: number, y2: number): (t: number) => number {
  return (t: number) => {
    if (t <= 0) return 0;
    if (t >= 1) return 1;
    let u = t;
    for (let i = 0; i < 6; i++) {
      const xu = 3 * (1 - u) * (1 - u) * u * x1 + 3 * (1 - u) * u * u * x2 + u * u * u;
      const dx = 3 * (1 - u) * (1 - u) * x1 + 6 * (1 - u) * u * (x2 - x1) + 3 * u * u * (1 - x2);
      if (Math.abs(dx) < 1e-6) break;
      u = Math.max(0, Math.min(1, u - (xu - t) / dx));
    }
    return 3 * (1 - u) * (1 - u) * u * y1 + 3 * (1 - u) * u * u * y2 + u * u * u;
  };
}

const EASE_OUT = cubicBezier(0.16, 1, 0.3, 1);
const EASE_IN = cubicBezier(0.45, 0, 1, 1);

const clamp01 = (v: number) => Math.max(0, Math.min(1, v));
const lerp = (a: number, b: number, t: number) => a + (b - a) * t;

interface LaunchSnapshot {
  fade: number;
  coreScale: number;
  hubShift: number;
  hubRot: number;
  hubScale: number;
  deltas: number[];
  scales: number[];
}

function LaunchBackdrop({ enterDelayMs }: { enterDelayMs: number }) {
  const [isPresent, safeToRemove] = usePresence();
  const reducedMotion = useReducedMotion();

  const layerRef = useRef<SVGGElement | null>(null);
  const hubRef = useRef<SVGGElement | null>(null);
  const coreRef = useRef<SVGGElement | null>(null);
  const petalRefs = useRef<(SVGGElement | null)[]>([]);
  const enterDelayRef = useRef(enterDelayMs);
  enterDelayRef.current = enterDelayMs;
  const safeToRemoveRef = useRef(safeToRemove);
  safeToRemoveRef.current = safeToRemove;

  const snapRef = useRef<LaunchSnapshot>({
    fade: 0,
    coreScale: 0.55,
    hubShift: 0,
    hubRot: 0,
    hubScale: 1,
    deltas: LAUNCH_CORNER_PETALS.map((p) => LAUNCH_ORIGIN_ANG - p.ang),
    scales: LAUNCH_CORNER_PETALS.map(() => 0.92),
  });

  const applyFrame = (
    fade: number,
    coreScale: number,
    hubShift: number,
    hubRot: number,
    hubScale: number,
    deltas: number[],
    scales: number[],
  ) => {
    const snap = snapRef.current;
    snap.fade = fade;
    snap.coreScale = coreScale;
    snap.hubShift = hubShift;
    snap.hubRot = hubRot;
    snap.hubScale = hubScale;

    if (layerRef.current) {
      layerRef.current.style.opacity = fade.toFixed(4);
    }
    if (hubRef.current) {
      const c = (LAUNCH_HUB_BASE + hubShift).toFixed(2);
      hubRef.current.setAttribute(
        "transform",
        `translate(${c} ${c}) rotate(${hubRot.toFixed(2)}) scale(${hubScale.toFixed(4)})`,
      );
    }
    if (coreRef.current) {
      coreRef.current.setAttribute("transform", `scale(${coreScale.toFixed(4)})`);
    }
    for (let i = 0; i < LAUNCH_CORNER_PETALS.length; i++) {
      snap.deltas[i] = deltas[i];
      snap.scales[i] = scales[i];
      const g = petalRefs.current[i];
      if (g) {
        g.setAttribute("transform", `rotate(${deltas[i].toFixed(2)}) scale(${scales[i].toFixed(4)})`);
      }
    }
  };

  // 首帧挂载即锁定初始隐藏折叠态，杜绝入场延迟期间静止图样闪现
  useLayoutEffect(() => {
    if (reducedMotion) {
      applyFrame(
        1,
        1,
        0,
        0,
        1,
        LAUNCH_CORNER_PETALS.map(() => 0),
        LAUNCH_CORNER_PETALS.map(() => 1),
      );
      return;
    }
    applyFrame(
      0,
      0.55,
      0,
      0,
      1,
      LAUNCH_CORNER_PETALS.map((p) => LAUNCH_ORIGIN_ANG - p.ang),
      LAUNCH_CORNER_PETALS.map(() => 0.92),
    );
  }, [reducedMotion]);

  useEffect(() => {
    const count = LAUNCH_CORNER_PETALS.length;

    if (reducedMotion) {
      if (isPresent) {
        applyFrame(
          1,
          1,
          0,
          0,
          1,
          LAUNCH_CORNER_PETALS.map(() => 0),
          LAUNCH_CORNER_PETALS.map(() => 1),
        );
      } else {
        safeToRemoveRef.current?.();
      }
      return;
    }

    let rafId = 0;
    let delayTimer: number | null = null;
    let cancelled = false;

    const deltasBuf = new Array<number>(count);
    const scalesBuf = new Array<number>(count);

    if (isPresent) {
      const startEnter = () => {
        if (cancelled) return;
        const startTs = performance.now();
        const step = (now: number) => {
          if (cancelled) return;
          const t = clamp01((now - startTs) / LAUNCH_ENTER_DURATION_MS);
          const fade = EASE_OUT(clamp01(t / 0.16));
          const coreScale = lerp(0.55, 1, EASE_OUT(clamp01(t / 0.38)));
          const petalScale = lerp(0.92, 1, EASE_OUT(clamp01(t / 0.45)));

          for (let i = 0; i < count; i++) {
            const spec = LAUNCH_CORNER_PETALS[i];
            const startDelta = LAUNCH_ORIGIN_ANG - spec.ang;
            const frac = (i + 1) / count;
            const endT = 0.36 + 0.64 * frac;
            const u = clamp01(t / endT);
            deltasBuf[i] = lerp(startDelta, 0, EASE_OUT(u));
            scalesBuf[i] = petalScale;
          }

          applyFrame(fade, coreScale, 0, 0, 1, deltasBuf, scalesBuf);
          if (t < 1) {
            rafId = requestAnimationFrame(step);
          }
        };
        rafId = requestAnimationFrame(step);
      };

      const delay = enterDelayRef.current;
      if (delay > 0) {
        applyFrame(
          0,
          0.55,
          0,
          0,
          1,
          LAUNCH_CORNER_PETALS.map((p) => LAUNCH_ORIGIN_ANG - p.ang),
          LAUNCH_CORNER_PETALS.map(() => 0.92),
        );
        delayTimer = window.setTimeout(startEnter, delay);
      } else {
        startEnter();
      }
    } else {
      // 退场：原地保持逆时针方向旋转并缩小至消失（完整 12 瓣一并收缩）
      const fromFade = snapRef.current.fade;
      const fromCore = snapRef.current.coreScale;
      const fromHubRot = snapRef.current.hubRot;
      const fromHubScale = snapRef.current.hubScale;
      const fromDeltas = snapRef.current.deltas.slice();
      const fromScales = snapRef.current.scales.slice();

      const startTs = performance.now();
      const step = (now: number) => {
        if (cancelled) return;
        const t = clamp01((now - startTs) / LAUNCH_EXIT_DURATION_MS);
        const e = EASE_IN(t);
        const fade = fromFade * (1 - EASE_IN(clamp01((t - 0.45) / 0.55)));
        const hubRot = lerp(fromHubRot, LAUNCH_EXIT_ROT_DEG, e);
        const hubScale = lerp(fromHubScale, 0, e);

        applyFrame(fade, fromCore, 0, hubRot, hubScale, fromDeltas, fromScales);
        if (t < 1) {
          rafId = requestAnimationFrame(step);
        } else {
          safeToRemoveRef.current?.();
        }
      };
      rafId = requestAnimationFrame(step);
    }

    return () => {
      cancelled = true;
      if (rafId) cancelAnimationFrame(rafId);
      if (delayTimer !== null) window.clearTimeout(delayTimer);
    };
  }, [isPresent, reducedMotion]);

  return (
    <div className="backdrop-pane">
      <div className="backdrop-claude-wrap">
        <svg className="backdrop-claude-svg" viewBox="0 0 180 180" fill="none">
          <g ref={layerRef}>
            <g ref={hubRef} transform="translate(162 162)">
              {/* 中心圆与右侧紧邻两瓣（静止时完全藏于中心圆内，退场整体逆时针旋转时自然补齐右缘不露缺口） */}
              <g ref={coreRef}>
                <circle cx={0} cy={0} r={30.2} fill="currentColor" />
                {LAUNCH_EXTRA_EXIT_PETALS.map((petal) => (
                  <path key={petal.k} d={petal.d} fill="currentColor" />
                ))}
              </g>
              <g>
                {LAUNCH_CORNER_PETALS.map((petal, idx) => (
                  <g
                    key={petal.k}
                    ref={(el) => {
                      petalRefs.current[idx] = el;
                    }}
                  >
                    <path d={petal.d} fill="currentColor" />
                  </g>
                ))}
              </g>
            </g>
          </g>
        </svg>
      </div>
    </div>
  );
}

function hash2(c: number, r: number, seed = 0): number {
  let n =
    Math.imul(c + 37, 374761393) ^
    Math.imul(r + 17, 668265263) ^
    Math.imul(seed + 11, 2246822519);
  n = Math.imul(n ^ (n >>> 13), 1274126177);
  return ((n ^ (n >>> 16)) >>> 0) / 4294967295;
}

/* =========================================================================
 * 环境页（Profiles）：全下屏单侧陶土暖红 ASCII 字符山水画
 * - 入场 / 退场：四层景深平滑亚像素差速上浮 / 下沉 + destination-out 透明抠除防叠字
 * - 静态常驻（idle）：位移归零，流云、偶发风起水波、微漪与归雁严格按整数 ASCII 网格步进
 * ========================================================================= */
const SHANSHUI_ACCENT = "#d97757";
const SHANSHUI_OPACITY = 0.22;
const SHANSHUI_FONT_SIZE = 11;
const SHANSHUI_FONT = `500 ${SHANSHUI_FONT_SIZE}px "JetBrains Mono", "Cascadia Mono", Consolas, monospace`;
const SHANSHUI_ENTER_DURATION_MS = 760;
const SHANSHUI_EXIT_DURATION_MS = 320;
const SHANSHUI_IDLE_INTERVAL_MS = 66;

const SHANSHUI_RAMPS = {
  far: [":", ";", "-", ".", "·", " "],
  mid: ["%", "#", "=", "+", ":", "-", ".", "·", " "],
  near: ["@", "#", "%", "&", "=", "+", ":", "-", ".", " "],
} as const;

const SHANSHUI_CLOUD_SPRITES = {
  skyWide: [
    "       .-~~~-.       ",
    "   .-~(   ·   )~-.   ",
    "  ~   ~~~~~~~~~   ~  ",
  ],
  skyMid: [
    "     .-~~-.     ",
    "  -~(  ··  )~-  ",
    "    ~------~    ",
  ],
  skySmall: [
    "   .---.   ",
    " ~(  ·  )~ ",
  ],
  waistMist: [
    "  · ~ - ~~~ - ~ ·  ",
    "    ~  -----  ~    ",
  ],
  waistWisp: [
    " · - ~~~ - · ",
  ],
} as const;

interface ShanshuiCloud {
  sprite: readonly string[];
  u0: number;
  rowF: number;
  speed: number;
  depth: "back" | "mid";
  alpha: number;
}

interface ShanshuiMountainLayer {
  ridgeRows: Int16Array;
  cells: string[][];
}

interface ShanshuiScene {
  cols: number;
  rows: number;
  waterRow: number;
  layers: ShanshuiMountainLayer[];
  colPeakHeight: Int16Array;
  clouds: ShanshuiCloud[];
}

interface ShanshuiLayerSnap {
  yShift: number;
  fade: number;
  alpha: number;
}

interface ShanshuiSnapshot {
  m0: ShanshuiLayerSnap;
  m1: ShanshuiLayerSnap;
  m2: ShanshuiLayerSnap;
  cloud: { yShift: number; localFade: number; fade: number };
  water: { yShift: number; localFade: number; fade: number };
}

function shanshuiPeak(u: number, center: number, leftW: number, rightW: number, amp: number, sharp = 1.44): number {
  const d = u < center ? (center - u) / leftW : (u - center) / rightW;
  if (d > 3.4) return 0;
  return amp * Math.exp(-Math.pow(d, sharp) * 1.48);
}

function sampleOneSideRidge(layerIdx: number, u: number): number {
  if (layerIdx === 0) {
    let h = 0;
    h = Math.max(h, shanshuiPeak(u, 0.04, 0.08, 0.11, 0.56, 1.45));
    h = Math.max(h, shanshuiPeak(u, 0.16, 0.11, 0.14, 0.94, 1.36));
    h = Math.max(h, shanshuiPeak(u, 0.29, 0.09, 0.13, 0.68, 1.44));
    h = Math.max(h, shanshuiPeak(u, 0.42, 0.09, 0.11, 0.36, 1.56));
    return clamp01(h);
  }
  if (layerIdx === 1) {
    let h = 0;
    h = Math.max(h, shanshuiPeak(u, 0.07, 0.09, 0.12, 0.64, 1.4));
    h = Math.max(h, shanshuiPeak(u, 0.21, 0.08, 0.12, 0.58, 1.38));
    h = Math.max(h, shanshuiPeak(u, 0.34, 0.07, 0.09, 0.32, 1.52));
    return clamp01(h);
  }
  let h = 0;
  h = Math.max(h, shanshuiPeak(u, 0.03, 0.08, 0.12, 0.42, 1.42));
  h = Math.max(h, shanshuiPeak(u, 0.15, 0.07, 0.1, 0.29, 1.5));
  return clamp01(h);
}

function generateShanshuiScene(cols: number, rows: number): ShanshuiScene {
  const waterRow = Math.max(6, Math.floor(rows * 0.72));
  const skyTop = 1;
  const mtnSpan = Math.max(6, waterRow - skyTop);

  const layers: ShanshuiMountainLayer[] = [0, 1, 2].map((li) => {
    const ridgeFloat = new Float32Array(cols);
    const ridgeRows = new Int16Array(cols);
    const cells: string[][] = Array.from({ length: rows }, () => new Array<string>(cols).fill(""));

    for (let c = 0; c < cols; c++) {
      const u = c / Math.max(1, cols - 1);
      const h = sampleOneSideRidge(li, u);
      ridgeFloat[c] = h;
      ridgeRows[c] = h <= 0.045 ? waterRow + 2 : Math.round(waterRow - h * mtnSpan);
    }

    const ramp = li === 0 ? SHANSHUI_RAMPS.far : li === 1 ? SHANSHUI_RAMPS.mid : SHANSHUI_RAMPS.near;

    for (let c = 0; c < cols; c++) {
      const topR = ridgeRows[c];
      if (topR >= waterRow - 1) continue;
      const hPrev = ridgeFloat[Math.max(0, c - 1)];
      const hCur = ridgeFloat[c];
      const hNext = ridgeFloat[Math.min(cols - 1, c + 1)];
      const dh = hNext - hPrev;

      let crestCh: string;
      if (hCur >= hPrev && hCur >= hNext && (hCur - hPrev > 0.002 || hCur - hNext > 0.002)) {
        crestCh = "^";
      } else if (dh > 0.012) {
        crestCh = "/";
      } else if (dh < -0.012) {
        crestCh = "\\";
      } else {
        crestCh = li === 0 ? "-" : "_";
      }

      if (topR >= 0 && topR < waterRow - 1) {
        cells[topR][c] = crestCh;
      }

      const heightInRows = Math.max(1, waterRow - topR);
      for (let r = topR + 1; r < waterRow; r++) {
        const depth = r - topR;
        const depthNorm = depth / heightInRows;
        const distFromWater = waterRow - r;
        if (distFromWater <= 1 && li < 2) continue;
        const mistBand = distFromWater <= 2 ? (li === 0 ? 0.8 : li === 1 ? 0.56 : 0.24) : 0;

        const shadeBias = dh < 0 ? -0.14 : dh > 0 ? 0.12 : 0;
        const tex = (hash2(c, r, li * 19 + 7) - 0.5) * 0.18;
        const grad = depthNorm * 0.72 + (li === 0 ? 0.18 : 0.08) + shadeBias + tex + mistBand;

        if (depth === 1 && Math.abs(dh) > 0.025 && hash2(c, r, 71) > 0.35) {
          cells[r][c] = dh > 0 ? "/" : "\\";
        } else if (depth === 2 && li >= 1 && Math.abs(dh) > 0.035 && hash2(c, r, 97) > 0.58) {
          cells[r][c] = dh > 0 ? "/" : "\\";
        } else if (grad < 0.96) {
          const rIdx = Math.min(ramp.length - 1, Math.max(0, Math.floor(grad * ramp.length)));
          const ch = ramp[rIdx];
          cells[r][c] = ch === " " ? "" : ch;
        }
      }
    }

    return { ridgeRows, cells };
  });

  const colPeakHeight = new Int16Array(cols);
  for (let c = 0; c < cols; c++) {
    const minR = Math.min(layers[0].ridgeRows[c], layers[1].ridgeRows[c], layers[2].ridgeRows[c]);
    colPeakHeight[c] = Math.max(0, waterRow - minR);
  }

  const clouds: ShanshuiCloud[] = [
    { sprite: SHANSHUI_CLOUD_SPRITES.skyWide, u0: 0.56, rowF: 0.1, speed: 1.5, depth: "back", alpha: 0.5 },
    { sprite: SHANSHUI_CLOUD_SPRITES.skyMid, u0: 0.82, rowF: 0.28, speed: 2.1, depth: "back", alpha: 0.42 },
    { sprite: SHANSHUI_CLOUD_SPRITES.skySmall, u0: 0.38, rowF: 0.06, speed: 1.2, depth: "back", alpha: 0.38 },
    { sprite: SHANSHUI_CLOUD_SPRITES.waistMist, u0: 0.12, rowF: 0.56, speed: 2.6, depth: "mid", alpha: 0.58 },
    { sprite: SHANSHUI_CLOUD_SPRITES.waistWisp, u0: 0.3, rowF: 0.44, speed: 1.9, depth: "mid", alpha: 0.46 },
    { sprite: SHANSHUI_CLOUD_SPRITES.skySmall, u0: 0.68, rowF: 0.48, speed: 2.4, depth: "mid", alpha: 0.44 },
  ];

  return { cols, rows, waterRow, layers, colPeakHeight, clouds };
}

const WATER_GUSTS = [
  { period: 7.4, phase: 1.2, active: 3.4, uStart: 0.24, uEnd: 0.92, rowF: 0.34, halfW: 16 },
  { period: 10.6, phase: 5.4, active: 4.0, uStart: 0.4, uEnd: 0.98, rowF: 0.68, halfW: 20 },
  { period: 13.2, phase: 9.0, active: 3.8, uStart: 0.12, uEnd: 0.74, rowF: 0.52, halfW: 14 },
] as const;

const WATER_RIPPLE_SPOTS = [
  { u: 0.58, rowF: 0.44, period: 6.8, phase: 0.5 },
  { u: 0.78, rowF: 0.76, period: 8.6, phase: 3.9 },
  { u: 0.66, rowF: 0.24, period: 11.4, phase: 7.2 },
] as const;

const WATER_RIPPLE_FRAMES = [
  { str: "·", a: 0.58 },
  { str: "- ~ -", a: 0.84 },
  { str: "· - ~ - ·", a: 0.74 },
  { str: "·   -   ·", a: 0.44 },
] as const;

function buildWaterGrid(scene: ShanshuiScene, sec: number): { chars: string[][]; alphas: Float32Array[] } {
  const { cols, rows, waterRow, colPeakHeight } = scene;
  const wRows = Math.max(1, rows - waterRow);
  const chars: string[][] = Array.from({ length: wRows }, () => new Array<string>(cols).fill(""));
  const alphas: Float32Array[] = Array.from({ length: wRows }, () => new Float32Array(cols));

  for (let dr = 0; dr < wRows; dr++) {
    const depthRatio = dr / Math.max(1, wRows - 1);
    const calmShift = Math.floor(sec * (0.55 + depthRatio * 0.45));
    const baseA = dr === 0 ? 0.56 : lerp(0.52, 0.72, depthRatio);

    for (let c = 0; c < cols; c++) {
      const peakH = colPeakHeight[c];
      const reflDepth = Math.round(peakH * 0.4);

      if (dr === 0) {
        const sv = Math.sin((c - calmShift) * 0.22) + 0.45 * Math.cos(c * 0.11);
        let ch = "";
        if (peakH > 2 && sv > -0.1) ch = sv > 0.78 ? "~" : "-";
        else if (sv > 0.58) ch = "-";
        else if (sv > 0.34) ch = "·";
        chars[dr][c] = ch;
        alphas[dr][c] = ch ? baseA : 0;
        continue;
      }

      if (dr <= reflDepth) {
        const reflFade = 1 - dr / Math.max(1, reflDepth + 1);
        const rv = Math.sin((c - calmShift) * 0.24 + dr * 1.9);
        let ch = "";
        if (rv > 0.45) ch = reflFade > 0.58 ? "=" : "-";
        else if (rv > 0.05) ch = "-";
        else if (rv > -0.25 && hash2(c - calmShift, dr, 19) > 0.55) ch = "·";
        chars[dr][c] = ch;
        alphas[dr][c] = ch ? baseA * lerp(0.42, 0.85, reflFade) : 0;
      } else {
        const qv = Math.sin((c - calmShift) * 0.16 + dr * 2.4) + Math.cos(c * 0.09 - dr);
        if (qv > 1.48) {
          chars[dr][c] = "-";
          alphas[dr][c] = baseA * 0.46;
        } else if (qv > 1.24 && (c + dr) % 3 === 0) {
          chars[dr][c] = "·";
          alphas[dr][c] = baseA * 0.34;
        }
      }
    }
  }

  const waveStep = Math.floor(sec * 5.2);
  for (const g of WATER_GUSTS) {
    const localT = (((sec + g.phase) % g.period) + g.period) % g.period;
    if (localT >= g.active) continue;
    const p = localT / g.active;
    const env = Math.sin(p * Math.PI);
    if (env <= 0.06) continue;

    const frontCol = Math.floor(lerp(g.uStart * cols, g.uEnd * cols, p));
    const centerDr = Math.max(1, Math.min(wRows - 1, Math.round(g.rowF * (wRows - 1))));
    const halfW = Math.max(8, Math.round(g.halfW * (cols / 108)));

    for (let dRow = -1; dRow <= 1; dRow++) {
      const dr = centerDr + dRow;
      if (dr < 1 || dr >= wRows) continue;
      const rowAtten = dRow === 0 ? 1.0 : 0.58;

      for (let c = Math.max(0, frontCol - halfW); c <= Math.min(cols - 1, frontCol + halfW); c++) {
        const distNorm = Math.abs(c - frontCol) / halfW;
        const localEnv = env * (1 - distNorm * distNorm) * rowAtten;
        if (localEnv <= 0.14) continue;

        const crest = Math.sin((c - waveStep) * 0.68 + dRow * 1.6);
        let ch = "";
        let a = 0;
        if (localEnv > 0.56 && crest > 0.15) {
          ch = crest > 0.62 ? "~" : "=";
          a = 0.95 * localEnv;
        } else if (localEnv > 0.34 && crest > -0.25) {
          ch = crest > 0.2 ? "~" : "-";
          a = 0.82 * localEnv;
        } else if (crest > 0.15) {
          ch = "-";
          a = 0.62 * localEnv;
        } else if (crest > -0.2) {
          ch = "·";
          a = 0.45 * localEnv;
        }

        if (ch && a >= alphas[dr][c]) {
          chars[dr][c] = ch;
          alphas[dr][c] = a;
        }
      }
    }
  }

  for (const sp of WATER_RIPPLE_SPOTS) {
    const lt = (((sec + sp.phase) % sp.period) + sp.period) % sp.period;
    const dur = 1.9;
    if (lt >= dur) continue;
    const fi = Math.min(WATER_RIPPLE_FRAMES.length - 1, Math.floor((lt / dur) * WATER_RIPPLE_FRAMES.length));
    const frame = WATER_RIPPLE_FRAMES[fi];
    const dr = Math.max(1, Math.min(wRows - 1, Math.round(sp.rowF * (wRows - 1))));
    const midC = Math.floor(sp.u * cols);
    const startC = midC - Math.floor(frame.str.length / 2);

    for (let k = 0; k < frame.str.length; k++) {
      const ch = frame.str[k];
      const c = startC + k;
      if (ch === " " || c < 0 || c >= cols) continue;
      if (frame.a >= alphas[dr][c]) {
        chars[dr][c] = ch;
        alphas[dr][c] = frame.a;
      }
    }
  }

  return { chars, alphas };
}

function ProfilesBackdrop({ enterDelayMs, windowActive }: { enterDelayMs: number; windowActive: boolean }) {
  const [isPresent, safeToRemove] = usePresence();
  const reducedMotion = useReducedMotion();

  const wrapRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const enterDelayRef = useRef(enterDelayMs);
  enterDelayRef.current = enterDelayMs;
  const safeToRemoveRef = useRef(safeToRemove);
  safeToRemoveRef.current = safeToRemove;
  const activeRef = useRef(windowActive);
  activeRef.current = windowActive;
  const resumeRef = useRef<(() => void) | null>(null);
  const idleElapsedRef = useRef(0);
  const idleTickRef = useRef<number | null>(null);

  const layoutRef = useRef<{
    width: number;
    height: number;
    charW: number;
    lineH: number;
    scene: ShanshuiScene | null;
  }>({ width: 0, height: 0, charW: 6.6, lineH: 14, scene: null });

  const snapRef = useRef<ShanshuiSnapshot>({
    m0: { yShift: 0, fade: 0, alpha: 0 },
    m1: { yShift: 0, fade: 0, alpha: 0 },
    m2: { yShift: 0, fade: 0, alpha: 0 },
    cloud: { yShift: 0, localFade: 0, fade: 0 },
    water: { yShift: 0, localFade: 0, fade: 0 },
  });

  const drawStateRef = useRef<{ phase: "enter" | "exit"; t: number; exitFrom: ShanshuiSnapshot | null }>({
    phase: "enter",
    t: 0,
    exitFrom: null,
  });

  const renderCanvas = (phase: "enter" | "exit", t: number, idleSec: number, exitFrom: ShanshuiSnapshot | null) => {
    const canvas = canvasRef.current;
    const wrap = wrapRef.current;
    if (!canvas || !wrap) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const rect = wrap.getBoundingClientRect();
    const w = Math.max(320, Math.round(rect.width));
    const h = Math.max(160, Math.round(rect.height));
    const dpr = Math.min(window.devicePixelRatio || 1, 2);

    const lay = layoutRef.current;
    if (w !== lay.width || h !== lay.height || !lay.scene) {
      lay.width = w;
      lay.height = h;
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.font = SHANSHUI_FONT;
      lay.charW = ctx.measureText("M").width;
      lay.lineH = Math.round(SHANSHUI_FONT_SIZE * 1.24);
      const cols = Math.max(52, Math.floor(w / lay.charW));
      const rows = Math.max(14, Math.floor(h / lay.lineH));
      lay.scene = generateShanshuiScene(cols, rows);
    }

    const { charW, lineH, scene } = lay;
    if (!scene) return;
    const { cols, rows, waterRow, layers, clouds } = scene;
    const isEnter = phase === "enter";

    const globalFade = isEnter
      ? EASE_OUT(clamp01(t / 0.15))
      : 1 - EASE_IN(clamp01((t - 0.66) / 0.34));

    ctx.clearRect(0, 0, w, h);
    if (globalFade <= 0.002) {
      snapRef.current = {
        m0: { yShift: h * 0.34, fade: 0, alpha: 0 },
        m1: { yShift: h * 0.25, fade: 0, alpha: 0 },
        m2: { yShift: h * 0.16, fade: 0, alpha: 0 },
        cloud: { yShift: h * 0.15, localFade: 0, fade: 0 },
        water: { yShift: h * 0.13, localFade: 0, fade: 0 },
      };
      return;
    }

    const offsetX = Math.floor((w - cols * charW) * 0.5);
    const offsetY = Math.floor(h - rows * lineH);
    ctx.font = SHANSHUI_FONT;
    ctx.textBaseline = "top";

    const mSpecs = [
      { risePx: h * 0.34, eStart: 0.1, eDur: 0.9, xStart: 0.0, xDur: 0.74, baseA: 0.46 },
      { risePx: h * 0.25, eStart: 0.04, eDur: 0.78, xStart: 0.08, xDur: 0.78, baseA: 0.74 },
      { risePx: h * 0.16, eStart: 0.0, eDur: 0.64, xStart: 0.16, xDur: 0.84, baseA: 1.0 },
    ] as const;

    const evalMountain = (sp: (typeof mSpecs)[number], from?: ShanshuiLayerSnap): ShanshuiLayerSnap => {
      if (isEnter) {
        const u = EASE_OUT(clamp01((t - sp.eStart) / sp.eDur));
        const f = EASE_OUT(clamp01((t - sp.eStart) / 0.24));
        return { yShift: (1 - u) * sp.risePx, fade: f, alpha: sp.baseA * f * globalFade };
      }
      const ex = EASE_IN(clamp01((t - sp.xStart) / sp.xDur));
      const fDecay = 1 - EASE_IN(clamp01((t - sp.xStart - 0.28) / 0.52));
      const f = (from ? from.fade : 1) * fDecay;
      const startY = from ? from.yShift : 0;
      return { yShift: lerp(startY, sp.risePx, ex), fade: f, alpha: (from ? from.alpha : sp.baseA) * fDecay * globalFade };
    };

    const m0 = evalMountain(mSpecs[0], exitFrom?.m0);
    const m1 = evalMountain(mSpecs[1], exitFrom?.m1);
    const m2 = evalMountain(mSpecs[2], exitFrom?.m2);

    const cloudYShift = isEnter
      ? (1 - EASE_OUT(clamp01((t - 0.12) / 0.82))) * h * 0.15
      : lerp(exitFrom?.cloud.yShift ?? 0, h * 0.15, EASE_IN(clamp01(t / 0.72)));
    const cloudLocalFade = isEnter
      ? EASE_OUT(clamp01((t - 0.14) / 0.35))
      : (exitFrom?.cloud.localFade ?? 1) * (1 - EASE_IN(clamp01((t - 0.3) / 0.6)));
    const cloudFade = cloudLocalFade * globalFade;

    const waterYShift = isEnter
      ? (1 - EASE_OUT(clamp01((t - 0.02) / 0.56))) * h * 0.13
      : lerp(exitFrom?.water.yShift ?? 0, h * 0.13, EASE_IN(clamp01((t - 0.18) / 0.82)));
    const waterLocalFade = isEnter
      ? EASE_OUT(clamp01(t / 0.24))
      : (exitFrom?.water.localFade ?? 1) * (1 - EASE_IN(clamp01((t - 0.45) / 0.55)));
    const waterFade = waterLocalFade * globalFade;

    snapRef.current = {
      m0,
      m1,
      m2,
      cloud: { yShift: cloudYShift, localFade: cloudLocalFade, fade: cloudFade },
      water: { yShift: waterYShift, localFade: waterLocalFade, fade: waterFade },
    };

    const drawCloud = (cl: ShanshuiCloud, maskBehind: boolean) => {
      if (cloudFade <= 0.01) return;
      const sp = cl.sprite;
      const sw = sp[0].length;
      const span = cols + sw + 8;
      const baseCol = Math.floor(cl.u0 * cols);
      const stepCols = Math.floor(idleSec * cl.speed);
      const startCol = ((((baseCol + stepCols + sw) % span) + span) % span) - sw;
      const startRow = Math.round(cl.rowF * waterRow);

      for (let sr = 0; sr < sp.length; sr++) {
        const line = sp[sr];
        const r = startRow + sr;
        if (r < 0 || r >= waterRow) continue;
        const firstNS = line.search(/\S/);
        const lastNS = line.search(/\S\s*$/);
        if (firstNS === -1) continue;
        const py = offsetY + r * lineH + cloudYShift;

        if (maskBehind) {
          ctx.save();
          ctx.globalCompositeOperation = "destination-out";
          ctx.fillStyle = "#000";
          ctx.globalAlpha = clamp01(cloudLocalFade);
          ctx.fillRect(offsetX + (startCol + firstNS) * charW, py, (lastNS - firstNS + 1) * charW, lineH);
          ctx.restore();
        }

        ctx.fillStyle = SHANSHUI_ACCENT;
        ctx.globalAlpha = cl.alpha * cloudFade * SHANSHUI_OPACITY;
        for (let sc = firstNS; sc <= lastNS; sc++) {
          const ch = line[sc];
          if (ch === " ") continue;
          const c = startCol + sc;
          if (c < 0 || c >= cols) continue;
          ctx.fillText(ch, offsetX + c * charW, py);
        }
      }
    };

    const drawMountain = (li: number, st: ShanshuiLayerSnap) => {
      if (st.alpha <= 0.005) return;
      const L = layers[li];

      ctx.save();
      ctx.globalCompositeOperation = "destination-out";
      ctx.fillStyle = "#000";
      ctx.globalAlpha = clamp01(st.fade);
      for (let c = 0; c < cols; c++) {
        const topR = L.ridgeRows[c];
        if (topR >= waterRow) continue;
        ctx.fillRect(offsetX + c * charW, offsetY + topR * lineH + st.yShift, charW + 0.6, (waterRow - topR) * lineH + 2);
      }
      ctx.restore();

      ctx.fillStyle = SHANSHUI_ACCENT;
      ctx.globalAlpha = st.alpha * SHANSHUI_OPACITY;
      for (let r = 0; r < waterRow; r++) {
        const rowArr = L.cells[r];
        const py = offsetY + r * lineH + st.yShift;
        if (py > h + lineH || py < -lineH) continue;
        for (let c = 0; c < cols; c++) {
          const ch = rowArr[c];
          if (ch) ctx.fillText(ch, offsetX + c * charW, py);
        }
      }
    };

    for (const cl of clouds) {
      if (cl.depth === "back") drawCloud(cl, false);
    }
    drawMountain(0, m0);

    if (cols >= 64 && waterRow >= 9 && cloudFade > 0.05) {
      const birdBaseC = Math.floor(cols * 0.52);
      const birdStep = Math.floor(idleSec * 1.15);
      const spanB = cols + 16;
      const bc1 = ((((birdBaseC + birdStep + 8) % spanB) + spanB) % spanB) - 8;
      const br1 = Math.max(2, Math.floor(waterRow * 0.24));
      const flap = Math.floor(idleSec * 1.6) % 2 === 0;
      ctx.fillStyle = SHANSHUI_ACCENT;
      ctx.globalAlpha = 0.54 * cloudFade * SHANSHUI_OPACITY;
      if (bc1 >= 0 && bc1 < cols) {
        ctx.fillText(flap ? "v" : "~", offsetX + bc1 * charW, offsetY + br1 * lineH + cloudYShift);
      }
      const bc2 = bc1 + 4;
      if (bc2 >= 0 && bc2 < cols) {
        ctx.globalAlpha = 0.46 * cloudFade * SHANSHUI_OPACITY;
        ctx.fillText(flap ? "~" : "v", offsetX + bc2 * charW, offsetY + (br1 + 1) * lineH + cloudYShift);
      }
    }

    drawMountain(1, m1);
    for (const cl of clouds) {
      if (cl.depth === "mid") drawCloud(cl, true);
    }
    drawMountain(2, m2);

    if (waterFade > 0.01) {
      const waterTopY = offsetY + waterRow * lineH + waterYShift;
      ctx.save();
      ctx.globalCompositeOperation = "destination-out";
      ctx.fillStyle = "#000";
      ctx.globalAlpha = clamp01(waterLocalFade);
      ctx.fillRect(0, waterTopY, w, h - waterTopY + 40);
      ctx.restore();

      const { chars, alphas } = buildWaterGrid(scene, idleSec);
      ctx.fillStyle = SHANSHUI_ACCENT;
      for (let dr = 0; dr < chars.length; dr++) {
        const py = offsetY + (waterRow + dr) * lineH + waterYShift;
        if (py > h + lineH) continue;
        const rowChars = chars[dr];
        const rowAlphas = alphas[dr];
        for (let c = 0; c < cols; c++) {
          const ch = rowChars[c];
          if (!ch) continue;
          const a = rowAlphas[c] * waterFade * SHANSHUI_OPACITY;
          if (a <= 0.004) continue;
          ctx.globalAlpha = a;
          ctx.fillText(ch, offsetX + c * charW, py);
        }
      }
    }

    ctx.globalAlpha = 1;
  };

  useLayoutEffect(() => {
    if (reducedMotion) {
      drawStateRef.current = { phase: "enter", t: 1, exitFrom: null };
      renderCanvas("enter", 1, 0, null);
      return;
    }
    drawStateRef.current = { phase: "enter", t: 0, exitFrom: null };
    renderCanvas("enter", 0, 0, null);
  }, [reducedMotion]);

  useEffect(() => {
    const wrap = wrapRef.current;
    if (!wrap || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      layoutRef.current.scene = null;
      const ds = drawStateRef.current;
      renderCanvas(ds.phase, ds.t, reducedMotion ? 0 : idleElapsedRef.current, ds.exitFrom);
    });
    ro.observe(wrap);
    return () => ro.disconnect();
  }, [reducedMotion]);

  useEffect(() => {
    if (windowActive) {
      resumeRef.current?.();
    } else {
      idleTickRef.current = null;
    }
  }, [windowActive]);

  useEffect(() => {
    if (reducedMotion) {
      if (isPresent) {
        drawStateRef.current = { phase: "enter", t: 1, exitFrom: null };
        renderCanvas("enter", 1, 0, null);
      } else {
        safeToRemoveRef.current?.();
      }
      return;
    }

    let rafId = 0;
    let delayTimer: number | null = null;
    let cancelled = false;

    const advanceIdleSec = (now: number) => {
      if (idleTickRef.current !== null) {
        idleElapsedRef.current += Math.min(0.25, Math.max(0, (now - idleTickRef.current) / 1000));
      }
      idleTickRef.current = now;
      return idleElapsedRef.current;
    };

    if (isPresent) {
      const startEnter = () => {
        if (cancelled) return;
        const startTs = performance.now();
        let lastIdleDraw = 0;
        let settled = false;

        const schedule = () => {
          if (cancelled || rafId !== 0) return;
          rafId = requestAnimationFrame(step);
        };

        const step = (now: number) => {
          rafId = 0;
          if (cancelled) return;
          const idleSec = advanceIdleSec(now);
          const t = clamp01((now - startTs) / SHANSHUI_ENTER_DURATION_MS);

          if (t < 1) {
            drawStateRef.current = { phase: "enter", t, exitFrom: null };
            renderCanvas("enter", t, idleSec, null);
            lastIdleDraw = now;
            schedule();
          } else if (!settled) {
            settled = true;
            drawStateRef.current = { phase: "enter", t: 1, exitFrom: null };
            renderCanvas("enter", 1, idleSec, null);
            lastIdleDraw = now;
            if (activeRef.current) {
              schedule();
            } else {
              idleTickRef.current = null;
            }
          } else {
            if (!activeRef.current) {
              idleTickRef.current = null;
              return;
            }
            if (now - lastIdleDraw >= SHANSHUI_IDLE_INTERVAL_MS) {
              lastIdleDraw = now;
              renderCanvas("enter", 1, idleSec, null);
            }
            schedule();
          }
        };

        resumeRef.current = () => {
          if (cancelled) return;
          idleTickRef.current = performance.now();
          schedule();
        };
        schedule();
      };

      const delay = enterDelayRef.current;
      if (delay > 0) {
        drawStateRef.current = { phase: "enter", t: 0, exitFrom: null };
        renderCanvas("enter", 0, idleElapsedRef.current, null);
        delayTimer = window.setTimeout(startEnter, delay);
      } else {
        startEnter();
      }
    } else {
      resumeRef.current = null;
      const exitFrom: ShanshuiSnapshot = {
        m0: { ...snapRef.current.m0 },
        m1: { ...snapRef.current.m1 },
        m2: { ...snapRef.current.m2 },
        cloud: { ...snapRef.current.cloud },
        water: { ...snapRef.current.water },
      };
      const startTs = performance.now();
      const step = (now: number) => {
        if (cancelled) return;
        const idleSec = idleElapsedRef.current;
        const t = clamp01((now - startTs) / SHANSHUI_EXIT_DURATION_MS);
        drawStateRef.current = { phase: "exit", t, exitFrom };
        renderCanvas("exit", t, idleSec, exitFrom);
        if (t < 1) {
          rafId = requestAnimationFrame(step);
        } else {
          safeToRemoveRef.current?.();
        }
      };
      rafId = requestAnimationFrame(step);
    }

    return () => {
      cancelled = true;
      resumeRef.current = null;
      idleTickRef.current = null;
      if (rafId) cancelAnimationFrame(rafId);
      if (delayTimer !== null) window.clearTimeout(delayTimer);
    };
  }, [isPresent, reducedMotion]);

  return (
    <div className="backdrop-pane">
      <div className="backdrop-shanshui-wrap" ref={wrapRef}>
        <canvas className="backdrop-shanshui-canvas" ref={canvasRef} />
      </div>
    </div>
  );
}

/* =========================================================================
 * 设置页（Global）：右下角博朗实心圆齿级数啮合齿轮组 (18:8 = 2.25:1)
 * - 入场：大小轮按精确模数同频旋转并沿连心线靠拢咬合落位
 * - 常驻：持续轻缓 2.25:1 反向真啮合运转（窗口失焦或最小化自动暂停）
 * - 退场：同步啮合加速并绕各自轴心原地旋缩至消失
 * ========================================================================= */
const GEARS_ENTER_DURATION_MS = 680;
const GEARS_EXIT_DURATION_MS = 300;
const GEARS_IDLE_DEG_PER_SEC = -6.5;
const GEARS_DEG_TO_RAD = Math.PI / 180;

const BRAUN_GEAR_SPEC = {
  z1: 18,
  z2: 8,
  m: 10.4,
  c1x: 162,
  c1y: 164,
  meshAngDeg: -134,
  backlash: 0.24,
} as const;

function buildInvoluteGearPath(
  z: number,
  m: number,
  opts: { addendum: number; dedendum: number; rootDuty: number; tipDuty: number; roundTip: number },
): string {
  const { addendum, dedendum, rootDuty, tipDuty, roundTip } = opts;
  const rPitch = (m * z) / 2;
  const rOuter = rPitch + addendum * m;
  const rRoot = Math.max(m * 1.4, rPitch - dedendum * m);
  const step = (Math.PI * 2) / z;
  const pt = (r: number, a: number) => `${(r * Math.cos(a)).toFixed(2)} ${(r * Math.sin(a)).toFixed(2)}`;

  const rFlank = rRoot + (rOuter - rRoot) * 0.56;
  const flankDuty = rootDuty * 0.42 + tipDuty * 0.58;
  const tipChamferR = rOuter - (rOuter - rPitch) * roundTip;
  const tipCornerDuty = tipDuty * 0.72;

  const cmds: string[] = [];
  for (let i = 0; i < z; i++) {
    const aC = i * step;
    const aNextRoot = (i + 1) * step - step * rootDuty;
    const aRootL = aC - step * rootDuty;
    const aFlankL = aC - step * flankDuty;
    const aTipOuterL = aC - step * tipDuty;
    const aTipTopL = aC - step * tipCornerDuty;
    const aTipTopR = aC + step * tipCornerDuty;
    const aTipOuterR = aC + step * tipDuty;
    const aFlankR = aC + step * flankDuty;
    const aRootR = aC + step * rootDuty;

    if (i === 0) cmds.push(`M ${pt(rRoot, aRootL)}`);
    cmds.push(`Q ${pt(rFlank, aFlankL)} ${pt(tipChamferR, aTipOuterL)}`);
    cmds.push(`Q ${pt(rOuter, aTipTopL)} ${pt(rOuter, aC)}`);
    cmds.push(`Q ${pt(rOuter, aTipTopR)} ${pt(tipChamferR, aTipOuterR)}`);
    cmds.push(`Q ${pt(rFlank, aFlankR)} ${pt(rRoot, aRootR)}`);
    cmds.push(`A ${rRoot.toFixed(2)} ${rRoot.toFixed(2)} 0 0 1 ${pt(rRoot, aNextRoot)}`);
  }
  cmds.push("Z");
  return cmds.join(" ");
}

const BRAUN_GEAR_GEOM = (() => {
  const { z1, z2, m, c1x, c1y, meshAngDeg, backlash } = BRAUN_GEAR_SPEC;
  const r1 = (m * z1) / 2;
  const r2 = (m * z2) / 2;
  const dist = r1 + r2 + backlash * m;
  const rad = meshAngDeg * GEARS_DEG_TO_RAD;
  const ux = Math.cos(rad);
  const uy = Math.sin(rad);
  const c2x = c1x + dist * ux;
  const c2y = c1y + dist * uy;
  const base1 = meshAngDeg;
  const base2 = meshAngDeg + 180 + 180 / z2;
  const ratio = z1 / z2;
  const toothOpts = { addendum: 0.82, dedendum: 1.04, rootDuty: 0.3, tipDuty: 0.16, roundTip: 0.32 };
  const d1 = buildInvoluteGearPath(z1, m, toothOpts);
  const d2 = buildInvoluteGearPath(z2, m, toothOpts);
  return { c1x, c1y, c2x, c2y, ux, uy, base1, base2, ratio, d1, d2 };
})();

interface GearsSnapshot {
  fade: number;
  driveDeg: number;
  x1: number;
  y1: number;
  s1: number;
  x2: number;
  y2: number;
  s2: number;
}

function GlobalBackdrop({ enterDelayMs, windowActive }: { enterDelayMs: number; windowActive: boolean }) {
  const [isPresent, safeToRemove] = usePresence();
  const reducedMotion = useReducedMotion();
  const maskUid = useId().replace(/:/g, "");
  const mask1Id = `gear-braun-m1-${maskUid}`;
  const mask2Id = `gear-braun-m2-${maskUid}`;

  const layerRef = useRef<SVGGElement | null>(null);
  const g1WrapRef = useRef<SVGGElement | null>(null);
  const g1SpinRef = useRef<SVGGElement | null>(null);
  const g2WrapRef = useRef<SVGGElement | null>(null);
  const g2SpinRef = useRef<SVGGElement | null>(null);

  const enterDelayRef = useRef(enterDelayMs);
  enterDelayRef.current = enterDelayMs;
  const safeToRemoveRef = useRef(safeToRemove);
  safeToRemoveRef.current = safeToRemove;
  const activeRef = useRef(windowActive);
  activeRef.current = windowActive;
  const resumeRef = useRef<(() => void) | null>(null);
  const idleElapsedRef = useRef(0);
  const idleTickRef = useRef<number | null>(null);

  const snapRef = useRef<GearsSnapshot>({
    fade: 0,
    driveDeg: 36,
    x1: BRAUN_GEAR_GEOM.c1x + 18,
    y1: BRAUN_GEAR_GEOM.c1y + 18,
    s1: 0.84,
    x2: BRAUN_GEAR_GEOM.c2x + 14 + BRAUN_GEAR_GEOM.ux * 14,
    y2: BRAUN_GEAR_GEOM.c2y + 14 + BRAUN_GEAR_GEOM.uy * 14,
    s2: 0.76,
  });

  const applyFrame = (s: GearsSnapshot) => {
    snapRef.current = s;
    const { base1, base2, ratio } = BRAUN_GEAR_GEOM;
    if (layerRef.current) {
      layerRef.current.style.opacity = s.fade.toFixed(4);
    }
    if (g1WrapRef.current) {
      g1WrapRef.current.setAttribute("transform", `translate(${s.x1.toFixed(2)} ${s.y1.toFixed(2)}) scale(${s.s1.toFixed(4)})`);
    }
    if (g1SpinRef.current) {
      g1SpinRef.current.setAttribute("transform", `rotate(${(base1 + s.driveDeg).toFixed(2)})`);
    }
    if (g2WrapRef.current) {
      g2WrapRef.current.setAttribute("transform", `translate(${s.x2.toFixed(2)} ${s.y2.toFixed(2)}) scale(${s.s2.toFixed(4)})`);
    }
    if (g2SpinRef.current) {
      g2SpinRef.current.setAttribute("transform", `rotate(${(base2 - s.driveDeg * ratio).toFixed(2)})`);
    }
  };

  useLayoutEffect(() => {
    const { c1x, c1y, c2x, c2y, ux, uy } = BRAUN_GEAR_GEOM;
    if (reducedMotion) {
      applyFrame({ fade: 1, driveDeg: 0, x1: c1x, y1: c1y, s1: 1, x2: c2x, y2: c2y, s2: 1 });
      return;
    }
    applyFrame({
      fade: 0,
      driveDeg: 36,
      x1: c1x + 18,
      y1: c1y + 18,
      s1: 0.84,
      x2: c2x + 14 + ux * 14,
      y2: c2y + 14 + uy * 14,
      s2: 0.76,
    });
  }, [reducedMotion]);

  useEffect(() => {
    if (windowActive) {
      resumeRef.current?.();
    } else {
      idleTickRef.current = null;
    }
  }, [windowActive]);

  useEffect(() => {
    const { c1x, c1y, c2x, c2y, ux, uy } = BRAUN_GEAR_GEOM;

    if (reducedMotion) {
      if (isPresent) {
        applyFrame({ fade: 1, driveDeg: 0, x1: c1x, y1: c1y, s1: 1, x2: c2x, y2: c2y, s2: 1 });
      } else {
        safeToRemoveRef.current?.();
      }
      return;
    }

    let rafId = 0;
    let delayTimer: number | null = null;
    let cancelled = false;

    const advanceIdleSec = (now: number) => {
      if (idleTickRef.current !== null) {
        idleElapsedRef.current += Math.min(0.25, Math.max(0, (now - idleTickRef.current) / 1000));
      }
      idleTickRef.current = now;
      return idleElapsedRef.current;
    };

    if (isPresent) {
      const startEnter = () => {
        if (cancelled) return;
        const startTs = performance.now();
        let settled = false;

        const schedule = () => {
          if (cancelled || rafId !== 0) return;
          rafId = requestAnimationFrame(step);
        };

        const step = (now: number) => {
          rafId = 0;
          if (cancelled) return;
          const idleSec = advanceIdleSec(now);
          const idleDrive = idleSec * GEARS_IDLE_DEG_PER_SEC;
          const t = clamp01((now - startTs) / GEARS_ENTER_DURATION_MS);

          if (t < 1) {
            const fade = EASE_OUT(clamp01(t / 0.16));
            const u1 = EASE_OUT(clamp01(t / 0.88));
            const u2 = EASE_OUT(clamp01((t - 0.06) / 0.94));
            const driveDeg = lerp(36, 0, EASE_OUT(t)) + idleDrive;
            const gapSlide = (1 - u2) * 14;
            applyFrame({
              fade,
              driveDeg,
              x1: c1x + (1 - u1) * 18,
              y1: c1y + (1 - u1) * 18,
              s1: lerp(0.84, 1, u1),
              x2: c2x + (1 - u2) * 14 + ux * gapSlide,
              y2: c2y + (1 - u2) * 14 + uy * gapSlide,
              s2: lerp(0.76, 1, u2),
            });
            schedule();
          } else if (!settled) {
            settled = true;
            applyFrame({
              fade: 1,
              driveDeg: idleDrive,
              x1: c1x,
              y1: c1y,
              s1: 1,
              x2: c2x,
              y2: c2y,
              s2: 1,
            });
            if (activeRef.current) {
              schedule();
            } else {
              idleTickRef.current = null;
            }
          } else {
            if (!activeRef.current) {
              idleTickRef.current = null;
              return;
            }
            applyFrame({
              fade: 1,
              driveDeg: idleDrive,
              x1: c1x,
              y1: c1y,
              s1: 1,
              x2: c2x,
              y2: c2y,
              s2: 1,
            });
            schedule();
          }
        };

        resumeRef.current = () => {
          if (cancelled) return;
          idleTickRef.current = performance.now();
          schedule();
        };
        schedule();
      };

      const delay = enterDelayRef.current;
      if (delay > 0) {
        applyFrame({
          fade: 0,
          driveDeg: 36,
          x1: c1x + 18,
          y1: c1y + 18,
          s1: 0.84,
          x2: c2x + 14 + ux * 14,
          y2: c2y + 14 + uy * 14,
          s2: 0.76,
        });
        delayTimer = window.setTimeout(startEnter, delay);
      } else {
        startEnter();
      }
    } else {
      resumeRef.current = null;
      const from = { ...snapRef.current };
      const startTs = performance.now();
      const step = (now: number) => {
        if (cancelled) return;
        const t = clamp01((now - startTs) / GEARS_EXIT_DURATION_MS);
        const e = EASE_IN(t);
        const fade = from.fade * (1 - EASE_IN(clamp01((t - 0.42) / 0.58)));
        const driveDeg = from.driveDeg - e * 48;
        applyFrame({
          fade,
          driveDeg,
          x1: lerp(from.x1, c1x, e),
          y1: lerp(from.y1, c1y, e),
          s1: lerp(from.s1, 0, e),
          x2: lerp(from.x2, c2x, e),
          y2: lerp(from.y2, c2y, e),
          s2: lerp(from.s2, 0, EASE_IN(clamp01(t / 0.88))),
        });
        if (t < 1) {
          rafId = requestAnimationFrame(step);
        } else {
          safeToRemoveRef.current?.();
        }
      };
      rafId = requestAnimationFrame(step);
    }

    return () => {
      cancelled = true;
      resumeRef.current = null;
      idleTickRef.current = null;
      if (rafId) cancelAnimationFrame(rafId);
      if (delayTimer !== null) window.clearTimeout(delayTimer);
    };
  }, [isPresent, reducedMotion]);

  return (
    <div className="backdrop-pane">
      <div className="backdrop-gears-wrap">
        <svg className="backdrop-gears-svg" viewBox="0 0 196 196" fill="none">
          <defs>
            <mask id={mask1Id} maskUnits="userSpaceOnUse" x={-160} y={-160} width={320} height={320}>
              <rect x={-160} y={-160} width={320} height={320} fill="white" />
              <circle cx={0} cy={0} r={58} fill="none" stroke="black" strokeWidth={7} />
              <circle cx={0} cy={0} r={22} fill="black" />
              <rect x={28} y={-3.5} width={22} height={7} rx={3.5} fill="black" />
            </mask>
            <mask id={mask2Id} maskUnits="userSpaceOnUse" x={-100} y={-100} width={200} height={200}>
              <rect x={-100} y={-100} width={200} height={200} fill="white" />
              <circle cx={0} cy={0} r={12.5} fill="black" />
              <circle cx={22.5} cy={0} r={4.5} fill="black" />
            </mask>
          </defs>
          <g ref={layerRef}>
            <g ref={g1WrapRef}>
              <g ref={g1SpinRef}>
                <path d={BRAUN_GEAR_GEOM.d1} fill="currentColor" mask={`url(#${mask1Id})`} />
              </g>
            </g>
            <g ref={g2WrapRef}>
              <g ref={g2SpinRef}>
                <path d={BRAUN_GEAR_GEOM.d2} fill="currentColor" mask={`url(#${mask2Id})`} />
              </g>
            </g>
          </g>
        </svg>
      </div>
    </div>
  );
}

/* =========================================================================
 * 模型卡页（Cards）：右下角折扇牌组 (Fanned Card Deck)
 * - 入场（1000ms）：5 张圆角模型卡围绕右下支点逆时针等比扇开，动态镂空缝零叠色
 * - 常驻：入场即停，静止落位（零常驻帧开销）
 * - 退场：
 *   · 默认：合扇对齐至最左最后一张（-64°）+ 并行整体逆时针旋转退场（-42°）
 *   · 转至环境页（profiles）：顺时针反向合扇对齐至最右第一张（+3°）+ 并行整体顺时针旋转退场（+42°）
 * ========================================================================= */
const CARDS_ORIGIN_ANG = 15;
const CARDS_EXIT_ROT_DELTA = -42;
const CARDS_ENTER_DURATION_MS = 1000;
const CARDS_EXIT_DURATION_MS = 320;

const DECK_CARD_RECT = { x: -72, y: -138, width: 84, height: 124, rx: 13 } as const;

const DECK_CARDS = [
  { ang: -64, order: 4 },
  { ang: -47, order: 3 },
  { ang: -30, order: 2 },
  { ang: -14, order: 1 },
  { ang: 3, order: 0 },
] as const;

interface CardsSnapshot {
  fade: number;
  hubScale: number;
  rots: number[];
  scales: number[];
}

function CardsBackdrop({
  enterDelayMs,
  targetViewRef,
}: {
  enterDelayMs: number;
  targetViewRef: RefObject<BackdropView>;
}) {
  const [isPresent, safeToRemove] = usePresence();
  const reducedMotion = useReducedMotion();
  const maskUid = useId().replace(/:/g, "");
  const faceMaskId = `deck-face-${maskUid}`;

  const layerRef = useRef<SVGGElement | null>(null);
  const hubRef = useRef<SVGGElement | null>(null);
  const cardRefs = useRef<(SVGGElement | null)[]>([]);
  const maskCutRefs = useRef<Map<number, (SVGGElement | null)[]>>(new Map());

  const enterDelayRef = useRef(enterDelayMs);
  enterDelayRef.current = enterDelayMs;
  const safeToRemoveRef = useRef(safeToRemove);
  safeToRemoveRef.current = safeToRemove;

  const snapRef = useRef<CardsSnapshot>({
    fade: 0,
    hubScale: 1,
    rots: DECK_CARDS.map(() => CARDS_ORIGIN_ANG),
    scales: DECK_CARDS.map(() => 0.9),
  });

  const applyFrame = (fade: number, hubScale: number, rots: number[], scales: number[]) => {
    const snap = snapRef.current;
    snap.fade = fade;
    snap.hubScale = hubScale;

    if (layerRef.current) {
      layerRef.current.style.opacity = fade.toFixed(4);
    }
    if (hubRef.current) {
      hubRef.current.setAttribute(
        "transform",
        hubScale === 1 ? "translate(166 178)" : `translate(166 178) scale(${hubScale.toFixed(4)})`,
      );
    }
    for (let i = 0; i < DECK_CARDS.length; i++) {
      snap.rots[i] = rots[i];
      snap.scales[i] = scales[i];
      const xf = `rotate(${rots[i].toFixed(2)}) scale(${scales[i].toFixed(4)})`;
      const g = cardRefs.current[i];
      if (g) g.setAttribute("transform", xf);
      const cuts = maskCutRefs.current.get(i);
      if (cuts) {
        for (const cut of cuts) {
          if (cut) cut.setAttribute("transform", xf);
        }
      }
    }
  };

  useLayoutEffect(() => {
    if (reducedMotion) {
      applyFrame(
        1,
        1,
        DECK_CARDS.map((c) => c.ang),
        DECK_CARDS.map(() => 1),
      );
      return;
    }
    applyFrame(
      0,
      1,
      DECK_CARDS.map(() => CARDS_ORIGIN_ANG),
      DECK_CARDS.map(() => 0.9),
    );
  }, [reducedMotion]);

  useEffect(() => {
    const count = DECK_CARDS.length;

    if (reducedMotion) {
      if (isPresent) {
        applyFrame(
          1,
          1,
          DECK_CARDS.map((c) => c.ang),
          DECK_CARDS.map(() => 1),
        );
      } else {
        safeToRemoveRef.current?.();
      }
      return;
    }

    let rafId = 0;
    let delayTimer: number | null = null;
    let cancelled = false;
    const rotsBuf = new Array<number>(count);
    const scalesBuf = new Array<number>(count);

    if (isPresent) {
      const startEnter = () => {
        if (cancelled) return;
        const startTs = performance.now();
        const step = (now: number) => {
          if (cancelled) return;
          const t = clamp01((now - startTs) / CARDS_ENTER_DURATION_MS);
          const fade = EASE_OUT(clamp01(t / 0.16));
          const cardScale = lerp(0.9, 1, EASE_OUT(clamp01(t / 0.46)));

          for (let i = 0; i < count; i++) {
            const c = DECK_CARDS[i];
            const u = clamp01(t / (0.38 + 0.62 * ((c.order + 1) / count)));
            rotsBuf[i] = lerp(CARDS_ORIGIN_ANG, c.ang, EASE_OUT(u));
            scalesBuf[i] = cardScale;
          }

          applyFrame(fade, 1, rotsBuf, scalesBuf);
          if (t < 1) {
            rafId = requestAnimationFrame(step);
          }
        };
        rafId = requestAnimationFrame(step);
      };

      const delay = enterDelayRef.current;
      if (delay > 0) {
        applyFrame(
          0,
          1,
          DECK_CARDS.map(() => CARDS_ORIGIN_ANG),
          DECK_CARDS.map(() => 0.9),
        );
        delayTimer = window.setTimeout(startEnter, delay);
      } else {
        startEnter();
      }
    } else {
      // 退场并行执行两路运动：
      // - 默认：向最左最后一张（-64°）逆时针合扇对齐 + 整体逆时针旋转退场（-42°）
      // - 切至环境页（profiles）：顺时针反向合扇对齐至最右一张（+3°）+ 整体顺时针旋转退场（+42°），避开左侧山水
      const reverseToProfiles = targetViewRef.current === "profiles";
      const fromFade = snapRef.current.fade;
      const fromHubScale = snapRef.current.hubScale;
      const fromRots = snapRef.current.rots.slice();
      const fromScales = snapRef.current.scales.slice();
      const anchorAng = reverseToProfiles ? fromRots[count - 1] : fromRots[0];
      const rotDelta = reverseToProfiles ? -CARDS_EXIT_ROT_DELTA : CARDS_EXIT_ROT_DELTA;

      const startTs = performance.now();
      const step = (now: number) => {
        if (cancelled) return;
        const t = clamp01((now - startTs) / CARDS_EXIT_DURATION_MS);
        const eAll = EASE_IN(t);
        const exitRot = lerp(0, rotDelta, eAll);
        const foldT = clamp01(t / 0.82);
        const fade = fromFade * (1 - EASE_IN(clamp01((t - 0.56) / 0.44)));
        const hubScale = lerp(fromHubScale, 0.78, eAll);

        for (let i = 0; i < count; i++) {
          const waveOrder = reverseToProfiles ? i : DECK_CARDS[i].order;
          const delay = (waveOrder / count) * 0.18;
          const u = clamp01((foldT - delay) / (1 - delay));
          const foldedAng = lerp(fromRots[i], anchorAng, EASE_IN(u));
          rotsBuf[i] = foldedAng + exitRot;
          scalesBuf[i] = lerp(fromScales[i], 0.94, eAll);
        }

        applyFrame(fade, hubScale, rotsBuf, scalesBuf);
        if (t < 1) {
          rafId = requestAnimationFrame(step);
        } else {
          safeToRemoveRef.current?.();
        }
      };
      rafId = requestAnimationFrame(step);
    }

    return () => {
      cancelled = true;
      if (rafId) cancelAnimationFrame(rafId);
      if (delayTimer !== null) window.clearTimeout(delayTimer);
    };
  }, [isPresent, reducedMotion, targetViewRef]);

  const setMaskCutRef = (cardIdx: number, slotKey: number) => (el: SVGGElement | null) => {
    let arr = maskCutRefs.current.get(cardIdx);
    if (!arr) {
      arr = [];
      maskCutRefs.current.set(cardIdx, arr);
    }
    arr[slotKey] = el;
  };

  return (
    <div className="backdrop-pane">
      <div className="backdrop-cards-wrap">
        <svg className="backdrop-cards-svg" viewBox="0 0 196 196" fill="none">
          <defs>
            {DECK_CARDS.slice(0, -1).map((_, i) => (
              <mask
                key={i}
                id={`deck-m-${i}-${maskUid}`}
                maskUnits="userSpaceOnUse"
                x={-240}
                y={-240}
                width={480}
                height={480}
              >
                <rect x={-240} y={-240} width={480} height={480} fill="white" />
                {DECK_CARDS.slice(i + 1).map((__, off) => {
                  const j = i + 1 + off;
                  return (
                    <g key={j} ref={setMaskCutRef(j, i)}>
                      <rect
                        x={DECK_CARD_RECT.x}
                        y={DECK_CARD_RECT.y}
                        width={DECK_CARD_RECT.width}
                        height={DECK_CARD_RECT.height}
                        rx={DECK_CARD_RECT.rx}
                        fill="black"
                        stroke="black"
                        strokeWidth={8.5}
                        strokeLinejoin="round"
                      />
                    </g>
                  );
                })}
              </mask>
            ))}
            <mask id={faceMaskId} maskUnits="userSpaceOnUse" x={-100} y={-160} width={160} height={180}>
              <rect x={-100} y={-160} width={160} height={180} fill="white" />
              <rect x={-56} y={-120} width={38} height={8.5} rx={4.25} fill="black" />
              <rect x={-56} y={-104} width={24} height={5.5} rx={2.75} fill="black" />
              <rect x={-28} y={-104} width={22} height={5.5} rx={2.75} fill="black" />
              <rect x={-56} y={-44} width={52} height={4.5} rx={2.25} fill="black" />
              <circle cx={-50} cy={-30} r={5.2} fill="black" />
            </mask>
          </defs>
          <g ref={layerRef}>
            <g ref={hubRef} transform="translate(166 178)">
              {DECK_CARDS.map((_, i) => {
                const isFront = i === DECK_CARDS.length - 1;
                return (
                  <g key={i} mask={isFront ? undefined : `url(#deck-m-${i}-${maskUid})`}>
                    <g
                      ref={(el) => {
                        cardRefs.current[i] = el;
                      }}
                    >
                      <rect
                        x={DECK_CARD_RECT.x}
                        y={DECK_CARD_RECT.y}
                        width={DECK_CARD_RECT.width}
                        height={DECK_CARD_RECT.height}
                        rx={DECK_CARD_RECT.rx}
                        fill="currentColor"
                        mask={isFront ? `url(#${faceMaskId})` : undefined}
                      />
                    </g>
                  </g>
                );
              })}
            </g>
          </g>
        </svg>
      </div>
    </div>
  );
}

function PageBackdrop({ view }: { view: BackdropView }) {
  const windowActive = useWindowActive();
  const prevViewRef = useRef<BackdropView>(view);
  const targetViewRef = useRef<BackdropView>(view);
  targetViewRef.current = view;
  const enterDelayMs = prevViewRef.current === view ? 0 : TAB_SWITCH_ENTER_DELAY_MS;

  useEffect(() => {
    prevViewRef.current = view;
  }, [view]);

  return (
    <div className="stage-backdrop" aria-hidden="true">
      <AnimatePresence>
        {view === "launch" && <LaunchBackdrop key="launch" enterDelayMs={enterDelayMs} />}
        {view === "profiles" && (
          <ProfilesBackdrop key="profiles" enterDelayMs={enterDelayMs} windowActive={windowActive} />
        )}
        {view === "cards" && (
          <CardsBackdrop key="cards" enterDelayMs={enterDelayMs} targetViewRef={targetViewRef} />
        )}
        {view === "global" && (
          <GlobalBackdrop key="global" enterDelayMs={enterDelayMs} windowActive={windowActive} />
        )}
      </AnimatePresence>
    </div>
  );
}

export default memo(PageBackdrop);
