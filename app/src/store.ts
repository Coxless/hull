// ブロックの表示の状態。出力そのものは持たない (実行中は xterm.js、終了後は `outputs`)。

import { create } from "zustand";
import type { BlockInfo, BlockResult, PromptState } from "./pty";
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
};

export const useSession = create<SessionState>(() => INITIAL);

/** 画面を作り直すとき (生の表示から戻るときなど) に、最初の状態に戻す。 */
export function resetSession(): void {
  useSession.setState(INITIAL, true);
  outputs.clear();
}

/** 終了したブロックの出力 (1行ずつの文字列)。`BlockEntry.key` で引く。 */
export const outputs = new Map<string, string[]>();
