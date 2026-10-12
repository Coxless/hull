import type { ITheme } from "@xterm/xterm";

export const FONT_FAMILY =
  '"JetBrains Mono", "BIZ UDGothic", "Noto Sans Mono CJK JP", "Cascadia Mono", Consolas, "DejaVu Sans Mono", monospace';

export const FONT_SIZE = 13;

/** 画面の地の色 (生の表示の背景)。 */
export const BACKGROUND = "#141316";
/** ブロックのカードの色 (実行中のブロックの背景)。 */
export const PANEL = "#1d1c21";

/** 暗い背景に琥珀色のアクセント。背景だけ、置く場所に合わせて変える。 */
export function terminalTheme(background: string): ITheme {
  return {
    background,
    foreground: "#e7e2d9",
    cursor: "#e6b562",
    cursorAccent: background,
    selectionBackground: "rgba(230, 181, 98, 0.30)",
    black: "#2a2830",
    red: "#f2946d",
    green: "#7cc4a4",
    yellow: "#e6b562",
    blue: "#8ab8ec",
    magenta: "#c79ad1",
    cyan: "#7fc7c9",
    white: "#c9c3b8",
    brightBlack: "#8f8a82",
    brightRed: "#f7b094",
    brightGreen: "#9ad9bd",
    brightYellow: "#f3cd86",
    brightBlue: "#a9cdf3",
    brightMagenta: "#dab7e2",
    brightCyan: "#a3dcdd",
    brightWhite: "#f3eee5",
  };
}
