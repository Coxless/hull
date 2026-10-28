//! 1 つの窓の状態と、PTY・入力・描画のつなぎ。
//!
//! 窓ごとの状態はすべてここに持ち、グローバルにしない
//! (あとでサーバーモードが要るとわかったときに、1 プロセスで複数の `App` を持てるように)。

use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    activation::{ActivationState, RequestData},
    compositor::{CompositorState, FrameCallbackData},
    data_device_manager::{
        DataDeviceManagerState, data_device::DataDevice, data_source::CopyPasteSource,
    },
    output::OutputState,
    presentation_time::PresentationTimeState,
    primary_selection::{
        PrimarySelectionManagerState, device::PrimarySelectionDevice,
        selection::PrimarySelectionSource,
    },
    reexports::{
        calloop::{
            Interest, LoopHandle, Mode, PostAction, RegistrationToken, channel,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        protocols::wp::{
            fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1,
            text_input::zv3::client::zwp_text_input_v3::{
                ContentHint, ContentPurpose, ZwpTextInputV3,
            },
            viewporter::client::wp_viewport::WpViewport,
        },
    },
    registry::RegistryState,
    seat::{
        SeatState,
        keyboard::{KeyEvent, Keysym, Modifiers},
        pointer::cursor_shape::CursorShapeManager,
    },
    shell::{WaylandSurface, xdg::window::Window},
    shm::{
        Shm,
        slot::{Buffer, SlotPool},
    },
};
use wayland_client::{
    QueueHandle,
    protocol::{wl_keyboard::WlKeyboard, wl_pointer::WlPointer, wl_seat::WlSeat, wl_shm},
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};

use hull_term::input::{self, Key, KeyAction, Mods, MouseAction, MouseButton};
use hull_term::{Event, MouseMode, Point, PromptMark, Selection, SelectionKind, Terminal};

use crate::config::{self, Action, Config, KeyCombo};
use crate::font::{FontConfig, Fonts};
use crate::gpu::Gpu;
use crate::plugin::{self, PluginHost};
use crate::pty::{Pty, WinSize};
use crate::render::{self, Button, Header, Layout, Renderer, Scene};
use crate::theme::{self, Desktop, Scheme};

/// 見出しの帯の高さ (論理ピクセル)。
const HEADER_HEIGHT: f32 = 38.0;
/// 窓の端でサイズを変えられる幅 (論理ピクセル)。
const RESIZE_BORDER: f64 = 6.0;
/// synchronized output を待つ上限。
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);
/// 出力が止まってから、履歴の行を詰めるまでの時間。
const COMPACT_DELAY: Duration = Duration::from_secs(1);
/// PTY を読むスレッドから受け取るのを待つ塊の数。埋まっていれば、読むスレッドが 1 つにまとめてためる。
const PTY_QUEUE: usize = 2;

pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

/// 選んだ文字のクリップボードへの書き方。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Clipboard {
    Clipboard,
    Primary,
}

#[derive(Default)]
pub struct Ime {
    enabled: bool,
    /// 最後に知らせたカーソルの位置。
    rect: Option<[i32; 4]>,
    pub preedit: Option<String>,
    pub pending_preedit: Option<String>,
    pub pending_commit: Option<String>,
}

/// 計測のために、画面に出た時刻を知りたいフレーム。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// GPU の準備ができる前に CPU で描いた、最初のフレーム。
    First,
    /// GPU で描いた最初のフレーム。
    Gpu,
    /// キーを押したあとの最初のフレーム (押した時刻、ミリ秒)。
    Latency(u32),
}

pub struct Gfx {
    pub gpu: Gpu,
    pub renderer: Renderer,
}

pub struct App {
    pub qh: QueueHandle<App>,
    pub handle: LoopHandle<'static, App>,
    pub registry: RegistryState,
    pub seat_state: SeatState,
    pub output_state: OutputState,
    _compositor: CompositorState,
    pub window: Window,
    pub data_device_manager: Option<DataDeviceManagerState>,
    pub primary_manager: Option<PrimarySelectionManagerState>,
    pub activation: Option<ActivationState>,
    pub presentation: Option<PresentationTimeState>,
    pub cursor_shape: Option<CursorShapeManager>,
    pub fractional: Option<WpFractionalScaleV1>,
    pub viewport: Option<WpViewport>,
    pub text_input_manager: Option<
        smithay_client_toolkit::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    >,

    pub seat: Option<WlSeat>,
    pub keyboard: Option<WlKeyboard>,
    pub pointer: Option<WlPointer>,
    pub shape_device: Option<WpCursorShapeDeviceV1>,
    pub text_input: Option<ZwpTextInputV3>,
    pub data_device: Option<DataDevice>,
    pub primary_device: Option<PrimarySelectionDevice>,
    pub copy_source: Option<(CopyPasteSource, String)>,
    pub primary_source: Option<(PrimarySelectionSource, String)>,
    pub last_serial: u32,
    pub pointer_serial: u32,
    pub ime: Ime,

    /// GPU の描画。EGL の初期化が済むまでは `None` で、そのあいだは CPU で背景だけを出す。
    pub gfx: Option<Gfx>,
    pub shm: Option<Shm>,
    /// GPU の準備ができる前に出した、背景だけのフレーム (バッファと大きさ)。
    placeholder: Option<(SlotPool, Buffer, (u32, u32))>,
    /// 計測: 画面に出たかを待っているフレーム (wp_presentation の答えと同じ順に並ぶ)。
    pub probes: std::collections::VecDeque<Probe>,
    pub bench_reported_first: bool,
    bench_gpu_sent: bool,
    /// `HULL_TRACE_LATENCY` のとき、最後に押したキーの時刻 (コンポジタの時計のミリ秒)。
    latency_key: Option<u32>,
    trace_latency: bool,
    pub fatal: Option<String>,
    pub fonts: Fonts,
    font_config: Arc<FontConfig>,
    pub term: Terminal,
    pub pty: Pty,
    pty_write_token: Option<RegistrationToken>,
    pub plugins: PluginHost,

    pub config_path: PathBuf,
    pub config: Config,
    keybinds: Vec<(KeyCombo, Action)>,
    pub desktop: Desktop,
    pub scheme: Scheme,
    font_points: f32,

    /// バッファの倍率 (分数スケールなら 1.25 など)。
    pub scale: f32,
    /// 窓の大きさ (論理ピクセル)。
    pub size: (u32, u32),
    pub configured: bool,
    frame_pending: bool,
    needs_draw: bool,
    sync_timer: Option<RegistrationToken>,
    /// 出力が止まってから履歴の行を詰めるタイマー。
    compact_timer: Option<RegistrationToken>,
    /// 最後に出力を受け取った時刻 (止まってから履歴の行を詰めるため)。
    last_output: Instant,
    pub focused: bool,
    pub maximized: bool,
    pub tiled: bool,
    pub client_decorations: bool,

    pub modifiers: Modifiers,
    selection: Option<Selection>,
    selecting: bool,
    last_click: Option<(Instant, Point, u8)>,
    pub pointer_pos: (f64, f64),
    hovered: Option<Button>,
    pressed_button: Option<MouseButton>,
    scroll_accum: f64,
    cursor_shape_now: Option<Shape>,
    title: String,

    pub bench_startup: bool,
    /// 確認用: 時間がたったら、窓の中身をこのファイルに書き出して終わる。
    snapshot: Option<(PathBuf, bool)>,
    pub exit: bool,
}

/// 設定ファイルの見張り。
pub fn watch_config(path: &std::path::Path) -> Option<OwnedFd> {
    // SAFETY: inotify の fd を作り、持ち主になる。
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: 作ったばかりの fd。
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_DELETE;
    let dir = path.parent()?;
    for d in [dir, dir.parent()?] {
        if let Ok(c) = std::ffi::CString::new(d.as_os_str().as_encoded_bytes()) {
            // SAFETY: 有効な fd とパス。見張れなくても困らない。
            unsafe { libc::inotify_add_watch(fd.as_raw_fd(), c.as_ptr(), mask) };
        }
    }
    Some(fd)
}

fn key_from_keysym(keysym: Keysym) -> Option<Key> {
    Some(match keysym {
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::Tab | Keysym::ISO_Left_Tab => Key::Tab,
        Keysym::BackSpace => Key::Backspace,
        Keysym::Escape => Key::Escape,
        Keysym::Up | Keysym::KP_Up => Key::Up,
        Keysym::Down | Keysym::KP_Down => Key::Down,
        Keysym::Left | Keysym::KP_Left => Key::Left,
        Keysym::Right | Keysym::KP_Right => Key::Right,
        Keysym::Home | Keysym::KP_Home => Key::Home,
        Keysym::End | Keysym::KP_End => Key::End,
        Keysym::Page_Up | Keysym::KP_Page_Up => Key::PageUp,
        Keysym::Page_Down | Keysym::KP_Page_Down => Key::PageDown,
        Keysym::Insert | Keysym::KP_Insert => Key::Insert,
        Keysym::Delete | Keysym::KP_Delete => Key::Delete,
        k if (Keysym::F1.raw()..=Keysym::F12.raw()).contains(&k.raw()) => {
            Key::F((k.raw() - Keysym::F1.raw() + 1) as u8)
        }
        k => {
            let c = k.key_char()?;
            if c.is_control() {
                return None;
            }
            // シフトを効かせる前の文字にする (kitty の形と、キーバインドの照合のため)。
            Key::Char(if c.is_uppercase() { c.to_lowercase().next().unwrap_or(c) } else { c })
        }
    })
}

/// 名前で書くキー。キーバインドとプラグインで使う名前は、この表から両方向に引く。
const KEY_NAMES: [(&str, Key); 18] = [
    ("plus", Key::Char('+')),
    ("minus", Key::Char('-')),
    ("equal", Key::Char('=')),
    ("space", Key::Char(' ')),
    ("enter", Key::Enter),
    ("tab", Key::Tab),
    ("backspace", Key::Backspace),
    ("escape", Key::Escape),
    ("up", Key::Up),
    ("down", Key::Down),
    ("left", Key::Left),
    ("right", Key::Right),
    ("home", Key::Home),
    ("end", Key::End),
    ("pageup", Key::PageUp),
    ("pagedown", Key::PageDown),
    ("insert", Key::Insert),
    ("delete", Key::Delete),
];

/// キーの名前から、キーに戻す (プラグインが返したキーを打つため)。
pub fn key_from_name(name: &str) -> Option<Key> {
    if let Some(&(_, key)) = KEY_NAMES.iter().find(|(n, _)| *n == name) {
        return Some(key);
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return (1..=12).contains(&n).then_some(Key::F(n));
    }
    let mut chars = name.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(Key::Char(ch))
}

/// キーバインドで使うキーの名前。
pub fn key_name(key: Key) -> String {
    if let Some((name, _)) = KEY_NAMES.iter().find(|(_, k)| *k == key) {
        return (*name).into();
    }
    match key {
        Key::Char(c) => c.to_string(),
        Key::F(n) => format!("f{n}"),
        _ => unreachable!("名前の表にないキー: {key:?}"),
    }
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        qh: QueueHandle<App>,
        handle: LoopHandle<'static, App>,
        globals: &wayland_client::globals::GlobalList,
        font_config: Arc<FontConfig>,
        config_path: PathBuf,
        config: Config,
        desktop: Desktop,
        bench_startup: bool,
    ) -> Result<App, String> {
        use smithay_client_toolkit::shell::xdg::{XdgShell, window::WindowDecorations};

        let compositor =
            CompositorState::bind(globals, &qh).map_err(|e| format!("wl_compositor: {e}"))?;
        let xdg = XdgShell::bind(globals, &qh).map_err(|e| format!("xdg_wm_base: {e}"))?;
        let surface = compositor.create_surface(&qh);
        let decorations = if config.window.decorations {
            WindowDecorations::RequestServer
        } else {
            WindowDecorations::None
        };
        let window = xdg.create_window(surface, decorations, &qh);
        window.set_title("hull");
        window.set_app_id("hull");
        window.set_min_size(Some((200, 100)));

        let fractional_manager = globals
            .bind::<smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, crate::wl::Protocol)
            .ok();
        let viewporter = globals
            .bind::<smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewporter::WpViewporter, _, _>(&qh, 1..=1, crate::wl::Protocol)
            .ok();
        let (fractional, viewport) = match (fractional_manager, viewporter) {
            (Some(fm), Some(vp)) => (
                Some(fm.get_fractional_scale(window.wl_surface(), &qh, crate::wl::Protocol)),
                Some(vp.get_viewport(window.wl_surface(), &qh, crate::wl::Protocol)),
            ),
            _ => (None, None),
        };
        window.commit();

        let scheme = Scheme::resolve(&config, &desktop);
        let (family, points) = theme::font_choice(&config, &desktop);
        let fonts = Fonts::new(
            &family,
            points * 96.0 / 72.0,
            &theme::font_lang(&config),
            font_config.clone(),
        )?;
        crate::trace("fonts");

        let m = fonts.metrics;
        let cols = usize::from(config.window.columns.max(2));
        let rows = usize::from(config.window.lines.max(1));
        let mut term = Terminal::new(cols, rows, config.scrollback.lines);
        term.set_cell_size(m.cell_width, m.cell_height);
        apply_scheme(&mut term, &scheme);

        let program =
            config.shell.program.clone().map(Into::into).unwrap_or_else(crate::pty::default_shell);
        let pty = Pty::spawn(
            &program,
            &config.shell.args,
            WinSize { cols: cols as u16, rows: rows as u16, width: 0, height: 0 },
        )
        .map_err(|e| format!("シェル {} を起動できません: {e}", program.to_string_lossy()))?;
        crate::trace("pty");

        let size = (
            config.window.padding * 2 + m.cell_width * cols as u32,
            config.window.padding * 2 + m.cell_height * rows as u32,
        );
        let keybinds = config::keybinds(&config);
        let shm = Shm::bind(globals, &qh).ok();
        let trace_latency = std::env::var_os("HULL_TRACE_LATENCY").is_some();
        Ok(App {
            registry: RegistryState::new(globals),
            seat_state: SeatState::new(globals, &qh),
            output_state: OutputState::new(globals, &qh),
            data_device_manager: DataDeviceManagerState::bind(globals, &qh).ok(),
            primary_manager: PrimarySelectionManagerState::bind(globals, &qh).ok(),
            activation: ActivationState::bind(globals, &qh).ok(),
            presentation: (bench_startup || trace_latency)
                .then(|| PresentationTimeState::bind(globals, &qh)),
            cursor_shape: CursorShapeManager::bind(globals, &qh).ok(),
            text_input_manager: globals.bind(&qh, 1..=1, crate::wl::Protocol).ok(),
            _compositor: compositor,
            window,
            fractional,
            viewport,
            qh,
            handle,
            seat: None,
            keyboard: None,
            pointer: None,
            shape_device: None,
            text_input: None,
            data_device: None,
            primary_device: None,
            copy_source: None,
            primary_source: None,
            last_serial: 0,
            pointer_serial: 0,
            ime: Ime::default(),
            gfx: None,
            shm,
            placeholder: None,
            probes: std::collections::VecDeque::new(),
            bench_reported_first: false,
            bench_gpu_sent: false,
            latency_key: None,
            trace_latency,
            fatal: None,
            fonts,
            font_config,
            term,
            pty,
            pty_write_token: None,
            plugins: PluginHost::default(),
            config_path,
            keybinds,
            desktop,
            scheme,
            font_points: points,
            config,
            scale: 1.0,
            size,
            configured: false,
            frame_pending: false,
            needs_draw: true,
            sync_timer: None,
            compact_timer: None,
            last_output: Instant::now(),
            focused: false,
            maximized: false,
            tiled: false,
            client_decorations: false,
            modifiers: Modifiers::default(),
            selection: None,
            selecting: false,
            last_click: None,
            pointer_pos: (0.0, 0.0),
            hovered: None,
            pressed_button: None,
            scroll_accum: 0.0,
            cursor_shape_now: None,
            title: "hull".into(),
            bench_startup,
            snapshot: std::env::var_os("HULL_SCREENSHOT").map(|p| (PathBuf::from(p), false)),
            exit: false,
        })
    }

    /// PTY やファイルの見張りをイベントループにつなぐ。
    pub fn register_sources(&mut self) -> Result<(), String> {
        // PTY は別のスレッドで読む。描画しているあいだもシェルの出力を受け取れるので、
        // 大量の出力で、シェルが hull の描画を待たされない。
        let fd = self.pty.dup_fd().map_err(|e| e.to_string())?;
        let (tx, rx) = channel::sync_channel::<Option<Vec<u8>>>(PTY_QUEUE);
        std::thread::Builder::new()
            .name("hull-pty".into())
            .spawn(move || crate::pty::read_loop(fd, tx))
            .map_err(|e| e.to_string())?;
        self.handle
            .insert_source(rx, |event, _, app: &mut App| match event {
                channel::Event::Msg(Some(data)) => app.on_pty_data(&data),
                _ => {
                    app.pty.reap();
                    app.exit = true;
                }
            })
            .map_err(|e| e.to_string())?;

        if let Some(fd) = watch_config(&self.config_path) {
            self.handle
                .insert_source(
                    Generic::new(fd, Interest::READ, Mode::Level),
                    |_, fd, app: &mut App| {
                        let mut buf = [0u8; 4096];
                        // SAFETY: inotify の fd から、たまったイベントを読み捨てる。
                        while unsafe {
                            libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())
                        } > 0
                        {}
                        app.reload_config();
                        Ok(PostAction::Continue)
                    },
                )
                .map_err(|e| e.to_string())?;
        }

        // GNOME の明暗とフォントの変更を見張る (自分で決めているときは要らない)。
        let needs_desktop = self.config.window.theme == config::ThemeChoice::Auto
            || self.config.font.family.is_none()
            || self.config.font.size.is_none();
        if needs_desktop && let Some((child, reader)) = theme::monitor() {
            let fd = reader.get_ref().as_raw_fd();
            // SAFETY: パイプをノンブロッキングにする。
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK)
            };
            let mut state = (child, reader);
            let dup = state.1.get_ref().as_fd().try_clone_to_owned().map_err(|e| e.to_string())?;
            self.handle
                .insert_source(
                    Generic::new(dup, Interest::READ, Mode::Level),
                    move |_, _, app: &mut App| {
                        let lines = theme::read_lines(&mut state.1);
                        if lines.is_empty() {
                            let _ = state.0.kill();
                            let _ = state.0.wait();
                            return Ok(PostAction::Remove);
                        }
                        let mut changed = false;
                        for line in &lines {
                            changed |= theme::apply_monitor_line(&mut app.desktop, line);
                        }
                        if changed {
                            app.apply_config(false);
                        }
                        Ok(PostAction::Continue)
                    },
                )
                .map_err(|e| e.to_string())?;
        }
        self.start_plugins();
        if let Ok(keys) = std::env::var("HULL_TEST_KEYS") {
            self.schedule_test_keys(&keys)?;
        }
        if self.snapshot.is_some() {
            let delay = std::env::var("HULL_SCREENSHOT_DELAY_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1500);
            self.handle
                .insert_source(
                    Timer::from_duration(Duration::from_millis(delay)),
                    |_, _, app: &mut App| {
                        if let Some(s) = &mut app.snapshot {
                            s.1 = true;
                        }
                        if let Some(gfx) = &mut app.gfx {
                            gfx.renderer.invalidate();
                        }
                        // 画面が消えているとフレームのコールバックが来ないので、待たずに描く。
                        app.frame_pending = false;
                        app.request_draw();
                        TimeoutAction::Drop
                    },
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    // ---- PTY ----

    fn on_pty_data(&mut self, data: &[u8]) {
        let was_sync = self.term.modes.sync;
        self.term.advance(data);
        if !was_sync && self.term.modes.sync {
            self.start_sync_timer();
        }
        self.after_output();
    }

    /// 端末に出力を流したあとの片づけ (応答、イベント、描画)。
    fn after_output(&mut self) {
        // 大量の出力のあいだは履歴の行を詰めず、止まってからまとめて詰める。
        self.last_output = Instant::now();
        if self.compact_timer.is_none() {
            self.compact_timer = self
                .handle
                .insert_source(Timer::from_duration(COMPACT_DELAY), |_, _, app: &mut App| {
                    // まだ出力が続いていれば、止まってから COMPACT_DELAY たつまで待つ。
                    let due = app.last_output + COMPACT_DELAY;
                    if Instant::now() < due {
                        return TimeoutAction::ToInstant(due);
                    }
                    app.compact_timer = None;
                    app.term.compact_history();
                    // 詰めて空いたメモリは細切れなので、malloc に OS へ返させる。
                    // SAFETY: glibc の関数で、引数は 0 (残す余白なし)。
                    unsafe { libc::malloc_trim(0) };
                    TimeoutAction::Drop
                })
                .ok();
        }
        let reply = self.term.take_reply();
        if !reply.is_empty() {
            self.write_pty(&reply);
        }
        for event in self.term.take_events() {
            self.on_term_event(event);
        }
        if self.term.take_colors_changed() {
            self.needs_draw = true;
        }
        // 出力で行が流れたら、選択がずれないよう絶対番号で持っているので、そのままでよい。
        self.request_draw();
    }

    fn on_term_event(&mut self, event: Event) {
        match &event {
            Event::Title(title) => self.set_title(title.clone()),
            Event::Bell => {
                if self.config.bell.attention && !self.focused {
                    self.request_attention();
                }
            }
            Event::Prompt(mark) => self.note_prompt(*mark),
            Event::Clipboard { primary, text } => {
                let target = if *primary { Clipboard::Primary } else { Clipboard::Clipboard };
                self.set_clipboard(target, text.clone());
            }
            _ => {}
        }
        if let Some(ev) = plugin::event_from_term(&event) {
            self.send_plugin_event(ev);
        }
    }

    pub fn write_pty(&mut self, data: &[u8]) {
        self.pty.write(data);
        if self.pty.has_pending() && self.pty_write_token.is_none() {
            let Ok(fd) = self.pty.dup_fd() else { return };
            let token = self
                .handle
                .insert_source(
                    Generic::new(fd, Interest::WRITE, Mode::Level),
                    |_, _, app: &mut App| {
                        if app.pty.flush_pending() {
                            app.pty_write_token = None;
                            return Ok(PostAction::Remove);
                        }
                        Ok(PostAction::Continue)
                    },
                )
                .ok();
            self.pty_write_token = token;
        }
    }

    fn start_sync_timer(&mut self) {
        if let Some(t) = self.sync_timer.take() {
            self.handle.remove(t);
        }
        self.sync_timer = self
            .handle
            .insert_source(Timer::from_duration(SYNC_TIMEOUT), |_, _, app: &mut App| {
                // 終わりの合図が来なくても、待つのはここまで。
                app.sync_timer = None;
                app.term.modes.sync = false;
                app.request_draw();
                TimeoutAction::Drop
            })
            .ok();
    }

    // ---- 窓の大きさと描画 ----

    pub fn layout(&self) -> Layout {
        let header =
            if self.client_decorations { (HEADER_HEIGHT * self.scale).round() as u32 } else { 0 };
        Layout {
            width: (self.size.0 as f32 * self.scale).round() as u32,
            height: (self.size.1 as f32 * self.scale).round() as u32 + header,
            header,
            padding: (self.config.window.padding as f32 * self.scale).round() as u32,
            scale: self.scale,
        }
    }

    /// 窓の大きさ (論理ピクセル、見出しを含む)。
    pub fn window_size(&self) -> (u32, u32) {
        let header = if self.client_decorations { HEADER_HEIGHT as u32 } else { 0 };
        (self.size.0, self.size.1 + header)
    }

    /// 窓の大きさや倍率が変わったら、バッファと端末の大きさを合わせる。
    pub fn apply_size(&mut self) {
        let layout = self.layout();
        if let Some(vp) = &self.viewport {
            let (w, h) = self.window_size();
            vp.set_destination(w as i32, h as i32);
        } else {
            // 分数スケールが使えないときは、倍率は整数。
            self.window.wl_surface().set_buffer_scale(self.scale as i32);
        }
        if let Some(gfx) = &mut self.gfx {
            let _ =
                gfx.gpu.resize(self.window.wl_surface(), layout.width as i32, layout.height as i32);
        }
        let m = self.fonts.metrics;
        let pad = layout.padding * 2;
        let cols = ((layout.width.saturating_sub(pad)) / m.cell_width).max(2) as usize;
        let rows =
            ((layout.height.saturating_sub(pad + layout.header)) / m.cell_height).max(1) as usize;
        if cols != self.term.cols() || rows != self.term.rows() {
            self.term.resize(cols, rows);
            self.pty.resize(WinSize {
                cols: cols as u16,
                rows: rows as u16,
                width: (cols as u32 * m.cell_width) as u16,
                height: (rows as u32 * m.cell_height) as u16,
            });
            self.send_plugin_event(plugin::HostEvent::Resize { cols, rows });
        }
        self.term.set_cell_size(m.cell_width, m.cell_height);
        self.needs_draw = true;
        self.update_ime_cursor();
    }

    /// 描画を頼む。フレームのコールバックを待っていなければ、すぐに描く。
    pub fn request_draw(&mut self) {
        self.needs_draw = true;
        if !self.frame_pending {
            self.draw();
        }
    }

    pub fn on_frame(&mut self) {
        self.frame_pending = false;
        if self.needs_draw {
            self.draw();
        }
    }

    /// 起動のあとで読み直した GNOME の設定。覚えていたものと違えば反映する。
    pub fn desktop_checked(&mut self, desktop: Desktop) {
        if desktop != self.desktop {
            desktop.save_cached();
            self.desktop = desktop;
            self.apply_config(false);
        }
    }

    /// 別のスレッドで EGL の初期化が済んだ。
    pub fn gpu_ready(&mut self, setup: Result<crate::gpu::EglSetup, String>) {
        let gfx = setup.and_then(|setup| {
            let gpu = Gpu::new(setup)?;
            let renderer = Renderer::new(&gpu.gl)?;
            Ok(Gfx { gpu, renderer })
        });
        match gfx {
            Ok(gfx) => {
                crate::trace("gpu ready");
                self.gfx = Some(gfx);
                if self.configured {
                    self.apply_size();
                    self.request_draw();
                }
            }
            Err(e) => {
                self.fatal = Some(e);
                self.exit = true;
            }
        }
    }

    fn draw(&mut self) {
        if !self.configured {
            return;
        }
        // アプリが画面をまとめて書き換えているあいだは待つ (mode 2026)。
        if self.term.modes.sync {
            return;
        }
        if self.gfx.is_none() {
            self.draw_placeholder();
            return;
        }
        self.needs_draw = false;
        let layout = self.layout();
        let selection = self.selection.map(|s| s.range(&self.term));
        let (highlights, overlays) = self.plugins.visible_decorations(&self.term);
        let header = self.client_decorations.then(|| Header {
            title: &self.title,
            focused: self.focused,
            hovered: self.hovered,
            maximized: self.maximized,
            rounded: !self.maximized && !self.tiled,
        });
        let scene = Scene {
            term: &self.term,
            scheme: &self.scheme,
            layout,
            focused: self.focused,
            selection,
            highlights: &highlights,
            overlays: &overlays,
            preedit: self.ime.preedit.as_deref(),
            header,
        };
        let Some(gfx) = &mut self.gfx else { return };
        if !gfx.gpu.has_surface() {
            return;
        }
        gfx.gpu.make_current();
        let age = gfx.gpu.buffer_age();
        let Some(damage) = gfx.renderer.render(&gfx.gpu.gl, &mut self.fonts, &scene, age) else {
            return;
        };
        crate::trace("render");
        if let Some((path, true)) = &self.snapshot {
            let pixels = crate::snapshot::read_pixels(&gfx.gpu.gl, layout.width, layout.height);
            let png = crate::snapshot::encode_png(layout.width, layout.height, &pixels);
            if let Err(e) = std::fs::write(path, png) {
                eprintln!("hull: {}: {e}", path.display());
            }
            self.exit = true;
        }
        let surface = self.window.wl_surface().clone();
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        let probe = if self.bench_startup && !self.bench_gpu_sent {
            self.bench_gpu_sent = true;
            Some(Probe::Gpu)
        } else {
            self.latency_key.take().map(Probe::Latency)
        };
        if let (Some(probe), Some(p)) = (probe, &self.presentation)
            && p.feedback(&surface, &self.qh).is_ok()
        {
            self.probes.push_back(probe);
        }
        self.frame_pending = true;
        gfx.gpu.swap(&damage);
        crate::trace("swap");
        // GPU で描き始めたら、背景だけのフレームは要らない。
        self.placeholder = None;
        self.update_ime_cursor();
    }

    /// GPU の準備ができる前に、背景と見出しの帯だけを CPU で描いて出す。
    /// シェルがプロンプトを出す前の最初のフレームは、どの端末でも空の画面なので、見た目は変わらない。
    fn draw_placeholder(&mut self) {
        let layout = self.layout();
        let (w, h) = (layout.width, layout.height);
        if self.placeholder.as_ref().is_some_and(|p| p.2 == (w, h)) {
            return;
        }
        let Some(shm) = &self.shm else { return };
        let Ok(mut pool) = SlotPool::new((w * h * 4) as usize, shm) else { return };
        let Ok((buffer, canvas)) =
            pool.create_buffer(w as i32, h as i32, (w * 4) as i32, wl_shm::Format::Argb8888)
        else {
            return;
        };
        let bg = self.scheme.background;
        let header = self.scheme.header_backdrop;
        let radius = if self.client_decorations && !self.maximized && !self.tiled {
            (12.0 * self.scale).round()
        } else {
            0.0
        };
        // 1 行分の色を作り、行ごとに写す (1 ピクセルずつ計算すると、窓が大きいと 10ms ほどかかる)。
        let pixel = |[r, g, b]: [u8; 3], a: f32| {
            let m = |v: u8| (f32::from(v) * a).round() as u8;
            [m(b), m(g), m(r), (a * 255.0).round() as u8]
        };
        let stride = (w * 4) as usize;
        let row_of = |color: [u8; 3]| pixel(color, 1.0).repeat(w as usize);
        let (header_row, bg_row) = (row_of(header), row_of(bg));
        for (y, row) in canvas.chunks_exact_mut(stride).enumerate() {
            let in_header = (y as u32) < layout.header;
            row.copy_from_slice(if in_header { &header_row } else { &bg_row });
            // 見出しの上の角を丸める (GPU で描くときと同じ形)。
            let fy = y as f32 + 0.5;
            if fy < radius {
                let color = if in_header { header } else { bg };
                for x in (0..radius as usize).chain(w as usize - radius as usize..w as usize) {
                    let fx = x as f32 + 0.5;
                    let cx = if fx < radius { radius } else { w as f32 - radius };
                    let d = ((fx - cx).powi(2) + (fy - radius).powi(2)).sqrt() - radius;
                    let a = (0.5 - d).clamp(0.0, 1.0);
                    row[x * 4..x * 4 + 4].copy_from_slice(&pixel(color, a));
                }
            }
        }
        let surface = self.window.wl_surface();
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_err() {
            return;
        }
        if self.bench_startup
            && let Some(p) = &self.presentation
            && p.feedback(surface, &self.qh).is_ok()
        {
            self.probes.push_back(Probe::First);
        }
        surface.commit();
        crate::trace("placeholder");
        self.placeholder = Some((pool, buffer, (w, h)));
    }

    pub fn set_title(&mut self, title: String) {
        let shown = if title.is_empty() { "hull".to_string() } else { title };
        let shown = match self.plugins.title_mark() {
            Some(mark) => format!("{mark} {shown}"),
            None => shown,
        };
        if shown != self.title {
            self.window.set_title(shown.clone());
            self.title = shown;
            self.request_draw();
        }
    }

    /// プラグインが付けた印を反映するため、題名を付け直す。
    pub fn refresh_title(&mut self) {
        let title = self.term.title().to_string();
        self.set_title(title);
    }

    pub fn request_attention(&mut self) {
        if let Some(activation) = &self.activation {
            activation.request_token(
                &self.qh,
                RequestData {
                    app_id: Some("hull".into()),
                    seat_and_serial: None,
                    surface: Some(self.window.wl_surface().clone()),
                    udata: (),
                },
            );
        }
    }

    // ---- 設定 ----

    pub fn reload_config(&mut self) {
        let (config, error) = Config::load(&self.config_path);
        if let Some(e) = error {
            eprintln!("hull: 設定を読めません。前の設定のままにします: {e}");
            return;
        }
        if config == self.config {
            return;
        }
        self.config = config;
        self.apply_config(true);
    }

    /// 設定や GNOME の設定が変わったら、見た目とキーバインドとプラグインに反映する。
    pub fn apply_config(&mut self, config_changed: bool) {
        self.keybinds = config::keybinds(&self.config);
        self.term.set_max_history(self.config.scrollback.lines);
        let scheme = Scheme::resolve(&self.config, &self.desktop);
        if scheme != self.scheme {
            self.scheme = scheme;
            apply_scheme(&mut self.term, &self.scheme);
        }
        let (family, points) = theme::font_choice(&self.config, &self.desktop);
        if family != self.fonts.family() {
            match Fonts::new(
                &family,
                points * 96.0 / 72.0 * self.scale,
                &theme::font_lang(&self.config),
                self.font_config.clone(),
            ) {
                Ok(fonts) => {
                    self.fonts = fonts;
                    self.font_points = points;
                    if let Some(gfx) = &mut self.gfx {
                        gfx.gpu.make_current();
                        gfx.renderer.clear_glyphs(&gfx.gpu.gl);
                    }
                }
                Err(e) => eprintln!("hull: {e}"),
            }
        } else if points != self.font_points {
            self.font_points = points;
            self.update_font_size();
        }
        if config_changed {
            self.restart_plugins_if_changed();
        }
        self.apply_size();
        self.request_draw();
    }

    pub fn update_font_size(&mut self) {
        self.fonts.set_size(self.font_points * 96.0 / 72.0 * self.scale);
        if let Some(gfx) = &mut self.gfx {
            gfx.gpu.make_current();
            gfx.renderer.clear_glyphs(&gfx.gpu.gl);
        }
    }

    pub fn set_scale(&mut self, scale: f32) {
        if (scale - self.scale).abs() < f32::EPSILON {
            return;
        }
        self.scale = scale;
        self.update_font_size();
        if self.configured {
            self.apply_size();
            self.request_draw();
        }
    }

    // ---- キー ----

    pub fn mods(&self) -> Mods {
        Mods {
            shift: self.modifiers.shift,
            alt: self.modifiers.alt,
            ctrl: self.modifiers.ctrl,
            logo: self.modifiers.logo,
        }
    }

    pub fn on_key(&mut self, event: KeyEvent, action: KeyAction) {
        if self.trace_latency && action == KeyAction::Press {
            self.latency_key = Some(event.time);
        }
        let Some(key) = key_from_keysym(event.keysym) else { return };
        let mods = self.mods();
        self.handle_key(key, mods, event.utf8, action);
    }

    fn handle_key(&mut self, key: Key, mods: Mods, text: Option<String>, action: KeyAction) {
        // プラグインがキーを受け取っているあいだは、すべてプラグインに渡す。
        if self.plugins.grabbing() {
            if action != KeyAction::Release {
                self.plugins.send_key(plugin::HostEvent::Key {
                    key: key_name(key),
                    text: text.clone().unwrap_or_default(),
                    mods: plugin::mods_list(mods),
                });
                let handle = self.handle.clone();
                self.plugins.ensure_write_watch(&handle);
            }
            return;
        }

        if action != KeyAction::Release {
            let name = key_name(key);
            let bound = self
                .keybinds
                .iter()
                .find(|(combo, _)| combo.mods == mods && combo.key == name)
                .map(|(_, a)| a.clone());
            if let Some(action) = bound {
                self.run_action(action);
                return;
            }
        }

        let kitty = self.term.kitty_flags();
        let bytes = input::encode_key(key, mods, action, text.as_deref(), &self.term.modes, kitty);
        if let Some(bytes) = bytes {
            self.write_pty(&bytes);
            if self.term.display_offset() != 0 {
                self.term.scroll_to_bottom();
                self.request_draw();
            }
        }
    }

    /// 確認用: `HULL_TEST_KEYS="ctrl+shift+f,e,r,enter"` のキーを、起動の 0.5 秒後から順に打ったことにする。
    /// キーを打てない環境 (CI や自動の確認) で、キーバインドやプラグインを通しで試すために使う。
    fn schedule_test_keys(&mut self, keys: &str) -> Result<(), String> {
        let mut at = Duration::from_millis(500);
        for part in keys.split(',').filter(|p| !p.is_empty()) {
            let combo = KeyCombo::parse(part)?;
            let key = key_from_name(&combo.key).ok_or_else(|| format!("知らないキー: {part}"))?;
            let text = match key {
                Key::Char(c) if !combo.mods.ctrl && !combo.mods.alt => {
                    Some(if combo.mods.shift { c.to_uppercase().collect() } else { c.to_string() })
                }
                _ => None,
            };
            self.handle
                .insert_source(Timer::from_duration(at), move |_, _, app: &mut App| {
                    app.handle_key(key, combo.mods, text.clone(), KeyAction::Press);
                    TimeoutAction::Drop
                })
                .map_err(|e| e.to_string())?;
            at += Duration::from_millis(100);
        }
        Ok(())
    }

    /// プラグインが返したキーを、端末に打ったことにする。
    pub fn forward_key(&mut self, name: &str, text: &str, mods: &[String]) {
        let has = |m: &str| mods.iter().any(|x| x == m);
        let mods =
            Mods { shift: has("shift"), alt: has("alt"), ctrl: has("ctrl"), logo: has("super") };
        let encoded = key_from_name(name).and_then(|key| {
            let text = (!text.is_empty()).then_some(text);
            input::encode_key(
                key,
                mods,
                KeyAction::Press,
                text,
                &self.term.modes,
                self.term.kitty_flags(),
            )
        });
        match encoded {
            Some(bytes) => self.write_pty(&bytes),
            // IME で確定した文字のように、キーの名前がないものは文字をそのまま送る。
            None if !text.is_empty() => self.write_pty(text.as_bytes()),
            None => {}
        }
    }

    pub fn on_ime_commit(&mut self, text: &str) {
        if self.plugins.grabbing() {
            self.plugins.send_key(plugin::HostEvent::Key {
                key: String::new(),
                text: text.to_string(),
                mods: Vec::new(),
            });
            return;
        }
        self.write_pty(text.as_bytes());
        self.term.scroll_to_bottom();
        self.request_draw();
    }

    /// IME にカーソルの位置を知らせる。送るたびにコンポジタが done を返すので、変わったときだけ送る。
    pub fn update_ime_cursor(&mut self) {
        let Some(ti) = &self.text_input else { return };
        if !self.ime.enabled {
            return;
        }
        let layout = self.layout();
        let (ox, oy) = layout.origin();
        let m = self.fonts.metrics;
        let (row, col) = self.term.cursor();
        let s = self.scale;
        let rect = [
            ((ox + col as f32 * m.cell_width as f32) / s) as i32,
            ((oy + row as f32 * m.cell_height as f32) / s) as i32,
            (m.cell_width as f32 / s) as i32,
            (m.cell_height as f32 / s) as i32,
        ];
        if self.ime.rect == Some(rect) {
            return;
        }
        self.ime.rect = Some(rect);
        ti.set_cursor_rectangle(rect[0], rect[1], rect[2], rect[3]);
        ti.commit();
    }

    pub fn enable_ime(&mut self) {
        let Some(ti) = &self.text_input else { return };
        if self.ime.enabled {
            return;
        }
        ti.enable();
        ti.set_content_type(ContentHint::None, ContentPurpose::Terminal);
        self.ime.enabled = true;
        self.ime.rect = None;
        self.update_ime_cursor();
    }

    pub fn disable_ime(&mut self) {
        let Some(ti) = &self.text_input else { return };
        if !self.ime.enabled {
            return;
        }
        ti.disable();
        ti.commit();
        self.ime.enabled = false;
    }

    pub fn on_focus(&mut self, focused: bool) {
        self.focused = focused;
        if self.term.modes.focus_events {
            self.write_pty(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
        if focused {
            self.enable_ime();
        } else {
            self.disable_ime();
            self.ime.preedit = None;
        }
        self.send_plugin_event(plugin::HostEvent::Focus { focused });
        self.request_draw();
    }

    pub fn run_action(&mut self, action: Action) {
        let page = self.term.rows().saturating_sub(1).max(1) as isize;
        match action {
            Action::Copy => {
                if let Some(text) =
                    self.selection.map(|s| s.text(&self.term)).filter(|t| !t.is_empty())
                {
                    self.set_clipboard(Clipboard::Clipboard, text);
                }
            }
            Action::Paste => self.paste(Clipboard::Clipboard),
            Action::PastePrimary => self.paste(Clipboard::Primary),
            Action::ScrollPageUp => self.term.scroll_display(page),
            Action::ScrollPageDown => self.term.scroll_display(-page),
            Action::ScrollLineUp => self.term.scroll_display(1),
            Action::ScrollLineDown => self.term.scroll_display(-1),
            Action::ScrollTop => self.term.scroll_display(isize::MAX / 2),
            Action::ScrollBottom => self.term.scroll_to_bottom(),
            Action::FontBigger | Action::FontSmaller | Action::FontReset => {
                self.font_points = match action {
                    Action::FontBigger => (self.font_points + 1.0).min(200.0),
                    Action::FontSmaller => (self.font_points - 1.0).max(4.0),
                    _ => theme::font_choice(&self.config, &self.desktop).1,
                };
                self.update_font_size();
                self.apply_size();
            }
            Action::PromptPrevious | Action::PromptNext => {
                let up = action == Action::PromptPrevious;
                let top = self.term.visible_abs(0);
                if let Some(line) = self.plugins.prompt_line(top, up) {
                    self.term.scroll_to_line(line);
                }
            }
            Action::Plugin { plugin, name } => {
                self.plugins.send_to(&plugin, plugin::HostEvent::Keybind { name });
                let handle = self.handle.clone();
                self.plugins.ensure_write_watch(&handle);
            }
            Action::None => {}
        }
        self.request_draw();
    }

    // ---- クリップボード ----

    pub fn set_clipboard(&mut self, target: Clipboard, text: String) {
        let serial = self.last_serial;
        match target {
            Clipboard::Clipboard => {
                let (Some(manager), Some(device)) = (&self.data_device_manager, &self.data_device)
                else {
                    return;
                };
                let source =
                    manager.create_copy_paste_source(&self.qh, crate::wl::TEXT_MIME_TYPES.to_vec());
                source.set_selection(device, serial);
                self.copy_source = Some((source, text));
            }
            Clipboard::Primary => {
                let (Some(manager), Some(device)) = (&self.primary_manager, &self.primary_device)
                else {
                    return;
                };
                let source =
                    manager.create_selection_source(&self.qh, crate::wl::TEXT_MIME_TYPES.to_vec());
                source.set_selection(device, serial);
                self.primary_source = Some((source, text));
            }
        }
    }

    pub fn paste(&mut self, from: Clipboard) {
        let pipe = match from {
            Clipboard::Clipboard => {
                // 自分でコピーしたものは、そのまま使う。
                if let Some((_, text)) = &self.copy_source {
                    let text = text.clone();
                    self.paste_text(&text);
                    return;
                }
                let Some(offer) =
                    self.data_device.as_ref().and_then(|d| d.data().selection_offer())
                else {
                    return;
                };
                let Some(mime) = offer.with_mime_types(crate::wl::pick_mime) else { return };
                offer.receive(mime).ok()
            }
            Clipboard::Primary => {
                if let Some((_, text)) = &self.primary_source {
                    let text = text.clone();
                    self.paste_text(&text);
                    return;
                }
                let Some(offer) =
                    self.primary_device.as_ref().and_then(|d| d.data().selection_offer())
                else {
                    return;
                };
                let Some(mime) = offer.with_mime_types(crate::wl::pick_mime) else { return };
                offer.receive(mime).ok()
            }
        };
        let Some(pipe) = pipe else { return };
        let mut data = Vec::new();
        let _ = self.handle.insert_source(pipe, move |_, file, app: &mut App| {
            // SAFETY: パイプの fd は閉じずに読むだけ。
            let f: &mut std::fs::File = unsafe { file.get_mut() };
            let mut buf = [0u8; 8192];
            loop {
                match f.read(&mut buf) {
                    Ok(0) => {
                        let text = String::from_utf8_lossy(&data).into_owned();
                        app.paste_text(&text);
                        return PostAction::Remove;
                    }
                    Ok(n) => data.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        return PostAction::Continue;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return PostAction::Remove,
                }
            }
        });
    }

    /// 貼り付ける。改行は CR にし、bracketed paste なら囲む。
    pub fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut body = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.term.modes.bracketed_paste {
            // 貼り付けの中に終わりの合図があると、そこで貼り付けが終わったことにされてしまう。
            body = body.replace("\x1b[201~", "");
            let mut data = b"\x1b[200~".to_vec();
            data.extend_from_slice(body.as_bytes());
            data.extend_from_slice(b"\x1b[201~");
            self.write_pty(&data);
        } else {
            self.write_pty(body.as_bytes());
        }
        self.term.scroll_to_bottom();
        self.request_draw();
    }

    // ---- ポインタ ----

    /// 窓の中の位置 (論理ピクセル) にあるマス。端末の外なら、近いマスに寄せる。
    fn cell_at(&self, x: f64, y: f64) -> (usize, usize) {
        let layout = self.layout();
        let (ox, oy) = layout.origin();
        let m = self.fonts.metrics;
        let px = (x * f64::from(self.scale)) as f32 - ox;
        let py = (y * f64::from(self.scale)) as f32 - oy;
        let col =
            (px / m.cell_width as f32).floor().clamp(0.0, (self.term.cols() - 1) as f32) as usize;
        let row =
            (py / m.cell_height as f32).floor().clamp(0.0, (self.term.rows() - 1) as f32) as usize;
        (row, col)
    }

    fn point_at(&self, x: f64, y: f64) -> Point {
        let (row, col) = self.cell_at(x, y);
        Point { line: self.term.visible_abs(row), col }
    }

    fn in_header(&self, y: f64) -> bool {
        self.client_decorations && y < f64::from(HEADER_HEIGHT)
    }

    fn button_at(&self, x: f64, y: f64) -> Option<Button> {
        if !self.in_header(y) {
            return None;
        }
        let layout = self.layout();
        let (px, py) = ((x * f64::from(self.scale)) as f32, (y * f64::from(self.scale)) as f32);
        render::header_buttons(&layout)
            .into_iter()
            .find(|(_, [bx, by, bw, bh])| px >= *bx && px < bx + bw && py >= *by && py < by + bh)
            .map(|(b, _)| b)
    }

    /// 窓の端のどこにいるか (サイズを変えるため)。
    fn resize_edge(
        &self,
        x: f64,
        y: f64,
    ) -> Option<
        smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge,
    > {
        use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
        if !self.client_decorations || self.maximized || self.tiled {
            return None;
        }
        let (w, h) = self.window_size();
        let (w, h) = (f64::from(w), f64::from(h));
        let left = x < RESIZE_BORDER;
        let right = x > w - RESIZE_BORDER;
        let top = y < RESIZE_BORDER;
        let bottom = y > h - RESIZE_BORDER;
        Some(match (top, bottom, left, right) {
            (true, _, true, _) => ResizeEdge::TopLeft,
            (true, _, _, true) => ResizeEdge::TopRight,
            (_, true, true, _) => ResizeEdge::BottomLeft,
            (_, true, _, true) => ResizeEdge::BottomRight,
            (true, ..) => ResizeEdge::Top,
            (_, true, ..) => ResizeEdge::Bottom,
            (_, _, true, _) => ResizeEdge::Left,
            (_, _, _, true) => ResizeEdge::Right,
            _ => return None,
        })
    }

    fn set_cursor_shape(&mut self, shape: Shape) {
        if self.cursor_shape_now == Some(shape) {
            return;
        }
        if let Some(device) = &self.shape_device {
            device.set_shape(self.pointer_serial, shape);
            self.cursor_shape_now = Some(shape);
        }
    }

    /// アプリがマウスを受け取るか (Shift を押しているときは、選択のために hull が受け取る)。
    fn mouse_reporting(&self) -> bool {
        self.term.modes.mouse != MouseMode::None && !self.modifiers.shift
    }

    fn report_mouse(&mut self, button: MouseButton, action: MouseAction, x: f64, y: f64) -> bool {
        if !self.mouse_reporting() || self.in_header(y) {
            return false;
        }
        let (row, col) = self.cell_at(x, y);
        if let Some(bytes) =
            input::encode_mouse(button, action, self.mods(), col, row, &self.term.modes)
        {
            self.write_pty(&bytes);
        }
        true
    }

    pub fn pointer_enter(&mut self, serial: u32) {
        self.pointer_serial = serial;
        self.cursor_shape_now = None;
        self.pointer_motion(self.pointer_pos.0, self.pointer_pos.1);
    }

    pub fn pointer_leave(&mut self) {
        if self.hovered.take().is_some() {
            self.request_draw();
        }
    }

    pub fn pointer_motion(&mut self, x: f64, y: f64) {
        self.pointer_pos = (x, y);
        let hovered = self.button_at(x, y);
        if hovered != self.hovered {
            self.hovered = hovered;
            self.request_draw();
        }
        let shape = match self.resize_edge(x, y) {
            Some(edge) => {
                use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge::*;
                match edge {
                    Top => Shape::NResize,
                    Bottom => Shape::SResize,
                    Left => Shape::WResize,
                    Right => Shape::EResize,
                    TopLeft => Shape::NwResize,
                    TopRight => Shape::NeResize,
                    BottomLeft => Shape::SwResize,
                    BottomRight => Shape::SeResize,
                    _ => Shape::Default,
                }
            }
            None if self.in_header(y) => Shape::Default,
            None if self.mouse_reporting() => Shape::Default,
            None if self.modifiers.ctrl && self.link_at(x, y).is_some() => Shape::Pointer,
            None => Shape::Text,
        };
        self.set_cursor_shape(shape);

        if self.selecting {
            let p = self.point_at(x, y);
            if let Some(sel) = &mut self.selection
                && sel.head != p
            {
                sel.head = p;
                self.request_draw();
            }
            return;
        }
        if let Some(button) = self.pressed_button {
            self.report_mouse(button, MouseAction::Motion, x, y);
        } else if self.term.modes.mouse == MouseMode::Any {
            self.report_mouse(MouseButton::None, MouseAction::Motion, x, y);
        }
    }

    fn link_at(&self, x: f64, y: f64) -> Option<String> {
        if self.in_header(y) {
            return None;
        }
        let (row, col) = self.cell_at(x, y);
        let cell = self.term.visible_row(row).cell(col);
        self.term.link(cell.link).map(str::to_string)
    }

    pub fn pointer_press(&mut self, button: u32, serial: u32) {
        self.last_serial = serial;
        let (x, y) = self.pointer_pos;

        if let (Some(edge), Some(seat)) = (self.resize_edge(x, y), &self.seat)
            && button == BTN_LEFT
        {
            self.window.resize(seat, serial, edge);
            return;
        }
        if self.in_header(y) {
            match (button, self.button_at(x, y)) {
                (BTN_LEFT, Some(_)) => {}
                (BTN_LEFT, None) => {
                    let now = Instant::now();
                    let double = self.last_click.is_some_and(|(t, p, _)| {
                        now - t < Duration::from_millis(400) && p.line == u64::MAX
                    });
                    if double {
                        self.toggle_maximize();
                        self.last_click = None;
                    } else {
                        self.last_click = Some((now, Point { line: u64::MAX, col: 0 }, 1));
                        if let Some(seat) = &self.seat {
                            self.window.move_(seat, serial);
                        }
                    }
                }
                (BTN_RIGHT, _) => {
                    if let Some(seat) = &self.seat {
                        self.window.show_window_menu(seat, serial, (x as i32, y as i32));
                    }
                }
                _ => {}
            }
            return;
        }

        let mb = match button {
            BTN_LEFT => MouseButton::Left,
            BTN_MIDDLE => MouseButton::Middle,
            BTN_RIGHT => MouseButton::Right,
            _ => return,
        };
        if self.report_mouse(mb, MouseAction::Press, x, y) {
            self.pressed_button = Some(mb);
            return;
        }
        match mb {
            MouseButton::Left => {
                if self.modifiers.ctrl
                    && let Some(link) = self.link_at(x, y)
                {
                    open_url(&link);
                    return;
                }
                let p = self.point_at(x, y);
                let now = Instant::now();
                let count = match self.last_click {
                    Some((t, lp, n)) if now - t < Duration::from_millis(400) && lp == p => {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                self.last_click = Some((now, p, count));
                let kind = match count {
                    2 => SelectionKind::Word,
                    3 => SelectionKind::Line,
                    _ => SelectionKind::Simple,
                };
                if kind == SelectionKind::Simple && self.modifiers.shift && self.selection.is_some()
                {
                    // Shift+クリックで、今の選択を広げる。
                    if let Some(sel) = &mut self.selection {
                        sel.head = p;
                    }
                } else {
                    self.selection = Some(Selection::new(p, kind));
                }
                self.selecting = true;
                self.request_draw();
            }
            MouseButton::Middle => self.paste(Clipboard::Primary),
            _ => {}
        }
    }

    pub fn pointer_release(&mut self, button: u32, serial: u32) {
        self.last_serial = serial;
        let (x, y) = self.pointer_pos;
        if self.in_header(y) && button == BTN_LEFT && !self.selecting {
            match self.button_at(x, y) {
                Some(Button::Close) => self.exit = true,
                Some(Button::Maximize) => self.toggle_maximize(),
                Some(Button::Minimize) => self.window.set_minimized(),
                None => {}
            }
            return;
        }
        if let Some(mb) = self.pressed_button.take() {
            self.report_mouse(mb, MouseAction::Release, x, y);
            return;
        }
        if button == BTN_LEFT && self.selecting {
            self.selecting = false;
            // 1 マスもドラッグしなかった選択は消す。
            if let Some(sel) = self.selection {
                if sel.kind == SelectionKind::Simple && sel.anchor == sel.head {
                    self.selection = None;
                    self.request_draw();
                    return;
                }
                let text = sel.text(&self.term);
                if !text.is_empty() {
                    self.send_plugin_event(plugin::HostEvent::Selection { text: text.clone() });
                    self.set_clipboard(Clipboard::Primary, text);
                }
            }
        }
    }

    pub fn pointer_axis(&mut self, vertical_px: f64, discrete120: i32) {
        let (x, y) = self.pointer_pos;
        let m = self.fonts.metrics;
        // ホイールは 1 目盛りで 3 行。タッチパッドはピクセルを行に直す。
        let lines = if discrete120 != 0 {
            self.scroll_accum += f64::from(discrete120) / 120.0 * 3.0;
            let l = self.scroll_accum.trunc();
            self.scroll_accum -= l;
            l as i32
        } else {
            self.scroll_accum += vertical_px * f64::from(self.scale) / f64::from(m.cell_height);
            let l = self.scroll_accum.trunc();
            self.scroll_accum -= l;
            l as i32
        };
        if lines == 0 {
            return;
        }
        let up = lines < 0;
        if self.mouse_reporting() && !self.in_header(y) {
            let button = if up { MouseButton::WheelUp } else { MouseButton::WheelDown };
            for _ in 0..lines.unsigned_abs() {
                self.report_mouse(button, MouseAction::Press, x, y);
            }
            return;
        }
        if self.term.is_alt_screen() && self.term.modes.alternate_scroll {
            let key = if up { Key::Up } else { Key::Down };
            if let Some(bytes) =
                input::encode_key(key, Mods::default(), KeyAction::Press, None, &self.term.modes, 0)
            {
                let data = bytes.repeat(lines.unsigned_abs() as usize);
                self.write_pty(&data);
            }
            return;
        }
        self.term.scroll_display(-lines as isize);
        self.request_draw();
    }

    pub fn toggle_maximize(&mut self) {
        if self.maximized {
            self.window.unset_maximized();
        } else {
            self.window.set_maximized();
        }
    }

    // ---- プラグイン ----

    pub fn send_plugin_event(&mut self, event: plugin::HostEvent) {
        self.plugins.broadcast(event);
        let handle = self.handle.clone();
        self.plugins.ensure_write_watch(&handle);
    }

    fn start_plugins(&mut self) {
        let configs = self.config.plugins.clone();
        let handle = self.handle.clone();
        self.plugins.start_all(&configs, &handle, (self.term.cols(), self.term.rows()));
    }

    fn restart_plugins_if_changed(&mut self) {
        let configs = self.config.plugins.clone();
        let handle = self.handle.clone();
        self.plugins.reconcile(&configs, &handle, (self.term.cols(), self.term.rows()));
    }

    /// プロンプトの印を覚える (OSC 133)。
    pub fn note_prompt(&mut self, mark: PromptMark) {
        if mark == PromptMark::PromptStart {
            let (row, _) = self.term.cursor();
            let line = self.term.grid().abs_of_screen(row);
            self.plugins.note_prompt(line);
        }
    }
}

/// 端末の色の表に、配色を入れる。
pub fn apply_scheme(term: &mut Terminal, scheme: &Scheme) {
    for (i, rgb) in scheme.palette.iter().enumerate() {
        term.palette.set_base(i, *rgb);
    }
    term.palette.set_base(hull_term::term::COLOR_FG, scheme.foreground);
    term.palette.set_base(hull_term::term::COLOR_BG, scheme.background);
    term.palette.set_base(hull_term::term::COLOR_CURSOR, scheme.cursor);
}

/// URL を既定のアプリで開く。
pub fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|mut child| std::thread::spawn(move || child.wait()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn キーの名前を行き来できる() {
        for key in
            [Key::Char('a'), Key::Char('+'), Key::Enter, Key::PageUp, Key::F(12), Key::Escape]
        {
            assert_eq!(key_from_name(&key_name(key)), Some(key));
        }
        assert_eq!(key_from_name("f13"), None);
        assert_eq!(key_from_name("nope"), None);
    }
}
