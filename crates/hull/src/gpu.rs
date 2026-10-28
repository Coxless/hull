//! EGL で Wayland の窓に GLES3 の文脈を作る。

use std::ffi::c_void;

use khronos_egl as egl;
use wayland_client::{Proxy, protocol::wl_surface::WlSurface};
use wayland_egl::WlEglSurface;

const PLATFORM_WAYLAND_KHR: egl::Enum = 0x31D8;
const BUFFER_AGE_EXT: egl::Int = 0x313D;

type SwapWithDamage = unsafe extern "C" fn(
    egl::EGLDisplay,
    egl::EGLSurface,
    *const egl::Int,
    egl::Int,
) -> egl::Boolean;

/// 別のスレッドで済ませる EGL の初期化 (ドライバによっては 100ms ほどかかる)。
/// 文脈はまだどのスレッドでも current にしていない。
pub struct EglSetup {
    egl: egl::Instance<egl::Static>,
    display: egl::Display,
    config: egl::Config,
    context: egl::Context,
    swap_with_damage: Option<SwapWithDamage>,
    buffer_age: bool,
}

// SAFETY: EGL の表示と文脈はスレッドをまたいで使える。文脈は current にしてから渡さない。
unsafe impl Send for EglSetup {}

impl EglSetup {
    /// `wl_display` は Wayland の接続の `wl_display` へのポインタ (接続は呼び出し側が生かしておく)。
    pub fn new(wl_display: usize) -> Result<EglSetup, String> {
        let egl = egl::Instance::new(egl::Static);
        // SAFETY: Wayland の接続が生きているあいだ、その表示から EGL の表示を作る。
        let display = unsafe {
            egl.get_platform_display(
                PLATFORM_WAYLAND_KHR,
                wl_display as *mut c_void,
                &[egl::ATTRIB_NONE],
            )
        }
        .map_err(|e| format!("EGL の表示を作れません: {e}"))?;
        egl.initialize(display).map_err(|e| format!("EGL を初期化できません: {e}"))?;
        egl.bind_api(egl::OPENGL_ES_API).map_err(|e| e.to_string())?;

        let attribs = [
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES3_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            // 見出しの角を丸めるために透明を使う。
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];
        let config = egl
            .choose_first_config(display, &attribs)
            .map_err(|e| e.to_string())?
            .ok_or("GLES3 を使える EGL の設定がありません")?;
        let context = egl
            .create_context(display, config, None, &[egl::CONTEXT_MAJOR_VERSION, 3, egl::NONE])
            .map_err(|e| format!("GLES3 の文脈を作れません: {e}"))?;

        let extensions = egl
            .query_string(Some(display), egl::EXTENSIONS)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let swap_with_damage = [
            ("EGL_KHR_swap_buffers_with_damage", "eglSwapBuffersWithDamageKHR"),
            ("EGL_EXT_swap_buffers_with_damage", "eglSwapBuffersWithDamageEXT"),
        ]
        .iter()
        .filter(|(ext, _)| extensions.split(' ').any(|e| e == *ext))
        .find_map(|(_, name)| egl.get_proc_address(name))
        // SAFETY: 拡張の関数の型は仕様で決まっている。
        .map(|f| unsafe { std::mem::transmute::<extern "system" fn(), SwapWithDamage>(f) });
        let buffer_age = extensions.contains("EGL_EXT_buffer_age");
        Ok(EglSetup { egl, display, config, context, swap_with_damage, buffer_age })
    }
}

pub struct Gpu {
    egl: egl::Instance<egl::Static>,
    display: egl::Display,
    config: egl::Config,
    context: egl::Context,
    surface: Option<(egl::Surface, WlEglSurface)>,
    swap_with_damage: Option<SwapWithDamage>,
    buffer_age: bool,
    pub gl: glow::Context,
    size: (i32, i32),
}

impl Gpu {
    /// 初期化を済ませた EGL を、このスレッドで使えるようにする。
    pub fn new(setup: EglSetup) -> Result<Gpu, String> {
        let EglSetup { egl, display, config, context, swap_with_damage, buffer_age } = setup;
        egl.make_current(display, None, None, Some(context)).map_err(|e| e.to_string())?;
        // SAFETY: 文脈が current のあいだに、GL の関数を EGL から引く。
        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                egl.get_proc_address(name).map_or(std::ptr::null(), |f| f as *const c_void)
            })
        };
        Ok(Gpu {
            egl,
            display,
            config,
            context,
            surface: None,
            swap_with_damage,
            buffer_age,
            gl,
            size: (0, 0),
        })
    }

    /// 窓の大きさ (バッファのピクセル) を決める。初めてなら EGL の面を作る。
    pub fn resize(
        &mut self,
        wl_surface: &WlSurface,
        width: i32,
        height: i32,
    ) -> Result<(), String> {
        let (width, height) = (width.max(1), height.max(1));
        if let Some((_, wl_egl)) = &self.surface {
            if self.size != (width, height) {
                wl_egl.resize(width, height, 0, 0);
                self.size = (width, height);
            }
            return Ok(());
        }
        let wl_egl =
            WlEglSurface::new(wl_surface.id(), width, height).map_err(|e| e.to_string())?;
        // SAFETY: wl_egl_surface は self.surface に入れて、EGL の面より長く生かす。
        let surface = unsafe {
            self.egl.create_window_surface(
                self.display,
                self.config,
                wl_egl.ptr() as egl::NativeWindowType,
                None,
            )
        }
        .map_err(|e| format!("EGL の面を作れません: {e}"))?;
        self.egl
            .make_current(self.display, Some(surface), Some(surface), Some(self.context))
            .map_err(|e| e.to_string())?;
        // 描く時機はフレームのコールバックで自分で決めるので、swap で待たない。
        let _ = self.egl.swap_interval(self.display, 0);
        self.surface = Some((surface, wl_egl));
        self.size = (width, height);
        Ok(())
    }

    pub fn has_surface(&self) -> bool {
        self.surface.is_some()
    }

    pub fn make_current(&self) {
        if let Some((surface, _)) = &self.surface {
            let _ = self.egl.make_current(
                self.display,
                Some(*surface),
                Some(*surface),
                Some(self.context),
            );
        }
    }

    /// いまのバッファの中身が何フレーム前のものか。0 はわからない (全部描き直す)。
    pub fn buffer_age(&self) -> u32 {
        match &self.surface {
            Some((surface, _)) if self.buffer_age => {
                self.egl.query_surface(self.display, *surface, BUFFER_AGE_EXT).unwrap_or(0).max(0)
                    as u32
            }
            _ => 0,
        }
    }

    /// 画面に出す。`damage` は変わった範囲 (左上が原点の x, y, 幅, 高さ)。空なら全体。
    pub fn swap(&self, damage: &[[i32; 4]]) {
        let Some((surface, _)) = &self.surface else { return };
        if let (Some(f), false) = (self.swap_with_damage, damage.is_empty()) {
            // EGL の矩形は左下が原点。
            let rects: Vec<egl::Int> =
                damage.iter().flat_map(|&[x, y, w, h]| [x, self.size.1 - y - h, w, h]).collect();
            // SAFETY: 表示と面は生きており、矩形の数は配列の長さと合っている。
            let ok = unsafe {
                f(self.display.as_ptr(), surface.as_ptr(), rects.as_ptr(), damage.len() as egl::Int)
            };
            if ok == egl::TRUE {
                return;
            }
        }
        let _ = self.egl.swap_buffers(self.display, *surface);
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.display, None, None, None);
        if let Some((surface, _)) = self.surface.take() {
            let _ = self.egl.destroy_surface(self.display, surface);
        }
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}
