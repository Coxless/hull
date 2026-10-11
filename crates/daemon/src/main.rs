// Windows でコンソール画面を出さない。
#![cfg_attr(windows, windows_subsystem = "windows")]

use hull_terminal_daemon::{run, Config, ShellSpec};

const USAGE: &str = "\
hull-terminal-daemon: hull-terminal の PTY を持つ管理デーモン

使い方: hull-terminal-daemon [--endpoint <接続先>] [--shell <bash の場所>]

  --endpoint  待ち受ける Unix ソケットのパス、または名前付きパイプの名前
              (省略時は環境変数 HULL_TERMINAL_SOCKET、なければ既定の場所)
  --shell     起動する bash (省略時は環境変数 HULL_TERMINAL_SHELL、なければ自動で探す)
";

fn main() {
    let config = match parse_args(std::env::args().skip(1)) {
        Ok(Some(config)) => config,
        Ok(None) => {
            print!("{USAGE}");
            return;
        }
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio のランタイムを作れません");
    match runtime.block_on(run(config)) {
        // bash の終了コードは Exited で UI に伝えてある。デーモン自体は正常終了。
        Ok(_) => {}
        Err(e) => {
            eprintln!("hull-terminal-daemon: {e:#}");
            std::process::exit(1);
        }
    }
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Config>, String> {
    let mut config = Config::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--endpoint" => {
                config.endpoint = args.next().ok_or("--endpoint には接続先が必要です")?;
            }
            "--shell" => {
                let program = args.next().ok_or("--shell には bash の場所が必要です")?;
                let mut shell = ShellSpec::detect().map_err(|e| e.to_string())?;
                shell.program = program.into();
                config.shell = Some(shell);
            }
            other => return Err(format!("不明な引数です: {other}")),
        }
    }
    Ok(Some(config))
}
