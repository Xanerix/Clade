import type { SVGProps } from "react";

export type IconProps = SVGProps<SVGSVGElement>;

/**
 * 统一采用 Anthropic / Claude 风格的 20×20 光学网格细描边图标（默认 1.5px 描边），
 * 匹配编辑风无衬线/衬线排版与暖纸张色板。
 */
const BASE_PROPS: SVGProps<SVGSVGElement> = {
  viewBox: "0 0 20 20",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.5,
  strokeLinecap: "round",
  strokeLinejoin: "round",
  "aria-hidden": true,
};

export function PlusIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <path d="M10 3.75v12.5" />
      <path d="M3.75 10h12.5" />
    </svg>
  );
}

export function CloseIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <path d="M5.25 5.25 14.75 14.75" />
      <path d="M14.75 5.25 5.25 14.75" />
    </svg>
  );
}

export function ArrowUpRightIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <path d="M5.25 14.75 14.75 5.25" />
      <path d="M7.5 5.25h7.25v7.25" />
    </svg>
  );
}

export function ArrowRightIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} strokeWidth={1.4} {...props}>
      <path d="M3.25 10h13.5" />
      <path d="M12.25 5.5 16.75 10l-4.5 4.5" />
    </svg>
  );
}

export function ArrowLeftRightIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} strokeWidth={1.45} {...props}>
      <path d="M3.5 10h13" />
      <path d="M6.75 6.75 3.5 10l3.25 3.25" />
      <path d="M13.25 6.75 16.5 10l-3.25 3.25" />
    </svg>
  );
}

export function CornerDownLeftIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} strokeWidth={1.6} {...props}>
      <path d="M15.75 4.75v4.75a3 3 0 0 1-3 3H4.5" />
      <path d="M8 9 4.5 12.5 8 16" />
    </svg>
  );
}

/** 聚合网关分支拓扑（横向 1→N，与直连 ArrowRightIcon 成对） */
export function GatewayForkIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} strokeWidth={1.4} {...props}>
      <path d="M3.25 10h3.5" />
      <path d="M6.75 10c2.75 0 2.75-4.5 5.5-4.5h4.5" />
      <path d="M6.75 10c2.75 0 2.75 4.5 5.5 4.5h4.5" />
      <path d="M14.25 3 16.75 5.5 14.25 8" />
      <path d="M14.25 12 16.75 14.5 14.25 17" />
    </svg>
  );
}

export function CheckIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} strokeWidth={2.25} {...props}>
      <path d="M4.25 10.5 8.25 14.5 15.75 6" />
    </svg>
  );
}

export function MinusIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} strokeWidth={2.25} {...props}>
      <path d="M4.5 10h11" />
    </svg>
  );
}

export function SearchIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <circle cx="9" cy="9" r="5.5" />
      <path d="M12.9 12.9 16.75 16.75" />
    </svg>
  );
}

export function AlertTriangleIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <path d="M8.84 3.98a1.34 1.34 0 0 1 2.32 0l5.64 10.05a1.34 1.34 0 0 1-1.16 2H4.36a1.34 1.34 0 0 1-1.16-2Z" />
      <path d="M10 7.75v3.5" />
      <circle cx="10" cy="13.75" r="0.7" fill="currentColor" stroke="none" />
    </svg>
  );
}

export function FolderIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <path d="M3.25 15.25a1.5 1.5 0 0 0 1.5 1.5h10.5a1.5 1.5 0 0 0 1.5-1.5V7.75a1.5 1.5 0 0 0-1.5-1.5h-5.15a1.25 1.25 0 0 1-1-.5l-.7-.93a1.25 1.25 0 0 0-1-.5H4.75a1.5 1.5 0 0 0-1.5 1.5Z" />
    </svg>
  );
}

export function SunIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <circle cx="10" cy="10" r="3.25" />
      <path d="M10 2.75v1.75M10 15.5v1.75M4.87 4.87l1.24 1.24M13.89 13.89l1.24 1.24M2.75 10h1.75M15.5 10h1.75M4.87 15.13l1.24-1.24M13.89 6.11l1.24-1.24" />
    </svg>
  );
}

export function MoonIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <path d="M16.25 11.35A6.4 6.4 0 0 1 8.65 3.75a6.4 6.4 0 1 0 7.6 7.6Z" />
    </svg>
  );
}

export function SlidersIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <path d="M4.75 16.75v-4.75M4.75 9.25V3.25M10 16.75V9.5M10 6.75V3.25M15.25 16.75v-3.5M15.25 10.5V3.25" />
      <path d="M3 12h3.5M8.25 6.75h3.5M13.5 13.25h3.5" />
    </svg>
  );
}

export function LayersIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <path d="M10 3.25 3 6.75 10 10.25 17 6.75 10 3.25Z" />
      <path d="M3 10.25 10 13.75 17 10.25" />
      <path d="M3 13.5 10 17 17 13.5" />
    </svg>
  );
}

export function MessageIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <path d="M16.75 12a1.75 1.75 0 0 1-1.75 1.75H7l-3.75 3V5A1.75 1.75 0 0 1 5 3.25h10A1.75 1.75 0 0 1 16.75 5Z" />
    </svg>
  );
}

export function GlobeIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <circle cx="10" cy="10" r="6.75" />
      <path d="M3.25 10h13.5" />
      <path d="M10 3.25a10 10 0 0 1 2.75 6.75A10 10 0 0 1 10 16.75 10 10 0 0 1 7.25 10 10 10 0 0 1 10 3.25Z" />
    </svg>
  );
}

export function ShieldIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <path d="M10 17s5.75-2.75 5.75-7V5.25L10 3.25 4.25 5.25V10c0 4.25 5.75 7 5.75 7Z" />
    </svg>
  );
}

export function WrenchIcon(props: IconProps) {
  return (
    <svg {...BASE_PROPS} {...props}>
      <path d="M12.35 5.25a.8.8 0 0 0 0 1.15l1.25 1.25a.8.8 0 0 0 1.15 0l2.45-2.45a4.4 4.4 0 0 1-5.85 5.85l-5.3 5.3a1.65 1.65 0 0 1-2.35-2.35l5.3-5.3a4.4 4.4 0 0 1 5.85-5.85l-2.5 2.4Z" />
    </svg>
  );
}
