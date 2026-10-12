import { useEffect, useRef } from "react";
import { Block } from "./Block";
import { RunningBlock } from "./RunningBlock";
import { useSession } from "./store";

/** いちばん下からこの高さ以内にいれば、「下に付いている」とみなす。 */
const STICK_THRESHOLD = 48;

/** ブロックの一覧。新しいブロックや出力で伸びたときは、下に付いていれば下まで送る。 */
export function BlockList() {
  const blocks = useSession((state) => state.blocks);
  const running = useSession((state) => state.running);
  const connection = useSession((state) => state.connection);
  const list = useRef<HTMLDivElement>(null);
  const inner = useRef<HTMLDivElement>(null);
  const stuck = useRef(true);

  useEffect(() => {
    const element = list.current!;
    const onScroll = () => {
      stuck.current =
        element.scrollHeight - element.scrollTop - element.clientHeight < STICK_THRESHOLD;
    };
    const follow = () => {
      if (stuck.current) {
        element.scrollTop = element.scrollHeight;
      }
    };
    element.addEventListener("scroll", onScroll, { passive: true });
    const observer = new ResizeObserver(follow);
    observer.observe(inner.current!);
    observer.observe(element);
    return () => {
      element.removeEventListener("scroll", onScroll);
      observer.disconnect();
    };
  }, []);

  // コマンドを実行したら、上を見ていても下まで送る。
  const runningKey = running?.key;
  useEffect(() => {
    if (runningKey !== undefined) {
      stuck.current = true;
      list.current!.scrollTop = list.current!.scrollHeight;
    }
  }, [runningKey]);

  const empty = blocks.length === 0 && running === null && connection === "connected";
  return (
    <div className="blocks" ref={list}>
      <div className="blocks-inner" ref={inner}>
        {blocks.map((block) => (
          <Block key={block.key} block={block} />
        ))}
        {running && <RunningBlock key={running.key} block={running} />}
        {empty && (
          <div className="blocks-empty">下の入力欄にコマンドを入力して、Enter で実行します</div>
        )}
      </div>
    </div>
  );
}
