//! hull: Ubuntu 専用に削った、速いターミナル。
//!
//! タブ・分割・セッションは持たず tmux に任せる。足りない機能はプラグインで足す。

mod app;
mod boxdraw;
mod config;
mod font;
mod gpu;
mod plugin;
mod pty;
mod render;
mod snapshot;
mod theme;
mod wl;

use smithay_client_toolkit::reexports::calloop::{EventLoop, channel};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use wayland_client::{Connection, globals::registry_queue_init};

use crate::app::App;
use crate::config::Config;

/// `HULL_TRACE=1` のとき、起動からの時間と段階を出す (起動の速さを調べるため)。
pub fn trace(what: &str) {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Option<Instant>> = OnceLock::new();
    let start = START.get_or_init(|| std::env::var_os("HULL_TRACE").map(|_| Instant::now()));
    if let Some(start) = start {
        eprintln!("hull-trace {:7.2}ms {what}", start.elapsed().as_secs_f64() * 1000.0);
    }
}

const USAGE: &str = "\
使い方: hull [オプション] [-e コマンド [引数...]]

  -e, --command コマンド  シェルの代わりにコマンドを起動する (残りの引数はコマンドに渡す)
      --config ファイル   設定ファイル (既定: ~/.config/hull/hull.toml)
      --print-config      既定値を書いた設定ファイルを出す
      --bench-startup     最初のフレームが画面に出た時刻を出して終わる (計測用)
  -h, --help              この説明
  -V, --version           版
";

struct Args {
    command: Option<(String, Vec<String>)>,
    config: Option<std::path::PathBuf>,
    bench_startup: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { command: None, config: None, bench_startup: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-e" | "--command" => {
                let program = it.next().ok_or("-e のあとにコマンドがありません")?;
                args.command = Some((program, it.by_ref().collect()));
            }
            "--config" => {
                args.config = Some(it.next().ok_or("--config のあとにファイルがありません")?.into())
            }
            "--print-config" => {
                print!("{}", config::EXAMPLE);
                std::process::exit(0);
            }
            "--bench-startup" => args.bench_startup = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("hull {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => return Err(format!("知らないオプション: {other}\n\n{USAGE}")),
        }
    }
    args.bench_startup |= std::env::var_os("HULL_BENCH_STARTUP").is_some();
    Ok(args)
}

fn run() -> Result<(), String> {
    trace("main");
    let args = parse_args()?;
    // fontconfig の初期化は 15ms ほどかかるので、いちばん先に別のスレッドで始める。
    let font_config = font::FontConfig::start();
    let config_path = args.config.clone().unwrap_or_else(config::config_path);
    let (mut config, error) = Config::load(&config_path);
    if let Some(e) = error {
        eprintln!("hull: 設定を読めません。既定値で起動します: {e}");
    }
    if let Some((program, rest)) = args.command {
        config.shell.program = Some(program);
        config.shell.args = rest;
    }

    // GNOME の設定は gsettings で読む。前に読んだ値を覚えていれば、それで起動して、あとで確かめる。
    let needs_desktop = config.window.theme == config::ThemeChoice::Auto
        || config.font.family.is_none()
        || config.font.size.is_none();
    let cached_desktop = needs_desktop.then(theme::Desktop::load_cached).flatten();
    let desktop_query = needs_desktop.then(|| std::thread::spawn(theme::query_desktop));

    let conn = Connection::connect_to_env().map_err(|e| format!("Wayland につなげません: {e}"))?;
    let (globals, queue) = registry_queue_init::<App>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().map_err(|e| e.to_string())?;
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| e.to_string())?;

    trace("wayland");
    // EGL の初期化はドライバによっては 100ms ほどかかるので、別のスレッドで進める。
    let (egl_tx, egl_rx) = channel::channel();
    let wl_display = conn.backend().display_ptr() as usize;
    std::thread::spawn(move || {
        let _ = egl_tx.send(gpu::EglSetup::new(wl_display));
    });
    event_loop
        .handle()
        .insert_source(egl_rx, |event, _, app: &mut App| {
            if let channel::Event::Msg(setup) = event {
                app.gpu_ready(setup);
            }
        })
        .map_err(|e| e.to_string())?;
    let desktop = match (cached_desktop, desktop_query) {
        (Some(cached), Some(query)) => {
            let (tx, rx) = channel::channel();
            std::thread::spawn(move || {
                if let Ok(d) = query.join() {
                    let _ = tx.send(d);
                }
            });
            event_loop
                .handle()
                .insert_source(rx, |event, _, app: &mut App| {
                    if let channel::Event::Msg(d) = event {
                        app.desktop_checked(d);
                    }
                })
                .map_err(|e| e.to_string())?;
            cached
        }
        (None, Some(query)) => {
            let d = query.join().unwrap_or_default();
            d.save_cached();
            d
        }
        _ => theme::Desktop::default(),
    };
    trace("gsettings");

    let mut app = App::new(
        qh,
        event_loop.handle(),
        &globals,
        font_config,
        config_path,
        config,
        desktop,
        args.bench_startup,
    )?;
    trace("app");
    app.register_sources()?;

    while !app.exit {
        event_loop.dispatch(None, &mut app).map_err(|e| e.to_string())?;
    }
    match app.fatal.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("hull: {e}");
        std::process::exit(1);
    }
}
