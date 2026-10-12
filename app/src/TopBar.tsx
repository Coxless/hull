import { useSession } from "./store";

/** 生の表示とブロックの表示を切り替えるキー。 */
export const TOGGLE_RAW_KEYS = "Ctrl+Shift+R";

interface Props {
  raw: boolean;
  onToggleRaw(): void;
}

/** 上のバー。失敗したブロックだけに絞り込む切り替えと、生の表示への切り替えを置く。 */
export function TopBar({ raw, onToggleRaw }: Props) {
  return (
    <header className="topbar">
      <span className="topbar-title">{raw ? "生の表示" : "bash"}</span>
      <span className="topbar-spacer" />
      {!raw && <Filter />}
      <button
        type="button"
        className="topbar-button"
        aria-pressed={raw}
        title={`区切りを通さない bash の出力をそのまま見る (${TOGGLE_RAW_KEYS})`}
        onClick={onToggleRaw}
      >
        {raw ? "ブロックの表示に戻る" : "生の表示"}
      </button>
    </header>
  );
}

function Filter() {
  const failedOnly = useSession((state) => state.failedOnly);
  const total = useSession(
    (state) =>
      state.blocks.filter((block) => block.info.command !== null).length +
      (state.running && state.running.info.command !== null ? 1 : 0),
  );
  const failed = useSession((state) => state.blocks.filter(isFailed).length);
  const show = (only: boolean) => useSession.setState({ failedOnly: only });
  return (
    <div className="topbar-filter" role="group" aria-label="表示の絞り込み">
      <button type="button" aria-pressed={!failedOnly} onClick={() => show(false)}>
        すべて {total}
      </button>
      <button type="button" aria-pressed={failedOnly} onClick={() => show(true)}>
        失敗 {failed}
      </button>
    </div>
  );
}

/** 0 以外の終了コードで終わったか。 */
export function isFailed(block: { result: { exitCode: number | null } }): boolean {
  return block.result.exitCode !== null && block.result.exitCode !== 0;
}
