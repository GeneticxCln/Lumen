import type { CSSProperties } from "react";

export type IconName = "play" | "pause" | "search" | "plus" | "check" | "close" | "chevron" | "info" | "settings" | "volume" | "mute" | "stop" | "folder" | "grid" | "arrow" | "refresh" | "compass" | "film" | "series" | "tv" | "palette" | "keyboard" | "bookmark" | "sun" | "moon" | "monitor" | "profile";
const paths: Record<IconName, string> = {
  profile: "M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8ZM4 21a8 8 0 0 1 16 0",
  compass: "M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0Zm-7-3-2 4-4 2 2-4 4-2Z",
  film: "M4 3h16v18H4V3ZM4 8h16M4 16h16M8 3v18M16 3v18",
  series: "M5 3h14M3 7h18v14H3V7Zm5 4 7 3-7 3v-6Z",
  tv: "M3 7h18v14H3V7Zm5-5 4 5 4-5M7 17h.01",
  palette: "M12 3a9 9 0 1 0 0 18h1a2 2 0 0 0 0-4h-1a2 2 0 0 1 0-4h5a4 4 0 0 0 4-4c0-4-5-6-9-6ZM7 8h.01M11 6h.01M16 7h.01M6 13h.01",
  keyboard: "M2 5h20v14H2V5ZM6 9h.01M10 9h.01M14 9h.01M18 9h.01M6 13h.01M10 13h.01M14 13h.01M18 13h.01M7 16h10",
  bookmark: "M6 3h12v18l-6-4-6 4V3Z",
  sun: "M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0ZM12 2v2M12 20v2M2 12h2M20 12h2M5 5l1 1M18 18l1 1M5 19l1-1M18 6l1-1",
  moon: "M20 15A9 9 0 0 1 9 4a9 9 0 1 0 11 11Z",
  monitor: "M2 3h20v14H2V3ZM12 17v4M7 21h10",
  play: "m8 5 11 7-11 7V5Z",
  pause: "M8 5v14M16 5v14",
  search: "M21 21l-5-5M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0Z",
  plus: "M12 5v14M5 12h14",
  check: "m5 12 4 4L19 6",
  close: "m6 6 12 12M6 18 18 6",
  chevron: "m9 5 7 7-7 7",
  info: "M12 11v6M12 7h.01M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0Z",
  settings: "m9 3-1 3-3 1-2 5 2 5 3 1 1 3h6l1-3 3-1 2-5-2-5-3-1-1-3H9ZM16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z",
  volume: "M11 4 5 9H2v6h3l6 5V4ZM15 8a6 6 0 0 1 0 8M18 5a10 10 0 0 1 0 14",
  mute: "M11 4 5 9H2v6h3l6 5V4ZM16 9l6 6M16 15l6-6",
  stop: "M5 5h14v14H5V5Z",
  folder: "M3 7V4h6l3 3h9v13H3V7Z",
  grid: "M3 3h7v7H3V3ZM14 3h7v7h-7V3ZM3 14h7v7H3v-7ZM14 14h7v7h-7v-7Z",
  arrow: "M19 12H5m7-7-7 7 7 7",
  refresh: "M20 7v5h-5M4 17v-5h5M5 7a8 8 0 0 1 13-2l2 7M4 12l2 7a8 8 0 0 0 13-2",
};
export function Icon({ name, size = 20, style }: { name: IconName; size?: number; style?: CSSProperties }) {
  return <svg width={size} height={size} viewBox="0 0 24 24" fill={name === "play" ? "currentColor" : "none"} stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={style}><path d={paths[name]} /></svg>;
}
