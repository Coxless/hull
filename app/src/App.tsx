import { useState } from "react";
import { TerminalView, type TerminalStatus } from "./TerminalView";

const INITIAL: TerminalStatus = {
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

export function App() {
  const [status, setStatus] = useState(INITIAL);

  return (
    <div className="app">
      <TerminalView onStatus={setStatus} />
      <footer className="statusbar" data-state={status.state} data-renderer={status.renderer}>
        <span className="statusbar-state">{STATE_LABEL[status.state]}</span>
        <span>bash{status.pid !== null && ` (PID ${status.pid})`}</span>
        <span className="statusbar-spacer" />
        {status.cols > 0 && (
          <span>
            {status.cols}×{status.rows}
          </span>
        )}
        {status.renderer && <span>{RENDERER_LABEL[status.renderer]}</span>}
      </footer>
    </div>
  );
}
