// Rust コアとのやりとり。出力は Channel でバイト列のまま受け取り、入力はバイト列のまま送る。

import { Channel, invoke } from "@tauri-apps/api/core";

/** 出力以外の知らせ。 */
export type ControlEvent =
  { type: "exited"; code: number } | { type: "disconnected"; reason: string | null };

export interface AttachInfo {
  daemonVersion: string;
  pid: number | null;
  /** この接続のためにデーモンを起動したか。false なら、動いていた bash に再接続した。 */
  startedDaemon: boolean;
}

export interface AppOptions {
  renderer: string | null;
  warning: string | null;
}

export interface PtyHandlers {
  /** 直前の画面を戻すための出力。接続のたびに最初に1回だけ呼ばれる (新しい bash なら空)。 */
  onReplay(data: Uint8Array): void;
  onOutput(data: Uint8Array): void;
  onControl(event: ControlEvent): void;
}

export function appOptions(): Promise<AppOptions> {
  return invoke("app_options");
}

/** デーモンに接続して画面をつなぐ。 */
export function attach(cols: number, rows: number, handlers: PtyHandlers): Promise<AttachInfo> {
  const onEvent = new Channel<ArrayBuffer | number[] | ControlEvent>();
  let replayed = false;
  onEvent.onmessage = (message) => {
    let data: Uint8Array;
    if (message instanceof ArrayBuffer) {
      data = new Uint8Array(message);
    } else if (Array.isArray(message)) {
      data = Uint8Array.from(message);
    } else {
      handlers.onControl(message);
      return;
    }
    if (replayed) {
      handlers.onOutput(data);
    } else {
      replayed = true;
      handlers.onReplay(data);
    }
  };
  return invoke("pty_attach", { cols, rows, onEvent });
}

export function resize(cols: number, rows: number): void {
  invoke("pty_resize", { cols, rows }).catch(() => {
    // 切断中のリサイズは捨ててよい。つなぎ直すときに今の大きさを送る。
  });
}

/**
 * 描き終えた出力の量を Rust コアに知らせる。
 *
 * コアは、まだ描かれていない出力が多いあいだデーモンからの読み取りを止める。
 * 細かく呼ぶと遅くなるので、ある程度まとめて送る。
 */
export class AckBatcher {
  private pending = 0;
  private timer: number | undefined;

  add(bytes: number): void {
    this.pending += bytes;
    if (this.pending >= 64 * 1024) {
      this.flush();
    } else if (this.timer === undefined) {
      this.timer = window.setTimeout(() => this.flush(), 50);
    }
  }

  private flush(): void {
    window.clearTimeout(this.timer);
    this.timer = undefined;
    const bytes = this.pending;
    this.pending = 0;
    if (bytes > 0) {
      invoke("pty_ack", { bytes }).catch(() => {});
    }
  }
}

/**
 * キー入力を順番どおりに送る。
 *
 * 前の送信が終わるまで次を送らないので、貼り付けのように入力が続いても順番が入れ替わらない。
 * 待っているあいだの入力は1回にまとめる。
 */
export class InputQueue {
  private pending: Uint8Array[] = [];
  private sending = false;

  push(data: Uint8Array): void {
    this.pending.push(data);
    if (!this.sending) {
      void this.drain();
    }
  }

  clear(): void {
    this.pending = [];
  }

  private async drain(): Promise<void> {
    this.sending = true;
    while (this.pending.length > 0) {
      const chunk = concat(this.pending);
      this.pending = [];
      try {
        await invoke("pty_input", chunk);
      } catch {
        // 切断中の入力は捨てる。
      }
    }
    this.sending = false;
  }
}

function concat(chunks: Uint8Array[]): Uint8Array {
  if (chunks.length === 1) {
    return chunks[0];
  }
  const out = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.length, 0));
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}
