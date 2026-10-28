#!/bin/sh
# Packages a Linux build of Slate: a .deb (Debian 12, Raspberry Pi OS 12, Ubuntu 23.04 and newer: GTK 4.8 or later)
# and a tarball of the same files, each with a .sha256 next to it. Their names have no version
# (slate-linux-arm64.deb), so .../releases/latest/download/slate-linux-<arch>.deb always gets the newest one.
#   packaging/linux/package.sh <binary> <version> <deb arch: amd64|arm64> <out dir>
set -eu
bin=$1
ver=$2
arch=$3
out=$4
case "$arch" in
    amd64) machine=x86_64 ;;
    arm64) machine=aarch64 ;;
    *) machine=$arch ;;
esac
here=$(cd "$(dirname "$0")/../.." && pwd)
root=$(mktemp -d)
# (mktemp makes it private: the package's / must be 755 like everyone's)
chmod 755 "$root"
id=io.github.jamesccupps.Slate

install -Dm755 "$bin" "$root/usr/bin/slate"
install -Dm644 "$here/packaging/linux/$id.desktop" "$root/usr/share/applications/$id.desktop"
for s in 16 24 32 48 64 128 256; do
    install -Dm644 "$here/res/icon/s$s.png" "$root/usr/share/icons/hicolor/${s}x${s}/apps/$id.png"
done
install -Dm644 "$here/res/icon/slate.svg" "$root/usr/share/icons/hicolor/scalable/apps/$id.svg"
install -Dm644 "$here/packaging/linux/slate-archive-keyring.gpg" "$root/usr/share/keyrings/slate-archive-keyring.gpg"
install -Dm644 "$here/THIRD-PARTY-NOTICES.md" "$root/usr/share/doc/slate/THIRD-PARTY-NOTICES.md"
# The license in Debian's machine-readable form (DEP-5): its text indented by one space, empty lines as " .".
{
    printf 'Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n'
    printf 'Upstream-Name: Slate\nUpstream-Contact: https://github.com/jamesccupps/Slate/issues\n'
    printf 'Source: https://github.com/jamesccupps/Slate\n'
    printf 'Comment: The program includes Rust libraries under their own licenses: THIRD-PARTY-NOTICES.md\n\n'
    printf 'Files: *\nCopyright: %s\nLicense: MIT\n\n' "$(tr -d '\r' < "$here/LICENSE" | sed -n 's/^Copyright (c) //p' | head -n 1)"
    printf 'License: MIT\n'
    tr -d '\r' < "$here/LICENSE" | sed -n '/^Permission is hereby granted/,$p' | sed 's/^$/./; s/^/ /'
} > "$root/usr/share/doc/slate/copyright"
chmod 644 "$root/usr/share/doc/slate/copyright"
# A changelog, as every package has (a native package's: changelog.gz); the release notes are on GitHub.
{
    printf 'slate (%s) stable; urgency=medium\n\n' "$ver"
    printf '  * Release notes: https://github.com/jamesccupps/Slate/releases\n\n'
    printf ' -- jamesccupps <148652101+jamesccupps@users.noreply.github.com>  %s\n' "$(date -R -u)"
} | gzip -9n > "$root/usr/share/doc/slate/changelog.gz"
chmod 644 "$root/usr/share/doc/slate/changelog.gz"

mkdir -p "$root/DEBIAN" "$out"
size=$(du -sk "$root/usr" | cut -f1)
cat > "$root/DEBIAN/control" <<EOF
Package: slate
Version: $ver
Architecture: $arch
Maintainer: jamesccupps <148652101+jamesccupps@users.noreply.github.com>
Installed-Size: $size
Depends: libgtk-4-1 (>= 4.8), libc6 (>= 2.36)
Recommends: libenchant-2-2, hunspell-en-us | hunspell-dictionary
Section: editors
Priority: optional
Homepage: https://github.com/jamesccupps/Slate
Description: fast, simple text editor that opens files of any size
 Slate opens huge files instantly (an 800 MB JSON file scrolls, searches and
 saves like a small one), with tabs and unsaved changes that come back after a
 restart, syntax colors for 52 languages, JSON and XML tools and line tools.
EOF
# Installing adds Slate's apt repository (signed with the key above), so apt upgrade updates Slate from then on;
# removing Slate removes it again. A slate.list that's there already is left as it is, and so is a system that has the
# repository another way (a slate.sources, say): a second entry would make apt warn at every update.
cat > "$root/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
list=/etc/apt/sources.list.d/slate.list
if [ "$1" = configure ] && [ ! -e "$list" ] && \
    ! grep -qs 'jamesccupps.github.io/Slate/apt' /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; then
    cat > "$list" <<'LIST'
# Slate's updates (added by the slate package, removed with it)
deb [signed-by=/usr/share/keyrings/slate-archive-keyring.gpg] https://jamesccupps.github.io/Slate/apt stable main
LIST
fi
EOF
cat > "$root/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
case "$1" in
    remove|purge) rm -f /etc/apt/sources.list.d/slate.list ;;
esac
EOF
chmod 755 "$root/DEBIAN/postinst" "$root/DEBIAN/postrm"
# (what dpkg --verify and debsums check the installed files against)
(cd "$root" && find usr -type f -exec md5sum {} + | sort -k 2) > "$root/DEBIAN/md5sums"
chmod 644 "$root/DEBIAN/md5sums"
deb="slate-linux-$arch.deb"
dpkg-deb --root-owner-group --build "$root" "$out/$deb"

# the same files as a tarball, to run without installing
tdir="slate-$ver-linux-$machine"
mkdir -p "$root/$tdir"
cp "$root/usr/bin/slate" "$root/$tdir/slate"
cp "$here/LICENSE" "$here/README.md" "$here/THIRD-PARTY-NOTICES.md" "$root/$tdir/"
tar=slate-linux-$arch.tar.gz
tar -C "$root" -czf "$out/$tar" "$tdir"

cd "$out"
for f in "$deb" "$tar"; do
    sha256sum "$f" > "$f.sha256"
    cat "$f.sha256"
done
rm -rf "$root"
