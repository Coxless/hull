// 入力欄の履歴 (↑↓)。このセッションで実行したコマンドをたどる。

export interface HistoryCursor {
  /** いま見ている履歴の位置。たどっていなければ null。 */
  index: number | null;
  /** たどり始める前に、入力欄に書いてあったテキスト。 */
  draft: string;
}

export const NOT_BROWSING: HistoryCursor = { index: null, draft: "" };

/** 実行したコマンドを、履歴の並び (古い順) にする。続けて同じものは1つにまとめる。 */
export function historyEntries(commands: (string | null)[]): string[] {
  const entries: string[] = [];
  for (const command of commands) {
    if (command && command.trim() !== "" && entries[entries.length - 1] !== command) {
      entries.push(command);
    }
  }
  return entries;
}

/**
 * 履歴を1つ前 (`older`) か、1つ後へたどる。動けないとき (これ以上ない、たどっていないのに
 * 後へ行こうとした) は null。いちばん新しいものから後へ進むと、書きかけのテキストに戻る。
 */
export function browseHistory(
  entries: string[],
  cursor: HistoryCursor,
  older: boolean,
  current: string,
): { text: string; cursor: HistoryCursor } | null {
  if (cursor.index === null) {
    if (!older || entries.length === 0) {
      return null;
    }
    const index = entries.length - 1;
    return { text: entries[index], cursor: { index, draft: current } };
  }
  const index = cursor.index + (older ? -1 : 1);
  if (index < 0) {
    return null;
  }
  if (index >= entries.length) {
    return { text: cursor.draft, cursor: NOT_BROWSING };
  }
  return { text: entries[index], cursor: { index, draft: cursor.draft } };
}

/** カーソルが1行目にあるか (↑ で履歴をたどってよいか)。 */
export function onFirstLine(text: string, caret: number): boolean {
  return !text.slice(0, caret).includes("\n");
}

/** カーソルが最後の行にあるか (↓ で履歴をたどってよいか)。 */
export function onLastLine(text: string, caret: number): boolean {
  return !text.slice(caret).includes("\n");
}
