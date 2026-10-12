import { describe, expect, it } from "vitest";
import {
  NOT_BROWSING,
  browseHistory,
  historyEntries,
  onFirstLine,
  onLastLine,
  type HistoryCursor,
} from "./history";

describe("historyEntries", () => {
  it("コマンドのないブロックと空のコマンドを除き、続けて同じものをまとめる", () => {
    expect(historyEntries(["ls", null, "", "ls", "pwd", "  ", "ls"])).toEqual(["ls", "pwd", "ls"]);
  });
});

describe("browseHistory", () => {
  const entries = ["first", "second", "third"];

  /** キーを順に押したときの、入力欄のテキストの移り変わり。 */
  function press(keys: string, start = "draft") {
    let cursor: HistoryCursor = NOT_BROWSING;
    let text = start;
    const seen: string[] = [];
    for (const key of keys) {
      const moved = browseHistory(entries, cursor, key === "↑", text);
      if (moved) {
        ({ text, cursor } = moved);
      }
      seen.push(moved ? text : "-");
    }
    return { seen, cursor };
  }

  it("↑ で新しいものから古いものへたどり、いちばん古いところで止まる", () => {
    expect(press("↑↑↑↑").seen).toEqual(["third", "second", "first", "-"]);
  });

  it("↓ で戻り、最後は書きかけのテキストに戻る", () => {
    const { seen, cursor } = press("↑↑↓↓");
    expect(seen).toEqual(["third", "second", "third", "draft"]);
    expect(cursor).toEqual(NOT_BROWSING);
  });

  it("たどっていないときの ↓ と、履歴がないときの ↑ は何もしない", () => {
    expect(press("↓").seen).toEqual(["-"]);
    expect(browseHistory([], NOT_BROWSING, true, "x")).toBeNull();
  });
});

describe("カーソルのある行", () => {
  it("1行なら、どこにあっても最初の行で最後の行", () => {
    expect(onFirstLine("echo a", 3)).toBe(true);
    expect(onLastLine("echo a", 3)).toBe(true);
  });

  it("複数行では、改行をはさんだ側だけが変わる", () => {
    const text = "one\ntwo\nthree";
    expect([onFirstLine(text, 3), onLastLine(text, 3)]).toEqual([true, false]);
    expect([onFirstLine(text, 5), onLastLine(text, 5)]).toEqual([false, false]);
    expect([onFirstLine(text, 9), onLastLine(text, 9)]).toEqual([false, true]);
  });
});
