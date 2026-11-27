import { memo, useEffect, useLayoutEffect, useRef } from "react";
import { AnimatePresence, usePresence, useReducedMotion } from "motion/react";

export type BackdropView = "launch" | "profiles" | "cards" | "global";

interface PetalSpec {
  k: number;
  ang: number;
  d: string;
}

/**
 * 精确分解自官方 Claude Logo (logos/claude-icon.svg, viewBox 256×257)：
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
const SHANSHUI_ORIGIN_TS = typeof performance !== "undefined" ? performance.now() : 0;

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

function ProfilesBackdrop({ enterDelayMs }: { enterDelayMs: number }) {
  const [isPresent, safeToRemove] = usePresence();
  const reducedMotion = useReducedMotion();

  const wrapRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const enterDelayRef = useRef(enterDelayMs);
  enterDelayRef.current = enterDelayMs;
  const safeToRemoveRef = useRef(safeToRemove);
  safeToRemoveRef.current = safeToRemove;

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
      const idleSec = reducedMotion ? 0 : (performance.now() - SHANSHUI_ORIGIN_TS) / 1000;
      renderCanvas(ds.phase, ds.t, idleSec, ds.exitFrom);
    });
    ro.observe(wrap);
    return () => ro.disconnect();
  }, [reducedMotion]);

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

    if (isPresent) {
      const startEnter = () => {
        if (cancelled) return;
        const startTs = performance.now();
        let lastIdleDraw = 0;
        let settled = false;

        const step = (now: number) => {
          if (cancelled) return;
          const idleSec = (now - SHANSHUI_ORIGIN_TS) / 1000;
          const t = clamp01((now - startTs) / SHANSHUI_ENTER_DURATION_MS);

          if (t < 1) {
            drawStateRef.current = { phase: "enter", t, exitFrom: null };
            renderCanvas("enter", t, idleSec, null);
            lastIdleDraw = now;
          } else if (!settled) {
            settled = true;
            drawStateRef.current = { phase: "enter", t: 1, exitFrom: null };
            renderCanvas("enter", 1, idleSec, null);
            lastIdleDraw = now;
          } else if (!document.hidden && now - lastIdleDraw >= SHANSHUI_IDLE_INTERVAL_MS) {
            lastIdleDraw = now;
            renderCanvas("enter", 1, idleSec, null);
          }
          rafId = requestAnimationFrame(step);
        };
        rafId = requestAnimationFrame(step);
      };

      const delay = enterDelayRef.current;
      if (delay > 0) {
        drawStateRef.current = { phase: "enter", t: 0, exitFrom: null };
        renderCanvas("enter", 0, (performance.now() - SHANSHUI_ORIGIN_TS) / 1000, null);
        delayTimer = window.setTimeout(startEnter, delay);
      } else {
        startEnter();
      }
    } else {
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
        const idleSec = (now - SHANSHUI_ORIGIN_TS) / 1000;
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

function PageBackdrop({ view }: { view: BackdropView }) {
  const prevViewRef = useRef<BackdropView>(view);
  const enterDelayMs = prevViewRef.current === view ? 0 : TAB_SWITCH_ENTER_DELAY_MS;

  useEffect(() => {
    prevViewRef.current = view;
  }, [view]);

  return (
    <div className="stage-backdrop" aria-hidden="true">
      <AnimatePresence>
        {view === "launch" && <LaunchBackdrop key="launch" enterDelayMs={enterDelayMs} />}
        {view === "profiles" && <ProfilesBackdrop key="profiles" enterDelayMs={enterDelayMs} />}
      </AnimatePresence>
    </div>
  );
}

export default memo(PageBackdrop);
