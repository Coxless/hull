//! Wayland のイベントを `App` の操作につなぐ。

use std::io::Write;
use std::os::fd::OwnedFd;

use smithay_client_toolkit::{
    activation::{ActivationHandler, RequestData},
    compositor::CompositorHandler,
    data_device_manager::{
        WritePipe,
        data_device::DataDeviceHandler,
        data_offer::{DataOfferHandler, DragOffer},
        data_source::DataSourceHandler,
    },
    delegate_dispatch2, delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputState},
    presentation_time::{PresentTime, PresentationTimeHandler, PresentationTimeState},
    primary_selection::{
        device::PrimarySelectionDeviceHandler, selection::PrimarySelectionSourceHandler,
    },
    reexports::protocols::wp::{
        fractional_scale::v1::client::{
            wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
            wp_fractional_scale_v1::{self, WpFractionalScaleV1},
        },
        presentation_time::client::wp_presentation_feedback::{self, WpPresentationFeedback},
        text_input::zv3::client::{
            zwp_text_input_manager_v3::ZwpTextInputManagerV3,
            zwp_text_input_v3::{self, ZwpTextInputV3},
        },
        viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        xdg::window::{DecorationMode, Window, WindowConfigure, WindowHandler},
    },
    shm::{Shm, ShmHandler},
};
use wayland_client::{
    Connection, Proxy, QueueHandle, WEnum,
    protocol::{
        wl_data_device::WlDataDevice, wl_data_source::WlDataSource, wl_keyboard::WlKeyboard,
        wl_output, wl_pointer::WlPointer, wl_seat::WlSeat, wl_surface::WlSurface,
    },
};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
    zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
};

use hull_term::input::KeyAction;

use crate::app::{App, Probe};

/// クリップボードで扱う文字の種類。
pub const TEXT_MIME_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain;charset=UTF-8",
    "UTF8_STRING",
    "STRING",
    "text/plain",
    "TEXT",
];

pub fn pick_mime(mimes: &[String]) -> Option<String> {
    TEXT_MIME_TYPES.iter().find(|m| mimes.iter().any(|x| x == *m)).map(|m| m.to_string())
}

/// sctk が扱わないプロトコル (分数スケール、viewporter、text-input-v3) の受け口。
pub struct Protocol;

impl Dispatch2<WpFractionalScaleManagerV1, App> for Protocol {
    fn event(
        &self,
        _: &mut App,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch2<WpFractionalScaleV1, App> for Protocol {
    fn event(
        &self,
        app: &mut App,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            app.set_scale(scale as f32 / 120.0);
        }
    }
}

impl Dispatch2<WpViewporter, App> for Protocol {
    fn event(
        &self,
        _: &mut App,
        _: &WpViewporter,
        _: <WpViewporter as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch2<WpViewport, App> for Protocol {
    fn event(
        &self,
        _: &mut App,
        _: &WpViewport,
        _: <WpViewport as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch2<ZwpTextInputManagerV3, App> for Protocol {
    fn event(
        &self,
        _: &mut App,
        _: &ZwpTextInputManagerV3,
        _: <ZwpTextInputManagerV3 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch2<ZwpTextInputV3, App> for Protocol {
    fn event(
        &self,
        app: &mut App,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        use zwp_text_input_v3::Event::*;
        match event {
            Enter { .. } => app.enable_ime(),
            Leave { .. } => {
                app.disable_ime();
                app.ime.preedit = None;
                app.request_draw();
            }
            PreeditString { text, .. } => app.ime.pending_preedit = text,
            CommitString { text } => app.ime.pending_commit = text,
            Done { .. } => {
                // done で、たまった変更をまとめて反映する。
                if let Some(text) = app.ime.pending_commit.take() {
                    app.on_ime_commit(&text);
                }
                app.ime.preedit = app.ime.pending_preedit.take().filter(|t| !t.is_empty());
                app.request_draw();
            }
            _ => {}
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        factor: i32,
    ) {
        // 分数スケールが使えるときは、そちらに従う。
        if self.fractional.is_none() {
            self.set_scale(factor.max(1) as f32);
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {
        self.on_frame();
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        self.client_decorations =
            configure.decoration_mode == DecorationMode::Client && self.config.window.decorations;
        self.maximized = configure.is_maximized();
        self.tiled = configure.is_tiled() || configure.is_fullscreen();
        let header = if self.client_decorations { self.window_size().1 - self.size.1 } else { 0 };
        if let (Some(w), Some(h)) = configure.new_size {
            self.size = (w.get(), h.get().saturating_sub(header).max(1));
        }
        crate::trace("configure");
        self.configured = true;
        self.apply_size();
        self.request_draw();
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if self.seat.is_none() {
            self.seat = Some(seat.clone());
            if let Some(m) = &self.data_device_manager {
                self.data_device = Some(m.get_data_device(qh, &seat));
            }
            if let Some(m) = &self.primary_manager {
                self.primary_device = Some(m.get_selection_device(qh, &seat));
            }
            if let Some(m) = &self.text_input_manager {
                self.text_input = Some(m.get_text_input(&seat, qh, Protocol));
            }
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.handle.clone(),
                Box::new(|app: &mut App, _, event| app.on_key(event, KeyAction::Repeat)),
            );
            self.keyboard = keyboard.ok();
        }
        if capability == Capability::Pointer
            && self.pointer.is_none()
            && let Ok(pointer) = self.seat_state.get_pointer(qh, &seat)
        {
            self.shape_device =
                self.cursor_shape.as_ref().map(|m| m.get_shape_device(&pointer, qh));
            self.pointer = Some(pointer);
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(k) = self.keyboard.take()
        {
            k.release();
        }
        if capability == Capability::Pointer {
            if let Some(d) = self.shape_device.take() {
                d.destroy();
            }
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        surface: &WlSurface,
        serial: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        if surface == self.window.wl_surface() {
            self.last_serial = serial;
            self.on_focus(true);
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        surface: &WlSurface,
        _: u32,
    ) {
        if surface == self.window.wl_surface() {
            self.on_focus(false);
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.last_serial = serial;
        self.on_key(event, KeyAction::Press);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.on_key(event, KeyAction::Repeat);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.on_key(event, KeyAction::Release);
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.window.wl_surface() {
                continue;
            }
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.pointer_pos = event.position;
                    self.pointer_enter(serial);
                }
                PointerEventKind::Leave { .. } => self.pointer_leave(),
                PointerEventKind::Motion { .. } => {
                    self.pointer_motion(event.position.0, event.position.1)
                }
                PointerEventKind::Press { button, serial, .. } => {
                    self.pointer_press(button, serial)
                }
                PointerEventKind::Release { button, serial, .. } => {
                    self.pointer_release(button, serial)
                }
                PointerEventKind::Axis { vertical, .. } => {
                    self.pointer_axis(vertical.absolute, vertical.value120);
                }
            }
        }
    }
}

impl DataDeviceHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &WlSurface,
    ) {
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for App {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction,
    ) {
    }
}

/// 相手に文字を渡す。大きいときに hull が止まらないよう、別のスレッドで書く。
fn send_text(pipe: WritePipe, text: String) {
    let fd = OwnedFd::from(pipe);
    std::thread::spawn(move || {
        let mut f = std::fs::File::from(fd);
        let _ = f.write_all(text.as_bytes());
    });
}

impl DataSourceHandler for App {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        _: String,
        pipe: WritePipe,
    ) {
        if let Some((s, text)) = &self.copy_source
            && s.inner() == source
        {
            send_text(pipe, text.clone());
        }
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self.copy_source.as_ref().is_some_and(|(s, _)| s.inner() == source) {
            self.copy_source = None;
        }
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction,
    ) {
    }
}

impl PrimarySelectionDeviceHandler for App {
    fn selection(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpPrimarySelectionDeviceV1,
    ) {
    }
}

impl PrimarySelectionSourceHandler for App {
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
        _: String,
        pipe: WritePipe,
    ) {
        if let Some((s, text)) = &self.primary_source
            && s.inner() == source
        {
            send_text(pipe, text.clone());
        }
    }

    fn cancelled(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
    ) {
        if self.primary_source.as_ref().is_some_and(|(s, _)| s.inner() == source) {
            self.primary_source = None;
        }
    }
}

impl ActivationHandler for App {
    type RequestUdata = ();

    fn new_token(&mut self, token: String, _: &RequestData<()>) {
        if let Some(a) = &self.activation {
            a.activate::<App>(self.window.wl_surface(), token);
        }
    }
}

impl PresentationTimeHandler for App {
    fn presentation_time_state(&mut self) -> &mut PresentationTimeState {
        self.presentation.as_mut().expect("計測のときだけ使う")
    }

    fn presented(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WpPresentationFeedback,
        _: &WlSurface,
        _: Vec<wl_output::WlOutput>,
        time: PresentTime,
        _: u32,
        _: u64,
        _: WEnum<wp_presentation_feedback::Kind>,
    ) {
        // 計測の道具が、起動した時刻と比べる (時計は wp_presentation が決める clk_id)。
        let line = |name: &str| {
            println!("hull-{name} clk_id={} sec={} nsec={}", time.clk_id, time.tv_sec, time.tv_nsec)
        };
        match self.probes.pop_front() {
            Some(Probe::First) => {
                line("first-frame");
                self.bench_reported_first = true;
            }
            Some(Probe::Gpu) => {
                // GPU のフレームが最初なら、それが最初のフレームでもある。
                if !self.bench_reported_first {
                    line("first-frame");
                }
                line("first-gpu-frame");
                self.exit = true;
            }
            Some(Probe::Latency(key_ms)) => {
                // キーの時刻はコンポジタの時計 (CLOCK_MONOTONIC) のミリ秒を 32 ビットに丸めたもの。
                let shown_us = time.tv_sec * 1_000_000 + u64::from(time.tv_nsec) / 1000;
                let key_us = u64::from(key_ms) * 1000;
                let wrap = (1u64 << 32) * 1000;
                let delta_us = (shown_us % wrap + wrap - key_us) % wrap;
                eprintln!("hull-latency {:.2}ms", delta_us as f64 / 1000.0);
            }
            None => {}
        }
        let _ = std::io::stdout().flush();
    }

    fn discarded(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WpPresentationFeedback,
        _: &WlSurface,
    ) {
        // 画面に出なかったフレームも、答えの順番をそろえるために取り出す。
        self.probes.pop_front();
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        self.shm.as_mut().expect("shm を使うときだけ呼ばれる")
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
delegate_dispatch2!(App);
