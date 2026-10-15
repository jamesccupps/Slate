# Security

Slate updates itself (on Windows from this repository's releases; on Linux through its apt repository), so a security
problem in it reaches everyone who uses it. Please report one privately, not in a public issue.

## Reporting a problem

Use GitHub's private reporting: **[Report a vulnerability](https://github.com/jamesccupps/Slate/security/advisories/new)**
(the repository's Security tab). Only the maintainer sees it. Please include:

- what's affected, and what someone could do with it;
- how to make it happen (a small file that shows it helps);
- the Slate version (Help → About Slate; on Linux `apt policy slate`) and the system (Windows 10/11, Linux and which).

Once it's fixed, the release notes say what was fixed (and who found it, if you'd like that).

## What's covered

- `Slate.exe` and the Linux packages and tarballs from the [releases](https://github.com/jamesccupps/Slate/releases),
  built by this repository's GitHub Actions workflows;
- the updater (it checks each download's SHA-256 and that it's signed by the same publisher);
- the apt repository at `https://jamesccupps.github.io/Slate/apt` and its signing key;
- the website at `https://jamesccupps.github.io/Slate/`.

Only the latest release gets fixes: Slate offers its updates by itself on Windows, and `apt upgrade` brings them on
Linux.
