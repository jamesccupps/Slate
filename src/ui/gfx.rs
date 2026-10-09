//! Direct2D / DirectWrite helpers: factories, the render target, cached brushes, text formats, and a few drawing
//! shortcuts. Everything is in DIPs (1/96 inch); the render target's DPI does the scaling.
//!
//! The window is drawn through a Direct3D 11 device and a flip-model swap chain (what modern apps such as Windows
//! Terminal use). Direct2D's simpler HWND render target is only a fallback: on this developer's PC (a virtual
//! display adapter next to the GPU) it reported the window as occluded and never showed anything.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Foundation::{BOOL, HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
    D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Factory1,
    ID2D1HwndRenderTarget, ID2D1RenderTarget, ID2D1SolidColorBrush,
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, ID2D1Bitmap1, ID2D1DeviceContext,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_MWA_NO_ALT_ENTER, DXGI_PRESENT, DXGI_SCALING_NONE,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_METRICS, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteFont1, IDWriteFontCollection, IDWriteTextFormat,
    IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::{HSTRING, Interface, PCWSTR, w};

/// A rectangle in DIPs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn inset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
    pub fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
}

pub fn color(argb: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((argb >> 16) & 0xFF) as f32 / 255.0,
        g: ((argb >> 8) & 0xFF) as f32 / 255.0,
        b: (argb & 0xFF) as f32 / 255.0,
        a: ((argb >> 24) & 0xFF) as f32 / 255.0,
    }
}

/// Opaque color from 0xRRGGBB.
pub const fn rgb(c: u32) -> u32 {
    0xFF00_0000 | c
}

/// Color with alpha (0..=255) from 0xRRGGBB.
pub const fn rgba(c: u32, a: u32) -> u32 {
    (a << 24) | (c & 0xFF_FFFF)
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// What the window is drawn through.
enum WinTarget {
    /// Direct3D 11 + flip-model swap chain + Direct2D device context (normal).
    Flip { dc: ID2D1DeviceContext, chain: IDXGISwapChain1, size: (u32, u32) },
    /// Direct2D's HWND render target (fallback).
    Hwnd(ID2D1HwndRenderTarget),
}

fn target_bitmap(dc: &ID2D1DeviceContext, chain: &IDXGISwapChain1, dpi: f32) -> windows::core::Result<ID2D1Bitmap1> {
    unsafe {
        let surface: IDXGISurface = chain.GetBuffer(0)?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
            dpiX: dpi,
            dpiY: dpi,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))
    }
}

pub struct Gfx {
    pub d2d: ID2D1Factory1,
    pub dw: IDWriteFactory,
    win: Option<WinTarget>,
    /// The current render target (the window's, or an offscreen one for tests).
    pub rt: Option<ID2D1RenderTarget>,
    brushes: RefCell<HashMap<u32, ID2D1SolidColorBrush>>,
    pub dpi: f32,
    /// Bumped whenever the render target is recreated (cached text layouts hold its brushes).
    pub generation: u64,
    /// Drawing into an offscreen bitmap (test mode): don't create a window target.
    pub offscreen: bool,
    /// The user's locale ("ja-JP"...): DirectWrite picks fallback fonts by it, so kanji look Japanese to a Japanese
    /// user rather than Chinese.
    locale: HSTRING,
}

fn user_locale() -> HSTRING {
    let mut buf = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
    let n = unsafe { windows::Win32::Globalization::GetUserDefaultLocaleName(&mut buf) };
    if n > 1 { HSTRING::from_wide(&buf[..n as usize - 1]).unwrap_or_else(|_| HSTRING::from("en-us")) } else { HSTRING::from("en-us") }
}

fn device(kind: D3D_DRIVER_TYPE) -> windows::core::Result<ID3D11Device> {
    let mut dev = None;
    unsafe {
        D3D11CreateDevice(
            None,
            kind,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut dev),
            None,
            None,
        )?;
    }
    dev.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
}

/// (A Direct3D 11 device can be used from any thread.)
struct Device(ID3D11Device);
unsafe impl Send for Device {}

/// The hardware device `make_device_early` is making.
static EARLY: std::sync::Mutex<Option<std::thread::JoinHandle<Option<Device>>>> = std::sync::Mutex::new(None);

/// Starts making the hardware Direct3D device on another thread while Slate starts: the graphics driver can take a
/// quarter of a second to load, which the first frame would otherwise wait for on its own. Dropping what this returns
/// waits for that thread (Slate never ends while the driver loads on it).
pub fn make_device_early() -> EarlyDevice {
    let made = std::thread::Builder::new().name("slate-d3d".into()).spawn(|| {
        let made = device(D3D_DRIVER_TYPE_HARDWARE).ok().map(Device);
        super::mark("device");
        made
    });
    *EARLY.lock().unwrap() = made.ok();
    EarlyDevice
}

pub struct EarlyDevice;

impl Drop for EarlyDevice {
    fn drop(&mut self) {
        drop(early_device());
    }
}

/// The device `make_device_early` made, once (waiting for it if it isn't done yet).
fn early_device() -> Option<ID3D11Device> {
    let made = EARLY.lock().unwrap().take()?;
    made.join().ok().flatten().map(|d| d.0)
}

impl Gfx {
    pub fn new() -> windows::core::Result<Gfx> {
        unsafe {
            let d2d: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Gfx {
                d2d,
                dw,
                win: None,
                rt: None,
                brushes: RefCell::new(HashMap::new()),
                dpi: 96.0,
                generation: 1,
                offscreen: false,
                locale: user_locale(),
            })
        }
    }

    /// The normal window target: a flip-model swap chain on a Direct3D 11 device (hardware, else WARP).
    fn create_flip(&self, hwnd: HWND, w: u32, h: u32, dpi: f32) -> windows::core::Result<WinTarget> {
        unsafe {
            let dev = match early_device() {
                Some(dev) => dev,
                None => device(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| device(D3D_DRIVER_TYPE_WARP))?,
            };
            let dxgi_dev: IDXGIDevice = dev.cast()?;
            let d2d_dev = self.d2d.CreateDevice(&dxgi_dev)?;
            let dc = d2d_dev.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let factory: IDXGIFactory2 = dxgi_dev.GetAdapter()?.GetParent()?;
            let mut desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w,
                Height: h,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let chain = match factory.CreateSwapChainForHwnd(&dev, hwnd, &desc, None, None) {
                Ok(c) => c,
                Err(_) => {
                    desc.SwapEffect = DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL;
                    factory.CreateSwapChainForHwnd(&dev, hwnd, &desc, None, None)?
                }
            };
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
            let bmp = target_bitmap(&dc, &chain, dpi)?;
            dc.SetTarget(&bmp);
            dc.SetDpi(dpi, dpi);
            Ok(WinTarget::Flip { dc, chain, size: (w, h) })
        }
    }

    /// Makes sure there is a render target for `hwnd` of the given pixel size.
    pub fn ensure_target(&mut self, hwnd: HWND, w: u32, h: u32, dpi: f32) -> windows::core::Result<()> {
        if self.offscreen && self.rt.is_some() {
            return Ok(());
        }
        let (w, h) = (w.max(1), h.max(1));
        if self.win.is_none() {
            let target = match self.create_flip(hwnd, w, h, dpi) {
                Ok(t) => t,
                Err(_) => self.create_hwnd_target(hwnd, w, h, dpi)?,
            };
            self.rt = Some(match &target {
                WinTarget::Flip { dc, .. } => dc.cast()?,
                WinTarget::Hwnd(rt) => rt.cast()?,
            });
            self.win = Some(target);
            self.brushes.borrow_mut().clear();
            self.generation += 1;
            self.dpi = dpi;
        }
        match self.win.as_mut().unwrap() {
            WinTarget::Flip { dc, chain, size } => unsafe {
                if *size != (w, h) {
                    // The old back buffer must be released before the swap chain can resize.
                    dc.SetTarget(None);
                    chain.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
                    let bmp = target_bitmap(dc, chain, dpi)?;
                    dc.SetTarget(&bmp);
                    *size = (w, h);
                    self.dpi = 0.0;
                }
                if (self.dpi - dpi).abs() > 0.01 {
                    dc.SetDpi(dpi, dpi);
                }
            },
            WinTarget::Hwnd(rt) => unsafe {
                let size = rt.GetPixelSize();
                if size.width != w || size.height != h {
                    rt.Resize(&D2D_SIZE_U { width: w, height: h })?;
                }
                if (self.dpi - dpi).abs() > 0.01 {
                    rt.SetDpi(dpi, dpi);
                }
            },
        }
        self.dpi = dpi;
        Ok(())
    }

    /// Ends the frame and shows it. Returns false if the device was lost (discard the target and paint again).
    pub fn present(&mut self) -> bool {
        let Some(rt) = self.rt.as_ref() else { return false };
        if unsafe { rt.EndDraw(None, None) }.is_err() {
            return false;
        }
        if let Some(WinTarget::Flip { chain, .. }) = &self.win {
            let hr = unsafe { chain.Present(1, DXGI_PRESENT(0)) };
            if hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET {
                return false;
            }
        }
        true
    }

    fn create_hwnd_target(&self, hwnd: HWND, w: u32, h: u32, dpi: f32) -> windows::core::Result<WinTarget> {
        {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: dpi,
                dpiY: dpi,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U { width: w.max(1), height: h.max(1) },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = unsafe { self.d2d.CreateHwndRenderTarget(&props, &hprops)? };
            unsafe { rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE) };
            Ok(WinTarget::Hwnd(rt))
        }
    }

    /// Uses an offscreen target (tests and screenshots).
    pub fn use_target(&mut self, rt: ID2D1RenderTarget, dpi: f32) {
        unsafe {
            rt.SetDpi(dpi, dpi);
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }
        self.rt = Some(rt);
        self.win = None;
        self.dpi = dpi;
        self.brushes.borrow_mut().clear();
        self.generation += 1;
    }

    /// Drops the render target (after D2DERR_RECREATE_TARGET); the next paint makes a new one.
    pub fn discard_target(&mut self) {
        self.rt = None;
        self.win = None;
        self.brushes.borrow_mut().clear();
        self.generation += 1;
    }

    pub fn has_target(&self) -> bool {
        self.rt.is_some()
    }

    pub fn rt(&self) -> &ID2D1RenderTarget {
        self.rt.as_ref().expect("no render target")
    }

    pub fn brush(&self, argb: u32) -> ID2D1SolidColorBrush {
        if let Some(b) = self.brushes.borrow().get(&argb) {
            return b.clone();
        }
        let b = unsafe { self.rt().CreateSolidColorBrush(&color(argb), None) }.expect("brush");
        self.brushes.borrow_mut().insert(argb, b.clone());
        b
    }

    pub fn fill(&self, r: Rect, argb: u32) {
        if r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        unsafe { self.rt().FillRectangle(&r.d2d(), &self.brush(argb)) };
    }

    pub fn fill_round(&self, r: Rect, radius: f32, argb: u32) {
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt().FillRoundedRectangle(&rr, &self.brush(argb)) };
    }

    pub fn stroke_round(&self, r: Rect, radius: f32, argb: u32, width: f32) {
        let h = width / 2.0;
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: r.x + h, top: r.y + h, right: r.right() - h, bottom: r.bottom() - h },
            radiusX: radius,
            radiusY: radius,
        };
        unsafe { self.rt().DrawRoundedRectangle(&rr, &self.brush(argb), width, None) };
    }

    /// `v` (DIPs) moved to the nearest edge between device pixels.
    pub fn snap(&self, v: f32) -> f32 {
        snap(v, self.dpi)
    }

    /// `r` with its edges on device pixels.
    pub fn snap_rect(&self, r: Rect) -> Rect {
        let (x, y) = (self.snap(r.x), self.snap(r.y));
        Rect::new(x, y, self.snap(r.right()) - x, self.snap(r.bottom()) - y)
    }

    /// How thick a hairline is (DIPs): one device pixel, or as many whole ones as a DIP covers (two at 200%).
    pub fn hair(&self) -> f32 {
        let k = self.dpi.max(96.0) / 96.0;
        k.floor() / k
    }

    /// A crisp hairline across from `x0` to `x1`, on the pixels just below `y` (`above`: just above it). A line
    /// a DIP thick at a fraction of a pixel would be smeared over two rows at 125% and 150%.
    pub fn hline(&self, x0: f32, x1: f32, y: f32, above: bool, argb: u32) {
        let (x0, x1, y, t) = (self.snap(x0), self.snap(x1), self.snap(y), self.hair());
        self.fill(Rect::new(x0, if above { y - t } else { y }, x1 - x0, t), argb);
    }

    /// The same down from `y0` to `y1`, on the pixels just right of `x` (`left`: just left of it).
    pub fn vline(&self, x: f32, y0: f32, y1: f32, left: bool, argb: u32) {
        let (x, y0, y1, t) = (self.snap(x), self.snap(y0), self.snap(y1), self.hair());
        self.fill(Rect::new(if left { x - t } else { x }, y0, t, y1 - y0), argb);
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, argb: u32, width: f32) {
        unsafe {
            self.rt().DrawLine(
                D2D_POINT_2F { x: x0, y: y0 },
                D2D_POINT_2F { x: x1, y: y1 },
                &self.brush(argb),
                width,
                None,
            )
        };
    }

    pub fn push_clip(&self, r: Rect) {
        unsafe { self.rt().PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn pop_clip(&self) {
        unsafe { self.rt().PopAxisAlignedClip() };
    }

    pub fn format(&self, family: &str, size: f32, weight: DWRITE_FONT_WEIGHT) -> IDWriteTextFormat {
        unsafe {
            self.dw
                .CreateTextFormat(
                    &HSTRING::from(family),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    &self.locale,
                )
                .or_else(|_| {
                    self.dw.CreateTextFormat(
                        w!("Segoe UI"),
                        None,
                        weight,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        size,
                        &self.locale,
                    )
                })
                .expect("text format")
        }
    }

    /// A one-line UI format: no wrapping, vertically centered, ellipsis when too long.
    pub fn ui_format(&self, family: &str, size: f32, weight: DWRITE_FONT_WEIGHT) -> IDWriteTextFormat {
        let f = self.format(family, size, weight);
        unsafe {
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            if let Ok(sign) = self.dw.CreateEllipsisTrimmingSign(&f) {
                let t = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                let _ = f.SetTrimming(&t, &sign);
            }
        }
        f
    }

    pub fn layout(&self, text: &[u16], fmt: &IDWriteTextFormat, max_w: f32, max_h: f32) -> IDWriteTextLayout {
        unsafe { self.dw.CreateTextLayout(text, fmt, max_w.max(0.0), max_h.max(0.0)) }.expect("text layout")
    }

    /// Width and height of `s` in `fmt`.
    pub fn measure(&self, s: &str, fmt: &IDWriteTextFormat) -> (f32, f32) {
        let l = self.layout(&wide(s), fmt, 100_000.0, 10_000.0);
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = l.GetMetrics(&mut m);
        }
        (m.widthIncludingTrailingWhitespace, m.height)
    }

    /// Draws one line of text inside `r` (vertically centered when the format says so).
    pub fn text(&self, s: &str, fmt: &IDWriteTextFormat, r: Rect, argb: u32, align: Align) {
        if r.w <= 0.0 {
            return;
        }
        let l = self.layout(&wide(s), fmt, r.w, r.h);
        let a: DWRITE_TEXT_ALIGNMENT = match align {
            Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
        };
        unsafe {
            let _ = l.SetTextAlignment(a);
        }
        self.draw_layout(&l, r.x, r.y, argb);
    }

    pub fn draw_layout(&self, l: &IDWriteTextLayout, x: f32, y: f32, argb: u32) {
        let opts: D2D1_DRAW_TEXT_OPTIONS = D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT;
        unsafe { self.rt().DrawTextLayout(D2D_POINT_2F { x, y }, l, &self.brush(argb), opts) };
    }

    pub fn draw_layout_clipped(&self, l: &IDWriteTextLayout, x: f32, y: f32, argb: u32) {
        let opts = D2D1_DRAW_TEXT_OPTIONS(
            D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT.0 | D2D1_DRAW_TEXT_OPTIONS_CLIP.0,
        );
        unsafe { self.rt().DrawTextLayout(D2D_POINT_2F { x, y }, l, &self.brush(argb), opts) };
    }
}

/// `v` (DIPs) moved to the nearest edge between device pixels at `dpi`.
pub fn snap(v: f32, dpi: f32) -> f32 {
    let k = dpi.max(96.0) / 96.0;
    (v * k).round() / k
}

/// Facts about a font needed for layout.
pub struct FontInfo {
    /// Line height and baseline (from the top of the line) in DIPs at the given size.
    pub line_height: f32,
    pub baseline: f32,
    pub monospace: bool,
}

pub fn font_info(dw: &IDWriteFactory, family: &str, size: f32) -> Option<FontInfo> {
    unsafe {
        let mut coll: Option<IDWriteFontCollection> = None;
        dw.GetSystemFontCollection(&mut coll, false).ok()?;
        let coll = coll?;
        let mut index = 0u32;
        let mut exists = BOOL(0);
        coll.FindFamilyName(&HSTRING::from(family), &mut index, &mut exists).ok()?;
        if !exists.as_bool() {
            return None;
        }
        let fam = coll.GetFontFamily(index).ok()?;
        let font =
            fam.GetFirstMatchingFont(DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL).ok()?;
        let mut m = DWRITE_FONT_METRICS::default();
        font.GetMetrics(&mut m);
        let em = m.designUnitsPerEm as f32;
        let ascent = m.ascent as f32 * size / em;
        let descent = m.descent as f32 * size / em;
        let gap = m.lineGap as f32 * size / em;
        let monospace = font.cast::<IDWriteFont1>().map(|f| f.IsMonospacedFont().as_bool()).unwrap_or(false);
        // A little extra air between lines reads better than the font's tight default.
        let line_height = ((ascent + descent + gap) * 1.12).round().max(size.ceil());
        let baseline = ((line_height - (ascent + descent)) / 2.0 + ascent).round();
        Some(FontInfo { line_height, baseline, monospace })
    }
}

/// Installed font families, alphabetically, with whether each is monospaced.
pub fn font_families(dw: &IDWriteFactory) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    unsafe {
        let mut coll: Option<IDWriteFontCollection> = None;
        if dw.GetSystemFontCollection(&mut coll, false).is_err() {
            return out;
        }
        let Some(coll) = coll else { return out };
        for i in 0..coll.GetFontFamilyCount() {
            let Ok(fam) = coll.GetFontFamily(i) else { continue };
            let Ok(names) = fam.GetFamilyNames() else { continue };
            let mut idx = 0u32;
            let mut exists = BOOL(0);
            let _ = names.FindLocaleName(w!("en-us"), &mut idx, &mut exists);
            if !exists.as_bool() {
                idx = 0;
            }
            let Ok(len) = names.GetStringLength(idx) else { continue };
            let mut buf = vec![0u16; len as usize + 1];
            if names.GetString(idx, &mut buf).is_err() {
                continue;
            }
            let name = String::from_utf16_lossy(&buf[..len as usize]);
            let mono = fam
                .GetFirstMatchingFont(DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL)
                .ok()
                .and_then(|f| f.cast::<IDWriteFont1>().ok())
                .map(|f| f.IsMonospacedFont().as_bool())
                .unwrap_or(false);
            out.push((name, mono));
        }
    }
    out.sort_by_key(|(n, _)| n.to_lowercase());
    out
}

/// Premultiplied BGRA format for offscreen WIC targets.
pub fn offscreen_pixel_format() -> D2D1_PIXEL_FORMAT {
    D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED }
}

pub fn pcwstr(v: &[u16]) -> PCWSTR {
    PCWSTR(v.as_ptr())
}
