// ブロックの表示の中心。デーモンとの接続と、使い回す xterm.js 1つを持つ。
//
// xterm.js はコマンドのたびに作らず (WebGL のコンテキストを作り直すことになる)、1つを
// 実行中のブロックの場所に載せ替えて、ブロックが始まるたびにリセットする。プロンプトで
// 待っている間は、見えない場所に置いておく。

import { Unicode11Addon } from "@xterm/addon-unicode11";
import { Terminal, type IDisposable } from "@xterm/xterm";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { KittyKeyboard } from "./kittyKeyboard";
import {
  AckBatcher,
  InputQueue,
  appOptions,
  attach,
  blockOutput,
  resize,
  run,
  sendInput,
  type AttachInfo,
  type ControlEvent,
} from "./pty";
import { MAX_LINES, renderBuffer, type Rendered } from "./render";
import { enableRenderer, flushWebglRenders } from "./renderer";
import { isMaximized, outputs, useSession, type BlockEntry } from "./store";
import { attachTerminalKeys, encodeBinary, encodeData } from "./terminalKeys";
import { FONT_FAMILY, FONT_SIZE, PANEL, terminalTheme } from "./theme";

const APP_TITLE = "hull-terminal";

/** これより小さい画面にはしない (狭いウィンドウでも、コマンドが動ける大きさを保つ)。 */
const MIN_COLS = 20;
const MIN_ROWS = 4;

interface CellSize {
  width: number;
  height: number;
}

export interface BlockSessionOptions {
  /** bash が統合スクリプトを読み込んでいなくて、ブロックに区切れない。 */
  onUnintegrated(): void;
}

export class BlockSession {
  private readonly term: Terminal;
  /** xterm.js を開いてある要素。これごと、実行中のブロックの場所へ載せ替える。 */
  private readonly host = document.createElement("div");
  private readonly kitty: KittyKeyboard;
  private readonly input = new InputQueue();
  private readonly acks = new AckBatcher();
  private readonly subscriptions: IDisposable[] = [];
  private readonly observer: ResizeObserver;
  private frame = 0;
  private disposed = false;

  /** 届いた出力と知らせを、届いた順に片づけるための列。 */
  private tail: Promise<void> = Promise.resolve();
  /** 接続のたびに増える。前の接続から遅れて届いたものを捨てるために使う。 */
  private connectionId = 0;
  /** bash が変わるたびに増える。ブロックの名前に使う。 */
  private bash = 0;
  private lastPid: number | null = null;
  /** xterm.js が読み終えていない、流し直しの出力の数。残っている間は、端末の応答を bash に送らない。 */
  private replaying = 0;
  private rejections = 0;

  /** 実行中のブロックの、見せる高さを決める要素。 */
  private clip: HTMLElement | null = null;

  /** 開き直して戻したブロックの出力を HTML にするための、画面に出さない端末。 */
  private offscreen: Terminal | null = null;
  /** 出力を取っている最中のブロック。同じブロックを二重に取らないために持つ。 */
  private readonly loading = new Map<string, Promise<Rendered | null>>();
  /** 画面に出さない端末は1つなので、変換は1つずつ行う。 */
  private conversions: Promise<unknown> = Promise.resolve();

  /**
   * @param sizer ブロック1つが使える最大の大きさを持つ、見えない要素。列数と行数はここから決める。
   *   実行中のブロックがない間は、xterm.js をここに置いておく。
   */
  constructor(
    private readonly sizer: HTMLElement,
    private readonly options: BlockSessionOptions,
  ) {
    this.host.className = "term-host";
    sizer.appendChild(this.host);

    this.term = new Terminal({
      allowProposedApi: true,
      cursorBlink: true,
      fontFamily: FONT_FAMILY,
      fontSize: FONT_SIZE,
      scrollback: MAX_LINES,
      theme: terminalTheme(PANEL),
    });
    // 絵文字や全角文字の幅を新しい Unicode の定義で数える。
    this.term.loadAddon(new Unicode11Addon());
    this.term.unicode.activeVersion = "11";
    this.term.open(this.host);
    this.kitty = new KittyKeyboard(this.term);

    attachTerminalKeys(this.term, this.kitty, { send: (data) => this.send(data) });
    this.subscriptions.push(
      this.kitty,
      this.term.onData((data) => this.send(encodeData(data))),
      this.term.onBinary((data) => this.send(encodeBinary(data))),
      this.term.onResize(({ cols, rows }) => {
        useSession.setState({ cols, rows });
        if (useSession.getState().connection === "connected") {
          resize(cols, rows);
        }
        this.updateClip();
      }),
      this.term.onWriteParsed(() => this.updateClip()),
      // vim や Claude Code のような全画面のアプリには、ウィンドウいっぱいの大きさを渡す。
      // 代替画面を抜けても戻さない (vim の `:!ls` のように出入りするたびに、大きさが変わって
      // 画面が跳ねるのを避ける)。戻すのは、ブロックが終わったときと、キーやボタンで戻したとき。
      this.term.buffer.onBufferChange((buffer) => {
        if (buffer.type === "alternate") {
          useSession.setState({ maximized: true });
        }
      }),
      {
        dispose: useSession.subscribe((state, previous) => {
          if (isMaximized(state) !== isMaximized(previous)) {
            this.updateClip();
          }
        }),
      },
      this.term.onTitleChange((title) => {
        void getCurrentWindow().setTitle(title ? `${title} — ${APP_TITLE}` : APP_TITLE);
      }),
      flushWebglRenders(this.term, () => useSession.getState().renderer === "webgl"),
    );

    this.observer = new ResizeObserver(() => {
      cancelAnimationFrame(this.frame);
      this.frame = requestAnimationFrame(() => this.fit());
    });
    this.observer.observe(sizer);
    this.fit();
    // フォントの読み込みが終わると文字の幅が変わることがあるので、列数を数え直す。
    void document.fonts.ready.then(() => !this.disposed && this.fit());

    void this.start();
  }

  dispose(): void {
    this.disposed = true;
    this.connectionId += 1;
    this.observer.disconnect();
    cancelAnimationFrame(this.frame);
    for (const subscription of this.subscriptions) {
      subscription.dispose();
    }
    this.term.dispose();
    this.offscreen?.dispose();
    this.host.remove();
  }

  private async start(): Promise<void> {
    const options = await appOptions().catch(() => null);
    if (this.disposed) {
      return;
    }
    useSession.setState({
      home: options?.home ?? null,
      renderer: enableRenderer(this.term, options?.renderer ?? null, (changed) => {
        useSession.setState({ renderer: changed });
      }),
    });
    this.connect();
  }

  /** デーモンにつなぐ。bash が終了したあとや、切れたあとにも呼ぶ (新しい bash が起動する)。 */
  connect(): void {
    if (this.disposed) {
      return;
    }
    const id = ++this.connectionId;
    const current = () => id === this.connectionId && !this.disposed;
    this.input.clear();
    useSession.setState({
      connection: "connecting",
      exitCode: null,
      problem: null,
      pid: null,
      ready: false,
      pending: null,
    });

    // 知らせは `attach` の結果より先に届くことがある。結果 (同じ bash かどうか) を見てから
    // 片づけるよう、列の先頭で結果を待つ。
    const attached = attach(this.term.cols, this.term.rows, false, {
      onReplay: (data) => this.enqueue(current, () => this.write(data, true)),
      onOutput: (data) => this.enqueue(current, () => this.write(data, false)),
      // 知らせは、それより前に届いた出力を xterm.js が読み終えてから片づける
      // (ブロックの終わりで、出力の途中の画面を固定しないように)。
      onControl: (event) =>
        this.enqueue(current, async () => {
          await this.parsed();
          if (current()) {
            this.handle(event);
          }
        }),
    });
    this.tail = attached.then(
      (info) => {
        if (current()) {
          this.attached(info);
        }
      },
      (error: unknown) => {
        if (current()) {
          useSession.setState({ connection: "disconnected", problem: String(error) });
        }
      },
    );
  }

  private attached(info: AttachInfo): void {
    if (!info.integrated) {
      this.options.onUnintegrated();
      return;
    }
    if (info.startedDaemon || info.pid !== this.lastPid) {
      this.bash += 1;
    }
    this.lastPid = info.pid;
    useSession.setState({ connection: "connected", pid: info.pid });
    // つないでいる途中で画面の大きさが変わると、その知らせはデーモンに届いていない。
    // つながったあとで今の大きさを送り直す。
    resize(this.term.cols, this.term.rows);
  }

  private enqueue(current: () => boolean, task: () => void | Promise<void>): void {
    this.tail = this.tail
      .then(() => (current() ? task() : undefined))
      .catch((error: unknown) => console.error(error));
  }

  /**
   * 出力を xterm.js に渡す。読み終えるのは待たない (1つずつ待つと、xterm.js がまとめて
   * 読めず、流し続ける出力に画面が追いつかなくなる)。
   */
  private write(data: Uint8Array, replay: boolean): void {
    // 流し直す出力に入っている問い合わせ (カーソル位置や色など) に端末が応答すると、
    // その応答が実行中のコマンドへの入力になってしまう。書き終えるまで入力を送らない。
    if (replay) {
      this.replaying += 1;
    }
    this.term.write(data, () => {
      if (replay) {
        this.replaying -= 1;
      }
      this.acks.add(data.length);
    });
  }

  /** ここまでに渡した出力を、xterm.js が読み終えるのを待つ。 */
  private parsed(): Promise<void> {
    return new Promise((resolve) => this.term.write("", resolve));
  }

  private key(id: number): string {
    return `${this.bash}:${id}`;
  }

  private handle(event: ControlEvent): void {
    switch (event.type) {
      case "history": {
        // 前の bash のブロックは、そのまま上に残す。今の bash のぶんは、届いた一覧に置き換える。
        const prefix = `${this.bash}:`;
        const older = useSession.getState().blocks.filter((block) => !block.key.startsWith(prefix));
        const blocks = event.blocks.map((block) => ({ key: this.key(block.info.id), ...block }));
        this.reset();
        useSession.setState({
          blocks: [...older, ...blocks],
          running: event.running && { key: this.key(event.running.id), info: event.running },
          prompt: event.prompt,
          ready: event.ready,
          pending: null,
        });
        break;
      }
      case "prompt":
        useSession.setState({ prompt: event.prompt, ready: true, pending: null });
        break;
      case "blockStarted":
        this.reset();
        useSession.setState({
          running: { key: this.key(event.block.id), info: event.block },
          ready: false,
          pending: null,
        });
        break;
      case "blockFinished": {
        const { running, blocks } = useSession.getState();
        if (running?.info.id !== event.id) {
          break;
        }
        const entry: BlockEntry = { key: running.key, info: running.info, result: event.result };
        if (!event.result.fullscreen) {
          // 載っていた xterm.js の内容を、そのまま変換する。
          outputs.set(entry.key, renderBuffer(this.term.buffer.normal));
        }
        useSession.setState({ blocks: [...blocks, entry], running: null });
        break;
      }
      case "runRejected":
        this.rejections += 1;
        useSession.setState({
          pending: null,
          rejected: { text: event.text, reason: event.reason, serial: this.rejections },
        });
        break;
      case "exited":
        this.input.clear();
        useSession.setState({ connection: "exited", exitCode: event.code, ready: false });
        break;
      case "disconnected":
        this.input.clear();
        useSession.setState({ connection: "disconnected", problem: event.reason, ready: false });
        break;
    }
  }

  /** 次のブロックのために、端末をまっさらにする。 */
  private reset(): void {
    this.term.reset();
    this.kitty.reset();
    useSession.setState({ maximized: false });
    this.updateClip();
  }

  /**
   * 開き直して戻したブロックの出力を、デーモンから取って HTML にする。
   * 前の bash のブロックなど、もう取れないものは null。結果は `outputs` にも入れる。
   */
  loadOutput(block: BlockEntry): Promise<Rendered | null> {
    const cached = outputs.get(block.key);
    if (cached) {
      return Promise.resolve(cached);
    }
    let loading = this.loading.get(block.key);
    if (!loading) {
      loading = this.fetchOutput(block).finally(() => this.loading.delete(block.key));
      this.loading.set(block.key, loading);
    }
    return loading;
  }

  private async fetchOutput(block: BlockEntry): Promise<Rendered | null> {
    const current = block.key === this.key(block.info.id);
    if (!current || useSession.getState().connection !== "connected") {
      return null;
    }
    try {
      const { data, truncated } = await blockOutput(block.info.id);
      const convert = this.conversions.then(() => this.convert(block, data, truncated));
      this.conversions = convert.catch(() => undefined);
      const rendered = await convert;
      outputs.set(block.key, rendered);
      return rendered;
    } catch {
      return null;
    }
  }

  /** 出力を、実行したときの幅の端末に通して HTML にする。 */
  private convert(block: BlockEntry, data: Uint8Array, truncated: boolean): Promise<Rendered> {
    const cols = Math.max(MIN_COLS, block.info.cols);
    const rows = Math.max(MIN_ROWS, block.info.rows);
    // 開かない (`open` を呼ばない) 端末は、描画をせずに出力の解釈だけを行う。
    this.offscreen ??= new Terminal({ allowProposedApi: true, cols, rows, scrollback: MAX_LINES });
    const offscreen = this.offscreen;
    if (!offscreen.unicode.versions.includes("11")) {
      offscreen.loadAddon(new Unicode11Addon());
      offscreen.unicode.activeVersion = "11";
    }
    offscreen.reset();
    offscreen.resize(cols, rows);
    return new Promise((resolve) => {
      offscreen.write(data, () => {
        const rendered = renderBuffer(offscreen.buffer.normal);
        offscreen.reset();
        resolve({ lines: rendered.lines, omitted: rendered.omitted || truncated });
      });
    });
  }

  /** 入力欄のテキストを実行する。受け付けられる状態でなければ false。 */
  run(text: string): boolean {
    const state = useSession.getState();
    if (state.connection !== "connected" || !state.ready) {
      return false;
    }
    useSession.setState({ ready: false, pending: text, rejected: null });
    run(text).catch(() => useSession.setState({ pending: null }));
    return true;
  }

  /**
   * 送ったテキストに bash が応えないとき (プロンプトが端末に問い合わせる設定など) に、
   * Ctrl+C を送って取り消す。
   */
  interrupt(): void {
    if (useSession.getState().connection === "connected") {
      sendInput(Uint8Array.of(0x03));
    }
  }

  private send(data: Uint8Array): void {
    const { connection, running } = useSession.getState();
    if (connection === "connected" && running !== null && this.replaying === 0) {
      this.input.push(data);
    }
  }

  focus(): void {
    this.term.focus();
  }

  /** xterm.js を、実行中のブロックの場所に載せる。`clip` の高さで、見せる行数を決める。 */
  mount(slot: HTMLElement, clip: HTMLElement): void {
    slot.appendChild(this.host);
    this.clip = clip;
    this.updateClip();
    this.term.focus();
  }

  /** xterm.js を、見えない場所に戻す。 */
  unmount(): void {
    this.clip = null;
    if (!this.disposed) {
      this.sizer.appendChild(this.host);
    }
  }

  private cell(): CellSize | null {
    // FitAddon と同じ場所から、1文字ぶんの大きさを読む (公開の API にはない)。
    const core = (this.term as unknown as { _core?: CoreLike })._core;
    const cell = core?._renderService?.dimensions?.css?.cell;
    return cell && cell.width > 0 && cell.height > 0 ? cell : null;
  }

  /**
   * ブロックが使える最大の大きさに合わせて、列数と行数を決める。ウィンドウいっぱいに広げると
   * `sizer` も広がるので、ここを通って端末と PTY の大きさが変わる。
   */
  private fit(): void {
    const cell = this.cell();
    if (this.disposed || !cell) {
      return;
    }
    const cols = Math.max(MIN_COLS, Math.floor(this.sizer.clientWidth / cell.width));
    const rows = Math.max(MIN_ROWS, Math.floor(this.sizer.clientHeight / cell.height));
    if (cols !== this.term.cols || rows !== this.term.rows) {
      this.term.resize(cols, rows);
    } else {
      useSession.setState({ cols, rows });
    }
    document.documentElement.style.setProperty("--cell-height", `${cell.height}px`);
  }

  /**
   * 実行中のブロックの高さを、出力が届いた行までに切り詰める。`ls` のような短いコマンドで
   * 画面が大きく跳ねないようにするため。PTY の大きさは変えないので、コマンドから見た端末の
   * 大きさは変わらない。画面いっぱいまで出力が届くか、ウィンドウいっぱいに広げている間は
   * 全体を見せる。
   */
  private updateClip(): void {
    const cell = this.cell();
    if (!this.clip || !cell) {
      return;
    }
    const buffer = this.term.buffer.active;
    let used = this.term.rows;
    if (!isMaximized(useSession.getState()) && buffer.type !== "alternate" && buffer.baseY === 0) {
      // カーソルより下に描くアプリもあるので、文字のある最後の行も見る。
      used = buffer.cursorY + 1;
      for (let y = this.term.rows - 1; y >= used; y--) {
        if (buffer.getLine(y)?.translateToString(true)) {
          used = y + 1;
          break;
        }
      }
    }
    this.clip.style.height = `${used * cell.height}px`;
  }
}

interface CoreLike {
  _renderService?: { dimensions?: { css?: { cell?: CellSize } } };
}
