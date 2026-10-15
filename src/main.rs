#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    // (as they are: a file name doesn't have to be valid Unicode, and `args()` would end Slate over one)
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
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

/// The start of IMAGE_LOAD_CONFIG_DIRECTORY64, through the fields Slate sets.
#[cfg(windows)]
#[repr(C)]
struct LoadConfig {
    size: u32,
    time_date_stamp: u32,
    major_version: u16,
    minor_version: u16,
    global_flags_clear: u32,
    global_flags_set: u32,
    critical_section_default_timeout: u32,
    decommit_free_block_threshold: u64,
    decommit_total_free_threshold: u64,
    lock_prefix_table: u64,
    maximum_allocation_size: u64,
    virtual_memory_threshold: u64,
    process_affinity_mask: u64,
    process_heap_flags: u32,
    csd_version: u16,
    dependent_load_flags: u16,
    edit_list: u64,
    security_cookie: u64,
    se_handler_table: u64,
    se_handler_count: u64,
}

/// The loader looks for the DLLs Slate.exe imports in System32 only, never first in the folder Slate.exe is in: the
/// first start is often from Downloads, where a web page may have got a `dwmapi.dll` saved (DependentLoadFlags =
/// LOAD_LIBRARY_SEARCH_SYSTEM32, Windows 10 1607 and later). GNU ld fills in the image's load configuration from this
/// symbol; `size` says how much of the structure there is.
#[cfg(windows)]
#[used]
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
static _load_config_used: LoadConfig = LoadConfig {
    size: size_of::<LoadConfig>() as u32,
    time_date_stamp: 0,
    major_version: 0,
    minor_version: 0,
    global_flags_clear: 0,
    global_flags_set: 0,
    critical_section_default_timeout: 0,
    decommit_free_block_threshold: 0,
    decommit_total_free_threshold: 0,
    lock_prefix_table: 0,
    maximum_allocation_size: 0,
    virtual_memory_threshold: 0,
    process_affinity_mask: 0,
    process_heap_flags: 0,
    csd_version: 0,
    // LOAD_LIBRARY_SEARCH_SYSTEM32
    dependent_load_flags: 0x800,
    edit_list: 0,
    security_cookie: 0,
    se_handler_table: 0,
    se_handler_count: 0,
};
