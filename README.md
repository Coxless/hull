# hull-terminal

コマンドと出力を1往復ずつブロックとして扱う、チャット履歴型のターミナルです。

いまは Phase 1 (v0.1) で、ブロック表示はまだなく、**ウィンドウを閉じても bash が残る普通のターミナル**として動きます。
設計と計画は [docs/concept.md](docs/concept.md) と [docs/phases/phase1.md](docs/phases/phase1.md) にあります。

| OS | 状態 |
| --- | --- |
| Ubuntu 24.04 以降 | `.deb` を配布。Ubuntu 24.04 / 26.04 で動作を確認 |
| Windows 10 / 11 | ビルドと結合テストは CI で通過。実機では未確認で、パッケージは未配布 |

## インストール (Ubuntu)

[Releases](https://github.com/Coxless/hull/releases) から `.deb` を入れます。

```bash
gh release download -R Coxless/hull -p '*.deb'   # 最新版。gh がなければ Releases のページからダウンロード
sudo apt install ./hull-terminal_*_amd64.deb
hull-terminal                                    # アプリの一覧からも起動できます
```

更新は、新しい `.deb` を同じ手順で入れ直します。開いているセッションは、先に `exit` で終了してください
(古い版のデーモンが残っていると、新しい版の画面とつながらないことがあります)。

削除は `sudo apt remove hull-terminal` です。

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

## リリース

`Cargo.toml` の `[workspace.package]` の `version` を上げてコミットし、同じ版のタグを push します。
GitHub Actions が `.deb` を作り、入れて起動できることを確かめてから、Releases に公開します。

```bash
git tag v0.1.0
git push origin v0.1.0
```

ブランチへの push では、CI が同じ `.deb` を作って検査し、Actions の Artifacts に置きます (公開はしません)。
