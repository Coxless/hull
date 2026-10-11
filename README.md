# hull-terminal

コマンドと出力を1往復ずつブロックとして扱う、チャット履歴型のターミナルです。

いまは Phase 1 (v0.1) で、ブロック表示はまだなく、**ウィンドウを閉じても bash が残る普通のターミナル**として動きます。
設計と計画は [docs/concept.md](docs/concept.md) と [docs/phases/phase1.md](docs/phases/phase1.md) にあります。

| OS | 状態 |
| --- | --- |
| Ubuntu 24.04 以降 | `.deb` を配布。Ubuntu 24.04 / 26.04 で動作を確認 |
| Windows 10 / 11 | インストーラーを配布。CI で入れて起動できることを確認。実機では未確認 |

## インストール (Ubuntu)

[Releases](https://github.com/Coxless/hull/releases) から `.deb` を入れます。

```bash
gh release download -R Coxless/hull -p '*.deb'   # 最新版。gh がなければ Releases のページからダウンロード
sudo apt install ./hull-terminal_*_amd64.deb
hull                                             # hull-terminal でも、アプリの一覧からも起動できます
```

更新は、新しい `.deb` を同じ手順で入れ直します。開いているセッションは、先に `exit` で終了してください
(古い版のデーモンが残っていると、新しい版の画面とつながらないことがあります)。

削除は `sudo apt remove hull-terminal` です。

## インストール (Windows)

先に [Git for Windows](https://gitforwindows.org/) を入れます (Git Bash を使います)。
[Releases](https://github.com/Coxless/hull/releases) から `hull-terminal_<版>_x64-setup.exe` をダウンロードして実行し、
スタートメニューの hull-terminal から起動します。

インストーラーには署名がないので、実行時に Windows の警告 (SmartScreen) が出ます。
削除は、Windows の「インストールされているアプリ」から行います。

## 使い方

- ウィンドウを閉じても bash は裏で動き続け、開き直すと同じ bash につながって直前の画面が戻ります
- `exit` で bash を終了すると、裏のデーモン (`hull-terminal-daemon`) も終了します
- コピーは Ctrl+Shift+C、貼り付けは Ctrl+Shift+V
- Claude Code では Shift+Enter で改行できます
- 画面は GPU があれば WebGL、なければ DOM で描きます (下のステータスバーに表示)

| 環境変数 | 内容 |
| --- | --- |
| `HULL_TERMINAL_SHELL` | 起動する bash の場所 |
| `HULL_TERMINAL_RENDERER` | 描画方法を固定する (`webgl` / `dom`) |
| `HULL_TERMINAL_SOCKET` | デーモンとの接続先 (別々のセッションを使い分けるとき) |
| `HULL_TERMINAL_DAEMON` | デーモンの実行ファイルの場所 |

うまく動かないときは、デーモンのログ `$XDG_RUNTIME_DIR/hull-terminal/daemon.log` を見てください。

## 開発

開発環境は [Workshop](WORKSHOP.md) で用意します。

```bash
workshop launch && workshop run -- setup
workshop run -- test       # 単体テストと、bash を操作する結合テスト
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

プルリクエストでは、CI が作った `.deb` と Windows のインストーラーを Actions の Artifacts から取って試せます (公開はしません)。

## リリース

`Cargo.toml` の `[workspace.package]` の `version` を上げる変更も、プルリクエストで `main` に入れます。
マージしたあと、`main` のそのコミットに同じ版のタグを付けて push します。
GitHub Actions が `.deb` と Windows のインストーラーを作り、それぞれ入れて起動できることを確かめてから、Releases に公開します。

```bash
git switch main && git pull
git tag v0.1.0
git push origin v0.1.0
```

`main` にないコミットに付けたタグや、`Cargo.toml` と版が合わないタグは公開されません。

## ライセンス

[MIT](LICENSE)
