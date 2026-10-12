// Rust コアとのやりとり。出力は Channel でバイト列のまま受け取り、入力はバイト列のまま送る。

import { Channel, invoke } from "@tauri-apps/api/core";

/** ブロック1つ。コマンドと出力の1往復。 */
export interface BlockInfo {
  /** セッション (bash 1つ) の中で 1 から順に付く番号。 */
  id: number;
  /**
   * 実行したテキスト。入力欄を通さずに実行されたコマンドでは空。
   * null は、コマンドの外で出た出力 (`.bashrc` の出力など)。
   */
  command: string | null;
  cwd: string | null;
  /** 開始時刻 (Unix エポックからのミリ秒)。 */
  startedAt: number;
  /** 実行したときの画面の大きさ。 */
  cols: number;
  rows: number;
}

export interface BlockResult {
  /** 終了コード。わからなかったときは null。 */
  exitCode: number | null;
  finishedAt: number;
  /** 代替画面を使った (vim などの全画面アプリ)。 */
  fullscreen: boolean;
}

export interface FinishedBlock {
  info: BlockInfo;
  result: BlockResult;
}

export interface PromptState {
  cwd: string | null;
  /** git ブランチ。リポジトリの外では null。 */
  branch: string | null;
}

/** 出力以外の知らせ。ブロックに関するものは、ブロックの表示でつないだときだけ届く。 */
export type ControlEvent =
  | {
      type: "history";
      blocks: FinishedBlock[];
      running: BlockInfo | null;
      prompt: PromptState;
      /** プロンプトで入力を待っていて、テキストを実行できる。 */
      ready: boolean;
    }
  | { type: "prompt"; prompt: PromptState }
  | { type: "blockStarted"; block: BlockInfo }
  | { type: "blockFinished"; id: number; result: BlockResult }
  | { type: "runRejected"; reason: "incomplete" | "busy"; text: string }
  | { type: "exited"; code: number }
  | { type: "disconnected"; reason: string | null };

export interface AttachInfo {
  daemonVersion: string;
  pid: number | null;
  /** この接続のためにデーモンを起動したか。false なら、動いていた bash に再接続した。 */
  startedDaemon: boolean;
  /** bash が統合スクリプトを読み込んでいて、コマンドごとに区切れる。 */
  integrated: boolean;
}

export interface AppOptions {
  renderer: string | null;
  /** 生の表示で始める (`HULL_TERMINAL_MODE=raw`)。 */
  raw: boolean;
}

export interface PtyHandlers {
  /**
   * 直前の画面を戻すための出力。生の表示では接続のたびに最初に1回 (新しい bash なら空)、
   * ブロックの表示では、実行中のブロックがあるときだけ呼ばれる。
   */
  onReplay(data: Uint8Array): void;
  /** 実行中のブロックの出力。生の表示では、bash のすべての出力。 */
  onOutput(data: Uint8Array): void;
  onControl(event: ControlEvent): void;
}

export function appOptions(): Promise<AppOptions> {
  return invoke("app_options");
}

/**
 * デーモンに接続して画面をつなぐ。`raw` なら、区切りを通さない生の表示でつなぐ。
 *
 * 出力と知らせは、届いた順のまま `handlers` に渡る。
 */
export function attach(
  cols: number,
  rows: number,
  raw: boolean,
  handlers: PtyHandlers,
): Promise<AttachInfo> {
  const onEvent = new Channel<ArrayBuffer | number[] | ControlEvent | { type: "replay" }>();
  // `replay` の知らせの次のバイト列が、直前の画面を戻すための出力。
  let replayNext = false;
  onEvent.onmessage = (message) => {
    let data: Uint8Array;
    if (message instanceof ArrayBuffer) {
      data = new Uint8Array(message);
    } else if (Array.isArray(message)) {
      data = Uint8Array.from(message);
    } else if (message.type === "replay") {
      replayNext = true;
      return;
    } else {
      handlers.onControl(message);
      return;
    }
    if (replayNext) {
      replayNext = false;
      handlers.onReplay(data);
    } else {
      handlers.onOutput(data);
    }
  };
  return invoke("pty_attach", { cols, rows, raw, onEvent });
}

/** 入力欄のテキストを実行する。実行されなかったときは、`runRejected` の知らせが届く。 */
export function run(text: string): Promise<void> {
  return invoke("session_run", { text });
}

/** 終了したブロックの出力を取る。 */
export async function blockOutput(id: number): Promise<{ data: Uint8Array; truncated: boolean }> {
  // 先頭の1バイトが、出力の先頭を省いたかどうか。残りが出力。
  const body = new Uint8Array(await invoke<ArrayBuffer>("block_output", { id }));
  return { data: body.subarray(1), truncated: body[0] === 1 };
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
