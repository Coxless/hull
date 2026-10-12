import { useEffect, useState } from "react";
import { BlockView } from "./BlockView";
import { appOptions } from "./pty";
import { INITIAL_STATUS, StatusBar, type TerminalStatus } from "./StatusBar";
import { useSession } from "./store";
import { TerminalView } from "./TerminalView";

/** ブロックの表示か、区切りを通さない生の表示か。 */
type Mode = "blocks" | "raw";

export function App() {
  // 環境変数で生の表示が指定されているかがわかるまでは、どちらも出さない。
  const [mode, setMode] = useState<Mode | null>(null);
  const [rawStatus, setRawStatus] = useState(INITIAL_STATUS);
  const blockStatus = useBlockStatus();

  useEffect(() => {
    appOptions()
      .then((options) => setMode(options.raw ? "raw" : "blocks"))
      .catch(() => setMode("blocks"));
  }, []);

  return (
    <div className="app">
      {mode === "blocks" && <BlockView onUnintegrated={() => setMode("raw")} />}
      {mode === "raw" && <TerminalView onStatus={setRawStatus} />}
      {mode === null && <div className="terminal-host" />}
      <StatusBar status={mode === "raw" ? rawStatus : blockStatus} raw={mode === "raw"} />
    </div>
  );
}

function useBlockStatus(): TerminalStatus {
  const connection = useSession((state) => state.connection);
  const renderer = useSession((state) => state.renderer);
  const cols = useSession((state) => state.cols);
  const rows = useSession((state) => state.rows);
  const pid = useSession((state) => state.pid);
  return { state: connection === "connected" ? "running" : connection, renderer, cols, rows, pid };
}
