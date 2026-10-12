import { useEffect, useRef, useState } from "react";
import { BlockHeader } from "./Block";
import { useBlockSession } from "./BlockView";
import { formatDuration } from "./format";
import type { RunningEntry } from "./store";

/**
 * 始まってからこの時間は、端末を見せない。その間に終われば、最初から静的な表示になる
 * (すぐ終わるコマンドで、端末が一瞬だけ出て入れ替わるのを避ける)。
 */
const SHOW_DELAY_MS = 80;

interface Props {
  block: RunningEntry;
}

/** 実行中のブロック。本物のターミナル (xterm.js) を載せて、キー入力をすべてここに届ける。 */
export function RunningBlock({ block }: Props) {
  const session = useBlockSession();
  const slot = useRef<HTMLDivElement>(null);
  const clip = useRef<HTMLDivElement>(null);
  const [shown, setShown] = useState(() => Date.now() - block.info.startedAt >= SHOW_DELAY_MS);

  useEffect(() => {
    if (shown) {
      return;
    }
    const timer = window.setTimeout(() => setShown(true), SHOW_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [shown]);

  useEffect(() => {
    if (!session) {
      return;
    }
    session.mount(slot.current!, clip.current!);
    return () => session.unmount();
  }, [session]);

  // コマンドの外で出た出力は、始まってすぐ終わる。見出しは付けず、端末を見せる前は何も出さない。
  const outside = block.info.command === null;
  return (
    <section
      className={outside ? "block block-outside" : "block"}
      data-running="true"
      hidden={outside && !shown}
      onMouseDown={(event) => {
        // 文字の選択は xterm.js に任せ、それ以外の場所を押したときだけフォーカスを移す。
        if (!slot.current?.contains(event.target as Node)) {
          event.preventDefault();
          session?.focus();
        }
      }}
    >
      {!outside && (
        <BlockHeader info={block.info}>
          <Elapsed since={block.info.startedAt} />
          <span className="block-pill" data-kind="running">
            実行中
          </span>
        </BlockHeader>
      )}
      <div className="block-terminal" data-shown={shown}>
        <div className="block-terminal-clip" ref={clip}>
          <div ref={slot} />
        </div>
      </div>
    </section>
  );
}

/** 始まってからの時間。1秒ごとに進める。 */
function Elapsed({ since }: { since: number }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const elapsed = Math.max(0, now - since);
  // 1秒ごとにしか進めないので、小数は出さない。
  return (
    <span className="block-meta">
      {elapsed < 60_000 ? `${Math.floor(elapsed / 1000)}s` : formatDuration(elapsed)}
    </span>
  );
}
