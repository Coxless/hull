// ブロックの表示の状態。出力そのものは持たない (実行中は xterm.js、終了後は `outputs`)。

import { create } from "zustand";
import type { BlockInfo, BlockResult, PromptState } from "./pty";
import type { Rendered } from "./render";
import type { Renderer } from "./renderer";

export type Connection = "connecting" | "connected" | "exited" | "disconnected";

/** 終了したブロック。 */
export interface BlockEntry {
  /** 画面の中で重ならない名前。bash を起動し直すと番号が 1 に戻るので、番号だけでは足りない。 */
  key: string;
  info: BlockInfo;
  result: BlockResult;
}

export interface RunningEntry {
  key: string;
  info: BlockInfo;
}

export interface SessionState {
  connection: Connection;
  /** bash の終了コード (`connection` が exited のとき)。 */
  exitCode: number | null;
  /** つなげなかった、または切れた理由。 */
  problem: string | null;
  pid: number | null;
  renderer: Renderer | null;
  cols: number;
  rows: number;

  /** 終了したブロック (古い順)。 */
  blocks: BlockEntry[];
  running: RunningEntry | null;
  prompt: PromptState;
  /** プロンプトで入力を待っていて、テキストを実行できる。 */
  ready: boolean;
  /** 送って、まだ実行が始まっていないテキスト。 */
  pending: string | null;
  /** 実行されなかったテキスト。入力欄に戻す。 */
  rejected: Rejected | null;
  /** ホームディレクトリ。ディレクトリの表示を `~` で短くするために使う。 */
  home: string | null;
  /** 失敗したブロックだけを見せる。 */
  failedOnly: boolean;
  /**
   * 実行中のブロックを、ウィンドウいっぱいに広げる。アプリが代替画面に入ると立ち、次の
   * ブロックが始まると下りる。キーとボタンでも切り替えられる。実行中のブロックがない間は
   * 意味を持たない ([`isMaximized`] で見る)。
   */
  maximized: boolean;
}

export interface Rejected {
  text: string;
  /** 入力が途中だった / コマンドの実行中などで受け付けられなかった。 */
  reason: "incomplete" | "busy";
  /** 同じテキストが続けて断られても見分けられるように、断られるたびに増やす。 */
  serial: number;
}

const INITIAL: SessionState = {
  connection: "connecting",
  exitCode: null,
  problem: null,
  pid: null,
  renderer: null,
  cols: 0,
  rows: 0,
  blocks: [],
  running: null,
  prompt: { cwd: null, branch: null },
  ready: false,
  pending: null,
  rejected: null,
  home: null,
  failedOnly: false,
  maximized: false,
};

export const useSession = create<SessionState>(() => INITIAL);

/** 実行中のブロックを、ウィンドウいっぱいに広げているか。 */
export function isMaximized(state: SessionState): boolean {
  return state.maximized && state.running !== null;
}

/** 実行中のブロックを、ウィンドウいっぱいに広げる・戻すキー。 */
export const TOGGLE_MAXIMIZED_KEYS = "Ctrl+Shift+M";

/** 実行中のブロックを広げる・戻す。実行中のブロックがなければ何もしない。 */
export function toggleMaximized(): void {
  useSession.setState((state) => (state.running ? { maximized: !state.maximized } : {}));
}

/** 画面を作り直すとき (生の表示から戻るときなど) に、最初の状態に戻す。 */
export function resetSession(): void {
  useSession.setState(INITIAL, true);
  outputs.clear();
}

/**
 * 終了したブロックの出力を、色付きの HTML にしたもの。`BlockEntry.key` で引く。
 *
 * この画面で終わるのを見たブロックは、終わったときに入れる。開き直して戻したブロックは、
 * 画面に入ったときにデーモンから出力を取って入れる。
 */
export const outputs = new Map<string, Rendered>();
