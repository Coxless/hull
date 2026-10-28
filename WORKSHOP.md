# hull-terminal 開発環境 (Workshop)

開発環境は `.workshop/dev.yaml` 1ファイルで定義され、LXD のシステムコンテナとして起動します。
ホストを汚さずに、同じ環境をどのマシンでも再現できます。

## 構成

```
hull-terminal/
├── .workshop/
│   └── dev.yaml    # Workshop 定義 (Rust + actions)
├── crates/         # 本体 (hull)、端末の中核 (term)、プラグインのプロトコル (plugin)
├── plugins/        # 同梱のプラグイン
├── bench/          # ベンチマーク
├── docs/           # コンセプト、プラグイン、ベンチの結果
├── .gitignore
└── WORKSHOP.md     # この説明
```

ワークショップには次が入ります。

| ツール | 入手元 | 用途 |
| ------ | ------ | ---- |
| Rust (cargo / clippy / rustfmt) | `rust` SDK (latest/stable) | 本体のビルド |
| gcc, pkg-config, libwayland-dev, libxkbcommon-dev, libfontconfig-dev, libegl-dev | `setup` で apt | ビルド |

## 前提条件

- Ubuntu (または WSL)
- LXD 6.8 以上
- Workshop

```bash
sudo snap install --channel=6/stable lxd
sudo snap install --classic workshop
```

## 使い方

### 1. 起動と初期化

```bash
workshop launch            # 初回のみ (SDK を取得して環境を作る)
workshop run -- setup      # 前提パッケージを入れ、`cargo fetch` を実行
```

プロジェクトディレクトリはワークショップ内の `/project` にマウントされます。
ホストに Rust がなくても、`workshop shell` の中で `cargo` を使えます。

### 2. actions

| action  | 内容                                 | 例                      |
| ------- | ------------------------------------ | ----------------------- |
| `setup` | システム依存の導入と `cargo fetch`   | `workshop run -- setup` |
| `build` | `cargo build --workspace`            | `workshop run -- build` |
| `test`  | `cargo test --workspace`             | `workshop run -- test`  |
| `lint`  | `cargo clippy`                       | `workshop run -- lint`  |
| `fmt`   | `cargo fmt`                          | `workshop run -- fmt`   |
| `deb`   | `.deb` を作る (`target/deb/`)        | `workshop run -- deb`   |

引数は cargo 側に渡ります (例: `workshop run -- test -p <crate>`)。

### 3. アプリの起動について

hull は Wayland のウィンドウを開くため、ディスプレイのないワークショップ内ではそのままでは動きません。
画面の動作確認はホストで行います。ワークショップでビルドした `target/debug/hull` を、ホストでそのまま起動できます
(ホストに要るのは実行用のライブラリだけで、Ubuntu のデスクトップには最初から入っています)。
キーを打てない確認には `HULL_SCREENSHOT` と `HULL_TEST_KEYS` を使います (README.md の「開発」)。

### 4. 定義を変更したとき

`base` や `sdks` を変更したら `workshop refresh` で反映します (actions は即時反映)。

```bash
workshop changes    # 変更履歴
workshop restore    # 最後の launch / refresh の状態に戻す
```

## その他の操作

```bash
workshop list       # このプロジェクトのワークショップ一覧
workshop info       # 状態の詳細
workshop stop       # 停止 (状態は保持される)
workshop start      # 再開
workshop remove     # 削除 (定義ファイルは残る)
```

ワークショップが1つだけのときは、名前を省略すると `dev` が対象になります。
複数ある場合は、`workshop shell dev` のように名前を指定してください。

## ワークショップを追加する

`.workshop/` に定義を追加すると、1つのプロジェクトで複数の環境を使い分けられます (例: `dev` と `ci`)。

```bash
workshop init ci --base ubuntu@24.04 --sdks rust
workshop launch ci
```

## Git での注意

- `.workshop/*.yaml` はコミットして、チームで共有します。
- `.workshop.lock` はコミットしません (`.gitignore` に登録済み)。

## 参考

- [Workshop ドキュメント](https://ubuntu.com/workshop/docs/)
- [チュートリアル](https://ubuntu.com/workshop/docs/tutorial/part-1-get-started/)
- [定義ファイルのリファレンス](https://ubuntu.com/workshop/docs/reference/definition-files/workshop-definition/)
- [CLI リファレンス](https://ubuntu.com/workshop/docs/reference/cli/workshop/)
- [GitHub: canonical/workshop](https://github.com/canonical/workshop)
