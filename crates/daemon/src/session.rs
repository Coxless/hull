use std::io::{Read, Write};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Context;
use portable_pty::{native_pty_system, MasterPty, PtySize};
use tokio::sync::{mpsc, oneshot};

use crate::{Ring, ShellSpec};

/// 1クライアントぶんの待ち行列の長さ (チャンク数)。
///
/// いっぱいになると PTY の読み取りが止まり、出力しているコマンドが待たされる。
/// 画面の描画が追いつかないときに、際限なくメモリにためないための歯止め。
const CLIENT_QUEUE: usize = 256;

const READ_BUF: usize = 64 * 1024;

/// セッションからクライアントへ流すもの。
#[derive(Debug)]
pub(crate) enum ClientEvent {
    Output(Arc<[u8]>),
    Exited(u32),
}

/// 接続したクライアントが受け取るもの。
pub(crate) struct Attachment {
    /// 直前の画面を戻すための直近の出力。
    pub replay: Vec<u8>,
    /// このあとの出力。`replay` の続きから、抜けも重なりもなく届く。
    pub events: mpsc::Receiver<ClientEvent>,
}

struct Shared {
    ring: Ring,
    clients: Vec<mpsc::Sender<ClientEvent>>,
    exited: Option<u32>,
}

/// PTY の下で動く bash 1つ。
pub(crate) struct Session {
    shared: Arc<Mutex<Shared>>,
    input: std::sync::mpsc::Sender<Vec<u8>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    pid: Option<u32>,
}

/// セッションの終わりを知らせる受け口。
pub(crate) struct SessionEnd {
    /// bash の終了コード。
    pub exited: oneshot::Receiver<u32>,
    /// PTY の出力を最後まで読み終えた。
    pub drained: oneshot::Receiver<()>,
}

impl Session {
    pub fn spawn(shell: &ShellSpec, ring_capacity: usize) -> anyhow::Result<(Self, SessionEnd)> {
        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .context("PTY を開けません")?;
        let mut child = pair
            .slave
            .spawn_command(shell.command())
            .with_context(|| format!("{} を起動できません", shell.program.display()))?;
        // 子プロセス側の端を持ち続けると、bash が終わっても読み取りが終わらない。
        drop(pair.slave);

        let pid = child.process_id();
        let mut reader = pair.master.try_clone_reader().context("PTY の読み取り口を開けません")?;
        let mut writer = pair.master.take_writer().context("PTY の書き込み口を開けません")?;

        let shared = Arc::new(Mutex::new(Shared {
            ring: Ring::new(ring_capacity),
            clients: Vec::new(),
            exited: None,
        }));

        let (input, input_rx) = std::sync::mpsc::channel::<Vec<u8>>();

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
                // リングへの追記と宛先の確定を同じロックの中で行い、
                // attach と競合しても抜けや重なりが出ないようにする。
                let clients = {
                    let mut shared = lock(&reader_shared);
                    shared.ring.push(&chunk);
                    shared.clients.retain(|client| !client.is_closed());
                    shared.clients.clone()
                };
                for client in clients {
                    let _ = client.blocking_send(ClientEvent::Output(Arc::clone(&chunk)));
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

        let session = Self { shared, input, master: Mutex::new(Some(pair.master)), pid };
        Ok((session, SessionEnd { exited, drained }))
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn attach(&self) -> Attachment {
        let (tx, events) = mpsc::channel(CLIENT_QUEUE);
        let mut shared = lock(&self.shared);
        let replay = shared.ring.snapshot();
        match shared.exited {
            Some(code) => {
                let _ = tx.try_send(ClientEvent::Exited(code));
            }
            None => shared.clients.push(tx),
        }
        Attachment { replay, events }
    }

    pub fn write_input(&self, data: Vec<u8>) {
        let _ = self.input.send(data);
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        if let Some(master) = lock(&self.master).as_ref() {
            let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    /// PTY を閉じる。
    pub fn close_pty(&self) {
        lock(&self.master).take();
    }

    /// つながっているクライアントすべてに終了を知らせる。出力のあとに届く。
    pub async fn announce_exit(&self, code: u32) {
        let clients = {
            let mut shared = lock(&self.shared);
            shared.exited = Some(code);
            std::mem::take(&mut shared.clients)
        };
        for client in clients {
            let _ = client.send(ClientEvent::Exited(code)).await;
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
