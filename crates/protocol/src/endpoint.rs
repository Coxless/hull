use std::path::PathBuf;

/// 接続先を上書きする環境変数。テストや、複数のデーモンを分けて動かすときに使う。
pub const ENDPOINT_ENV: &str = "HULL_TERMINAL_SOCKET";

/// UI とデーモンが使う接続先。
///
/// - Unix: `$XDG_RUNTIME_DIR/hull-terminal/daemon.sock`
/// - Windows: `\\.\pipe\hull-terminal-<ユーザー名>`
pub fn default_endpoint() -> String {
    if let Some(endpoint) = std::env::var_os(ENDPOINT_ENV).filter(|v| !v.is_empty()) {
        return endpoint.to_string_lossy().into_owned();
    }
    platform_endpoint()
}

#[cfg(windows)]
fn platform_endpoint() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    // パイプ名に使えない文字 (`\`) などを避ける。
    let user: String = user
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect();
    format!(r"\\.\pipe\hull-terminal-{user}")
}

#[cfg(not(windows))]
fn platform_endpoint() -> String {
    runtime_dir().join("daemon.sock").to_string_lossy().into_owned()
}

#[cfg(not(windows))]
fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("hull-terminal"),
        None => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
            std::env::temp_dir().join(format!("hull-terminal-{user}"))
        }
    }
}

/// デーモンのログなどを置く、ユーザーごとのディレクトリ。
pub fn state_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::temp_dir().join("hull-terminal")
    }
    #[cfg(not(windows))]
    {
        runtime_dir()
    }
}
