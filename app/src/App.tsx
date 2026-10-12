import { useCallback, useEffect, useState } from "react";
import { BlockView } from "./BlockView";
import { appOptions } from "./pty";
import { INITIAL_STATUS, StatusBar, type TerminalStatus } from "./StatusBar";
import { useSession } from "./store";
import { TerminalView } from "./TerminalView";
import { TopBar } from "./TopBar";

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

  const toggleRaw = useCallback(() => {
    setRawStatus(INITIAL_STATUS);
    setMode((current) => (current === "raw" ? "blocks" : "raw"));
  }, []);

  // Ctrl+Shift+R で、生の表示とブロックの表示を切り替える。端末に渡る前に受け取る。
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.ctrlKey && event.shiftKey && !event.altKey && event.code === "KeyR") {
        event.preventDefault();
        event.stopPropagation();
        toggleRaw();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [toggleRaw]);

  return (
    <div className="app">
      {mode !== null && <TopBar raw={mode === "raw"} onToggleRaw={toggleRaw} />}
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
