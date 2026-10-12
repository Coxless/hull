import { WebglAddon } from "@xterm/addon-webgl";
import type { IDisposable, Terminal } from "@xterm/xterm";

export type Renderer = "webgl" | "dom";

/**
 * GPU (WebGL) で描く。使えない環境や、途中で GPU のコンテキストを失ったときは DOM 描画にする。
 *
 * GPU があるかどうかは画面側からは見分けられない (WebKit は描画デバイスの名前を隠す) ので、
 * Rust コアが調べて、GPU がなければ `preference` に `dom` を入れてくる。
 */
export function enableRenderer(
  term: Terminal,
  preference: string | null,
  onChange: (renderer: Renderer) => void,
): Renderer {
  if (preference === "dom" || !supportsWebgl2()) {
    return "dom";
  }
  try {
    const webgl = new WebglAddon();
    webgl.onContextLoss(() => {
      // アドオンを外すと、xterm.js は元の DOM 描画に戻る。
      webgl.dispose();
      onChange("dom");
    });
    term.loadAddon(webgl);
    return "webgl";
  } catch {
    return "dom";
  }
}

function supportsWebgl2(): boolean {
  try {
    return document.createElement("canvas").getContext("webgl2") !== null;
  } catch {
    return false;
  }
}

/**
 * WebGL の描画は、次に何かを描くまで画面に出ないことがある (WebKitGTK)。カーソルの点滅が
 * 止まる非アクティブなウィンドウでは、最後の出力が出ないままになるので、描画が落ち着いたら
 * もう一度だけ描き直して押し出す。
 */
export function flushWebglRenders(term: Terminal, isWebgl: () => boolean): IDisposable {
  let timer = 0;
  let flushing = false;
  const subscription = term.onRender(() => {
    if (flushing || !isWebgl()) {
      flushing = false;
      return;
    }
    window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      flushing = true;
      term.refresh(0, term.rows - 1);
    }, 40);
  });
  return {
    dispose() {
      window.clearTimeout(timer);
      subscription.dispose();
    },
  };
}
