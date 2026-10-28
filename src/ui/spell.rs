//! Windows' own spell checker (ISpellChecker, the one Windows' apps use) for `crate::spell`, in the language Windows
//! is set to (else English). Add to dictionary goes into the user's dictionary of that language, which Windows keeps
//! for every program.

use windows::Win32::Globalization::{GetUserDefaultLocaleName, ISpellChecker, ISpellCheckerFactory, SpellCheckerFactory};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::core::{HSTRING, PWSTR};

use crate::spell::{Checker, Speller};

pub struct WinChecker {
    checker: ISpellChecker,
    /// Test mode: Add to dictionary only ignores (the user's dictionary is never written).
    scripted: bool,
}

/// A speller in Windows' language (or the first of its region's languages Windows has a dictionary for, else
/// English); None when Windows has no spell checker or no dictionary.
pub fn speller(scripted: bool) -> Option<Speller> {
    unsafe {
        let f: ISpellCheckerFactory = CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut buf = [0u16; 85];
        let n = GetUserDefaultLocaleName(&mut buf);
        let user = String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]);
        let lang = user.split('-').next().unwrap_or_default().to_string();
        for tag in [user.clone(), lang, "en-US".into()] {
            if tag.is_empty() || !f.IsSupported(&HSTRING::from(&tag)).is_ok_and(|b| b.as_bool()) {
                continue;
            }
            if let Ok(checker) = f.CreateSpellChecker(&HSTRING::from(&tag)) {
                return Some(Speller::new(Box::new(WinChecker { checker, scripted }), tag));
            }
        }
        None
    }
}

impl Checker for WinChecker {
    fn check(&self, word: &str) -> bool {
        unsafe {
            let Ok(errors) = self.checker.Check(&HSTRING::from(word)) else { return true };
            let mut e = None;
            // (S_OK with an error; S_FALSE: none)
            errors.Next(&mut e) != windows::Win32::Foundation::S_OK || e.is_none()
        }
    }

    fn suggest(&self, word: &str) -> Vec<String> {
        let mut out = Vec::new();
        unsafe {
            let Ok(list) = self.checker.Suggest(&HSTRING::from(word)) else { return out };
            loop {
                let mut s = [PWSTR::null()];
                let mut n = 0u32;
                if list.Next(&mut s, Some(&mut n)).is_err() || n == 0 || s[0].is_null() {
                    break;
                }
                out.push(s[0].to_string().unwrap_or_default());
                CoTaskMemFree(Some(s[0].0 as *const _));
                if out.len() >= crate::spell::SUGGEST_MAX {
                    break;
                }
            }
        }
        out
    }

    fn add(&self, word: &str) {
        unsafe {
            let _ = if self.scripted { self.checker.Ignore(&HSTRING::from(word)) } else { self.checker.Add(&HSTRING::from(word)) };
        }
    }

    fn ignore(&self, word: &str) {
        unsafe {
            let _ = self.checker.Ignore(&HSTRING::from(word));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How long a word takes to check, and that the checker says what's expected (on this PC's dictionary).
    #[test]
    #[ignore]
    fn spell_speed() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let t = std::time::Instant::now();
        let Some(mut s) = speller(true) else {
            println!("no spell checker");
            return;
        };
        println!("made for {} in {:?}", s.lang, t.elapsed());
        assert!(s.wrong("teh") && !s.wrong("the"));
        println!("teh: {:?}", s.suggest("teh"));
        let words: Vec<String> = (0..2000).map(|i| format!("wurd{}", (b'a' + (i % 26) as u8) as char).repeat(1 + i / 26 % 3)).collect();
        let t = std::time::Instant::now();
        let mut wrong = 0;
        for w in &words {
            wrong += s.wrong(w) as usize;
        }
        println!("{} new words in {:?} ({} wrong); again: ", words.len(), t.elapsed(), wrong);
        let t = std::time::Instant::now();
        for w in &words {
            s.wrong(w);
        }
        println!("  {:?}", t.elapsed());
    }
}
