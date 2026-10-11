//! デーモンへの接続と、いないときの起動。GPU が使えるかの確認もここで行う。

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hull_terminal_protocol::{
    state_dir,
    transport::{self, LocalRecvHalf, LocalSendHalf},
};

/// デーモンの実行ファイルの場所を上書きする環境変数。
const DAEMON_ENV: &str = "HULL_TERMINAL_DAEMON";

const DAEMON_NAME: &str = "hull-terminal-daemon";

/// 起動したデーモンが待ち受けを始めるまで待つ時間。
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Connected {
    pub recv: LocalRecvHalf,
    pub send: LocalSendHalf,
    /// この接続のためにデーモンを起動したか (false なら、動いていたデーモンに再接続した)。
    pub started_daemon: bool,
}

/// デーモンに接続する。いなければ起動して接続し直す。
pub async fn connect_or_spawn(endpoint: &str) -> Result<Connected, String> {
    if let Ok((recv, send)) = transport::connect(endpoint).await {
        return Ok(Connected { recv, send, started_daemon: false });
    }

    spawn(endpoint)?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        match transport::connect(endpoint).await {
            Ok((recv, send)) => return Ok(Connected { recv, send, started_daemon: true }),
            Err(e) if Instant::now() >= deadline => {
                return Err(format!(
                    "デーモンに接続できません ({e})。ログ: {}",
                    log_path().display()
                ));
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(30)).await,
        }
    }
}

fn daemon_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os(DAEMON_ENV).filter(|v| !v.is_empty()) {
        return Ok(path.into());
    }
    // アプリと同じディレクトリに置く (開発中は target/debug、配布物ではアプリの隣)。
    let exe = std::env::current_exe().map_err(|e| format!("自分の場所がわかりません: {e}"))?;
    let path = exe.with_file_name(DAEMON_NAME);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "{} が見つかりません。`cargo build -p hull-terminal-daemon` でビルドするか、環境変数 {DAEMON_ENV} で場所を指定してください",
            path.display()
        ))
    }
}

fn log_path() -> PathBuf {
    state_dir().join("daemon.log")
}

/// デーモンを、UI が終了しても生き続ける独立したプロセスとして起動する。
fn spawn(endpoint: &str) -> Result<(), String> {
    let path = daemon_path()?;

    let log = log_path();
    if let Some(dir) = log.parent() {
        create_private_dir(dir);
    }
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());

    let mut command = Command::new(&path);
    command
        .arg("--endpoint")
        .arg(endpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);

    let child =
        spawn_detached(command).map_err(|e| format!("{} を起動できません: {e}", path.display()))?;
    // 先に終了した場合にゾンビを残さないよう、待つだけのスレッドを置く。
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

fn spawn_detached(mut command: Command) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;

    // 新しいセッションに入れて、UI を起動した端末の Ctrl+C や切断の影響を受けないようにする。
    // SAFETY: fork と exec の間で呼ぶのは、async-signal-safe な setsid だけ。
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command.spawn()
}

fn create_private_dir(dir: &std::path::Path) {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
}

/// 画面を GPU で描けるか。描けない環境では、WebGL ではなく DOM で描く。
///
/// WebKitGTK は GPU がなくても WebGL をソフトウェアで動かすが、DOM 描画より重く、
/// GPU での合成を切ってある (`WEBKIT_DISABLE_COMPOSITING_MODE`) と画面が正しく更新されない。
/// GPU を扱えるドライバーは `/dev/dri/renderD*` を作るので、その有無で見分ける。
pub fn gpu_available() -> bool {
    if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE")
        .is_some_and(|v| !v.is_empty() && v != "0")
    {
        return false;
    }
    std::fs::read_dir("/dev/dri").is_ok_and(|entries| {
        entries.flatten().any(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
    })
}
