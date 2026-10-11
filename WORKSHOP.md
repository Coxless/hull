# hull-terminal 開発環境 (Workshop)

開発環境は `.workshop/dev.yaml` 1ファイルで定義され、LXD のシステムコンテナとして起動します。
ホストを汚さずに、同じ環境をどのマシンでも再現できます。

## 構成

```
hull-terminal/
├── .workshop/
│   └── dev.yaml    # Workshop 定義 (Rust / Node 22 + actions)
├── crates/         # protocol (メッセージ定義) と daemon (PTY を持つデーモン)
├── app/            # Tauri アプリ (フロントと Rust コア)
├── docs/           # 要件定義と Phase ごとの実行計画
├── .gitignore
└── WORKSHOP.md     # この説明
```

ワークショップには次が入ります。

| ツール | 入手元 | 用途 |
| ------ | ------ | ---- |
| Rust (cargo / clippy / rustfmt) | `rust` SDK (latest/stable) | デーモン (`crates/`) と Tauri の Rust コア (`app/src-tauri`) |
| Node.js 22 | `node` SDK (22/stable) | フロントエンド (React + TypeScript + Vite) と Tauri CLI |
| WebKitGTK 4.1 などのライブラリ, gcc | `setup` で apt | Tauri v2 のビルドと実行 |

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
workshop run -- setup      # Tauri の前提パッケージを入れ、`cargo fetch` と `npm install` を実行
```

プロジェクトディレクトリはワークショップ内の `/project` にマウントされます。
ホストに Rust や Node がなくても、`workshop shell` の中で `cargo` / `npm` を使えます。

### 2. actions

| action    | 内容                                              | 例                        |
| --------- | ------------------------------------------------- | ------------------------- |
| `setup`   | システム依存の導入、`cargo fetch`、`npm install`  | `workshop run -- setup`   |
| `build`   | `cargo build --workspace` とフロントのビルド      | `workshop run -- build`   |
| `package` | デーモンを同梱した配布パッケージを作成          | `workshop run -- package --bundles deb` |
| `test`    | `cargo test --workspace` (bash を操作する結合テストを含む) | `workshop run -- test`    |
| `lint`    | `cargo clippy` と `tsc --noEmit`                  | `workshop run -- lint`    |
| `fmt`     | `cargo fmt` と prettier                           | `workshop run -- fmt`     |

`Cargo.toml` や `app/package.json` がまだない段階では、該当する処理は実行されません。
引数は cargo 側に渡ります (例: `workshop run -- test -p protocol`)。

### 3. アプリの起動について

`tauri dev` はウィンドウを開くため、ディスプレイのないワークショップ内ではそのままでは動きません。
画面の動作確認 (日本語入力や WebGL を含む) は、次のどちらかで行います。

- ホストに Rust と Node を入れて、`app/` で `npm install` のあと `npm run tauri dev` を実行する
  (デーモンのビルドと Vite の起動も一緒に行われます)
- ワークショップで `workshop run -- package --bundles deb` を実行し、`target/release/bundle/deb/` にできた
  `.deb` をホストに入れて `hull` (または `hull-terminal`) を起動する

デーモンだけを試すには、テスト用の小さなクライアントが使えます。

```bash
cargo run -p hull-terminal-daemon &
cargo run -p hull-terminal-daemon --example hull-ctl -- 'echo hello'
cargo run -p hull-terminal-daemon --example hull-ctl -- exit
```

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

## ホストにポートを公開する

Vite の開発サーバーなどをホストのブラウザで見たいときは、`dev.yaml` の `tunnel` の例のコメントを外して `workshop refresh` します。

```yaml
sdks:
  - name: system
    plugs:
      web:
        interface: tunnel
        endpoint: 127.0.0.1:5173
```

## ワークショップを追加する

`.workshop/` に定義を追加すると、1つのプロジェクトで複数の環境を使い分けられます (例: `dev` と `ci`)。

```bash
workshop init ci --base ubuntu@24.04 --sdks rust,node/22/stable
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
- [Tauri v2 の前提条件](https://v2.tauri.app/start/prerequisites/)
- [GitHub: canonical/workshop](https://github.com/canonical/workshop)
