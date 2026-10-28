# hull

Ubuntu 専用に削った、速いターミナルです。足りない機能はプラグインで足します。

- Wayland と直接話し、GPU (GLES3) で変わった行だけを描きます。何もしていないときは一切動きません
- タブ・分割・セッションは持たず、tmux に任せます
- 本体にない機能は、プロセスの外で動くプラグインで足します。検索、URL ヒント、Claude Code の状態のプラグインを同梱しています
- 日本語の入力 (IME の変換中の文字をその場に出す)、全角、Noto CJK へのフォールバックを最初から扱います
- GNOME の明暗の設定と等幅フォントに、そのまま従います

考え方は [docs/concept.md](docs/concept.md)、プラグインの書き方は [docs/plugins.md](docs/plugins.md)、
速さの測り方と結果は [docs/bench.md](docs/bench.md) にあります。

Ubuntu 24.04 以降の Wayland (GNOME) で動きます。X11 には対応しません。

## インストール

[Releases](https://github.com/Coxless/hull/releases) から `.deb` を入れます。

```bash
gh release download -R Coxless/hull -p '*.deb'
sudo apt install ./hull-terminal_*_amd64.deb
hull
```

アプリの一覧からも起動できます。削除は `sudo apt remove hull-terminal` です。
v0.2 までのチャット履歴型の hull-terminal を入れていれば、置き換わります
(v0.2 はタグ `v0.2.0` と Releases に残っています)。

## 使い方

| キー | 動き |
| --- | --- |
| Ctrl+Shift+C / Ctrl+Shift+V | コピー / 貼り付け |
| Shift+Insert、中ボタン | 選択範囲を貼り付け |
| Shift+PageUp / Shift+PageDown | 1 画面ずつスクロール |
| Ctrl+Shift+↑ / ↓ | 1 行ずつスクロール |
| Shift+Home / Shift+End | 一番上 / 一番下へ |
| Ctrl+Shift+Z / Ctrl+Shift+X | 前 / 次のプロンプトへ (シェルが OSC 133 を出すとき) |
| Ctrl++ / Ctrl+- / Ctrl+0 | 文字を大きく / 小さく / 元に戻す |
| Ctrl+Shift+F | 検索 (プラグイン) |
| Ctrl+Shift+U / Ctrl+Shift+Y | URL ヒントで開く / コピーする (プラグイン) |

- マウスで選ぶと、選択範囲 (中ボタンで貼れるもの) に入ります。ダブルクリックで単語、トリプルクリックで行を選びます
- アプリがマウスを使っているときも、Shift を押しながらなら選べます
- Ctrl を押しながら OSC 8 のリンクをクリックすると開きます
- `hull -e コマンド` で、シェルの代わりにコマンドを起動します

## 設定

`~/.config/hull/hull.toml` のファイル 1 つです。保存すると、開いている窓にそのまま反映されます。
書いていない項目は既定値になります。既定値を書いた例は `hull --print-config` で出せます。

```toml
[font]
family = "Ubuntu Sans Mono"   # 省略すると GNOME の等幅フォント
size = 12

[window]
theme = "auto"                # GNOME の明暗に従う。"light" か "dark" で固定

[[keybind]]
keys = "ctrl+shift+c"
action = "copy"

[[plugin]]
name = "search"
command = "/usr/lib/hull/plugins/hull-search"
```

## tmux と使う

hull はタブも分割も持たないので、tmux と一緒に使うのが普通です。次の設定で、窓の題名、フォーカス、クリップボード
(OSC 52)、色が tmux を通って hull に届きます。

```tmux
set -g default-terminal "tmux-256color"
set -as terminal-features ",xterm-256color:RGB:clipboard:extkeys:sync"
set -g set-titles on
set -g focus-events on
set -s set-clipboard on
set -s extended-keys on
```

## 開発

開発環境は Workshop (LXD) で作ります。手順は [WORKSHOP.md](WORKSHOP.md) にあります。

```bash
workshop launch && workshop run -- setup
workshop run -- test     # テスト
workshop run -- lint     # clippy
workshop run -- deb      # .deb を作る (target/deb/)
```

画面の動きはホストで確かめます (`target/debug/hull` をホストで起動します)。
`HULL_SCREENSHOT=ファイル.png` を付けて起動すると、1.5 秒後に窓の中身を PNG に書き出して終わります。
`HULL_TEST_KEYS="ctrl+shift+f,e,r,r"` を付けると、起動の 0.5 秒後からキーを打ったことにします。
`HULL_TRACE=1` で起動の段階ごとの時間を、`HULL_TRACE_LATENCY=1` でキーを押してから画面に出るまでの時間を出します。

開発の流れは GitHub Flow です。作業ブランチを切ってプルリクエストを出し、CI が通ったらスカッシュでマージします。

## ライセンス

MIT
