#!/bin/sh
# Makes Slate's apt repository from a release's .deb files: <out>/apt/{pool,dists/stable}, the Release file signed
# with the key in $APT_SIGNING_KEY (ASCII-armored, no passphrase), and the keyring next to it. The Pages workflow
# (.github/workflows/apt.yml) publishes <out> at https://jamesccupps.github.io/Slate/ (with the site's pages from
# site/) when a release is published, and again every month: the Release file is valid for a year from signing
# (Valid-Until), so apt never takes an old one for current, and each run signs it anew.
# Users' systems read it through /etc/apt/sources.list.d/slate.list, which the package itself adds:
#   deb [signed-by=/usr/share/keyrings/slate-archive-keyring.gpg] https://jamesccupps.github.io/Slate/apt stable main
#   packaging/linux/apt-repo.sh <dir with the .debs> <out dir>
set -eu
# (absolute: the script changes folders on the way)
debs=$(cd "$1" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
repo="$out/apt"
mkdir -p "$repo/pool/main"
for f in "$debs"/*.deb; do
    name=$(dpkg-deb -f "$f" Package)
    ver=$(dpkg-deb -f "$f" Version)
    arch=$(dpkg-deb -f "$f" Architecture)
    cp "$f" "$repo/pool/main/${name}_${ver}_${arch}.deb"
done

cd "$repo"
# armhf and i386 have no Slate, but systems that also take those (Raspberry Pi OS 64-bit takes armhf, PCs with
# i386 for Wine or Steam) look for them in every repository: empty lists keep apt from noting it each time.
for arch in amd64 arm64 armhf i386; do
    dir=dists/stable/main/binary-$arch
    mkdir -p "$dir"
    apt-ftparchive --arch "$arch" packages pool > "$dir/Packages"
    gzip -9kn "$dir/Packages"
done
apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=Slate \
    -o APT::FTPArchive::Release::Label=Slate \
    -o APT::FTPArchive::Release::Suite=stable \
    -o APT::FTPArchive::Release::Codename=stable \
    -o APT::FTPArchive::Release::Architectures="amd64 arm64 armhf i386" \
    -o APT::FTPArchive::Release::Components=main \
    -o APT::FTPArchive::Release::Description="Slate, a fast text editor for files of any size" \
    -o APT::FTPArchive::Release::ValidTime=31536000 \
    release dists/stable > Release
mv Release dists/stable/Release

GNUPGHOME=$(mktemp -d)
export GNUPGHOME
printf '%s\n' "$APT_SIGNING_KEY" | gpg --batch --quiet --import
# The repository must be signed by a key the package brings (users' apt checks it against those): the keyring holds
# the signing key and a backup kept offline, which can take over without every system installing a new key by hand.
signer=$(gpg --batch --with-colons --list-secret-keys | awk -F: '$1 == "fpr" { print $10; exit }')
if ! gpg --batch --with-colons --show-keys "$here/slate-archive-keyring.gpg" | grep -q "^fpr:.*:$signer:\$"; then
    echo "APT_SIGNING_KEY isn't one of the keys in packaging/linux/slate-archive-keyring.gpg" >&2
    exit 1
fi
gpg --batch --yes --armor --detach-sign -o dists/stable/Release.gpg dists/stable/Release
gpg --batch --yes --clearsign -o dists/stable/InRelease dists/stable/Release
cp "$here/slate-archive-keyring.gpg" slate-archive-keyring.gpg
gpgconf --kill gpg-agent
rm -rf "$GNUPGHOME"

find "$out" -type f | sort
