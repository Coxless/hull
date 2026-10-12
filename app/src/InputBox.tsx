import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useBlockSession } from "./BlockView";
import { useSession } from "./store";

/** 入力欄が伸びる行数の上限。これを超えたら、中でスクロールする。 */
const MAX_ROWS = 8;

/** 下に固定した入力欄。Enter で実行する。 */
export function InputBox() {
  const session = useBlockSession();
  const connection = useSession((state) => state.connection);
  const exitCode = useSession((state) => state.exitCode);
  const problem = useSession((state) => state.problem);
  const ready = useSession((state) => state.ready);
  const running = useSession((state) => state.running !== null);
  const [text, setText] = useState("");
  const textarea = useRef<HTMLTextAreaElement>(null);

  const ended = connection === "exited" || connection === "disconnected";

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
    window.addEventListener("focus", focus);
    return () => window.removeEventListener("focus", focus);
  }, [session]);

  // 行数に合わせて高さを変える。
  useLayoutEffect(() => {
    const element = textarea.current!;
    element.style.height = "auto";
    const style = getComputedStyle(element);
    const line = parseFloat(style.lineHeight);
    const padding = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
    element.style.height = `${Math.min(element.scrollHeight, line * MAX_ROWS + padding)}px`;
  }, [text]);

  const submit = () => {
    if (ended) {
      // bash が終了したあとや、切れたあとは、Enter でつなぎ直す (新しい bash が起動する)。
      session?.connect();
      return;
    }
    if (session?.run(text)) {
      setText("");
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
    placeholder = "コマンドの実行中です";
  } else if (!ready) {
    placeholder = "bash の応答を待っています…";
  }

  return (
    <div className="composer">
      <div className="composer-box" data-active={ready || ended}>
        <textarea
          ref={textarea}
          className="composer-input"
          rows={1}
          value={text}
          placeholder={placeholder}
          aria-label="コマンド入力"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          onChange={(event) => setText(event.target.value)}
          onKeyDown={(event) => {
            // 日本語の変換を確定する Enter では実行しない。
            if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              submit();
            }
          }}
        />
        <div className="composer-foot">
          <span>Enter 実行</span>
          <span>Shift+Enter 改行</span>
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
