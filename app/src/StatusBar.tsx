import type { Renderer } from "./renderer";
import { TOGGLE_MAXIMIZED_KEYS, toggleMaximized } from "./store";

export interface TerminalStatus {
  state: "connecting" | "running" | "exited" | "disconnected";
  renderer: Renderer | null;
  cols: number;
  rows: number;
  pid: number | null;
}

export const INITIAL_STATUS: TerminalStatus = {
  state: "connecting",
  renderer: null,
  cols: 0,
  rows: 0,
  pid: null,
};

const STATE_LABEL: Record<TerminalStatus["state"], string> = {
  connecting: "接続中",
  running: "実行中",
  exited: "終了",
  disconnected: "切断",
};

const RENDERER_LABEL = { webgl: "WebGL", dom: "DOM" } as const;

interface Props {
  status: TerminalStatus;
  /** 生の表示か。 */
  raw: boolean;
  /** 実行中のブロックを、ウィンドウいっぱいに広げているか。 */
  maximized: boolean;
}

export function StatusBar({ status, raw, maximized }: Props) {
  return (
    <footer className="statusbar" data-state={status.state} data-renderer={status.renderer}>
      <span className="statusbar-state">{STATE_LABEL[status.state]}</span>
      <span>bash{status.pid !== null && ` (PID ${status.pid})`}</span>
      {raw && <span>生の表示</span>}
      {maximized && (
        <button
          type="button"
          className="statusbar-button"
          // 押しても、キー入力の届け先を端末から移さない。
          onMouseDown={(event) => event.preventDefault()}
          onClick={toggleMaximized}
        >
          元の大きさに戻す ({TOGGLE_MAXIMIZED_KEYS})
        </button>
      )}
      <span className="statusbar-spacer" />
      {status.cols > 0 && (
        <span>
          {status.cols}×{status.rows}
        </span>
      )}
      {status.renderer && <span>{RENDERER_LABEL[status.renderer]}</span>}
    </footer>
  );
}
