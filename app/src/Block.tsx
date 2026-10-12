import { memo, type ReactNode } from "react";
import { formatClock, formatDuration } from "./format";
import type { BlockInfo } from "./pty";
import { outputs, type BlockEntry } from "./store";

interface HeaderProps {
  info: BlockInfo;
  /** 時刻の右に並べるもの (実行時間や終了コード)。 */
  children: ReactNode;
}

/** ブロックの見出し。コマンドと開始時刻を出す。 */
export function BlockHeader({ info, children }: HeaderProps) {
  const command = info.command ?? "";
  return (
    <div className="block-head">
      <span className="block-mark" aria-hidden="true">
        ❯
      </span>
      {command === "" ? (
        <span className="block-command block-command-unknown">入力欄を通さずに実行</span>
      ) : (
        <span className="block-command" title={command}>
          {command.replaceAll("\n", " ⏎ ")}
        </span>
      )}
      <span className="block-meta">{formatClock(info.startedAt)}</span>
      {children}
    </div>
  );
}

interface Props {
  block: BlockEntry;
}

/** 終了したブロック。 */
export const Block = memo(function Block({ block }: Props) {
  const { info, result } = block;
  const lines = outputs.get(block.key);

  // コマンドの外で出た出力 (`.bashrc` の出力など) は、見出しを付けない。
  if (info.command === null) {
    if (!lines || lines.length === 0) {
      return null;
    }
    return (
      <section className="block block-outside">
        <Output lines={lines} />
      </section>
    );
  }

  const failed = result.exitCode !== null && result.exitCode !== 0;
  return (
    <section className="block" data-failed={failed}>
      <BlockHeader info={info}>
        <span className="block-meta">{formatDuration(result.finishedAt - info.startedAt)}</span>
        {result.exitCode !== null && (
          <span className="block-pill" data-kind={failed ? "failed" : "ok"}>
            exit {result.exitCode}
          </span>
        )}
      </BlockHeader>
      {result.fullscreen ? (
        <div className="block-fullscreen">
          全画面で実行しました。画面の内容はブロックに残りません。
        </div>
      ) : (
        <Output lines={lines} />
      )}
    </section>
  );
});

function Output({ lines }: { lines: string[] | undefined }) {
  if (!lines || lines.length === 0) {
    return null;
  }
  return <pre className="block-output">{lines.join("\n")}</pre>;
}
