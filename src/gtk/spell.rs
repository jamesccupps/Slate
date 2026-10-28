//! Enchant (the spell checking library GNOME's apps use, with Hunspell's dictionaries) for `crate::spell`, loaded the
//! first time it's wanted: where it isn't installed, or has no dictionary for the language, spell check isn't offered
//! (the package recommends both). Add to dictionary goes into Enchant's personal word list for the language, which
//! other programs using Enchant share.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

use crate::spell::{Checker, Speller};

type Check = unsafe extern "C" fn(*mut c_void, *const c_char, isize) -> c_int;
type Suggest = unsafe extern "C" fn(*mut c_void, *const c_char, isize, *mut usize) -> *mut *mut c_char;
type FreeList = unsafe extern "C" fn(*mut c_void, *mut *mut c_char);
type Add = unsafe extern "C" fn(*mut c_void, *const c_char, isize);

pub struct Enchant {
    dict: *mut c_void,
    check: Check,
    suggest: Suggest,
    free_list: FreeList,
    add: Add,
    add_to_session: Add,
    /// Test mode: Add to dictionary only ignores (the user's word list is never written).
    scripted: bool,
}

/// A speller in the language the session is set to (`LC_ALL`, `LC_MESSAGES`, `LANG`: `de_DE.UTF-8`), else its
/// language without the region, else English; None without Enchant or a dictionary.
pub fn speller(scripted: bool) -> Option<Speller> {
    unsafe {
        let lib = libc::dlopen(c"libenchant-2.so.2".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if lib.is_null() {
            return None;
        }
        let sym = |name: &CStr| -> Option<*mut c_void> { Some(libc::dlsym(lib, name.as_ptr())).filter(|p| !p.is_null()) };
        let init: unsafe extern "C" fn() -> *mut c_void = std::mem::transmute(sym(c"enchant_broker_init")?);
        let exists: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int = std::mem::transmute(sym(c"enchant_broker_dict_exists")?);
        let request: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void = std::mem::transmute(sym(c"enchant_broker_request_dict")?);
        let e = |dict| -> Option<Enchant> {
            Some(Enchant {
                dict,
                check: std::mem::transmute::<*mut c_void, Check>(sym(c"enchant_dict_check")?),
                suggest: std::mem::transmute::<*mut c_void, Suggest>(sym(c"enchant_dict_suggest")?),
                free_list: std::mem::transmute::<*mut c_void, FreeList>(sym(c"enchant_dict_free_string_list")?),
                add: std::mem::transmute::<*mut c_void, Add>(sym(c"enchant_dict_add")?),
                add_to_session: std::mem::transmute::<*mut c_void, Add>(sym(c"enchant_dict_add_to_session")?),
                scripted,
            })
        };
        let broker = init();
        if broker.is_null() {
            return None;
        }
        // (the broker and the library stay for as long as Slate runs)
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().filter_map(|v| std::env::var(v).ok()).find(|v| !v.is_empty()).unwrap_or_default();
        let tag = locale.split(['.', '@']).next().unwrap_or_default().to_string();
        let lang = tag.split('_').next().unwrap_or_default().to_string();
        for t in [tag, lang, "en_US".into(), "en".into()] {
            let Ok(c) = CString::new(t.as_str()) else { continue };
            if t.is_empty() || t == "C" || t == "POSIX" || exists(broker, c.as_ptr()) == 0 {
                continue;
            }
            let dict = request(broker, c.as_ptr());
            if !dict.is_null() {
                return e(dict).map(|e| Speller::new(Box::new(e), t));
            }
        }
        None
    }
}

impl Checker for Enchant {
    fn check(&self, word: &str) -> bool {
        // (0: right; above 0: wrong; below: it couldn't tell, taken as right)
        unsafe { (self.check)(self.dict, word.as_ptr() as *const c_char, word.len() as isize) <= 0 }
    }

    fn suggest(&self, word: &str) -> Vec<String> {
        let mut out = Vec::new();
        unsafe {
            let mut n = 0usize;
            let list = (self.suggest)(self.dict, word.as_ptr() as *const c_char, word.len() as isize, &mut n);
            if list.is_null() {
                return out;
            }
            for i in 0..n {
                let s = *list.add(i);
                if !s.is_null() {
                    out.push(CStr::from_ptr(s).to_string_lossy().into_owned());
                }
            }
            (self.free_list)(self.dict, list);
        }
        out
    }

    fn add(&self, word: &str) {
        let f = if self.scripted { self.add_to_session } else { self.add };
        unsafe { f(self.dict, word.as_ptr() as *const c_char, word.len() as isize) }
    }

    fn ignore(&self, word: &str) {
        unsafe { (self.add_to_session)(self.dict, word.as_ptr() as *const c_char, word.len() as isize) }
    }
}
