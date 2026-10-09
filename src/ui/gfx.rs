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
            let make = |kind: D3D_DRIVER_TYPE| -> windows::core::Result<ID3D11Device> {
                let mut dev = None;
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
                dev.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
            };
            let dev = make(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| make(D3D_DRIVER_TYPE_WARP))?;
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

    /// `draw_layout`, but only the glyphs that reach into `view`: Direct2D draws every run of a layout, and a long row
    /// of colored text has thousands, nearly all out of view (see `text_renderer`).
    pub fn draw_layout_in(&self, l: &IDWriteTextLayout, x: f32, y: f32, argb: u32, view: Rect, glyphs: Ink) {
        text_renderer::draw(self, l, x, y, argb, view, glyphs);
    }
}

/// How `Gfx::draw_layout_in` colors a layout's text.
#[derive(Clone, Copy, Default)]
pub struct Ink<'a> {
    /// The text may have color glyphs (emoji), which only Windows 11's Direct2D draws run by run (elsewhere the whole
    /// layout is drawn).
    pub color_glyphs: bool,
    /// Where its colors start (UTF-16 positions, of a text `at` positions before the layout's), instead of colors set
    /// on the layout (those cost a lot more when a segment is packed with them).
    pub colors: &'a [(u32, u32)],
    pub at: u32,
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

/// The text renderer behind `Gfx::draw_layout_in`, a COM object made by hand (a vtable and the data after it): only
/// the glyphs in view, a row's runs in one font as one run per color (Direct2D's cost is per run), and colors given as
/// a list rather than set on the layout (where each costs a lot when a segment is packed with them).
mod text_renderer {
    use std::cell::RefCell;
    use std::ffi::c_void;

    use windows::Win32::Foundation::{BOOL, E_NOINTERFACE, S_OK};
    use windows::Win32::Graphics::Direct2D::Common::{D2D_POINT_2F, D2D_RECT_F};
    use windows::Win32::Graphics::Direct2D::{
        D2D1_COLOR_BITMAP_GLYPH_SNAP_OPTION_DEFAULT, ID2D1DeviceContext7, ID2D1RenderTarget, ID2D1SolidColorBrush,
        ID2D1SvgGlyphStyle,
    };
    use windows::Win32::Graphics::DirectWrite::{
        DWRITE_GLYPH_OFFSET, DWRITE_GLYPH_RUN, DWRITE_GLYPH_RUN_DESCRIPTION, DWRITE_MATRIX, DWRITE_MEASURING_MODE,
        DWRITE_STRIKETHROUGH, DWRITE_UNDERLINE, IDWriteFontFace, IDWriteFontFace2, IDWritePixelSnapping,
        IDWritePixelSnapping_Vtbl, IDWriteTextLayout, IDWriteTextRenderer, IDWriteTextRenderer_Vtbl,
    };
    use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};

    use super::{Gfx, Ink, Rect};

    type GlyphOffset = DWRITE_GLYPH_OFFSET;

    /// See `Gfx::draw_layout_in`.
    pub fn draw(g: &Gfx, l: &IDWriteTextLayout, x: f32, y: f32, argb: u32, view: Rect, glyphs: Ink) {
        let Ink { color_glyphs, colors, at } = glyphs;
        let rt = g.rt().clone();
        let color = if color_glyphs {
            match rt.cast::<ID2D1DeviceContext7>() {
                Ok(dc) => Some(dc),
                Err(_) => return g.draw_layout(l, x, y, argb),
            }
        } else {
            None
        };
        let (mut dx, mut dy) = (96.0f32, 96.0f32);
        unsafe { rt.GetDpi(&mut dx, &mut dy) };
        let mut c = Culler {
            vtbl: &CULLER_VTBL,
            g,
            rt,
            argb,
            view: view.d2d(),
            ppd: dx / 96.0,
            color,
            colors,
            at,
            held: RefCell::new(Vec::new()),
        };
        unsafe {
            // (The renderer lives on the stack for this call only: it counts no references.)
            let r = std::mem::ManuallyDrop::new(IDWriteTextRenderer::from_raw(&mut c as *mut Culler as *mut c_void));
            let _ = l.Draw(None, &*r, x, y);
            c.flush();
        }
    }

    #[repr(C)]
    struct Culler<'a> {
        vtbl: *const IDWriteTextRenderer_Vtbl,
        g: &'a Gfx,
        rt: ID2D1RenderTarget,
        /// The color of text the layout gives none.
        argb: u32,
        view: D2D_RECT_F,
        ppd: f32,
        /// Draws color glyphs in color.
        color: Option<ID2D1DeviceContext7>,
        /// Where the text's colors start (UTF-16, of a text `at` positions before the layout's), if not on the layout.
        colors: &'a [(u32, u32)],
        at: u32,
        /// Runs kept back to be drawn together (`flush`).
        held: RefCell<Vec<Held>>,
    }

    /// The part in view of a glyph run, copied out of the layout until its row is done.
    struct Held {
        x: f32,
        y: f32,
        face: IDWriteFontFace,
        em: f32,
        mode: DWRITE_MEASURING_MODE,
        bidi: u32,
        glyphs: Vec<u16>,
        advances: Vec<f32>,
        offsets: Vec<DWRITE_GLYPH_OFFSET>,
        /// Each glyph's color.
        colors: Vec<u32>,
    }

    impl Culler<'_> {
        /// Draws the runs held back: those next to each other on a row in one font (left to right) go as one run per
        /// color, in which the other colors' glyphs are blanks. Direct2D's cost is mostly per run, and a row of code
        /// has dozens of colors; this way it has as many runs as it has colors.
        fn flush(&self) {
            let held = self.held.take();
            let width = |h: &Held| h.advances.iter().sum::<f32>();
            let mut i = 0;
            while i < held.len() {
                let a = &held[i];
                let mut end = a.x + width(a);
                let mut j = i + 1;
                while j < held.len() && a.bidi & 1 == 0 {
                    let b = &held[j];
                    let same = b.y == a.y && b.face.as_raw() == a.face.as_raw() && b.em == a.em && b.mode == a.mode;
                    if !same || b.bidi != a.bidi || b.x < end - 0.01 {
                        break;
                    }
                    end = b.x + width(b);
                    j += 1;
                }
                let group = &held[i..j];
                i = j;
                // The group's glyphs, with a blank over any gap between runs (no color).
                let (mut glyphs, mut advances, mut offsets, mut colors) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                let mut x = a.x;
                for h in group {
                    if h.x > x + 0.01 {
                        glyphs.push(0);
                        advances.push(h.x - x);
                        offsets.push(DWRITE_GLYPH_OFFSET::default());
                        colors.push(None);
                    }
                    glyphs.extend_from_slice(&h.glyphs);
                    advances.extend_from_slice(&h.advances);
                    if h.offsets.is_empty() {
                        offsets.extend(std::iter::repeat_n(DWRITE_GLYPH_OFFSET::default(), h.glyphs.len()));
                    } else {
                        offsets.extend_from_slice(&h.offsets);
                    }
                    colors.extend(h.colors.iter().map(|&c| Some(c)));
                    x = h.x + width(h);
                }
                // (the font's space: a glyph with nothing to draw, in place of the other colors' glyphs)
                let mut blank = 0u16;
                let space = b' ' as u32;
                if unsafe { a.face.GetGlyphIndices(&space, 1, &mut blank) }.is_err() || blank == 0 {
                    // without one: each stretch of one color on its own
                    let mut s = 0;
                    while s < glyphs.len() {
                        let mut e = s + 1;
                        while e < glyphs.len() && colors[e] == colors[s] {
                            e += 1;
                        }
                        if let Some(c) = colors[s] {
                            self.draw_run(a, s, c, &glyphs[s..e], &advances, &offsets[s..e]);
                        }
                        s = e;
                    }
                    continue;
                }
                let mut done: Vec<u32> = Vec::new();
                for &c in colors.iter().flatten() {
                    if done.contains(&c) {
                        continue;
                    }
                    done.push(c);
                    let mine = |k: &Option<u32>| *k == Some(c);
                    let (Some(first), Some(last)) = (colors.iter().position(mine), colors.iter().rposition(mine)) else {
                        continue;
                    };
                    let ids: Vec<u16> =
                        (first..=last).map(|g| if mine(&colors[g]) { glyphs[g] } else { blank }).collect();
                    self.draw_run(a, first, c, &ids, &advances, &offsets[first..=last]);
                }
            }
        }

        /// Draws `glyphs` in the font and size of `h`, from the glyph `from` of the row's advances on, in `argb`.
        fn draw_run(&self, h: &Held, from: usize, argb: u32, glyphs: &[u16], advances: &[f32], offsets: &[GlyphOffset]) {
            let before: f32 = advances[..from].iter().sum();
            let x = if h.bidi & 1 == 1 { h.x - before } else { h.x + before };
            let run = DWRITE_GLYPH_RUN {
                // (borrowed: no reference of its own)
                fontFace: std::mem::ManuallyDrop::new(Some(unsafe { std::mem::transmute_copy(&h.face) })),
                fontEmSize: h.em,
                glyphCount: glyphs.len() as u32,
                glyphIndices: glyphs.as_ptr(),
                glyphAdvances: advances[from..from + glyphs.len()].as_ptr(),
                glyphOffsets: if offsets.is_empty() { std::ptr::null() } else { offsets.as_ptr() },
                isSideways: BOOL(0),
                bidiLevel: h.bidi,
            };
            unsafe { self.rt.DrawGlyphRun(D2D_POINT_2F { x, y: h.y }, &run, &self.g.brush(argb), h.mode) };
        }

        /// Each glyph's color in glyphs `[a, b)` of a run: from the colors given (through the characters of each
        /// glyph's cluster), else `default` (the run's own, or the text's).
        unsafe fn glyph_colors(&self, desc: *const DWRITE_GLYPH_RUN_DESCRIPTION, n: usize, a: usize, b: usize, default: u32)
        -> Vec<u32> {
            let mut out = vec![default; b - a];
            let colors = self.colors;
            if colors.is_empty() || desc.is_null() {
                return out;
            }
            let d = unsafe { &*desc };
            if d.clusterMap.is_null() || d.stringLength == 0 {
                return out;
            }
            let map = unsafe { std::slice::from_raw_parts(d.clusterMap, d.stringLength as usize) };
            // the characters whose clusters reach into [a, b): from the one holding glyph `a` (its first character)
            let mut i = map.partition_point(|&g| g as usize <= a).saturating_sub(1);
            while i > 0 && map[i - 1] == map[i] {
                i -= 1;
            }
            let base = self.at + d.textPosition;
            let mut k = colors.partition_point(|e| e.0 <= base + i as u32).saturating_sub(1);
            while i < map.len() && (map[i] as usize) < b {
                let mut next = i + 1;
                while next < map.len() && map[next] == map[i] {
                    next += 1;
                }
                let g1 = if next < map.len() { map[next] as usize } else { n };
                let pos = base + i as u32;
                while k + 1 < colors.len() && colors[k + 1].0 <= pos {
                    k += 1;
                }
                if colors[k].0 <= pos {
                    for g in (map[i] as usize).max(a)..g1.min(b) {
                        out[g - a] = colors[k].1;
                    }
                }
                i = next;
            }
            out
        }
    }

    static CULLER_VTBL: IDWriteTextRenderer_Vtbl = IDWriteTextRenderer_Vtbl {
        base__: IDWritePixelSnapping_Vtbl {
            base__: IUnknown_Vtbl { QueryInterface: query_interface, AddRef: no_count, Release: no_count },
            IsPixelSnappingDisabled: no_snap,
            GetCurrentTransform: transform,
            GetPixelsPerDip: pixels_per_dip,
        },
        DrawGlyphRun: glyph_run,
        DrawUnderline: underline,
        DrawStrikethrough: strikethrough,
        DrawInlineObject: inline_object,
    };

    unsafe extern "system" fn query_interface(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
        let iid = unsafe { *iid };
        let ok = iid == IUnknown::IID || iid == IDWritePixelSnapping::IID || iid == IDWriteTextRenderer::IID;
        unsafe { *out = if ok { this } else { std::ptr::null_mut() } };
        if ok { S_OK } else { E_NOINTERFACE }
    }

    unsafe extern "system" fn no_count(_: *mut c_void) -> u32 {
        1
    }

    unsafe extern "system" fn no_snap(_: *mut c_void, _: *const c_void, out: *mut BOOL) -> HRESULT {
        unsafe { *out = BOOL(0) };
        S_OK
    }

    unsafe extern "system" fn transform(this: *mut c_void, _: *const c_void, out: *mut DWRITE_MATRIX) -> HRESULT {
        let c = unsafe { &*(this as *const Culler) };
        let mut m = windows::Foundation::Numerics::Matrix3x2::default();
        unsafe {
            c.rt.GetTransform(&mut m);
            *out = DWRITE_MATRIX { m11: m.M11, m12: m.M12, m21: m.M21, m22: m.M22, dx: m.M31, dy: m.M32 };
        }
        S_OK
    }

    unsafe extern "system" fn pixels_per_dip(this: *mut c_void, _: *const c_void, out: *mut f32) -> HRESULT {
        unsafe { *out = (*(this as *const Culler)).ppd };
        S_OK
    }

    /// The color a run was given on the layout (`SetDrawingEffect`), else the default one.
    unsafe fn color_of(c: &Culler, effect: *mut c_void) -> u32 {
        let brush = unsafe { IUnknown::from_raw_borrowed(&effect) }.and_then(|e| e.cast::<ID2D1SolidColorBrush>().ok());
        let Some(brush) = brush else { return c.argb };
        let k = unsafe { brush.GetColor() };
        let to = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u32;
        to(k.a) << 24 | to(k.r) << 16 | to(k.g) << 8 | to(k.b)
    }

    unsafe extern "system" fn glyph_run(
        this: *mut c_void,
        _: *const c_void,
        x: f32,
        y: f32,
        mode: DWRITE_MEASURING_MODE,
        run: *const DWRITE_GLYPH_RUN,
        desc: *const DWRITE_GLYPH_RUN_DESCRIPTION,
        effect: *mut c_void,
    ) -> HRESULT {
        let c = unsafe { &*(this as *const Culler) };
        let r = unsafe { &*run };
        let em = r.fontEmSize;
        let v = c.view;
        // (with an em to spare for what glyphs reach beyond their advance: accents, italics, tall fallback fonts)
        if y + em < v.top || y - 2.0 * em > v.bottom {
            return S_OK;
        }
        let n = r.glyphCount as usize;
        let advances = match r.glyphAdvances.is_null() || n == 0 {
            true => &[],
            false => unsafe { std::slice::from_raw_parts(r.glyphAdvances, n) },
        };
        let w: f32 = advances.iter().sum();
        let rtl = r.bidiLevel & 1 == 1;
        let (x0, x1) = if rtl { (x - w, x) } else { (x, x + w) };
        if x1 + em < v.left || x0 - em > v.right {
            return S_OK;
        }
        unsafe {
            let color = color_of(c, effect);
            // (only a color font's runs: drawn so, any run costs ten times as much)
            let colored = |f: &IDWriteFontFace| f.cast::<IDWriteFontFace2>().is_ok_and(|f| f.IsColorFont().as_bool());
            match (&c.color, r.fontFace.as_ref()) {
                (Some(dc), Some(f)) if colored(f) => {
                    let snap = D2D1_COLOR_BITMAP_GLYPH_SNAP_OPTION_DEFAULT;
                    let brush = c.g.brush(color);
                    let at = D2D_POINT_2F { x, y };
                    let svg = None::<&ID2D1SvgGlyphStyle>;
                    dc.DrawGlyphRunWithColorSupport(at, run, Some(desc), &brush, svg, 0, mode, snap);
                }
                // kept to be drawn with the rest of its row; left to right, only what's in view of it (a row of plain
                // text without word wrap is one run of up to 8 KiB)
                (_, Some(f)) if !r.isSideways.as_bool() && w > 0.0 => {
                    let (mut a, mut ax, mut b) = (0, x, n);
                    if !rtl {
                        while a < n && ax + advances[a] + em < v.left {
                            ax += advances[a];
                            a += 1;
                        }
                        let mut bx = ax;
                        b = a;
                        while b < n && bx - em <= v.right {
                            bx += advances[b];
                            b += 1;
                        }
                    }
                    if a == b {
                        return S_OK;
                    }
                    let offsets = if r.glyphOffsets.is_null() { &[] } else { std::slice::from_raw_parts(r.glyphOffsets, n) };
                    let colors = c.glyph_colors(desc, n, a, b, color);
                    c.held.borrow_mut().push(Held {
                        x: ax,
                        y,
                        face: f.clone(),
                        em,
                        mode,
                        bidi: r.bidiLevel,
                        glyphs: std::slice::from_raw_parts(r.glyphIndices, n)[a..b].to_vec(),
                        advances: advances[a..b].to_vec(),
                        offsets: offsets.get(a..b).unwrap_or_default().to_vec(),
                        colors,
                    });
                }
                _ => c.rt.DrawGlyphRun(D2D_POINT_2F { x, y }, run, &c.g.brush(color), mode),
            }
        }
        S_OK
    }

    unsafe fn line(c: &Culler, x: f32, y: f32, width: f32, thickness: f32, offset: f32, effect: *mut c_void) {
        let r = D2D_RECT_F { left: x, top: y + offset, right: x + width, bottom: y + offset + thickness };
        unsafe { c.rt.FillRectangle(&r, &c.g.brush(color_of(c, effect))) };
    }

    unsafe extern "system" fn underline(
        this: *mut c_void,
        _: *const c_void,
        x: f32,
        y: f32,
        u: *const DWRITE_UNDERLINE,
        effect: *mut c_void,
    ) -> HRESULT {
        unsafe {
            let u = &*u;
            line(&*(this as *const Culler), x, y, u.width, u.thickness, u.offset, effect);
        }
        S_OK
    }

    unsafe extern "system" fn strikethrough(
        this: *mut c_void,
        _: *const c_void,
        x: f32,
        y: f32,
        s: *const DWRITE_STRIKETHROUGH,
        effect: *mut c_void,
    ) -> HRESULT {
        unsafe {
            let s = &*s;
            line(&*(this as *const Culler), x, y, s.width, s.thickness, s.offset, effect);
        }
        S_OK
    }

    unsafe extern "system" fn inline_object(
        _: *mut c_void,
        _: *const c_void,
        _: f32,
        _: f32,
        _: *mut c_void,
        _: BOOL,
        _: BOOL,
        _: *mut c_void,
    ) -> HRESULT {
        // (The text has no inline objects.)
        S_OK
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICImagingFactory, WICBitmapCacheOnLoad,
    };
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};

    /// Gives `g` a render target: a bitmap of `w`×`h` pixels.
    pub(crate) fn bitmap_target(g: &mut Gfx, w: u32, h: u32) -> IWICBitmap {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let bmp = wic.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad).unwrap();
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: offscreen_pixel_format(),
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            g.use_target(g.d2d.CreateWicBitmapRenderTarget(&bmp, &props).unwrap(), 96.0);
            bmp
        }
    }

    /// The pixels of what `f` draws into a fresh 300×120 bitmap (white).
    fn render(g: &mut Gfx, f: impl Fn(&Gfx)) -> Vec<u8> {
        let bmp = bitmap_target(g, 300, 120);
        unsafe {
            g.rt().BeginDraw();
            g.rt().Clear(Some(&color(rgb(0xFFFFFF))));
            f(g);
            g.rt().EndDraw(None, None).unwrap();
            let mut px = vec![0u8; 300 * 120 * 4];
            bmp.CopyPixels(std::ptr::null(), 300 * 4, &mut px).unwrap();
            px
        }
    }

    #[test]
    fn drawing_only_what_is_in_view_looks_the_same() {
        // colored runs next to each other (drawn as one run per color), wrapped into rows; tabs, a fallback font for
        // the kanji between runs in the main one, combining accents; emoji
        let code = "let x = \"two\" + 3; // in colors\tand a tab, then \u{65E5}\u{672C} in between, e\u{301}, more words";
        in_view_looks_the_same(code, false);
        in_view_looks_the_same(&"a long run of plain text in one color, ".repeat(8), false);
        in_view_looks_the_same("emoji \u{1F600}\u{2764}\u{FE0F} \u{65E5}\u{672C} x", true);
    }

    fn in_view_looks_the_same(s: &str, color_glyphs: bool) {
        let mut g = Gfx::new().unwrap();
        let fmt = g.format("Consolas", 14.0, DWRITE_FONT_WEIGHT_NORMAL);
        let text = wide(s);
        let draw = |view: Option<Rect>| {
            let (text, fmt) = (&text, &fmt);
            move |g: &Gfx| {
                let l = g.layout(text, fmt, 260.0, 100.0);
                let colors = [(0, 3, 0x0000FF), (8, 5, 0xA31515), (14, 1, 0x555555), (16, 1, 0x098658), (19, 12, 0x008000)];
                // the colors on the layout, or (drawn in view) as a list of where each starts
                let mut each = vec![rgb(0x1B1B1B); text.len()];
                for (at, n, c) in colors {
                    let len = each.len();
                    each[(at as usize).min(len)..((at + n) as usize).min(len)].fill(rgb(c));
                    if view.is_none() || color_glyphs {
                        let range = DWRITE_TEXT_RANGE { startPosition: at, length: n };
                        unsafe {
                            let _ = l.SetDrawingEffect(&g.brush(rgb(c)), range);
                        }
                    }
                }
                let mut list: Vec<(u32, u32)> = Vec::new();
                for (i, &c) in each.iter().enumerate() {
                    if list.last().is_none_or(|&(_, k)| k != c) {
                        list.push((i as u32, c));
                    }
                }
                let ink = Ink { color_glyphs, colors: if color_glyphs { &[] } else { &list }, at: 0 };
                match view {
                    Some(v) => g.draw_layout_in(&l, 10.5, 10.25, rgb(0x1B1B1B), v, ink),
                    None => g.draw_layout(&l, 10.5, 10.25, rgb(0x1B1B1B)),
                }
            }
        };
        let whole = render(&mut g, draw(None));
        assert!(whole.iter().any(|&b| b < 0x80), "something is drawn");
        assert!(render(&mut g, draw(Some(Rect::new(0.0, 0.0, 300.0, 120.0)))) == whole, "the same pixels: {s}");
        // runs out of view aren't drawn
        let below = render(&mut g, draw(Some(Rect::new(0.0, 200.0, 300.0, 20.0))));
        assert!(below.iter().all(|&b| b == 0xFF));
        let right = render(&mut g, draw(Some(Rect::new(400.0, 0.0, 100.0, 120.0))));
        assert!(right.iter().all(|&b| b == 0xFF));
        // in view, runs cut down to what's in view look the same
        let part = render(&mut g, draw(Some(Rect::new(100.0, 0.0, 80.0, 120.0))));
        let cols = |px: &[u8]| -> Vec<u8> { px.chunks(300 * 4).flat_map(|row| row[100 * 4..180 * 4].to_vec()).collect() };
        assert!(cols(&part) == cols(&whole), "the same pixels in view: {s}");
    }
}
