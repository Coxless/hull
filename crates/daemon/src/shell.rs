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
    /// この OS で使う bash を探す。
    ///
    /// - Ubuntu: `bash -i`
    /// - Windows: Git Bash の `bin\bash.exe --login -i`
    pub fn detect() -> anyhow::Result<Self> {
        let args = default_args();
        if let Some(program) = std::env::var_os(SHELL_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self { program: program.into(), args });
        }
        Ok(Self { program: find_bash()?, args })
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
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_dir())
}

#[cfg(not(windows))]
fn default_args() -> Vec<String> {
    vec!["-i".into()]
}

#[cfg(not(windows))]
fn find_bash() -> anyhow::Result<PathBuf> {
    Ok("bash".into())
}

#[cfg(windows)]
fn default_args() -> Vec<String> {
    vec!["--login".into(), "-i".into()]
}

/// Git Bash を探す。`C:\Windows\System32\bash.exe` は WSL の起動用なので使わない。
#[cfg(windows)]
fn find_bash() -> anyhow::Result<PathBuf> {
    let candidates = git_bash_candidates(
        |name| std::env::var_os(name).map(PathBuf::from),
        std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default(),
    );
    candidates.into_iter().find(|path| path.is_file()).ok_or_else(|| {
        anyhow::anyhow!(
            "Git Bash が見つかりません。Git for Windows を入れるか、環境変数 {SHELL_ENV} に bash.exe の場所を指定してください"
        )
    })
}

/// Git Bash がありそうな場所を、確からしい順に並べる。
#[cfg_attr(not(windows), allow(dead_code))]
fn git_bash_candidates(
    env: impl Fn(&str) -> Option<PathBuf>,
    path_dirs: Vec<PathBuf>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(dir) = env(var) {
            candidates.push(dir.join("Git").join("bin").join("bash.exe"));
        }
    }
    if let Some(dir) = env("LOCALAPPDATA") {
        candidates.push(dir.join("Programs").join("Git").join("bin").join("bash.exe"));
    }
    if let Some(dir) = env("USERPROFILE") {
        // scoop で入れた Git
        candidates.push(
            dir.join("scoop").join("apps").join("git").join("current").join("bin").join("bash.exe"),
        );
    }
    // PATH に `<Git>\cmd\git.exe` があれば、その隣の `bin\bash.exe`。
    for dir in path_dirs {
        if dir.join("git.exe").is_file() {
            if let Some(root) = dir.parent() {
                candidates.push(root.join("bin").join("bash.exe"));
            }
        }
    }
    candidates.dedup();
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_bash_candidates_prefer_program_files_and_skip_missing_vars() {
        let env = |name: &str| match name {
            "ProgramFiles" => Some(PathBuf::from("C:/Program Files")),
            "LOCALAPPDATA" => Some(PathBuf::from("C:/Users/me/AppData/Local")),
            _ => None,
        };
        let candidates = git_bash_candidates(env, Vec::new());
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("C:/Program Files/Git/bin/bash.exe"),
                PathBuf::from("C:/Users/me/AppData/Local/Programs/Git/bin/bash.exe"),
            ]
        );
    }

    #[test]
    fn git_bash_candidates_use_git_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = dir.path().join("Git").join("cmd");
        std::fs::create_dir_all(&cmd).unwrap();
        std::fs::write(cmd.join("git.exe"), b"").unwrap();

        let candidates = git_bash_candidates(|_| None, vec![dir.path().join("other"), cmd]);
        assert_eq!(candidates, vec![dir.path().join("Git").join("bin").join("bash.exe")]);
    }
}
