// 配布パッケージに入れるデーモンを用意する。
//
// Tauri は同梱する実行ファイルを「名前-ターゲットトリプル」で探すので、
// リリースビルドしたデーモンをその名前で src-tauri/binaries/ に置く。

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const app = dirname(dirname(fileURLToPath(import.meta.url)));
const root = dirname(app);

const triple = /^host: (.+)$/m.exec(execFileSync("rustc", ["-vV"], { encoding: "utf8" }))?.[1];
if (!triple) {
  throw new Error("rustc -vV からターゲットトリプルを読み取れません");
}
const exe = triple.includes("windows") ? ".exe" : "";

execFileSync("cargo", ["build", "--release", "-p", "hull-terminal-daemon"], {
  cwd: root,
  stdio: "inherit",
});

const binaries = join(app, "src-tauri", "binaries");
mkdirSync(binaries, { recursive: true });
const target = join(binaries, `hull-terminal-daemon-${triple}${exe}`);
copyFileSync(join(root, "target", "release", `hull-terminal-daemon${exe}`), target);
console.log(`デーモンを ${target} に置きました`);
