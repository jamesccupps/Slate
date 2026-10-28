//! Input methods (Chinese, Japanese, Korean…) through IMM32: the text being composed is drawn in the text at the
//! caret (`View::composing`), never in Windows' own composition window; what's chosen is typed as keys type. The
//! candidate list goes under the clause being converted. Test mode has no real input method: `imemsg:` puts what
//! one would have in `FAKE` and sends the real messages.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::LOGFONTW;
use windows::Win32::UI::Input::Ime::*;

use crate::edit::Composing;

/// What an input method has, in IMM32's form: the text composed (UTF-16), an attribute for each unit (`ATTR_…`), the
/// clauses' starts (UTF-16, then the text's end), the cursor (UTF-16); and a result to type.
#[derive(Clone, Default)]
pub struct Raw {
    pub comp: Vec<u16>,
    pub attrs: Vec<u8>,
    pub clauses: Vec<u32>,
    pub cursor: u32,
    pub result: Vec<u16>,
}

thread_local! {
    /// Test mode: what the input method has (`imemsg:`), read instead of asking IMM32.
    pub static FAKE: RefCell<Option<Raw>> = const { RefCell::new(None) };
}

/// What WM_IME_COMPOSITION (its `lp` saying what changed) brings: a result to type, and the composition now (empty
/// when the message has none: after a result, or when everything composed was deleted).
pub fn read(hwnd: HWND, lp: LPARAM) -> (Option<String>, Composing) {
    let has = |f: IME_COMPOSITION_STRING| lp.0 as u32 & f.0 != 0;
    let raw = FAKE.with(|f| f.borrow().clone()).unwrap_or_else(|| unsafe {
        let himc = ImmGetContext(hwnd);
        let get = |kind: IME_COMPOSITION_STRING| -> Vec<u8> {
            let n = ImmGetCompositionStringW(himc, kind, None, 0);
            if n <= 0 {
                return Vec::new();
            }
            let mut buf = vec![0u8; n as usize];
            let m = ImmGetCompositionStringW(himc, kind, Some(buf.as_mut_ptr() as *mut _), n as u32);
            buf.truncate(m.max(0) as usize);
            buf
        };
        let units = |b: Vec<u8>| b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<u16>>();
        let raw = Raw {
            comp: if has(GCS_COMPSTR) { units(get(GCS_COMPSTR)) } else { Vec::new() },
            attrs: if has(GCS_COMPATTR) { get(GCS_COMPATTR) } else { Vec::new() },
            clauses: if has(GCS_COMPCLAUSE) {
                get(GCS_COMPCLAUSE).chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
            } else {
                Vec::new()
            },
            // (the position itself, not a length)
            cursor: if has(GCS_CURSORPOS) { ImmGetCompositionStringW(himc, GCS_CURSORPOS, None, 0).max(0) as u32 } else { u32::MAX },
            result: if has(GCS_RESULTSTR) { units(get(GCS_RESULTSTR)) } else { Vec::new() },
        };
        let _ = ImmReleaseContext(hwnd, himc);
        raw
    });
    let result = (has(GCS_RESULTSTR) && !raw.result.is_empty()).then(|| String::from_utf16_lossy(&raw.result));
    let comp = if has(GCS_COMPSTR) { composing(&raw) } else { Composing::default() };
    (result, comp)
}

/// IMM32's composition as Slate keeps it: UTF-8, with byte offsets.
pub fn composing(r: &Raw) -> Composing {
    let text = String::from_utf16_lossy(&r.comp);
    let n = r.comp.len() as u32;
    let byte = |u: u32| String::from_utf16_lossy(&r.comp[..u.min(n) as usize]).len();
    // (the clauses the input method gives, else runs of one attribute)
    let mut starts: Vec<u32> = r.clauses.iter().map(|&c| c.min(n)).collect();
    if starts.len() < 2 {
        starts = vec![0];
        starts.extend((1..r.attrs.len().min(n as usize)).filter(|&i| r.attrs[i] != r.attrs[i - 1]).map(|i| i as u32));
        starts.push(n);
    }
    let clauses = starts
        .windows(2)
        .filter(|w| w[1] > w[0])
        .map(|w| {
            let a = r.attrs.get(w[0] as usize).copied().unwrap_or(0) as u32;
            (byte(w[0]), byte(w[1]), a == ATTR_TARGET_CONVERTED || a == ATTR_TARGET_NOTCONVERTED)
        })
        .collect();
    Composing { text, clauses, cursor: byte(r.cursor) }
}

/// Drops what the input method is composing (its text was typed already, or can't be: `App::ime_finish`, a read-only
/// tab).
pub fn cancel(hwnd: HWND) {
    if FAKE.with(|f| f.borrow_mut().take()).is_some() {
        return;
    }
    unsafe {
        let himc = ImmGetContext(hwnd);
        let _ = ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
        let _ = ImmReleaseContext(hwnd, himc);
    }
}

/// Tells the input method where the composition is (`x`, `y`: its row's top-left, client pixels; `h` the row's
/// height), so its candidate list goes under it and not over it; and the font, for an input method that shows the
/// composition itself after all.
pub fn place(hwnd: HWND, x: i32, y: i32, h: i32, font: &str) {
    unsafe {
        let himc = ImmGetContext(hwnd);
        let cf = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: POINT { x, y }, rcArea: RECT::default() };
        let _ = ImmSetCompositionWindow(himc, &cf);
        let mut lf = LOGFONTW { lfHeight: -((h as f32 * 0.8) as i32), ..Default::default() };
        for (i, c) in font.encode_utf16().take(31).enumerate() {
            lf.lfFaceName[i] = c;
        }
        let _ = ImmSetCompositionFontW(himc, &lf);
        let cand = CANDIDATEFORM {
            dwIndex: 0,
            dwStyle: CFS_EXCLUDE,
            ptCurrentPos: POINT { x, y: y + h },
            rcArea: RECT { left: x, top: y, right: x + 1, bottom: y + h },
        };
        let _ = ImmSetCandidateWindow(himc, &cand);
        let _ = ImmReleaseContext(hwnd, himc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imm32_compositions_become_byte_offsets() {
        // にほんご as two clauses, the first being converted, the cursor at the end
        let comp: Vec<u16> = "にほんご".encode_utf16().collect();
        let r = Raw { comp, attrs: vec![1, 1, 2, 2], clauses: vec![0, 2, 4], cursor: 4, result: Vec::new() };
        let c = composing(&r);
        assert_eq!(c.text, "にほんご");
        assert_eq!(c.clauses, vec![(0, 6, true), (6, 12, false)]);
        assert_eq!(c.cursor, 12);
        // without clauses: runs of one attribute; an emoji's two units count as one character's bytes
        let comp: Vec<u16> = "a😀b".encode_utf16().collect();
        let r = Raw { comp, attrs: vec![0, 3, 3, 0], clauses: Vec::new(), cursor: 3, result: Vec::new() };
        let c = composing(&r);
        assert_eq!(c.clauses, vec![(0, 1, false), (1, 5, true), (5, 6, false)]);
        assert_eq!(c.cursor, 5);
        // nothing at all
        assert_eq!(composing(&Raw::default()), Composing::default());
        // the test mode's form
        let c = Composing::parse("にほんご|2|2*,4");
        assert_eq!((c.clauses.clone(), c.cursor, c.anchor()), (vec![(0, 6, true), (6, 12, false)], 6, 0));
    }
}
