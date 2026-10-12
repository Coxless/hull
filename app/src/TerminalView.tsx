import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { Terminal } from "@xterm/xterm";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useRef } from "react";
import { KittyKeyboard } from "./kittyKeyboard";
import { AckBatcher, InputQueue, appOptions, attach, resize, type ControlEvent } from "./pty";
import { enableRenderer, flushWebglRenders } from "./renderer";
import type { TerminalStatus } from "./StatusBar";
import { attachTerminalKeys, encodeBinary, encodeData } from "./terminalKeys";
import { BACKGROUND, FONT_FAMILY, FONT_SIZE, terminalTheme } from "./theme";

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
      theme: terminalTheme(BACKGROUND),
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
    let replaying = false;
    const notice = (text: string) => term.write(`\r\n\x1b[2m[${text}]\x1b[0m\r\n`);

    const connect = async () => {
      update({ state: "connecting", pid: null });
      replaying = true;
      try {
        const info = await attach(term.cols, term.rows, true, {
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
      // ブロックの知らせは、生の表示には届かない。
      if (event.type !== "exited" && event.type !== "disconnected") {
        return;
      }
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

    attachTerminalKeys(term, kitty, {
      send,
      intercept: (event) => {
        if (status.state !== "exited" && status.state !== "disconnected") {
          return false;
        }
        if (event.key === "Enter" && !event.isComposing) {
          restart();
        }
        return true;
      },
    });

    const subscriptions = [
      kitty,
      term.onData((data) => send(encodeData(data))),
      term.onBinary((data) => send(encodeBinary(data))),
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

    subscriptions.push(flushWebglRenders(term, () => status.renderer === "webgl"));

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
      const renderer = await appOptions()
        .then((options) => options.renderer)
        .catch(() => null);
      if (disposed) {
        return;
      }
      update({
        renderer: enableRenderer(term, renderer, (renderer) => update({ renderer })),
      });
      await connect();
    })();

    return () => {
      disposed = true;
      observer.disconnect();
      cancelAnimationFrame(frame);
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
