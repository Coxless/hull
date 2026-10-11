//! デーモンへの接続と、いないときの起動。この OS で何が使えるかの確認もここで行う。

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hull_terminal_protocol::{
    state_dir,
    transport::{self, LocalRecvHalf, LocalSendHalf},
};

/// デーモンの実行ファイルの場所を上書きする環境変数。
const DAEMON_ENV: &str = "HULL_TERMINAL_DAEMON";

const DAEMON_NAME: &str =
    if cfg!(windows) { "hull-terminal-daemon.exe" } else { "hull-terminal-daemon" };

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

#[cfg(unix)]
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

#[cfg(windows)]
fn spawn_detached(mut command: Command) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    // UI がジョブに入れられていると、UI の終了でデーモンも止められることがある。
    // まずジョブから外して起動を試み、許されていなければそのまま起動する。
    let base = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    command.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB);
    match command.spawn() {
        Ok(child) => Ok(child),
        Err(_) => {
            command.creation_flags(base);
            command.spawn()
        }
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &std::path::Path) {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
}

#[cfg(not(unix))]
fn create_private_dir(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
}

/// この OS で PTY が使えない場合の警告。
///
/// ConPTY は Windows 10 1809 (ビルド 17763) から。それより古いと bash を起動できない。
#[cfg(windows)]
pub fn platform_warning() -> Option<String> {
    const CONPTY_MIN_BUILD: u32 = 17763;

    #[repr(C)]
    struct OsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform_id: u32,
        csd_version: [u16; 128],
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }

    let mut info = OsVersionInfo {
        size: std::mem::size_of::<OsVersionInfo>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform_id: 0,
        csd_version: [0; 128],
    };
    // SAFETY: `info` は RtlGetVersion が求める OSVERSIONINFOW と同じ配置で、size を設定してある。
    let status = unsafe { RtlGetVersion(&mut info) };
    let too_old = info.major < 10 || (info.major == 10 && info.build < CONPTY_MIN_BUILD);
    if status == 0 && too_old {
        Some(format!(
            "この Windows (ビルド {}) では hull-terminal は動きません。Windows 10 バージョン 1809 (ビルド {CONPTY_MIN_BUILD}) 以降が必要です",
            info.build
        ))
    } else {
        None
    }
}

#[cfg(not(windows))]
pub fn platform_warning() -> Option<String> {
    None
}

/// 画面を GPU で描けるか。描けない環境では、WebGL ではなく DOM で描く。
///
/// WebKitGTK は GPU がなくても WebGL をソフトウェアで動かすが、DOM 描画より重く、
/// GPU での合成を切ってある (`WEBKIT_DISABLE_COMPOSITING_MODE`) と画面が正しく更新されない。
/// GPU を扱えるドライバーは `/dev/dri/renderD*` を作るので、その有無で見分ける。
#[cfg(target_os = "linux")]
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

#[cfg(not(target_os = "linux"))]
pub fn gpu_available() -> bool {
    true
}
