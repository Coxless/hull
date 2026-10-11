use std::path::PathBuf;

use portable_pty::CommandBuilder;

/// 起動するシェルを上書きする環境変数。
pub const SHELL_ENV: &str = "HULL_TERMINAL_SHELL";

/// PTY の下で起動するシェル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl ShellSpec {
    /// 起動する bash を決める (`bash -i`)。
    pub fn detect() -> anyhow::Result<Self> {
        let program = std::env::var_os(SHELL_ENV)
            .filter(|v| !v.is_empty())
            .map_or_else(|| "bash".into(), PathBuf::from);
        Ok(Self { program, args: vec!["-i".into()] })
    }

    pub(crate) fn command(&self) -> CommandBuilder {
        let mut cmd = CommandBuilder::new(&self.program);
        cmd.args(&self.args);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "hull-terminal");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        if let Some(home) = home_dir() {
            cmd.cwd(home);
        }
        cmd
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_dir())
}
