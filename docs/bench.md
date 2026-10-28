# hull のベンチマーク

concept.md の「速さを主張でなく数字で示す」のための、測り方と結果です。
測る道具は [bench/hullbench.py](../bench/hullbench.py) で、どの端末も同じ方法で測ります。

```bash
cargo build --release --workspace
python3 bench/hullbench.py all --terminals hull,alacritty,foot,ghostty
```

結果は `bench/results/` に Markdown で書き出します。**画面がついているときに測ってください**
(画面が消えているとコンポジタがフレームを出さず、表示の時刻が測れません)。

## 測り方

| 指標 | 測り方 |
| --- | --- |
| 起動 | **最初のコミット**: WAYLAND_DEBUG の時刻で、起動してから最初にバッファを付けてコミットするまで (全端末で同じ方法)。**画面に出た**: hull だけ、`--bench-startup` で wp_presentation の時刻を取る |
| アイドル | 窓を開いて 5 秒置いたあと、10 秒のあいだの文脈切り替えの数と CPU 時間 (`/proc`) |
| メモリ | スクロールバックを 1 万行埋めたときの RSS。GPU ドライバのライブラリのマッピングを除いた値も出す (ドライバがヒープに取った分は分けられない) |
| スループット | vtebench。別に入れる (`--vtebench` と `--vtebench-benchmarks`) |
| 変化の型ごとの描画時間 | vim で 300 回操作し、redraw までの 1 回あたりの時間。1 文字の入力、1 行のスクロール、画面全体の変化、左右に分けてスクロール、tmux で左右に分けてスクロール |
| 入力の遅れ | Typometer は X11 前提で使えない。hull だけ `HULL_TRACE_LATENCY=1` で、キーのイベントの時刻 (コンポジタの時計) から、そのあとのフレームが画面に出た時刻 (wp_presentation) までを出す。端末をまたいだ比較は、カメラで画面とキーを撮る方法を Phase 3 までに用意する |
| バイナリ | strip したリリースビルドの大きさ |

## 結果 (2026-10-28、途中)

Ubuntu 26.04 / GNOME (Wayland) / NVIDIA GeForce GTX 1660 Ti (ドライバ 595.99) / 16 スレッド。
alacritty、foot、tmux、vim、vtebench はまだ入れていないので、hull と ghostty だけです。

| 指標 | 目標 | hull | ghostty |
| --- | --- | --- | --- |
| バイナリ | 5MB 未満 | 2.8MB (プラグイン 3 つは別に計 1.4MB) | - |
| 起動: 最初のコミット | - | 5〜8ms | (測れず。下を参照) |
| 起動: 画面に出た | 30ms 未満 | 45〜70ms | - |
| 起動: GPU で描いて画面に出た | - | 120〜130ms | - |
| アイドル: 10 秒の wakeup | 0 回 | 0〜4 回 | 15〜84 回 |
| アイドル: CPU | 0% | 0ms | 10ms |
| メモリ (RSS、1 万行) | 20MB 未満 | 113MB (ドライバのライブラリを除くと 26MB) | 271MB (同 193MB) |
| `cat` で 43.6MB を流しきる時間 | alacritty と同等以上 | 0.38 秒 | 0.67 秒 |
| VT の処理だけの速さ (`examples/throughput`) | - | 145〜158MB/s | - |

### わかったこと

- **EGL の初期化に 100ms かかる** (NVIDIA のドライバ。Mesa に切り替えても 128ms)。これだけで起動の目標 30ms を超える。
  そこで EGL の初期化を別のスレッドで進め、そのあいだに最初のフレーム (背景と見出しの帯) を CPU で描いて wl_shm で出す。
  シェルがプロンプトを出す前の最初のフレームは、どの端末でも空の画面なので、見た目は変わらない。
  GPU で描いたフレームが出るまでの時間も、別に出す
- **fontconfig の初期化に 15〜20ms かかる**。前の起動で決まったフォントのファイルと GNOME の設定を `~/.cache/hull/` に覚えておき、
  次からは fontconfig と gsettings を待たずに最初のフレームを出す。fontconfig は裏のスレッドで初期化を進め、
  フォールバックが要ったときに使う。gsettings の値は起動のあとで確かめ、変わっていれば反映する。
  これで最初のコミットまでが 38ms から 5ms になった
- **最初のコミットから画面に出るまでに 40〜60ms かかる**のは、GNOME が窓を出すまでの時間で、hull の側では縮められない。
  競合も同じ条件なので、端末どうしの比較は「最初のコミット」で行う。concept.md のとおり、目標の数字は
  競合を測ったあとで「競合と比べて何割か」の形に決め直す
- **RSS の大半は GPU ドライバ**。hull のヒープは、スクロールバックを 1 万行埋めても 5.6MB。
  履歴に移った行は 1 セル 4 バイトに詰めて持つ (画面の行は 16 バイト)。大量の出力のあいだは詰めず、
  止まって 1 秒たってからまとめて詰める (履歴からあふれて捨てる行は、詰める手間をかけずに済む)
- **ドライバを除いた 26MB にも、NVIDIA がヒープ以外に取った 16MB ほどが入っている**。hull 自身の分 (スクロールバック 0 行と比べた差) は 5MB ほど

### まだ測っていないもの

- alacritty と foot (`sudo apt install alacritty foot`)
- vtebench (https://github.com/alacritty/vtebench をビルドして渡す)
- vim の 5 つの型 (`sudo apt install vim tmux`)
- tmux が OSC を素通しにするか: `python3 bench/tmux_passthrough.py`
- 画面がついているときの起動の時刻と、入力の遅れ (`HULL_TRACE_LATENCY=1`)
- ghostty の最初のコミット (snap の ghostty では、WAYLAND_DEBUG の出力から最初のコミットを拾えなかった。原因を調べる)
