import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import type { Terminal } from "@xterm/xterm";
import type { KittyKeyboard } from "./kittyKeyboard";

const encoder = new TextEncoder();

interface Options {
  /** キー入力を bash に送る。 */
  send(data: Uint8Array): void;
  /**
   * xterm.js に渡す前に見るキー。true を返したキーは、ここで処理を終える
   * (bash が終了したあとの Enter など)。
   */
  intercept?(event: KeyboardEvent): boolean;
}

/**
 * 端末のキー入力を決める。コピーは Ctrl+Shift+C、貼り付けは Ctrl+Shift+V。
 * アプリが kitty keyboard protocol を有効にしていれば、修飾キー付きの Enter をその符号化で送る。
 */
export function attachTerminalKeys(term: Terminal, kitty: KittyKeyboard, options: Options): void {
  term.attachCustomKeyEventHandler((event) => {
    if (event.type !== "keydown") {
      return true;
    }
    if (event.ctrlKey && event.shiftKey && event.code === "KeyC") {
      const selection = term.getSelection();
      if (selection) {
        void writeText(selection);
      }
      event.preventDefault();
      return false;
    }
    if (event.ctrlKey && event.shiftKey && event.code === "KeyV") {
      void readText().then((text) => text && term.paste(text));
      event.preventDefault();
      return false;
    }
    if (options.intercept?.(event)) {
      return false;
    }
    const encoded = kitty.encode(event);
    if (encoded !== null) {
      options.send(encoder.encode(encoded));
      event.preventDefault();
      return false;
    }
    return true;
  });
}

/** `term.onData` の文字列を、送るバイト列にする。 */
export function encodeData(data: string): Uint8Array {
  return encoder.encode(data);
}

/** `term.onBinary` の文字列 (マウス報告などの、UTF-8 として送ってはいけないバイト列) を戻す。 */
export function encodeBinary(data: string): Uint8Array {
  return Uint8Array.from(data, (char) => char.charCodeAt(0));
}
