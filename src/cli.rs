//! The command line's own answers: `slate --version` and `slate --help` (both apps), and the switches each knows.

/// What `slate --version` prints.
pub fn version_text() -> String {
    format!("Slate {}\n", env!("CARGO_PKG_VERSION"))
}

/// What `slate --help` prints.
pub fn help_text() -> String {
    let mut s = format!(
        "Slate {}: a fast, simple text editor that opens files of any size.\n\
         \n\
         Usage: slate [switches] [--] [files]\n\
         \n\
         \x20 <file>:<line>[:<column>]  Open the file at that line (and column)\n\
         \x20 -                         Open the text piped in (standard input) in a new tab\n\
         \x20 --read-only               Open the files read only\n\
         \x20 --wait                    Return once the tabs opened for this are closed (git's core.editor)\n\
         \x20 --version                 Print Slate's version\n\
         \x20 --help                    Print this\n\
         \x20 --                        Only files after this, also names that start with -\n",
        env!("CARGO_PKG_VERSION")
    );
    if cfg!(windows) {
        s.push_str(
            "\n\
             \x20 --install                 Set Slate up for this user without asking (winget, scripts)\n\
             \x20 --unassociate             Stop opening files with Slate\n\
             \x20 --uninstall [--quiet]     Remove Slate\n",
        );
    }
    s.push_str("\nhttps://github.com/jamesccupps/Slate\n");
    s
}

/// The first argument (before a `--`) that looks like a switch but isn't one of `known`, or a lone `-`.
pub fn unknown_switch<'a>(args: &'a [String], known: &[&str]) -> Option<&'a str> {
    args.iter().take_while(|a| *a != "--").map(String::as_str).find(|a| a.starts_with('-') && *a != "-" && !known.contains(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_are_told_from_files() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let known = ["--read-only", "--wait"];
        assert_eq!(unknown_switch(&v(&["--wait", "notes.txt", "-"]), &known), None);
        assert_eq!(unknown_switch(&v(&["--bogus", "notes.txt"]), &known), Some("--bogus"));
        assert_eq!(unknown_switch(&v(&["-x"]), &known), Some("-x"));
        // after `--`, everything is a file
        assert_eq!(unknown_switch(&v(&["--", "--bogus"]), &known), None);
        assert!(help_text().contains("--read-only") && version_text().starts_with("Slate "));
    }
}
