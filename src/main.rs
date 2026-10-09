#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    std::process::exit(slate::ui::run(args));
    #[cfg(not(windows))]
    std::process::exit(slate::gtk::run(args));
}
