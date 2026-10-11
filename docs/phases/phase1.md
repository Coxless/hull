# hull-terminal Phase 1 実行計画

Sep 27, 2026 · @Coxless

## 概要

Phase 1 のゴールは、Ubuntu と Windows の両方で hull-terminal を開くと bash が動き、vim・less・Claude Code が普通に使え、ウィンドウを閉じて開き直しても同じ bash につながる状態にすることです。

描画は GPU（xterm.js の WebGL）を前提にし、WebGL が使えない環境では DOM 描画に自動で切り替えます。開発は Ubuntu で進め、Ubuntu で使えるようになった時点で Windows 実機の確認に移ります。

## 範囲

Phase 1 では「普通のターミナルとして両 OS で使える」ことだけを作り、ブロック表示や履歴の保存は Phase 2 以降に回します。

| 区分 | 内容 |
| --- | --- |
| やる | デーモンが bash を1つ起動し、UI が xterm.js 1つで操作する |
| やる | Unix ソケット（Ubuntu）と名前付きパイプ（Windows）での UI とデーモンの通信 |
| やる | UI を閉じてもデーモンと bash が残り、開き直すと再接続して直前の画面を戻す |
| やる | Git Bash の自動検出と ConPTY での動作 |
| やる | WebGL 描画と、使えないときの DOM 描画への自動切り替え |
| やる | GitHub Actions で Ubuntu / Windows の両方をビルドし、簡単な結合テストを回す |
| やらない | ブロック表示、統合スクリプト、OSC 133 の解析（届くかの確認だけ行う） |
| やらない | 下固定の入力欄、サイドバー、複数セッション |
| やらない | SQLite 保存、ピン留め、プロセス保持、終了セッションの再開 |
| やらない | 設定ファイル、Claude Code の入力待ち検知、インストーラー |

## 構成

リポジトリは Cargo ワークスペースと Tauri アプリを1つにまとめ、UI とデーモンが共有するメッセージ定義を独立した crate にします。

```
hull-terminal/
├─ Cargo.toml            ワークスペース
├─ crates/
│  ├─ protocol/          メッセージ定義とフレームの読み書き
│  └─ daemon/            hull-terminal-daemon（PTY とソケット）
├─ app/
│  ├─ src/               フロント（React + TypeScript + Vite）
│  └─ src-tauri/         Tauri の Rust コア
└─ .github/workflows/    CI（Ubuntu / Windows）
```

**名前と場所**

| 項目 | Ubuntu | Windows |
| --- | --- | --- |
| アプリ | hull-terminal | hull-terminal.exe |
| デーモン | hull-terminal-daemon | hull-terminal-daemon.exe（コンソールを出さないビルド） |
| 接続先 | `$XDG_RUNTIME_DIR/hull-terminal/daemon.sock` | `\\.\pipe\hull-terminal-<ユーザー名>` |
| 起動する bash | `bash -i`（ログインシェルの設定に従う） | Git Bash の `bin\bash.exe --login -i` |

**通信メッセージ**

フレームは「4バイトの長さ + MessagePack 本体」とし、出力と入力はバイト列のまま送ります。最初に互換性確認用のバージョンを交換します。

| 方向 | メッセージ | 中身 |
| --- | --- | --- |
| UI → デーモン | Hello | プロトコルのバージョン |
| UI → デーモン | Attach | 画面の列数・行数 |
| UI → デーモン | Input | キー入力のバイト列 |
| UI → デーモン | Resize | 新しい列数・行数 |
| UI → デーモン | Detach | 切断の通知（ウィンドウを閉じるとき） |
| デーモン → UI | Welcome | デーモンのバージョン、bash の PID |
| デーモン → UI | Replay | 再接続時に流し直す直近の出力（リングバッファ） |
| デーモン → UI | Output | PTY の出力のバイト列 |
| デーモン → UI | Exited | bash の終了コード |

Tauri の Rust コアからフロントへは、出力を Channel でバイト列のまま渡します。入力とリサイズはフロントからコマンドで受けてデーモンへ送ります。

## 作業計画

作業は10個で、1〜7を Ubuntu で進めて普通のターミナルとして使える状態にし、8〜10で Windows に広げます。各作業は完了条件を満たしてから次に進みます。

| # | 作業 | 内容 | 完了条件 |
| --- | --- | --- | --- |
| 1 | 土台づくり | Rust・Node・Tauri v2 の前提パッケージを入れ、ワークスペースと React + TypeScript の Tauri アプリを作る。CI でビルドだけ回す | 空のウィンドウが開き、CI が Ubuntu / Windows の両方で通る |
| 2 | protocol crate | メッセージ定義とフレームの読み書きを作り、単体テストを書く | 往復のテストと、フレームが途中で分かれて届くケースのテストが通る |
| 3 | デーモン | portable-pty で bash を起動し、interprocess でソケットを待ち受けて入出力を中継する。直近の出力をリングバッファに持つ。bash が終わったらデーモンも終わる | テスト用の小さな CLI から `echo hello` を送り、結果が返ってくる |
| 4 | Tauri の Rust コア | デーモンに接続し、いなければ起動して接続し直す。出力を Channel でフロントへ流し、入力とリサイズをコマンドで受ける | UI からデーモンを起動でき、出力がフロントまで届く |
| 5 | フロント | xterm.js に WebGL・fit・unicode11 アドオンを付け、サイズ変更を PTY に伝える。Ctrl+Shift+C / V でコピーと貼り付け。WebGL が使えなければ DOM 描画に切り替える | Ubuntu で ls、vim、less が普通に使える |
| 6 | 再接続 | ウィンドウを閉じるとき Detach を送り、開き直したら Replay で直前の画面を戻す | 閉じて開き直しても `echo $$` が同じ値になる |
| 7 | Ubuntu の確認 | 確認項目を一通り試し、WebKitGTK での日本語入力と WebGL、大量出力時の CPU 使用率を測る | Ubuntu の確認項目がすべて済み、結果を記録してある |
| 8 | Windows 対応 | Git Bash の自動検出、名前付きパイプ、親から切り離したデーモン起動、コンソールを出さないビルド、Windows 10 1809 未満の警告を入れる | Windows で起動し、Git Bash が動く |
| 9 | Windows の確認 | 確認項目を一通り試し、WebView2 での日本語入力と、ConPTY を通して OSC 133 が届くかを確かめる | Windows の確認項目がすべて済み、結果を記録してある |
| 10 | CI の結合テスト | 両 OS でデーモンを起動し、`echo` の結果と Exited を受け取るテストを CI に加える | CI の結合テストが両 OS で通る |

## 確認項目

次の項目がすべて済めば Phase 1 は完了です。

**Ubuntu**

- [ ] `ls --color`、`git log`、`less` が色付きで正しく表示される
- [ ] vim、htop、Claude Code が崩れずに動き、ウィンドウのサイズ変更に追従する
- [ ] Ctrl+C、Ctrl+D、Esc、Shift+Enter がそのまま届く（Claude Code で確認）
- [ ] 日本語（fcitx5 または ibus）の入力と表示ができ、文字幅がずれない
- [ ] ウィンドウを閉じて開き直すと同じ bash につながり、直前の画面が戻る
- [ ] `exit` で bash とデーモンが終了し、画面に終了が表示される
- [ ] WebGL で描画されていることを確認し、大きなファイルの `cat` と `yes` で CPU 使用率を WebGL / DOM の両方で測った
- [ ] GPU アクセラレーションのない仮想マシンで、DOM 描画に自動で切り替わって使える

**Windows**

- [ ] Git Bash が自動で見つかり、`git`、`ls --color` が普通に使える
- [ ] vim と Claude Code が崩れずに動き、サイズ変更に追従する
- [ ] Ctrl+C でコマンドが止まり、bash 自体は落ちない
- [ ] 日本語（MS-IME）の入力と表示ができる
- [ ] ウィンドウを閉じて開き直すと同じ bash につながり、コンソール画面が一瞬出たりしない
- [ ] ConPTY を通して OSC 133 のシーケンスが届くかを確かめ、結果を記録した

**共通**

- [ ] CI のビルドと結合テストが Ubuntu / Windows の両方で通る

## 決めること・リスク

**Phase 1 の中で決めること**

- [ ] メッセージの形式：Phase 2 以降でメッセージを足しても古い UI とデーモンが壊れない形にする（バージョン交換の扱い）
- [ ] 出力の渡し方：Channel で十分な速さが出るか、大量出力で試して決める。足りなければ数ミリ秒ごとにまとめて送る
- [ ] リングバッファの大きさ：再接続時に画面を戻せる量（仮に 1MB）
- [ ] 再接続で戻す範囲：生の出力を流し直すだけか、vim など全画面アプリ中の再接続で画面をどう戻すか

**リスクと対策**

| リスク | 対策 |
| --- | --- |
| WebKitGTK で日本語入力や WebGL が不安定 | 作業5・7で早めに確かめる。致命的なら画面層を Electron に移す判断をこの時点でする |
| ConPTY が画面を描き直したり制御シーケンスを書き換えたりする | 作業9で確かめる。問題があれば新しい版の conpty.dll を同梱する方法を検討する |
| Windows で UI を閉じるとデーモンも一緒に終了する | 親から切り離して起動し、作業9の確認項目で必ず試す |
| 大量出力で UI が固まる | 作業7で CPU 使用率を測り、描画をまとめて行う |
