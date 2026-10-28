//! PTY とシェル。
//!
//! 読み書きはノンブロッキング。書ききれなかった分はためておき、書けるようになったら続きを書く
//! (大きな貼り付けで、シェルの出力待ちと互いに止まらないようにするため)。

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

pub struct Pty {
    master: File,
    child: Child,
    pending: Vec<u8>,
}

#[derive(Clone, Copy)]
pub struct WinSize {
    pub cols: u16,
    pub rows: u16,
    pub width: u16,
    pub height: u16,
}

fn winsize(size: WinSize) -> libc::winsize {
    libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.width,
        ws_ypixel: size.height,
    }
}

fn set_nonblocking(fd: i32) -> io::Result<()> {
    // SAFETY: 開いている fd のフラグを読み書きするだけ。
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// ログインシェル。$SHELL、passwd、/bin/sh の順に探す。
pub fn default_shell() -> OsString {
    if let Some(shell) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) {
        return shell;
    }
    // SAFETY: getpwuid の結果は次の呼び出しまで有効。すぐに写す。
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_shell.is_null() {
            let s = std::ffi::CStr::from_ptr((*pw).pw_shell);
            if !s.to_bytes().is_empty() {
                use std::os::unix::ffi::OsStrExt;
                return std::ffi::OsStr::from_bytes(s.to_bytes()).to_os_string();
            }
        }
    }
    "/bin/sh".into()
}

impl Pty {
    pub fn spawn(program: &OsString, args: &[String], size: WinSize) -> io::Result<Pty> {
        let mut master = -1;
        let mut slave = -1;
        let ws = winsize(size);
        // SAFETY: openpty は成功すると 2 つの fd を返す。失敗時は何も開かない。
        let rc = unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &ws)
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openpty が返した fd の持ち主になる。
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // SAFETY: master に close-on-exec を立て、シェルに漏らさない。
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };

        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "hull")
            .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"))
            .env_remove("DESKTOP_STARTUP_ID")
            .env_remove("XDG_ACTIVATION_TOKEN");
        // ログインシェルとして起動する (argv[0] を "-bash" のようにする)。
        if args.is_empty() {
            let name = std::path::Path::new(program)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "sh".into());
            cmd.arg0(format!("-{name}"));
        }
        // SAFETY: fork と exec のあいだで、async-signal-safe な呼び出しだけを使う。
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                // 親が無視しているシグナルを既定に戻す。
                for sig in [
                    libc::SIGCHLD,
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGALRM,
                    libc::SIGPIPE,
                ] {
                    libc::signal(sig, libc::SIG_DFL);
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        set_nonblocking(master.as_raw_fd())?;
        Ok(Pty { master: File::from(master), child, pending: Vec::new() })
    }

    /// 書き込み待ちを見張るための、master の複製。
    pub fn dup_fd(&self) -> io::Result<OwnedFd> {
        self.master.as_fd().try_clone_to_owned()
    }

    pub fn resize(&self, size: WinSize) {
        let ws = winsize(size);
        // SAFETY: master に端末の大きさを伝える。子にはカーネルが SIGWINCH を送る。
        unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
    }

    /// 書く。書ききれない分はためて、`has_pending` が真になる。
    pub fn write(&mut self, data: &[u8]) {
        if self.pending.is_empty() {
            match self.master.write(data) {
                Ok(n) if n == data.len() => (),
                Ok(n) => self.pending.extend_from_slice(&data[n..]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.pending.extend_from_slice(data)
                }
                Err(_) => {}
            }
        } else {
            self.pending.extend_from_slice(data);
        }
    }

    /// ためた分の続きを書く。書ききったら `true`。
    pub fn flush_pending(&mut self) -> bool {
        while !self.pending.is_empty() {
            match self.master.write(&self.pending) {
                Ok(0) => break,
                Ok(n) => {
                    self.pending.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return false,
                Err(_) => {
                    self.pending.clear();
                }
            }
        }
        true
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// 子の終了を回収する。
    pub fn reap(&mut self) {
        let _ = self.child.try_wait();
    }
}

/// 受け取り側が追いつかないときに、読むスレッドがためておく上限。これを超えたら読むのを待つ。
const READ_AHEAD: usize = 8 << 20;

/// PTY を読むスレッドの中身。読めるだけ読んで送る。受け取り側 (描画もする本体) が追いついていなければ、
/// 読むのは止めずに 1 つにまとめてためておく (小さな書き込みが続くアプリを待たせないため)。
/// シェルが終わったら `None` を送って終わる。
pub fn read_loop(
    fd: OwnedFd,
    tx: smithay_client_toolkit::reexports::calloop::channel::SyncSender<Option<Vec<u8>>>,
) {
    use std::sync::mpsc::TrySendError;
    let mut file = File::from(fd);
    let mut buf = vec![0u8; 1 << 20];
    let mut pending: Vec<u8> = Vec::new();
    let mut closed = false;
    while !closed || !pending.is_empty() {
        if !closed && pending.len() < READ_AHEAD {
            // ためたものがあれば、送り直すために 1ms で起きる。なければ読めるまで眠る。
            let timeout = if pending.is_empty() { -1 } else { 1 };
            let mut pfd = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: 1 つの pollfd を渡して待つ。
            let ready = unsafe { libc::poll(&mut pfd, 1, timeout) };
            if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                closed = true;
            }
            if ready > 0 {
                loop {
                    match file.read(&mut buf) {
                        // 子が端末を閉じると EIO が返る。
                        Ok(0) => closed = true,
                        Err(e) if e.raw_os_error() == Some(libc::EIO) => closed = true,
                        Ok(n) => {
                            pending.extend_from_slice(&buf[..n]);
                            if pending.len() < READ_AHEAD {
                                continue;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(_) => closed = true,
                    }
                    break;
                }
            }
        } else if !pending.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if pending.is_empty() {
            continue;
        }
        match tx.try_send(Some(std::mem::take(&mut pending))) {
            Ok(()) => {}
            Err(TrySendError::Full(Some(back))) => pending = back,
            Err(_) => return,
        }
    }
    let _ = tx.send(None);
}

impl Drop for Pty {
    fn drop(&mut self) {
        // 窓を閉じたら、シェルに SIGHUP を送る (端末を閉じたときと同じ)。
        // SAFETY: 自分が起動した子プロセスのグループにシグナルを送る。
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGHUP) };
        let _ = self.child.try_wait();
    }
}
