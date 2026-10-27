//! The taskbar's jump list (right-click Slate's taskbar button, or its Start menu entry): the files opened or saved
//! last (Open recent's list), each opening in Slate. Kept up to date on another thread (COM's jump list takes a few
//! milliseconds), never in test mode. A file the user removed from the list isn't put back (Windows refuses the
//! whole list otherwise).

use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, PROPERTYKEY};
use windows::Win32::UI::Shell::{DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW, SHStrDupW, ShellLink};
use windows::core::{GUID, HSTRING, Interface, w};

/// The list to show next, and whether the thread writing it runs.
static NEXT: Mutex<(Option<Vec<PathBuf>>, bool)> = Mutex::new((None, false));

/// Shows `recent` (newest first) in the jump list's "Recent" category, soon.
pub fn update(recent: Vec<PathBuf>) {
    if super::win::testing() || super::settings::guest() {
        return;
    }
    let Ok(mut next) = NEXT.lock() else { return };
    next.0 = Some(recent);
    if next.1 {
        return;
    }
    next.1 = true;
    drop(next);
    let _ = std::thread::Builder::new().name("slate-jumplist".into()).spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        loop {
            let list = match NEXT.lock() {
                Ok(mut n) => match n.0.take() {
                    Some(l) => l,
                    None => {
                        n.1 = false;
                        break;
                    }
                },
                Err(_) => break,
            };
            let _ = write(&list);
        }
        CoUninitialize();
    });
}

/// The arguments that open `p` in Slate.
fn argument(p: &std::path::Path) -> String {
    format!("\"{}\"", p.display())
}

unsafe fn write(recent: &[PathBuf]) -> windows::core::Result<()> {
    let exe = std::env::current_exe().map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
    unsafe {
        let list: ICustomDestinationList = CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;
        let mut slots = 0u32;
        let removed: IObjectArray = list.BeginList(&mut slots)?;
        // (what the user took off the list: Windows refuses a list that has it again)
        let mut gone = Vec::new();
        for k in 0..removed.GetCount().unwrap_or(0) {
            if let Ok(link) = removed.GetAt::<IShellLinkW>(k) {
                let mut buf = [0u16; 1024];
                if link.GetArguments(&mut buf).is_ok() {
                    let n = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
                    gone.push(String::from_utf16_lossy(&buf[..n]));
                }
            }
        }
        let items: IObjectCollection = CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;
        let mut added = 0;
        for p in recent.iter().filter(|p| !gone.contains(&argument(p))).take(slots.clamp(5, 15) as usize) {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(exe.as_os_str()))?;
            link.SetArguments(&HSTRING::from(argument(p)))?;
            link.SetDescription(&HSTRING::from(p.as_os_str()))?;
            link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
            // its title: the file's name (System.Title, as a string the shell keeps)
            const PKEY_TITLE: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9), pid: 2 };
            const VT_LPWSTR: u16 = 31;
            let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
            let text = SHStrDupW(&HSTRING::from(name))?;
            let mut raw: windows_core::imp::PROPVARIANT = std::mem::zeroed();
            raw.Anonymous.Anonymous.vt = VT_LPWSTR;
            raw.Anonymous.Anonymous.Anonymous.pwszVal = text.0;
            // (dropped: PropVariantClear frees the string)
            let title = windows_core::PROPVARIANT::from_raw(raw);
            let store: IPropertyStore = link.cast()?;
            store.SetValue(&PKEY_TITLE, &title)?;
            store.Commit()?;
            items.AddObject(&link)?;
            added += 1;
        }
        if added > 0 {
            let array: IObjectArray = items.cast()?;
            list.AppendCategory(w!("Recent"), &array)?;
        }
        list.CommitList()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows takes the list as Slate makes it (the test program's own list, deleted again after).
    #[test]
    #[ignore]
    fn the_list_is_taken() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let r = write(&[PathBuf::from(r"C:\Windows\win.ini"), PathBuf::from(r"C:\some folder\notes 2.txt")]);
            let list: ICustomDestinationList = CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER).unwrap();
            let _ = list.DeleteList(None);
            r.unwrap();
        }
    }
}
