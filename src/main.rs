#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    // (as they are: a file name doesn't have to be valid Unicode, and `args()` would end Slate over one)
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(windows)]
    std::process::exit(slate::ui::run(args.iter().map(|a| a.to_string_lossy().into_owned()).collect()));
    #[cfg(target_os = "linux")]
    std::process::exit(slate::gtk::run(args));
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = args;
        eprintln!("Slate runs on Windows and Linux.");
        std::process::exit(1);
    }
}
