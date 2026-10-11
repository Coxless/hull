import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useEffect, useRef } from "react";
import { KittyKeyboard } from "./kittyKeyboard";
import { AckBatcher, InputQueue, appOptions, attach, resize, type ControlEvent } from "./pty";
import { FONT_FAMILY, FONT_SIZE, THEME } from "./theme";

export type Renderer = "webgl" | "dom";

export interface TerminalStatus {
  state: "connecting" | "running" | "exited" | "disconnected";
  renderer: Renderer | null;
  cols: number;
  rows: number;
  pid: number | null;
}

interface Props {
  onStatus(status: TerminalStatus): void;
}

const APP_TITLE = "hull-terminal";

export function TerminalView({ onStatus }: Props) {
  const container = useRef<HTMLDivElement>(null);
  const onStatusRef = useRef(onStatus);
  onStatusRef.current = onStatus;

  useEffect(() => {
    const element = container.current!;
    let disposed = false;

    const term = new Terminal({
      allowProposedApi: true,
      cursorBlink: true,
      fontFamily: FONT_FAMILY,
      fontSize: FONT_SIZE,
      scrollback: 10_000,
      theme: THEME,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    // 絵文字や全角文字の幅を新しい Unicode の定義で数える。
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
    term.open(element);
    fit.fit();

    const status: TerminalStatus = {
      state: "connecting",
      renderer: null,
      cols: term.cols,
      rows: term.rows,
      pid: null,
    };
    const update = (patch: Partial<TerminalStatus>) => {
      Object.assign(status, patch);
      if (!disposed) {
        onStatusRef.current({ ...status });
      }
    };

    const kitty = new KittyKeyboard(term);
    const input = new InputQueue();
    const acks = new AckBatcher();
    const encoder = new TextEncoder();
    let replaying = false;
    const notice = (text: string) => term.write(`\r\n\x1b[2m[${text}]\x1b[0m\r\n`);

    const connect = async () => {
      update({ state: "connecting", pid: null });
      replaying = true;
      try {
        const info = await attach(term.cols, term.rows, {
          onReplay: (data) => {
            // 再現する出力に入っている問い合わせ (カーソル位置や色など) に端末が応答すると、
            // その応答が bash への入力になってしまう。書き終えるまで入力を送らない。
            replaying = true;
            term.write(data, () => {
              replaying = false;
              acks.add(data.length);
            });
          },
          onOutput: (data) => term.write(data, () => acks.add(data.length)),
          onControl,
        });
        if (status.state === "connecting") {
          update({ state: "running", pid: info.pid });
        }
        // つないでいる途中で画面の大きさが変わると、その知らせはデーモンに届いていない。
        // つながったあとで今の大きさを送り直す。
        resize(term.cols, term.rows);
      } catch (error) {
        notice(`bash を起動できません: ${String(error)}`);
        notice("Enter でもう一度試します");
        update({ state: "disconnected" });
      }
    };

    const onControl = (event: ControlEvent) => {
      input.clear();
      if (event.type === "exited") {
        notice(`bash が終了しました (終了コード ${event.code})。Enter で新しい bash を起動します`);
        update({ state: "exited", pid: null });
      } else {
        const reason = event.reason ? `: ${event.reason}` : "";
        notice(`デーモンとの接続が切れました${reason}。Enter でつなぎ直します`);
        update({ state: "disconnected", pid: null });
      }
    };

    const restart = () => {
      // 終了した bash が残した画面のモード (代替画面やマウス報告など) を持ち越さない。
      term.write("\x1b[?1049l\x1b[?1;1000;1002;1003;1004;1006;2004l\x1b[?25h\x1b[0m");
      kitty.reset();
      void connect();
    };

    const send = (data: Uint8Array) => {
      if ((status.state === "running" || status.state === "connecting") && !replaying) {
        input.push(data);
      }
    };

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
      if (status.state === "exited" || status.state === "disconnected") {
        if (event.key === "Enter" && !event.isComposing) {
          restart();
        }
        return false;
      }
      const encoded = kitty.encode(event);
      if (encoded !== null) {
        send(encoder.encode(encoded));
        event.preventDefault();
        return false;
      }
      return true;
    });

    const subscriptions = [
      kitty,
      term.onData((data) => send(encoder.encode(data))),
      // マウス報告などの、UTF-8 として送ってはいけないバイト列。
      term.onBinary((data) => send(Uint8Array.from(data, (char) => char.charCodeAt(0)))),
      term.onResize(({ cols, rows }) => {
        update({ cols, rows });
        if (status.state === "running") {
          resize(cols, rows);
        }
      }),
      term.onTitleChange((title) => {
        void getCurrentWindow().setTitle(title ? `${title} — ${APP_TITLE}` : APP_TITLE);
      }),
    ];

    // WebGL の描画は、次に何かを描くまで画面に出ないことがある (WebKitGTK)。カーソルの点滅が
    // 止まる非アクティブなウィンドウでは、最後の出力が出ないままになるので、描画が落ち着いたら
    // もう一度だけ描き直して押し出す。
    let flushTimer = 0;
    let flushing = false;
    subscriptions.push(
      term.onRender(() => {
        if (flushing || status.renderer !== "webgl") {
          flushing = false;
          return;
        }
        window.clearTimeout(flushTimer);
        flushTimer = window.setTimeout(() => {
          flushing = true;
          term.refresh(0, term.rows - 1);
        }, 40);
      }),
    );

    let frame = 0;
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => fit.fit());
    });
    observer.observe(element);

    // フォントの読み込みが終わると文字の幅が変わることがあるので、列数を数え直す。
    void document.fonts.ready.then(() => {
      if (!disposed) {
        fit.fit();
      }
    });

    const focus = () => term.focus();
    window.addEventListener("focus", focus);
    const blockContextMenu = (event: Event) => event.preventDefault();
    element.addEventListener("contextmenu", blockContextMenu);
    term.focus();

    void (async () => {
      const options = await appOptions().catch(() => ({ renderer: null, warning: null }));
      if (disposed) {
        return;
      }
      update({
        renderer: enableRenderer(term, options.renderer, (renderer) => update({ renderer })),
      });
      if (options.warning) {
        notice(options.warning);
      }
      await connect();
    })();

    return () => {
      disposed = true;
      observer.disconnect();
      cancelAnimationFrame(frame);
      window.clearTimeout(flushTimer);
      window.removeEventListener("focus", focus);
      element.removeEventListener("contextmenu", blockContextMenu);
      for (const subscription of subscriptions) {
        subscription.dispose();
      }
      term.dispose();
    };
  }, []);

  return <div className="terminal-host" ref={container} />;
}

/**
 * GPU (WebGL) で描く。使えない環境や、途中で GPU のコンテキストを失ったときは DOM 描画にする。
 *
 * GPU があるかどうかは画面側からは見分けられない (WebKit は描画デバイスの名前を隠す) ので、
 * Rust コアが調べて、GPU がなければ `preference` に `dom` を入れてくる。
 */
function enableRenderer(
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
