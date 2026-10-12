import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { memo, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { useBlockSession } from "./BlockView";
import { formatClock, formatDuration } from "./format";
import type { BlockInfo } from "./pty";
import { plainText, type Rendered } from "./render";
import { outputs, useSession, type BlockEntry } from "./store";

/** 折りたたんだときに見せる行数。 */
const FOLD_LINES = 30;

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

/** 終了したブロック。出力は、色付きの静的な HTML で見せる。 */
export const Block = memo(function Block({ block }: Props) {
  const { info, result } = block;
  const section = useRef<HTMLElement>(null);
  const output = useOutput(block, section, !result.fullscreen);

  // コマンドの外で出た出力 (`.bashrc` の出力など) は、見出しを付けない。
  if (info.command === null) {
    return (
      <section className="block block-outside" ref={section}>
        <Output output={output} />
      </section>
    );
  }

  const failed = result.exitCode !== null && result.exitCode !== 0;
  return (
    <section className="block" data-failed={failed} ref={section}>
      <BlockHeader info={info}>
        <span className="block-meta">{formatDuration(result.finishedAt - info.startedAt)}</span>
        {result.exitCode !== null && (
          <span className="block-pill" data-kind={failed ? "failed" : "ok"}>
            exit {result.exitCode}
          </span>
        )}
        <Actions block={block} />
      </BlockHeader>
      {result.fullscreen ? (
        <div className="block-fullscreen">
          全画面で実行しました。画面の内容はブロックに残りません。
        </div>
      ) : (
        <Output output={output} />
      )}
    </section>
  );
});

/** ブロックの操作。コマンドのコピー、出力のコピー、もう一度実行。 */
function Actions({ block }: Props) {
  const session = useBlockSession();
  const ready = useSession((state) => state.ready && state.connection === "connected");
  const command = block.info.command ?? "";

  const copyOutput = async () => {
    // 開き直して戻したブロックで、まだ画面に入っていないものは、ここで出力を取る。
    const rendered = outputs.get(block.key) ?? (await session?.loadOutput(block));
    if (!rendered) {
      throw new Error("出力は残っていません");
    }
    await writeText(plainText(rendered.lines));
  };

  return (
    <span className="block-actions">
      {command !== "" && (
        <CopyButton label="コマンドをコピー" copy={() => writeText(command)}>
          <path d="M7 8l-4 4 4 4M17 8l4 4-4 4M14 4l-4 16" />
        </CopyButton>
      )}
      {!block.result.fullscreen && (
        <CopyButton label="出力をコピー" copy={copyOutput}>
          <rect x="9" y="9" width="11" height="11" rx="2" />
          <path d="M5 15V5a2 2 0 0 1 2-2h10" />
        </CopyButton>
      )}
      {command !== "" && (
        <button
          type="button"
          className="block-action"
          aria-label="もう一度実行"
          title="もう一度実行"
          disabled={!ready}
          onClick={() => session?.run(command)}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M3 12a9 9 0 1 0 3-6.7" />
            <path d="M3 4v5h5" />
          </svg>
        </button>
      )}
    </span>
  );
}

interface CopyButtonProps {
  label: string;
  copy(): Promise<void>;
  /** アイコンの中身 (SVG の図形)。 */
  children: ReactNode;
}

/** コピーのボタン。コピーできたら、少しの間だけチェックの印にする。 */
function CopyButton({ label, copy, children }: CopyButtonProps) {
  const [state, setState] = useState<"idle" | "done" | "failed">("idle");
  useEffect(() => {
    if (state === "idle") {
      return;
    }
    const timer = window.setTimeout(() => setState("idle"), 1200);
    return () => window.clearTimeout(timer);
  }, [state]);

  const title =
    state === "done" ? "コピーしました" : state === "failed" ? "コピーできません" : label;
  return (
    <button
      type="button"
      className="block-action"
      data-state={state}
      aria-label={label}
      title={title}
      onClick={() => {
        copy().then(
          () => setState("done"),
          () => setState("failed"),
        );
      }}
    >
      <svg width="14" height="14" viewBox="0 0 24 24" aria-hidden="true">
        {state === "done" ? <path d="M5 12l5 5 9-10" /> : children}
      </svg>
    </button>
  );
}

/** まだ取っていない / もう残っていない。 */
type OutputState = Rendered | "pending" | "gone";

/**
 * ブロックの出力。開き直して戻したブロックは、画面に入ったときにデーモンから取る
 * (数百ブロックを戻しても、見えないぶんは変換しない)。
 */
function useOutput(
  block: BlockEntry,
  section: React.RefObject<HTMLElement | null>,
  wanted: boolean,
): OutputState {
  const session = useBlockSession();
  const [output, setOutput] = useState<OutputState>(() => outputs.get(block.key) ?? "pending");
  // 読み込む前の高さ。上にあるブロックが伸びたぶん、一覧を送り直すために使う。
  const heightBefore = useRef<number | null>(null);

  useEffect(() => {
    const element = section.current;
    if (!wanted || output !== "pending" || !session || !element) {
      return;
    }
    let cancelled = false;
    const observer = new IntersectionObserver(
      (entries) => {
        if (!entries.some((entry) => entry.isIntersecting)) {
          return;
        }
        observer.disconnect();
        void session.loadOutput(block).then((rendered) => {
          if (!cancelled) {
            heightBefore.current = element.offsetHeight;
            setOutput(rendered ?? "gone");
          }
        });
      },
      { rootMargin: "600px 0px" },
    );
    observer.observe(element);
    return () => {
      cancelled = true;
      observer.disconnect();
    };
  }, [block, output, section, session, wanted]);

  // WebKit は、上の要素が伸びても見ている場所を保たない。見ている場所より上のブロックが
  // 伸びたら、そのぶん一覧を送って、見ているものが動かないようにする。
  useLayoutEffect(() => {
    const element = section.current;
    const before = heightBefore.current;
    heightBefore.current = null;
    const list = element?.closest(".blocks");
    if (!element || before === null || !list) {
      return;
    }
    if (element.getBoundingClientRect().top < list.getBoundingClientRect().top) {
      list.scrollTop += element.offsetHeight - before;
    }
  }, [output, section]);

  return output;
}

function Output({ output }: { output: OutputState }) {
  const [expanded, setExpanded] = useState(false);
  if (output === "pending") {
    return <div className="block-note">出力を読み込んでいます…</div>;
  }
  if (output === "gone") {
    return <div className="block-note">出力は残っていません</div>;
  }
  if (output.lines.length === 0) {
    return null;
  }
  const hidden = Math.max(0, output.lines.length - FOLD_LINES);
  const lines = expanded || hidden === 0 ? output.lines : output.lines.slice(-FOLD_LINES);
  return (
    <>
      {output.omitted && (expanded || hidden === 0) && (
        <div className="block-note">先頭の出力は省かれています</div>
      )}
      {hidden > 0 && !expanded && <div className="block-note">… 上に {hidden} 行</div>}
      <pre className="block-output" dangerouslySetInnerHTML={{ __html: lines.join("\n") }} />
      {hidden > 0 && (
        <div className="block-fold">
          <button type="button" onClick={() => setExpanded(!expanded)}>
            <svg width="12" height="12" viewBox="0 0 24 24" aria-hidden="true">
              <path d={expanded ? "M6 15l6-6 6 6" : "M6 9l6 6 6-6"} />
            </svg>
            {expanded ? "折りたたむ" : `すべて表示 (${output.lines.length} 行)`}
          </button>
        </div>
      )}
    </>
  );
}
