use std::path::PathBuf;

use portable_pty::CommandBuilder;

use crate::integration::{Integration, NONCE_ENV};

/// 起動するシェルを上書きする環境変数。
pub const SHELL_ENV: &str = "HULL_TERMINAL_SHELL";

/// PTY の下で起動するシェル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// デーモンの環境に足す (または上書きする) 環境変数。
    pub env: Vec<(String, String)>,
    /// 統合スクリプトを読み込ませて、コマンドの区切りの合図を出させるか。
    pub integration: bool,
}

impl ShellSpec {
    /// 起動する bash を決める (`bash -i`)。
    pub fn detect() -> anyhow::Result<Self> {
        let program = std::env::var_os(SHELL_ENV)
            .filter(|v| !v.is_empty())
            .map_or_else(|| "bash".into(), PathBuf::from);
        Ok(Self { program, args: vec!["-i".into()], env: Vec::new(), integration: true })
    }

    pub(crate) fn command(&self, integration: Option<&Integration>) -> CommandBuilder {
        let mut cmd = CommandBuilder::new(&self.program);
        if let Some(integration) = integration {
            // 長い名前のオプションは、1文字のオプションより前に置く必要がある。
            cmd.arg("--rcfile");
            cmd.arg(&integration.script);
            cmd.env(NONCE_ENV, &integration.nonce);
        }
        cmd.args(&self.args);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "hull-terminal");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        for (name, value) in &self.env {
            cmd.env(name, value);
        }
        if let Some(home) = self.home_dir() {
            cmd.cwd(home);
        }
        cmd
    }

    fn home_dir(&self) -> Option<PathBuf> {
        self.env
            .iter()
            .rev()
            .find(|(name, _)| name == "HOME")
            .map(|(_, value)| value.into())
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
    }
}
