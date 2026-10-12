# hull-terminal

コマンドと出力を1往復ずつブロックとして扱う、チャット履歴型のターミナルです。

いまは Phase 2 (v0.2) で、**下の入力欄でコマンドを実行するたびに、コマンドと出力が1枚のブロックとして上に積み上がります**。
ウィンドウを閉じても bash は裏で動き続け、開き直すとブロックの一覧ごと戻ります。
セッションの保存とサイドバーは、まだありません (Phase 3)。
設計と計画は [docs/concept.md](docs/concept.md) と [docs/phases/](docs/phases/) にあります。

Ubuntu 24.04 以降で動きます (24.04 / 26.04 で動作を確認)。シェルは bash だけに対応しています。

## インストール (Ubuntu)

[Releases](https://github.com/Coxless/hull/releases) から `.deb` を入れます。

```bash
gh release download -R Coxless/hull -p '*.deb'   # 最新版。gh がなければ Releases のページからダウンロード
sudo apt install ./hull-terminal_*_amd64.deb
hull                                             # hull-terminal でも、アプリの一覧からも起動できます
```

更新は、新しい `.deb` を同じ手順で入れ直します。開いているセッションは、先に `exit` で終了してください。
古い版のデーモンが残っていると、新しい版の画面とはつながらず、「プロトコルのバージョンが合いません」と表示されます
(v0.1 から v0.2 への更新では、必ずこうなります)。そのときは、残っているデーモンを終了してから Enter を押します。

```bash
pkill -f hull-terminal-daemon   # 動いている bash も一緒に終了します
```

削除は `sudo apt remove hull-terminal` です。

## 使い方

**ブロック**

- 下の入力欄にコマンドを書いて Enter で実行します。コマンドと出力が、1枚のブロックになって上に積み上がります
- ブロックには、開始時刻・実行時間・終了コードが出ます。失敗したブロックは赤い枠になり、上の「失敗」で失敗だけに絞り込めます
- 実行中のブロックは本物のターミナルで、キー入力はすべてそこに届きます (vim や Claude Code も動きます。Claude Code では Shift+Enter で改行できます)
- 終わったブロックは、色付きのまま固定されます。長い出力は最後の 30 行だけを表示し、「すべて表示」で広げます
- vim や less などの全画面アプリは、終了後に「全画面で実行しました」とだけ残ります
- ブロックの右上のボタンで、コマンドのコピー・出力のコピー・もう一度実行ができます

**入力欄**

| キー | 動き |
| --- | --- |
| Enter | 実行 (日本語の変換を確定する Enter では実行しません) |
| Shift+Enter | 改行 (複数行は、まとめて1つのブロックになります) |
| ↑ / ↓ | このセッションで実行したコマンドの履歴 |
| Tab | ファイル名の補完 (候補が複数あれば、共通する部分まで補って一覧を出します) |
| Ctrl+Shift+C / V | コピー / 貼り付け (実行中のブロックと、終了したブロックの出力) |
| Ctrl+Shift+R | 生の表示とブロックの表示の切り替え |

入力欄の上には、今のディレクトリと git ブランチが出ます。
`echo "abc` のように入力が途中のテキストは実行されず、入力欄に残ります。

**セッション**

- ウィンドウを閉じても bash は裏で動き続け、開き直すと同じ bash につながって、ブロックの一覧と実行中のブロックが戻ります
- `exit` で bash を終了すると、裏のデーモン (`hull-terminal-daemon`) も終了し、ブロックの記録も消えます。Enter で新しい bash を起動できます
- 画面は GPU があれば WebGL、なければ DOM で描きます (下のステータスバーに表示)

**生の表示**

bash の出力を区切らずにそのまま見る、Phase 1 と同じ画面です。ブロックの区切りがおかしいときや、
プロンプトを含む出力を確かめたいときに使います。Ctrl+Shift+R か、右上の「生の表示」で切り替えます。

**いまの制限**

- ブロックの記録はメモリにあるだけで、bash を終了すると消えます。出力は1ブロックにつき最後の 1MB、全体で 32MB までです
- プロンプトを表示している間に出たバックグラウンドのジョブの出力は、ブロックに出ません (生の表示では見えます)
- Tab の補完はファイル名だけです。コマンド名や引数は補完しません
- ブロックに区切るために、bash を `--rcfile` で起動して `~/.bashrc` のあとに統合スクリプトを読み込みます。`PROMPT_COMMAND` を丸ごと置き換える設定では、終了コードがわからなくなることがあります

| 環境変数 | 内容 |
| --- | --- |
| `HULL_TERMINAL_SHELL` | 起動する bash の場所 |
| `HULL_TERMINAL_RENDERER` | 描画方法を固定する (`webgl` / `dom`) |
| `HULL_TERMINAL_MODE` | `raw` にすると、生の表示で起動する |
| `HULL_TERMINAL_SOCKET` | デーモンとの接続先 (別々のセッションを使い分けるとき) |
| `HULL_TERMINAL_DAEMON` | デーモンの実行ファイルの場所 |

うまく動かないときは、デーモンのログ `$XDG_RUNTIME_DIR/hull-terminal/daemon.log` を見てください。

## 開発

開発環境は [Workshop](WORKSHOP.md) で用意します。

```bash
workshop launch && workshop run -- setup
workshop run -- test       # 単体テスト (Rust とフロント) と、bash を操作する結合テスト
workshop run -- lint
workshop run -- package --bundles deb
```

### 開発の流れ (GitHub Flow)

`main` はいつでもリリースできる状態に保ち、変更はすべてプルリクエストで入れます。

1. `main` から作業ブランチを切る (名前は内容がわかるものにする。例: `fix-resize-flicker`)
2. コミットして push し、`main` へのプルリクエストを作る
3. CI (整形・clippy・テストと、`.deb` を作って入れて起動する検査) が通ったらマージする
4. マージしたブランチは消す

```bash
git switch main && git pull
git switch -c fix-resize-flicker
# ... 変更してコミット ...
git push -u origin fix-resize-flicker
gh pr create --fill
gh pr merge --squash --delete-branch   # CI が通ってから
```

プルリクエストでは、CI が作った `.deb` を Actions の Artifacts から取って試せます (公開はしません)。

## リリース

`Cargo.toml` の `[workspace.package]` の `version` を上げる変更も、プルリクエストで `main` に入れます。
マージしたあと、`main` のそのコミットに同じ版のタグを付けて push します。
GitHub Actions が `.deb` を作り、入れて起動できることを確かめてから、Releases に公開します。

```bash
git switch main && git pull
git tag v0.1.0
git push origin v0.1.0
```

`main` にないコミットに付けたタグや、`Cargo.toml` と版が合わないタグは公開されません。

## ライセンス

[MIT](LICENSE)
