#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    std::process::exit(slate::ui::run(args));
    #[cfg(target_os = "linux")]
    std::process::exit(slate::gtk::run(args));
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = args;
        eprintln!("Slate runs on Windows and Linux.");
        std::process::exit(1);
    }
}
