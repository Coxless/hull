use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use hull_terminal_protocol::{BlockResult, PromptState, RunRejection, ServerMessage};
use portable_pty::{native_pty_system, MasterPty, PtySize};
use tokio::sync::{mpsc, oneshot};

use crate::block::{Blocks, Context, StoredOutput};
use crate::complete::{complete, Completion};
use crate::integration::Integration;
use crate::marker::{Event, Item, Splitter};
use crate::{Ring, ShellSpec};

/// 1クライアントぶんの待ち行列の長さ (チャンク数)。
///
/// いっぱいになると PTY の読み取りが止まり、出力しているコマンドが待たされる。
/// 画面の描画が追いつかないときに、際限なくメモリにためないための歯止め。
const CLIENT_QUEUE: usize = 256;

const READ_BUF: usize = 64 * 1024;

/// 括弧付き貼り付けの始まりと終わりの印。
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";
/// Ctrl+C。
const INTERRUPT: u8 = 0x03;
/// 続きの入力を待つプロンプトが出てから、取り消しの Ctrl+C を送るまでの間。
/// bash がプロンプトを描いている最中に届くと、割り込みを処理しないまま入力待ちに入ることがある。
const CANCEL_DELAY: Duration = Duration::from_millis(20);
/// 取り消しが効かなかったときに、Ctrl+C を送り直す間隔と回数。
const CANCEL_RETRY: Duration = Duration::from_millis(250);
const CANCEL_TRIES: usize = 20;

/// セッションからクライアントへ流すもの。
#[derive(Debug, Clone)]
pub(crate) enum ClientEvent {
    Output(Arc<[u8]>),
    Message(ServerMessage),
    Exited(u32),
}

/// 接続したクライアントが受け取るもの。
pub(crate) struct Attachment {
    /// 最初に送る、今の状態。生の表示では直近の出力 (`Replay`)。ブロックの表示では
    /// ブロックの一覧 (`History`) と、実行中のブロックのここまでの出力 (`Replay`)。
    pub greeting: Vec<ServerMessage>,
    /// このあとの出力と知らせ。`greeting` の続きから、抜けも重なりもなく届く。
    pub events: mpsc::Receiver<ClientEvent>,
}

struct Client {
    events: mpsc::Sender<ClientEvent>,
    /// 区切りを通さず、bash の出力をそのまま受け取る。
    raw: bool,
}

struct Shared {
    /// 生の表示のための、直近の出力。
    ring: Ring,
    /// 統合スクリプトを読み込ませていなければ `None` (区切れないので、ブロックはできない)。
    splitter: Option<Splitter>,
    blocks: Blocks,
    prompt: PromptState,
    /// `Run` で送って、まだ実行が始まっていないテキスト。
    pending: Option<String>,
    /// 実行中のブロックが `Run` で始まった。
    started_by_run: bool,
    /// 途中の入力を取り消している最中なら、その取り消しの番号。
    cancelling: Option<u64>,
    cancels: u64,
    cols: u16,
    rows: u16,
    clients: Vec<Client>,
    exited: Option<u32>,
}

impl Shared {
    fn context(&self) -> Context {
        Context { cwd: self.prompt.cwd.clone(), cols: self.cols, rows: self.rows, now: now() }
    }

    /// `Run` を受け付けられるか。
    fn ready(&self) -> bool {
        self.exited.is_none()
            && self.pending.is_none()
            && self.splitter.as_ref().is_some_and(Splitter::is_at_prompt)
    }

    /// PTY から読んだ出力を区切ってブロックに記録し、ブロックの表示へ流すものを `events` に足す。
    fn digest(&mut self, chunk: &[u8], canceller: &Canceller, events: &mut Vec<ClientEvent>) {
        let mut items = Vec::new();
        if let Some(splitter) = &mut self.splitter {
            splitter.feed(chunk, &mut items);
            // 取り消しが効いて、bash が次のプロンプトの準備に入った。
            if splitter.is_between() {
                self.cancelling = None;
            }
        }
        for item in items {
            match item {
                Item::Output(data) => {
                    self.blocks.push_output(&data);
                    events.push(ClientEvent::Output(data.into()));
                }
                Item::Outside(data) => self.blocks.push_outside(&data),
                Item::Event(Event::Started) => {
                    self.flush_outside(events);
                    let command = self.pending.take();
                    self.started_by_run = command.is_some();
                    let block =
                        self.blocks.start(Some(command.unwrap_or_default()), self.context());
                    events.push(ClientEvent::Message(ServerMessage::BlockStarted { block }));
                }
                Item::Event(Event::Finished { exit_code, fullscreen }) => {
                    self.finish_block(exit_code, fullscreen, events);
                }
                Item::Event(Event::Prompt(prompt)) => {
                    self.prompt = PromptState { cwd: prompt.cwd, branch: prompt.branch };
                    self.flush_outside(events);
                    // 何も実行されずにプロンプトへ戻った (空行やコメントだけの行)。
                    self.pending = None;
                    self.cancelling = None;
                    let prompt = self.prompt.clone();
                    events.push(ClientEvent::Message(ServerMessage::Prompt { prompt }));
                }
                // 入力欄から送ったテキストが途中だった。続きを bash に入力させずに取り消す。
                Item::Event(Event::Continuation) => {
                    if let Some(text) = self.pending.take() {
                        self.cancel(canceller);
                        let reason = RunRejection::Incomplete;
                        events.push(ClientEvent::Message(ServerMessage::RunRejected {
                            reason,
                            text,
                        }));
                    } else if self.started_by_run {
                        self.cancel(canceller);
                    }
                }
            }
        }
    }

    /// bash が待っている続きの入力を、Ctrl+C で取り消す。
    fn cancel(&mut self, canceller: &Canceller) {
        if self.cancelling.is_none() {
            self.cancels += 1;
            self.cancelling = Some(self.cancels);
            canceller.start(self.cancels);
        }
    }

    fn finish_block(
        &mut self,
        exit_code: Option<i32>,
        fullscreen: bool,
        events: &mut Vec<ClientEvent>,
    ) {
        self.started_by_run = false;
        let result = BlockResult { exit_code, finished_at: now(), fullscreen };
        if let Some(id) = self.blocks.finish(result.clone()) {
            events.push(ClientEvent::Message(ServerMessage::BlockFinished { id, result }));
        }
    }

    /// コマンドの外で出た出力を、始まってすぐ終わるブロックとして流す。
    fn flush_outside(&mut self, events: &mut Vec<ClientEvent>) {
        let Some((block, output)) = self.blocks.flush_outside(self.context()) else { return };
        let id = block.info.id;
        events.push(ClientEvent::Message(ServerMessage::BlockStarted { block: block.info }));
        events.push(ClientEvent::Output(output.into()));
        events
            .push(ClientEvent::Message(ServerMessage::BlockFinished { id, result: block.result }));
    }
}

/// 途中の入力を取り消す Ctrl+C を、効くまで送る。
struct Canceller {
    shared: Arc<Mutex<Shared>>,
    input: std::sync::mpsc::Sender<Vec<u8>>,
}

impl Canceller {
    fn start(&self, id: u64) {
        let shared = Arc::clone(&self.shared);
        let input = self.input.clone();
        let spawned = std::thread::Builder::new().name("pty-cancel".into()).spawn(move || {
            std::thread::sleep(CANCEL_DELAY);
            for _ in 0..CANCEL_TRIES {
                {
                    let shared = lock(&shared);
                    if shared.cancelling != Some(id) || shared.exited.is_some() {
                        return;
                    }
                }
                if input.send(vec![INTERRUPT]).is_err() {
                    return;
                }
                std::thread::sleep(CANCEL_RETRY);
            }
        });
        // スレッドを作れないときは、待たずに1回だけ送る。
        if spawned.is_err() {
            let _ = self.input.send(vec![INTERRUPT]);
        }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// PTY の下で動く bash 1つ。
pub(crate) struct Session {
    shared: Arc<Mutex<Shared>>,
    input: std::sync::mpsc::Sender<Vec<u8>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    pid: Option<u32>,
    /// bash のホームディレクトリ。
    home: Option<PathBuf>,
}

/// セッションの終わりを知らせる受け口。
pub(crate) struct SessionEnd {
    /// bash の終了コード。
    pub exited: oneshot::Receiver<u32>,
    /// PTY の出力を最後まで読み終えた。
    pub drained: oneshot::Receiver<()>,
}

impl Session {
    /// `integration` を渡すと、統合スクリプトを読み込ませて bash を起動する。
    ///
    /// `ring_capacity` は、流し直すために持つ出力の量 (生の表示の全体と、ブロック1つぶん)。
    /// `output_budget` は、終了したブロックの出力を合わせてどこまで持つか。
    pub fn spawn(
        shell: &ShellSpec,
        ring_capacity: usize,
        output_budget: usize,
        integration: Option<&Integration>,
    ) -> anyhow::Result<(Self, SessionEnd)> {
        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .context("PTY を開けません")?;
        let mut child = pair
            .slave
            .spawn_command(shell.command(integration))
            .with_context(|| format!("{} を起動できません", shell.program.display()))?;
        // 子プロセス側の端を持ち続けると、bash が終わっても読み取りが終わらない。
        drop(pair.slave);

        let pid = child.process_id();
        let mut reader = pair.master.try_clone_reader().context("PTY の読み取り口を開けません")?;
        let mut writer = pair.master.take_writer().context("PTY の書き込み口を開けません")?;

        let shared = Arc::new(Mutex::new(Shared {
            ring: Ring::new(ring_capacity),
            splitter: integration.map(|integration| Splitter::new(integration.nonce.as_str())),
            blocks: Blocks::new(ring_capacity, output_budget),
            prompt: PromptState::default(),
            pending: None,
            started_by_run: false,
            cancelling: None,
            cancels: 0,
            cols: 80,
            rows: 24,
            clients: Vec::new(),
            exited: None,
        }));

        let (input, input_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let canceller = Canceller { shared: Arc::clone(&shared), input: input.clone() };

        // PTY の読み書きはブロッキングなので、専用のスレッドで行う。
        let (drained_tx, drained) = oneshot::channel();
        let reader_shared = Arc::clone(&shared);
        std::thread::Builder::new().name("pty-reader".into()).spawn(move || {
            let mut buf = vec![0u8; READ_BUF];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let chunk: Arc<[u8]> = Arc::from(&buf[..n]);
                let mut events = Vec::new();
                // 記録と宛先の確定を同じロックの中で行い、
                // attach と競合しても抜けや重なりが出ないようにする。
                let clients = {
                    let mut shared = lock(&reader_shared);
                    shared.ring.push(&chunk);
                    shared.digest(&chunk, &canceller, &mut events);
                    shared.recipients()
                };
                for (client, raw) in clients {
                    if raw {
                        let _ = client.blocking_send(ClientEvent::Output(Arc::clone(&chunk)));
                        continue;
                    }
                    for event in &events {
                        let _ = client.blocking_send(event.clone());
                    }
                }
            }
            let _ = drained_tx.send(());
        })?;

        std::thread::Builder::new().name("pty-writer".into()).spawn(move || {
            for data in input_rx {
                if writer.write_all(&data).and_then(|()| writer.flush()).is_err() {
                    break;
                }
            }
        })?;

        let (exited_tx, exited) = oneshot::channel();
        std::thread::Builder::new().name("pty-wait".into()).spawn(move || {
            let code = child.wait().map(|status| status.exit_code()).unwrap_or(1);
            let _ = exited_tx.send(code);
        })?;

        let home = shell.home_dir();
        let session = Self { shared, input, master: Mutex::new(Some(pair.master)), pid, home };
        Ok((session, SessionEnd { exited, drained }))
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// bash が統合スクリプトを読み込んでいて、コマンドごとに区切れるか。
    pub fn integrated(&self) -> bool {
        lock(&self.shared).splitter.is_some()
    }

    pub fn attach(&self, raw: bool) -> Attachment {
        let (tx, events) = mpsc::channel(CLIENT_QUEUE);
        let mut shared = lock(&self.shared);
        let mut greeting = Vec::new();
        if raw {
            greeting.push(ServerMessage::Replay { data: shared.ring.snapshot() });
        } else {
            greeting.push(ServerMessage::History {
                blocks: shared.blocks.finished(),
                running: shared.blocks.running().cloned(),
                prompt: shared.prompt.clone(),
                ready: shared.ready(),
            });
            if let Some(data) = shared.blocks.running_output() {
                greeting.push(ServerMessage::Replay { data });
            }
        }
        match shared.exited {
            Some(code) => {
                let _ = tx.try_send(ClientEvent::Exited(code));
            }
            None => shared.clients.push(Client { events: tx, raw }),
        }
        Attachment { greeting, events }
    }

    pub fn write_input(&self, data: Vec<u8>) {
        let _ = self.input.send(data);
    }

    /// 入力欄のテキストを、括弧付き貼り付けで包んで bash に送る。
    /// 複数行でも1回の入力として渡る。プロンプトで入力を待っているときだけ受け付ける。
    pub fn run(&self, text: &str) -> Result<(), RunRejection> {
        let mut shared = lock(&self.shared);
        if !shared.ready() {
            return Err(RunRejection::Busy);
        }
        let text = sanitize(text);
        let _ = self.input.send(format!("{PASTE_START}{text}{PASTE_END}\r").into_bytes());
        shared.pending = Some(text);
        Ok(())
    }

    /// 入力欄の Tab 補完。`text` の `cursor` (バイト位置) の直前の単語を、bash の今の
    /// ディレクトリのファイル名として補う。ディレクトリを読むので、呼び出しは待たされることがある。
    pub fn complete(&self, text: &str, cursor: usize) -> Completion {
        let cwd = lock(&self.shared).prompt.cwd.clone().map(PathBuf::from);
        // プロンプトがまだ出ていなければ、bash はホームディレクトリにいる。
        match cwd.or_else(|| self.home.clone()) {
            Some(cwd) => complete(text, cursor, &cwd, self.home.as_deref()),
            None => Completion { start: cursor, ..Completion::default() },
        }
    }

    /// 終了したブロックの出力。そのブロックがなければ `None`。
    pub fn block_output(&self, id: u64) -> Option<StoredOutput> {
        lock(&self.shared).blocks.output(id)
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        let mut shared = lock(&self.shared);
        (shared.cols, shared.rows) = (cols, rows);
        drop(shared);
        if let Some(master) = lock(&self.master).as_ref() {
            let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    /// PTY を閉じる。
    pub fn close_pty(&self) {
        lock(&self.master).take();
    }

    /// つながっているクライアントすべてに終了を知らせる。出力のあとに届く。
    /// 実行中のブロックは、bash の終了コードで確定する。
    pub async fn announce_exit(&self, code: u32) {
        let mut events = Vec::new();
        let clients = {
            let mut shared = lock(&self.shared);
            shared.exited = Some(code);
            shared.pending = None;
            shared.finish_block(Some(code as i32), false, &mut events);
            shared.flush_outside(&mut events);
            std::mem::take(&mut shared.clients)
        };
        for client in clients {
            if !client.raw {
                for event in &events {
                    let _ = client.events.send(event.clone()).await;
                }
            }
            let _ = client.events.send(ClientEvent::Exited(code)).await;
        }
    }
}

impl Shared {
    /// いまつながっているクライアントの送り先と、生の表示かどうか。
    fn recipients(&mut self) -> Vec<(mpsc::Sender<ClientEvent>, bool)> {
        self.clients.retain(|client| !client.events.is_closed());
        self.clients.iter().map(|client| (client.events.clone(), client.raw)).collect()
    }
}

/// 貼り付けの終わりの印を取り除く (テキストの途中で貼り付けが終わってしまわないように)。
/// 行の区切りは LF にそろえ、末尾の改行は落とす。
fn sanitize(text: &str) -> String {
    let mut text = text.replace("\r\n", "\n").replace('\r', "\n");
    // 取り除いた前後がつながって、また印になることがある。
    while text.contains(PASTE_END) {
        text = text.replace(PASTE_END, "");
    }
    text.truncate(text.trim_end_matches('\n').len());
    text
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_removes_the_paste_end_mark_even_when_it_is_nested() {
        assert_eq!(sanitize("echo a\x1b[201~b"), "echo ab");
        assert_eq!(sanitize("a\x1b[20\x1b[201~1~b"), "ab");
        assert_eq!(sanitize("\x1b[200~x"), "\x1b[200~x");
    }

    #[test]
    fn sanitize_normalizes_line_ends() {
        assert_eq!(sanitize("echo a\r\necho b\recho c\n\n"), "echo a\necho b\necho c");
        assert_eq!(sanitize("  ls  "), "  ls  ");
    }
}
