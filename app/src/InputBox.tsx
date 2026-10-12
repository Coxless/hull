import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useBlockSession } from "./BlockView";
import { shortenPath, truncatePath } from "./format";
import {
  NOT_BROWSING,
  browseHistory,
  historyEntries,
  onFirstLine,
  onLastLine,
  type HistoryCursor,
} from "./history";
import { useSession } from "./store";

/** ディレクトリの表示の長さの上限 (文字数)。超えたら先頭を省く。 */
const MAX_PATH_CHARS = 48;

/** 入力欄が伸びる行数の上限。これを超えたら、中でスクロールする。 */
const MAX_ROWS = 8;

/**
 * 変換の確定から、この時間のうちに届いた Enter は、確定のための Enter とみなす。
 * WebKit は、確定の Enter を「変換中ではない」キーとして、確定の直後に知らせることがある。
 */
const COMPOSITION_GUARD_MS = 30;

/** 下に固定した入力欄。Enter で実行し、Shift+Enter で改行する。 */
export function InputBox() {
  const session = useBlockSession();
  const connection = useSession((state) => state.connection);
  const exitCode = useSession((state) => state.exitCode);
  const problem = useSession((state) => state.problem);
  const ready = useSession((state) => state.ready);
  const pending = useSession((state) => state.pending !== null);
  const running = useSession((state) => state.running !== null);
  const rejected = useSession((state) => state.rejected);
  const prompt = useSession((state) => state.prompt);
  const home = useSession((state) => state.home);
  const blocks = useSession((state) => state.blocks);

  const [text, setText] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const textarea = useRef<HTMLTextAreaElement>(null);
  const cursor = useRef<HistoryCursor>(NOT_BROWSING);
  const composedAt = useRef(0);

  const ended = connection === "exited" || connection === "disconnected";
  const entries = useMemo(
    () => historyEntries(blocks.map((block) => block.info.command)),
    [blocks],
  );

  // 実行中はブロックに、それ以外は入力欄にフォーカスを置く。
  useEffect(() => {
    if (!running) {
      textarea.current?.focus();
    }
  }, [running, connection]);
  useEffect(() => {
    const focus = () => {
      if (useSession.getState().running !== null) {
        session?.focus();
      } else {
        textarea.current?.focus();
      }
    };
    // 出力の文字を選ぶなどして入力欄の外にフォーカスがあっても、文字を打てば入力欄に入る。
    const onKeyDown = (event: KeyboardEvent) => {
      const typing = event.key.length === 1 && !event.ctrlKey && !event.altKey && !event.metaKey;
      const outside = !(event.target instanceof Element) || !event.target.closest("textarea");
      if (typing && outside && useSession.getState().running === null) {
        textarea.current?.focus();
      }
    };
    window.addEventListener("focus", focus);
    window.addEventListener("keydown", onKeyDown, true);
    return () => {
      window.removeEventListener("focus", focus);
      window.removeEventListener("keydown", onKeyDown, true);
    };
  }, [session]);

  // 実行されなかったテキストを入力欄に戻す (そのあいだに書き始めていたら、戻さない)。
  useEffect(() => {
    if (!rejected) {
      return;
    }
    setText((current) => (current === "" ? rejected.text : current));
    setNotice(
      rejected.reason === "incomplete"
        ? "入力が途中なので、実行しませんでした (引用符や括弧が閉じていません)"
        : "コマンドの実行中なので、受け付けられませんでした",
    );
  }, [rejected]);

  // 行数に合わせて高さを変える。
  useLayoutEffect(() => {
    const element = textarea.current!;
    element.style.height = "auto";
    const style = getComputedStyle(element);
    const line = parseFloat(style.lineHeight);
    const padding = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
    element.style.height = `${Math.min(element.scrollHeight, line * MAX_ROWS + padding)}px`;
  }, [text]);

  const edit = (next: string) => {
    setText(next);
    setNotice(null);
    cursor.current = NOT_BROWSING;
  };

  const submit = () => {
    if (ended) {
      // bash が終了したあとや、切れたあとは、Enter でつなぎ直す (新しい bash が起動する)。
      session?.connect();
      return;
    }
    if (session?.run(text)) {
      edit("");
    }
  };

  /** ↑↓ で履歴をたどる。たどったら true。 */
  const browse = (older: boolean): boolean => {
    const element = textarea.current!;
    // 履歴から出したテキストを書き換えていなければ、カーソルの行にかかわらず続けてたどる。
    const browsing = cursor.current.index !== null;
    const caret = element.selectionStart;
    const atEdge = older ? onFirstLine(text, caret) : onLastLine(text, caret);
    if (element.selectionStart !== element.selectionEnd || (!browsing && !atEdge)) {
      return false;
    }
    const moved = browseHistory(entries, cursor.current, older, text);
    if (!moved) {
      return browsing;
    }
    setText(moved.text);
    setNotice(null);
    cursor.current = moved.cursor;
    return true;
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    const native = event.nativeEvent;
    if (event.key === "Enter" && !event.shiftKey) {
      // 日本語の変換を確定する Enter では実行しない。
      const composing =
        native.isComposing ||
        native.keyCode === 229 ||
        performance.now() - composedAt.current < COMPOSITION_GUARD_MS;
      if (!composing) {
        event.preventDefault();
        submit();
      }
      return;
    }
    if (native.isComposing || native.keyCode === 229) {
      return;
    }
    if ((event.key === "ArrowUp" || event.key === "ArrowDown") && !event.shiftKey) {
      if (browse(event.key === "ArrowUp")) {
        event.preventDefault();
      }
      return;
    }
    // 送ったテキストに bash が応えないときは、Ctrl+C で取り消せる。
    const nothingSelected = event.currentTarget.selectionStart === event.currentTarget.selectionEnd;
    if (event.key === "c" && event.ctrlKey && !event.shiftKey && pending && nothingSelected) {
      event.preventDefault();
      session?.interrupt();
    }
  };

  let placeholder = "コマンドを入力…";
  if (connection === "connecting") {
    placeholder = "bash につないでいます…";
  } else if (connection === "exited") {
    placeholder = `bash が終了しました (終了コード ${exitCode ?? "?"})。Enter で新しい bash を起動します`;
  } else if (connection === "disconnected") {
    placeholder = `${problem ?? "デーモンとの接続が切れました"}。Enter でつなぎ直します`;
  } else if (running) {
    placeholder = "キー入力は、実行中のブロックに届きます";
  } else if (pending) {
    placeholder = "bash の応答を待っています… (Ctrl+C で取り消し)";
  } else if (!ready) {
    placeholder = "bash の準備を待っています…";
  }

  return (
    <div className="composer">
      <div
        className="composer-box"
        data-active={(ready || ended) && !running}
        data-running={running}
        onMouseDown={(event) => {
          // 実行中は、入力欄を押してもブロックにフォーカスを置いたままにする。
          if (running) {
            event.preventDefault();
            session?.focus();
          }
        }}
      >
        <div className="composer-chips">
          {running ? (
            <span className="composer-chip" data-kind="running">
              ブロックに入力中
            </span>
          ) : (
            <>
              {prompt.cwd !== null && (
                <span className="composer-chip composer-chip-path" title={prompt.cwd}>
                  {truncatePath(shortenPath(prompt.cwd, home), MAX_PATH_CHARS)}
                </span>
              )}
              <span className="composer-chip">bash</span>
              {prompt.branch !== null && (
                <span className="composer-chip" title="git ブランチ">
                  <svg width="11" height="11" viewBox="0 0 24 24" aria-hidden="true">
                    <circle cx="6" cy="6" r="2.5" />
                    <circle cx="6" cy="18" r="2.5" />
                    <circle cx="18" cy="8" r="2.5" />
                    <path d="M6 8.5v7M18 10.5c0 4-6 3-10 6" />
                  </svg>
                  {prompt.branch}
                </span>
              )}
            </>
          )}
        </div>
        <textarea
          ref={textarea}
          className="composer-input"
          rows={1}
          value={text}
          readOnly={running}
          placeholder={placeholder}
          aria-label="コマンド入力"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          onChange={(event) => edit(event.target.value)}
          onCompositionEnd={() => {
            composedAt.current = performance.now();
          }}
          onKeyDown={onKeyDown}
        />
        <div className="composer-foot">
          {notice ? (
            <span className="composer-notice">{notice}</span>
          ) : (
            <>
              <span>Enter 実行</span>
              <span>Shift+Enter 改行</span>
              <span>↑ 履歴</span>
            </>
          )}
          <span className="composer-spacer" />
          <button
            type="button"
            className="composer-run"
            disabled={!ended && !ready}
            onClick={() => {
              submit();
              textarea.current?.focus();
            }}
          >
            {ended ? "起動" : "実行"}
          </button>
        </div>
      </div>
    </div>
  );
}
