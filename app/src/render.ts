// 端末のバッファを、色付きの静的な HTML にする。
//
// 終了したブロックは xterm.js を持たず、ここで作った HTML を表示する。カーソル移動や上書きは
// xterm.js が解釈し終えているので、バッファの1行を1行の HTML にするだけでよい。

import type { IBuffer, IBufferCell, IBufferLine } from "@xterm/xterm";

/** 変換する行数の上限。これより長い出力は、最後のぶんだけを変換する。 */
export const MAX_LINES = 10_000;

export interface Rendered {
  /** 1行ずつの HTML (末尾の空行は含めない)。 */
  lines: string[];
  /** 先頭を省いた (行数の上限を超えたか、もとの出力の先頭がもう残っていない)。 */
  omitted: boolean;
}

/** バッファを1行ずつの HTML にする。 */
export function renderBuffer(buffer: IBuffer, maxLines = MAX_LINES): Rendered {
  let end = buffer.length;
  while (end > 0 && isBlank(buffer.getLine(end - 1))) {
    end--;
  }
  const start = Math.max(0, end - maxLines);
  const cell = buffer.getNullCell();
  const lines: string[] = [];
  for (let y = start; y < end; y++) {
    const line = buffer.getLine(y);
    lines.push(line ? renderLine(line, cell) : "");
  }
  return { lines, omitted: start > 0 };
}

function isBlank(line: IBufferLine | undefined): boolean {
  return !line || line.translateToString(true) === "";
}

function renderLine(line: IBufferLine, cell: IBufferCell): string {
  // 行末の、何も描かれていないセルは出さない。
  let length = line.length;
  while (length > 0) {
    const last = line.getCell(length - 1, cell);
    if (last && !(isEmpty(last) && last.isBgDefault() && !last.isInverse())) {
      break;
    }
    length--;
  }

  let html = "";
  let text = "";
  let style = "";
  const flush = () => {
    if (text !== "") {
      html += style === "" ? escape(text) : `<span style="${style}">${escape(text)}</span>`;
      text = "";
    }
  };
  for (let x = 0; x < length; x++) {
    const current = line.getCell(x, cell);
    // 幅 0 は、全角文字の右半分。左半分のセルで出してある。
    if (!current || current.getWidth() === 0) {
      continue;
    }
    const next = current.isAttributeDefault() ? "" : styleOf(current);
    if (next !== style) {
      flush();
      style = next;
    }
    text += current.isInvisible() ? " ".repeat(current.getWidth()) : current.getChars() || " ";
  }
  flush();
  return html;
}

function isEmpty(cell: IBufferCell): boolean {
  const chars = cell.getChars();
  return chars === "" || chars === " ";
}

/** セルの見た目を、インラインの CSS にする。色は、テーマの CSS 変数を指す。 */
function styleOf(cell: IBufferCell): string {
  let foreground = color(
    cell.isFgDefault(),
    cell.isFgRGB(),
    cell.getFgColor(),
    cell.isBold() !== 0,
  );
  let background = color(cell.isBgDefault(), cell.isBgRGB(), cell.getBgColor(), false);
  if (cell.isInverse()) {
    [foreground, background] = [background ?? "var(--term-bg)", foreground ?? "var(--term-fg)"];
  }

  let style = "";
  if (foreground) {
    style += `color:${foreground};`;
  }
  if (background) {
    style += `background:${background};`;
  }
  if (cell.isBold()) {
    style += "font-weight:bold;";
  }
  if (cell.isDim()) {
    style += "opacity:0.6;";
  }
  if (cell.isItalic()) {
    style += "font-style:italic;";
  }
  const lines = [
    cell.isUnderline() ? "underline" : "",
    cell.isStrikethrough() ? "line-through" : "",
    cell.isOverline() ? "overline" : "",
  ].filter(Boolean);
  if (lines.length > 0) {
    style += `text-decoration:${lines.join(" ")};`;
  }
  return style;
}

/** 色を CSS の値にする。既定の色なら null。 */
function color(isDefault: boolean, isRgb: boolean, value: number, bold: boolean): string | null {
  if (isDefault) {
    return null;
  }
  if (isRgb) {
    return `#${value.toString(16).padStart(6, "0")}`;
  }
  if (value < 16) {
    // xterm.js と同じく、太字の基本 8 色は明るい色で描く。
    return `var(--ansi-${bold && value < 8 ? value + 8 : value})`;
  }
  return palette256(value);
}

const CUBE_LEVELS = [0, 95, 135, 175, 215, 255];

/** 256 色のうち、16 番以降 (6×6×6 の色の立方体と、24 段階の灰色)。 */
function palette256(index: number): string {
  if (index >= 232) {
    const level = 8 + (index - 232) * 10;
    return rgb(level, level, level);
  }
  const offset = index - 16;
  return rgb(
    CUBE_LEVELS[Math.floor(offset / 36)],
    CUBE_LEVELS[Math.floor(offset / 6) % 6],
    CUBE_LEVELS[offset % 6],
  );
}

function rgb(red: number, green: number, blue: number): string {
  return `#${[red, green, blue].map((level) => level.toString(16).padStart(2, "0")).join("")}`;
}

/** HTML にした行を、色のない文字に戻す (コピー用)。 */
export function plainText(lines: string[]): string {
  return lines.map((line) => line.replace(/<[^>]*>/g, "").replace(/&(\w+);/g, unescape)).join("\n");
}

function unescape(entity: string, name: string): string {
  return UNESCAPES[name] ?? entity;
}

const UNESCAPES: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"' };

function escape(text: string): string {
  return text.replace(/[&<>"]/g, (char) => ESCAPES[char]);
}

const ESCAPES: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
};
