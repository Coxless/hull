// kitty keyboard protocol の最小限の対応。
//
// 従来の端末の符号化では Shift+Enter と Enter がどちらも CR になり、区別できない。
// Claude Code などは、端末が kitty keyboard protocol に応えるとこれを有効にして、
// Shift+Enter を `CSI 13 ; 2 u` として受け取る。xterm.js 自体はこの仕組みを持たないので、
// 有効・無効の切り替えと問い合わせへの応答だけをここで行い、修飾キー付きの Enter を符号化する。
// ほかのキーは従来の符号化のまま送る (アプリ側は従来の符号化も受け付ける)。
//
// アプリが有効にしていないあいだ (普通の bash など) は何も変えない。

import type { IDisposable, Terminal } from "@xterm/xterm";

const ENTER = 13;

export class KittyKeyboard implements IDisposable {
  // 通常画面と代替画面で別々に持つ。
  private stacks: { main: number[]; alternate: number[] } = { main: [], alternate: [] };
  private disposables: IDisposable[];

  constructor(private term: Terminal) {
    const csi = term.parser.registerCsiHandler.bind(term.parser);
    this.disposables = [
      // CSI > flags u : 今の設定を積んで、新しい設定にする
      csi({ prefix: ">", final: "u" }, (params) => {
        const stack = this.stack();
        stack.push(first(params));
        if (stack.length > 32) {
          stack.shift();
        }
        return true;
      }),
      // CSI < n u : 積んだ設定を n 個戻す
      csi({ prefix: "<", final: "u" }, (params) => {
        const stack = this.stack();
        stack.splice(Math.max(0, stack.length - Math.max(1, first(params))));
        return true;
      }),
      // CSI = flags ; mode u : 今の設定を書き換える (1: 置き換え, 2: 追加, 3: 削除)
      csi({ prefix: "=", final: "u" }, (params) => {
        const stack = this.stack();
        const flags = first(params);
        const mode = typeof params[1] === "number" ? params[1] : 1;
        const current = stack.pop() ?? 0;
        stack.push(mode === 2 ? current | flags : mode === 3 ? current & ~flags : flags);
        return true;
      }),
      // CSI ? u : 今の設定を問い合わせる
      csi({ prefix: "?", final: "u" }, () => {
        term.input(`\x1b[?${this.flags()}u`, false);
        return true;
      }),
    ];
  }

  /**
   * このキー入力を kitty の符号化で送るなら、その文字列を返す。従来どおりでよければ null。
   */
  encode(event: KeyboardEvent): string | null {
    if (event.type !== "keydown" || event.key !== "Enter" || event.isComposing) {
      return null;
    }
    // 1: 区別のつかないキーを区別して送る
    if ((this.flags() & 1) === 0) {
      return null;
    }
    const modifiers =
      (event.shiftKey ? 1 : 0) |
      (event.altKey ? 2 : 0) |
      (event.ctrlKey ? 4 : 0) |
      (event.metaKey ? 8 : 0);
    return modifiers === 0 ? null : `\x1b[${ENTER};${modifiers + 1}u`;
  }

  /** 端末をリセットしたとき (bash を起動し直したときなど) に呼ぶ。 */
  reset(): void {
    this.stacks = { main: [], alternate: [] };
  }

  dispose(): void {
    for (const disposable of this.disposables) {
      disposable.dispose();
    }
  }

  private stack(): number[] {
    return this.term.buffer.active.type === "alternate" ? this.stacks.alternate : this.stacks.main;
  }

  private flags(): number {
    return this.stack().at(-1) ?? 0;
  }
}

function first(params: (number | number[])[]): number {
  const value = params[0];
  return typeof value === "number" ? value : 0;
}
