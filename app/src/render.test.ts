import { Terminal } from "@xterm/headless";
import type { IBuffer } from "@xterm/xterm";
import { describe, expect, it } from "vitest";
import { renderBuffer } from "./render";

/** 出力を端末に通して、HTML にする。 */
async function render(output: string, options: { cols?: number; maxLines?: number } = {}) {
  const term = new Terminal({
    cols: options.cols ?? 40,
    rows: 10,
    scrollback: 1000,
    allowProposedApi: true,
  });
  await new Promise<void>((resolve) => term.write(output, resolve));
  return renderBuffer(term.buffer.normal as unknown as IBuffer, options.maxLines);
}

describe("renderBuffer", () => {
  it("色のない出力は、そのままの文字にする", async () => {
    const { lines, omitted } = await render("hello\r\nworld  \r\n");
    expect(lines).toEqual(["hello", "world"]);
    expect(omitted).toBe(false);
  });

  it("末尾の空行は落とし、途中の空行は残す", async () => {
    expect((await render("a\r\n\r\nb\r\n\r\n\r\n")).lines).toEqual(["a", "", "b"]);
    expect((await render("")).lines).toEqual([]);
  });

  it("HTML の特別な文字を置き換える", async () => {
    expect((await render('<b> & "x"')).lines).toEqual(["&lt;b&gt; &amp; &quot;x&quot;"]);
  });

  it("基本の色は、テーマの CSS 変数にする", async () => {
    const { lines } = await render("\x1b[31mred\x1b[0m plain \x1b[42mbg\x1b[0m");
    expect(lines).toEqual([
      '<span style="color:var(--ansi-1);">red</span> plain <span style="background:var(--ansi-2);">bg</span>',
    ]);
  });

  it("太字の基本 8 色は、明るい色にする", async () => {
    const { lines } = await render("\x1b[1;34mblue\x1b[0m \x1b[1;94mbright\x1b[0m");
    expect(lines).toEqual([
      '<span style="color:var(--ansi-12);font-weight:bold;">blue</span> <span style="color:var(--ansi-12);font-weight:bold;">bright</span>',
    ]);
  });

  it("256 色と RGB の色を、16 進の色にする", async () => {
    const { lines } = await render(
      "\x1b[38;5;196ma\x1b[38;5;244mb\x1b[38;2;1;2;255mc\x1b[0m\x1b[48;5;16md\x1b[0m",
    );
    expect(lines).toEqual([
      '<span style="color:#ff0000;">a</span><span style="color:#808080;">b</span><span style="color:#0102ff;">c</span><span style="background:#000000;">d</span>',
    ]);
  });

  it("反転は、文字と背景の色を入れ替える", async () => {
    const { lines } = await render("\x1b[7mx\x1b[0m\x1b[7;31my\x1b[0m");
    expect(lines).toEqual([
      '<span style="color:var(--term-bg);background:var(--term-fg);">x</span><span style="color:var(--term-bg);background:var(--ansi-1);">y</span>',
    ]);
  });

  it("文字の飾りを CSS にする", async () => {
    const { lines } = await render("\x1b[2md\x1b[0m\x1b[3mi\x1b[0m\x1b[4;9mu\x1b[0m");
    expect(lines).toEqual([
      '<span style="opacity:0.6;">d</span><span style="font-style:italic;">i</span><span style="text-decoration:underline line-through;">u</span>',
    ]);
  });

  it("カーソル移動と上書きを反映する", async () => {
    // 進み具合の表示のように、同じ行を書き直す出力。
    const { lines } = await render("10%\r50%\r100%\r\nabc\x1b[1D\x1b[1DX\r\n\x1b[2K\x1b[1Atop");
    expect(lines).toEqual(["100%", "top"]);
  });

  it("全角文字を1文字として出し、行末の背景色は残す", async () => {
    const { lines } = await render("日本語 ok\r\n\x1b[41m  \x1b[0m  ");
    expect(lines).toEqual(["日本語 ok", '<span style="background:var(--ansi-1);">  </span>']);
  });

  it("幅で折り返された行は、端末の行のままにする", async () => {
    expect((await render("abcdefgh", { cols: 5 })).lines).toEqual(["abcde", "fgh"]);
  });

  it("行数の上限を超えたら、最後のぶんだけを残す", async () => {
    const output = Array.from({ length: 50 }, (_, i) => `line ${i + 1}\r\n`).join("");
    const { lines, omitted } = await render(output, { maxLines: 3 });
    expect(lines).toEqual(["line 48", "line 49", "line 50"]);
    expect(omitted).toBe(true);
  });

  it("見えない文字は、空白にする", async () => {
    expect((await render("a\x1b[8msecret\x1b[0mb")).lines).toEqual(["a      b"]);
  });
});
