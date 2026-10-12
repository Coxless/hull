import { createContext, useContext, useEffect, useRef, useState } from "react";
import { BlockList } from "./BlockList";
import { BlockSession } from "./blockSession";
import { InputBox } from "./InputBox";
import { resetSession } from "./store";

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
