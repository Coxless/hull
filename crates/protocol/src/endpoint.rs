use std::path::PathBuf;

/// 接続先を上書きする環境変数。テストや、複数のデーモンを分けて動かすときに使う。
pub const ENDPOINT_ENV: &str = "HULL_TERMINAL_SOCKET";

/// UI とデーモンが使う接続先 (`$XDG_RUNTIME_DIR/hull-terminal/daemon.sock`)。
pub fn default_endpoint() -> String {
    if let Some(endpoint) = std::env::var_os(ENDPOINT_ENV).filter(|v| !v.is_empty()) {
        return endpoint.to_string_lossy().into_owned();
    }
    runtime_dir().join("daemon.sock").to_string_lossy().into_owned()
}

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
    runtime_dir()
}
