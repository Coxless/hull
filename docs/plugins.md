# hull のプラグイン

hull の本体にない機能は、プラグインで足します。プラグインは普通の実行ファイルで、どの言語でも書けます。
同梱の検索、URL ヒント、Claude Code の状態も、この仕組みで作っています。

## しくみ

- **設定に書いたものだけを、hull が起動する**：`~/.config/hull/hull.toml` の `[[plugin]]` に書きます。
  1 つも書かなければ、同梱の 3 つを起動します
- **Unix ソケットの上で、1 行に 1 つの JSON をやりとりする**：ソケットは fd 3 に渡します (環境変数 `HULL_PLUGIN_FD=3`)
- **落ちても本体は落ちない**：プラグインが終わったら、1 秒、2 秒、4 秒…と間をあけて起動し直します。1 分に 5 回落ちたら諦めます
- **本体はプラグインを待たない**：本体はプラグインごとに上限つき (1024 件) の列を持ちます。
  読み出しが遅れてあふれたら、古いイベントから捨てて、捨てた数を `dropped` で知らせます
- **PTY の生のバイト列は流さない**：解析済みのイベントだけを渡します
- **信頼**：プラグインはユーザーの権限で動き、PTY に書けて画面も読めます。入れたプラグインは、すべて信頼することになります

```toml
[[plugin]]
name = "logger"                  # キーバインドで呼ぶときの名前
command = "/usr/bin/python3"     # 実行ファイル
args = ["/home/me/logger.py"]    # 引数 (省略できる)
```

プラグインには次の環境変数を渡します。

| 変数 | 中身 |
| --- | --- |
| `HULL_PLUGIN_FD` | hull とつながったソケットの fd (いつも 3) |
| `HULL_PLUGIN_NAME` | 設定に書いた名前 |
| `HULL_VERSION` | hull の版 |

## 最小の例

届いたイベントをファイルに書くだけのプラグインです ([bench/event_logger.py](../bench/event_logger.py))。

```python
import os, socket

sock = socket.socket(fileno=int(os.environ["HULL_PLUGIN_FD"]))
with sock.makefile("r", encoding="utf-8") as events, open("/tmp/events.jsonl", "a") as out:
    for line in events:
        out.write(line)
```

Rust なら `hull-plugin` クレートの `Client` を使えます。

```rust
use hull_plugin::{Action, Client, HostEvent};

fn main() -> std::io::Result<()> {
    let mut client = Client::connect()?;
    while let Some(event) = client.next_event()? {
        if let HostEvent::Bell = event {
            client.send(&Action::Attention)?;
        }
    }
    Ok(())
}
```

## 行と列

行は**絶対番号**で指します。スクロールバックからあふれて捨てた行も数えるので、出力が流れても同じ行は同じ番号のままです。
列は画面のマスの位置 (0 始まり) で、全角の文字は 2 列を使います。`read_text` で読んだ文字列の位置を列に直すには、
文字の幅を足していきます (`hull_plugin::col_of`)。

## イベント (hull → プラグイン)

`event` で種類を表します。知らない種類のイベントは読み飛ばしてください (hull のほうが新しいことがあります)。

| event | 中身 | いつ |
| --- | --- | --- |
| `hello` | `version`, `cols`, `rows` | 起動した直後に 1 回 |
| `keybind` | `name` | このプラグインに割り当てたキーバインドが押された |
| `key` | `key`, `text`, `mods` | キー入力。`grab_keys` で受け取っているあいだだけ |
| `prompt` | `mark` (`prompt_start` / `command_start` / `output_start` / `command_end`), `exit_code` | OSC 133 |
| `cwd` | `path` | OSC 7 |
| `notify` | `title`, `body` | OSC 9 / OSC 777 |
| `progress` | `state`, `value` | OSC 9;4 (進捗表示。通知とは別) |
| `bell` | | ベル |
| `title` | `title` | 窓の題名の変更 (OSC 0 / 2) |
| `focus` | `focused` | 窓が前に来た / 後ろに回った |
| `selection` | `text` | マウスで文字を選んだ |
| `resize` | `cols`, `rows` | 端末の大きさが変わった |
| `dropped` | `count` | 列があふれて捨てたイベントの数 |
| `text` | `id`, `text` | `read_text` の答え |

`key` の `key` は、キーの名前です (`"a"`、`"enter"`、`"escape"`、`"backspace"`、`"up"`、`"pageup"`、`"f1"` など)。
`text` は、そのキーで入る文字です。IME で確定した文字は、`key` が空で `text` に入ります。`mods` は `"ctrl"`、`"shift"`、`"alt"`、`"super"` の配列です。

`text` の中身:

```json
{"event": "text", "id": 1, "text": {
  "first_line": 120, "lines": ["$ ls", "Cargo.toml  crates"],
  "view_top": 120, "cols": 100, "rows": 30, "alt_screen": false
}}
```

## 操作 (プラグイン → hull)

`action` で種類を表します。

| action | 中身 | 何をするか |
| --- | --- | --- |
| `write` | `data` | PTY に書く (シェルへの入力になる) |
| `grab_keys` | `grab` | キー入力を受け取る / 返す。受け取っているあいだ、キーは端末に行かない |
| `forward_key` | `key`, `text`, `mods` | 受け取ったキーを、端末に打ったことにする |
| `read_text` | `id`, `source` (`screen` / `all`) | 画面 (`screen`) か、スクロールバックを含むすべて (`all`) の文字を読む |
| `highlight` | `id`, `ranges` (`[{line, start, end}]`), `color` | 範囲の背景を塗る。同じ `id` で送り直すと置き換わる |
| `clear_highlight` | `id` (省略できる) | ハイライトを消す。省くと、このプラグインのものをすべて |
| `overlay` | `id`, `items` (`[{row か line, col, text, fg, bg}]`) | 文字を画面の上に重ねて出す。`row` は見えている画面の行 (負なら下から)、`line` は絶対番号 |
| `clear_overlay` | `id` (省略できる) | オーバーレイを消す |
| `scroll_to` | `line` (省略できる) | 行が見えるようにスクロールする。省くと一番下へ |
| `attention` | | 窓に注意を向けさせる (xdg-activation) |
| `clipboard` | `text`, `primary` | クリップボード (`primary` なら選択範囲) に書く |
| `open_url` | `url` | URL を既定のアプリで開く |
| `title_mark` | `mark` (省略できる) | 窓の題名の頭に印をつける。省くと外す |

色は `"#rrggbb"` です。

```json
{"action": "grab_keys", "grab": true}
{"action": "read_text", "id": 1, "source": "all"}
{"action": "highlight", "id": "matches", "ranges": [{"line": 121, "start": 0, "end": 10}], "color": "#f6d32d"}
{"action": "overlay", "id": "bar", "items": [{"row": -1, "col": 0, "text": " 検索: foo ", "bg": "#f6d32d"}]}
```

## キーバインド

プラグインを呼ぶキーバインドは、`plugin:<プラグインの名前>/<名前>` と書きます。押すと、そのプラグインに
`{"event": "keybind", "name": "<名前>"}` が届きます。

```toml
[[keybind]]
keys = "ctrl+shift+f"
action = "plugin:search/search"
```

既定のキーバインドは次のとおりです。

| キー | 動き |
| --- | --- |
| Ctrl+Shift+F | 検索 (`plugin:search/search`) |
| Ctrl+Shift+U | URL ヒントで開く (`plugin:url-hints/open`) |
| Ctrl+Shift+Y | URL ヒントでコピーする (`plugin:url-hints/copy`) |

## tmux の中で

tmux は外側の端末に何でも素通しにするわけではありません。ベル、タイトル (`set-titles`)、フォーカス (`focus-events`) は届きますが、
OSC 7 / 9 / 133 / 777 は普通は届きません (`allow-passthrough` と DCS で包めば届きます)。スクロールバックは tmux が持つので、
hull の側は空です。

tmux のスクロールバックや OSC を使いたいプラグインは、自分で tmux に問い合わせてください (`tmux capture-pane` や hook)。
プラグインは普通の実行ファイルなので、本体を通さずに tmux と話せます。届くかどうかは
[bench/tmux_passthrough.py](../bench/tmux_passthrough.py) で確かめられます。

## 同梱のプラグイン

| 名前 | 何をするか |
| --- | --- |
| `search` | 画面の下に入力欄を出し、一致した所をハイライトして、選んだ一致までスクロールする。Enter で前、Shift+Enter で次、Esc で閉じる |
| `url-hints` | 見えている画面の URL にラベルを付ける。ラベルを打つと開く (`copy` ならコピーする) |
| `claude-status` | ベル、OSC 9、題名の記号から、Claude Code が実行中か入力待ちかを追い続け、題名の印 (⏳ / 🔔) と通知に出す。窓が後ろにあるときに入力待ちになったら、通知を出して窓に注意を向けさせる |

Claude Code の通知をベルで出すには、Claude Code の設定で通知の方法を `terminal_bell` にします。
