//! Colors and sizes. Light and dark palettes follow Windows 11 Notepad's look: the active tab, menu bar and text
//! area share one surface; the tab strip and status bar sit on a slightly darker (or lighter) frame. While Windows'
//! high contrast is on, every color comes from Windows instead (`Theme::high_contrast`).

use windows::Win32::Graphics::Gdi::{
    COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_HOTLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor,
    SYS_COLOR_INDEX,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};
use windows::core::w;

use super::gfx::{rgb, rgba};

#[derive(Clone, Debug)]
pub struct Theme {
    pub dark: bool,
    /// Windows' high contrast is on: these are its colors, text isn't colored by syntax, and selected text is
    /// drawn in `selection_text`.
    pub hc: bool,
    pub frame: u32,
    pub surface: u32,
    pub text: u32,
    pub text_dim: u32,
    pub text_faint: u32,
    pub hover: u32,
    pub pressed: u32,
    pub border: u32,
    pub accent: u32,
    /// Text on the accent color (the find bar's Go button).
    pub on_accent: u32,
    pub gutter: u32,
    pub gutter_active: u32,
    pub current_line: u32,
    pub selection: u32,
    pub selection_inactive: u32,
    /// Selected text in high contrast (other themes keep the text's own colors).
    pub selection_text: u32,
    pub caret: u32,
    pub match_bg: u32,
    pub match_current: u32,
    /// The bracket at the caret and the one it pairs with.
    pub bracket_bg: u32,
    pub bracket_border: u32,
    /// Spaces, tabs and line ends (View → Show whitespace).
    pub whitespace: u32,
    pub scroll_thumb: u32,
    pub scroll_thumb_hover: u32,
    pub scroll_mark: u32,
    pub input_bg: u32,
    pub notice_bg: u32,
    pub error: u32,
    pub warning: u32,
    pub ok: u32,
    // syntax
    pub syn_key: u32,
    pub syn_string: u32,
    pub syn_number: u32,
    pub syn_literal: u32,
    pub syn_punct: u32,
    pub syn_comment: u32,
    pub syn_section: u32,
    pub syn_error: u32,
    pub syn_warn: u32,
    pub syn_info: u32,
    pub syn_dim: u32,
    pub syn_keyword: u32,
    pub syn_control: u32,
    pub syn_type: u32,
    pub syn_func: u32,
    pub syn_tag: u32,
    pub syn_attr: u32,
    pub syn_var: u32,
    pub syn_heading: u32,
    pub syn_link: u32,
    pub syn_added: u32,
    pub syn_removed: u32,
    /// CSV columns 1..7 (column 0 keeps the text color).
    pub syn_cols: [u32; 8],
}

impl Theme {
    pub fn light(accent: u32) -> Theme {
        Theme {
            dark: false,
            hc: false,
            frame: rgb(0xEEEEEE),
            surface: rgb(0xFFFFFF),
            text: rgb(0x1B1B1B),
            text_dim: rgb(0x5C5C5C),
            text_faint: rgb(0x9A9A9A),
            hover: rgba(0x000000, 0x0F),
            pressed: rgba(0x000000, 0x1A),
            border: rgb(0xE0E0E0),
            accent,
            on_accent: rgb(0xFFFFFF),
            gutter: rgb(0xA0A0A0),
            gutter_active: rgb(0x404040),
            current_line: rgb(0xF6F6F6),
            selection: rgb(0xB4D7FF),
            selection_inactive: rgb(0xE2E6EC),
            selection_text: rgb(0x1B1B1B),
            caret: rgb(0x000000),
            match_bg: rgb(0xFFE59A),
            match_current: rgb(0xF7B93E),
            bracket_bg: rgba(0x000000, 0x12),
            bracket_border: rgb(0xA8A8A8),
            whitespace: rgba(0x000000, 0x40),
            scroll_thumb: rgba(0x000000, 0x40),
            scroll_thumb_hover: rgba(0x000000, 0x70),
            scroll_mark: rgba(0xD08A00, 0xC0),
            input_bg: rgb(0xFFFFFF),
            notice_bg: rgb(0xFFF4CE),
            error: rgb(0xC42B1C),
            warning: rgb(0x9D5D00),
            ok: rgb(0x0F7B0F),
            // (mostly VS Code's Light+; each at least 4.5:1 on the surface)
            syn_key: rgb(0x0451A5),
            syn_string: rgb(0xA31515),
            syn_number: rgb(0x098658),
            syn_literal: rgb(0x0000FF),
            syn_punct: rgb(0x555555),
            syn_comment: rgb(0x008000),
            syn_section: rgb(0x800080),
            syn_error: rgb(0xCD3131),
            syn_warn: rgb(0x9A6200),
            syn_info: rgb(0x0451A5),
            syn_dim: rgb(0x707070),
            syn_keyword: rgb(0x0000FF),
            syn_control: rgb(0xAF00DB),
            syn_type: rgb(0x267F99),
            syn_func: rgb(0x795E26),
            syn_tag: rgb(0x800000),
            syn_attr: rgb(0xE50000),
            syn_var: rgb(0x001080),
            syn_heading: rgb(0x800000),
            syn_link: rgb(0x0451A5),
            syn_added: rgb(0x098658),
            syn_removed: rgb(0xA31515),
            syn_cols: [
                rgb(0x1B1B1B),
                rgb(0x267F99),
                rgb(0x795E26),
                rgb(0xAF00DB),
                rgb(0x0451A5),
                rgb(0xA31515),
                rgb(0x098658),
                rgb(0xB05A00),
            ],
        }
    }

    pub fn dark(accent: u32) -> Theme {
        Theme {
            dark: true,
            hc: false,
            frame: rgb(0x1C1C1C),
            surface: rgb(0x272727),
            text: rgb(0xE6E6E6),
            text_dim: rgb(0xB0B0B0),
            text_faint: rgb(0x7A7A7A),
            hover: rgba(0xFFFFFF, 0x12),
            pressed: rgba(0xFFFFFF, 0x1C),
            border: rgb(0x3A3A3A),
            accent,
            on_accent: rgb(0xFFFFFF),
            gutter: rgb(0x6E6E6E),
            gutter_active: rgb(0xD0D0D0),
            current_line: rgb(0x2E2E2E),
            selection: rgb(0x264F78),
            selection_inactive: rgb(0x3A3D41),
            selection_text: rgb(0xE6E6E6),
            caret: rgb(0xFFFFFF),
            match_bg: rgba(0xEA9A3C, 0x55),
            match_current: rgba(0xF2B03A, 0xB0),
            bracket_bg: rgba(0xFFFFFF, 0x14),
            bracket_border: rgb(0x7A7A7A),
            whitespace: rgba(0xFFFFFF, 0x40),
            scroll_thumb: rgba(0xFFFFFF, 0x40),
            scroll_thumb_hover: rgba(0xFFFFFF, 0x70),
            scroll_mark: rgba(0xF2B03A, 0xC0),
            input_bg: rgb(0x1E1E1E),
            notice_bg: rgb(0x433519),
            error: rgb(0xFF99A4),
            warning: rgb(0xFCE100),
            ok: rgb(0x6CCB5F),
            // (mostly VS Code's Dark+; each at least 4.5:1 on the surface, which is lighter than VS Code's)
            syn_key: rgb(0x9CDCFE),
            syn_string: rgb(0xCE9178),
            syn_number: rgb(0xB5CEA8),
            syn_literal: rgb(0x569CD6),
            syn_punct: rgb(0xA0A0A0),
            syn_comment: rgb(0x72A35C),
            syn_section: rgb(0xC586C0),
            syn_error: rgb(0xFF5555),
            syn_warn: rgb(0xDCDCAA),
            syn_info: rgb(0x4FC1FF),
            syn_dim: rgb(0x8F8F8F),
            syn_keyword: rgb(0x569CD6),
            syn_control: rgb(0xC586C0),
            syn_type: rgb(0x4EC9B0),
            syn_func: rgb(0xDCDCAA),
            syn_tag: rgb(0x569CD6),
            syn_attr: rgb(0x9CDCFE),
            syn_var: rgb(0x9CDCFE),
            syn_heading: rgb(0x569CD6),
            syn_link: rgb(0x4FC1FF),
            syn_added: rgb(0xB5CEA8),
            syn_removed: rgb(0xF48771),
            syn_cols: [
                rgb(0xE6E6E6),
                rgb(0x4EC9B0),
                rgb(0xDCDCAA),
                rgb(0xC586C0),
                rgb(0x9CDCFE),
                rgb(0xCE9178),
                rgb(0xB5CEA8),
                rgb(0xD7BA7D),
            ],
        }
    }

    /// Windows' high-contrast colors (read again whenever they change). Everything sits on the window color with
    /// text in the window text color; selections, the accent and focus use the highlight colors. The few in-between
    /// shades (hover, matches, an unfocused selection) are see-through tints of those, so the text on them keeps
    /// its contrast.
    pub fn high_contrast() -> Theme {
        let window = sys_color(COLOR_WINDOW);
        let text = sys_color(COLOR_WINDOWTEXT);
        let hl = sys_color(COLOR_HIGHLIGHT);
        let hl_text = sys_color(COLOR_HIGHLIGHTTEXT);
        let gray = sys_color(COLOR_GRAYTEXT);
        let link = sys_color(COLOR_HOTLIGHT);
        Theme {
            dark: luminance(window) < 0.5,
            hc: true,
            frame: window,
            surface: window,
            text,
            text_dim: text,
            text_faint: gray,
            hover: rgba(text, 0x30),
            pressed: rgba(text, 0x50),
            border: text,
            accent: hl,
            on_accent: hl_text,
            gutter: text,
            gutter_active: text,
            current_line: window,
            selection: hl,
            selection_inactive: rgba(hl, 0x60),
            selection_text: hl_text,
            caret: text,
            match_bg: rgba(hl, 0x60),
            match_current: hl,
            bracket_bg: rgba(window, 0),
            bracket_border: link,
            whitespace: gray,
            scroll_thumb: gray,
            scroll_thumb_hover: text,
            scroll_mark: hl,
            input_bg: window,
            notice_bg: window,
            error: text,
            warning: text,
            ok: text,
            syn_key: text,
            syn_string: text,
            syn_number: text,
            syn_literal: text,
            syn_punct: text,
            syn_comment: text,
            syn_section: text,
            syn_error: text,
            syn_warn: text,
            syn_info: text,
            syn_dim: text,
            syn_keyword: text,
            syn_control: text,
            syn_type: text,
            syn_func: text,
            syn_tag: text,
            syn_attr: text,
            syn_var: text,
            syn_heading: text,
            syn_link: text,
            syn_added: text,
            syn_removed: text,
            syn_cols: [text; 8],
        }
    }
}

/// A Windows system color as 0xFFRRGGBB (GetSysColor gives 0x00BBGGRR).
fn sys_color(i: SYS_COLOR_INDEX) -> u32 {
    let c = unsafe { GetSysColor(i) };
    rgb(((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF))
}

/// Relative brightness of 0xAARRGGBB, 0..1 (rough: no gamma).
fn luminance(c: u32) -> f32 {
    let (r, g, b) = ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

thread_local! {
    /// Test mode: pretend high contrast is on (or off), whatever Windows says.
    pub static FORCED_HIGH_CONTRAST: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Whether Windows' high contrast is on (Settings → Accessibility → Contrast themes).
pub fn high_contrast_on() -> bool {
    if let Some(on) = FORCED_HIGH_CONTRAST.with(|f| f.get()) {
        return on;
    }
    let mut hc = HIGHCONTRASTW { cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            Some(&mut hc as *mut HIGHCONTRASTW as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    ok && hc.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0
}

fn reg_dword(path: windows::core::PCWSTR, name: windows::core::PCWSTR) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            path,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    Some(v)
}

/// Whether Windows is set to dark mode for apps.
pub fn system_prefers_dark() -> bool {
    reg_dword(
        w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
        w!("AppsUseLightTheme"),
    )
    .map(|v| v == 0)
    .unwrap_or(false)
}

/// The Windows accent color as 0xFFRRGGBB.
pub fn system_accent() -> u32 {
    // AccentColor is stored as 0xAABBGGRR.
    match reg_dword(w!(r"Software\Microsoft\Windows\DWM"), w!("AccentColor")) {
        Some(abgr) => {
            let r = abgr & 0xFF;
            let g = (abgr >> 8) & 0xFF;
            let b = (abgr >> 16) & 0xFF;
            rgb((r << 16) | (g << 8) | b)
        }
        None => rgb(0x0067C0),
    }
}

/// Sizes in DIPs.
pub mod metrics {
    pub const TAB_BAR_H: f32 = 38.0;
    pub const MENU_BAR_H: f32 = 32.0;
    pub const STATUS_H: f32 = 26.0;
    pub const FIND_ROW_H: f32 = 40.0;
    pub const NOTICE_H: f32 = 34.0;
    pub const TAB_MIN_W: f32 = 90.0;
    pub const TAB_MAX_W: f32 = 240.0;
    pub const SCROLLBAR_W: f32 = 14.0;
    pub const UI_FONT_SIZE: f32 = 12.0;
}
