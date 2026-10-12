import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { createContext, useContext, useEffect, useRef, useState } from "react";
import { BlockList } from "./BlockList";
import { BlockSession } from "./blockSession";
import { InputBox } from "./InputBox";
import { resetSession, toggleMaximized, useSession } from "./store";

const SessionContext = createContext<BlockSession | null>(null);

/** ブロックの表示の中心 ([`BlockSession`])。つなぐ準備ができるまでは null。 */
export function useBlockSession(): BlockSession | null {
  return useContext(SessionContext);
}

interface Props {
  /** bash をブロックに区切れないので、生の表示に切り替える。 */
  onUnintegrated(): void;
}

/** ブロックの表示。コマンドと出力がブロックとして積み上がり、下の入力欄から実行する。 */
export function BlockView({ onUnintegrated }: Props) {
  const sizer = useRef<HTMLDivElement>(null);
  const [session, setSession] = useState<BlockSession | null>(null);
  const onUnintegratedRef = useRef(onUnintegrated);
  onUnintegratedRef.current = onUnintegrated;

  useEffect(() => {
    resetSession();
    const created = new BlockSession(sizer.current!, {
      onUnintegrated: () => onUnintegratedRef.current(),
    });
    setSession(created);
    return () => created.dispose();
  }, []);

  // 終了したブロックの出力で選んだ文字を、端末と同じ Ctrl+Shift+C でコピーする。
  // 実行中のブロック (xterm.js) の選択は、xterm.js の側でコピーする。
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!(event.ctrlKey && event.shiftKey && event.code === "KeyC")) {
        return;
      }
      const selection = window.getSelection()?.toString() ?? "";
      if (selection !== "") {
        event.preventDefault();
        void writeText(selection);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  // TOGGLE_MAXIMIZED_KEYS で、実行中のブロックをウィンドウいっぱいに広げる・戻す。端末に渡る前に受け取る。
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const pressed = event.ctrlKey && event.shiftKey && !event.altKey && event.code === "KeyM";
      if (pressed && useSession.getState().running !== null) {
        event.preventDefault();
        event.stopPropagation();
        toggleMaximized();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, []);

  return (
    <SessionContext.Provider value={session}>
      <div className="blocks-wrap">
        <div className="blocks-sizer" ref={sizer} aria-hidden="true" />
        <BlockList />
      </div>
      <InputBox />
    </SessionContext.Provider>
  );
}
