import type { ITheme } from "@xterm/xterm";

export const FONT_FAMILY =
  '"JetBrains Mono", "BIZ UDGothic", "Noto Sans Mono CJK JP", "Cascadia Mono", Consolas, "DejaVu Sans Mono", monospace';

export const FONT_SIZE = 14;

/** 暗い背景に暖色のアクセント (ember)。 */
export const THEME: ITheme = {
  background: "#1a1714",
  foreground: "#e6dfd5",
  cursor: "#ff8a3d",
  cursorAccent: "#1a1714",
  selectionBackground: "rgba(255, 138, 61, 0.32)",
  black: "#2a2521",
  red: "#e5645a",
  green: "#9fbf6b",
  yellow: "#e2b55a",
  blue: "#6f9fd8",
  magenta: "#c58bc9",
  cyan: "#6fbfb4",
  white: "#cfc6ba",
  brightBlack: "#6f655b",
  brightRed: "#f2857c",
  brightGreen: "#b9d68a",
  brightYellow: "#f0cc7e",
  brightBlue: "#93b9e8",
  brightMagenta: "#d9a8dc",
  brightCyan: "#93d6cc",
  brightWhite: "#f5efe6",
};
