#!/bin/sh
# Makes Slate's apt repository from a release's .deb files: <out>/apt/{pool,dists/stable}, the Release file signed
# with the key in $APT_SIGNING_KEY (ASCII-armored, no passphrase), and the public key next to it. The Pages workflow
# (.github/workflows/apt.yml) publishes <out> at https://jamesccupps.github.io/Slate/ when a release is published.
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
    release dists/stable > Release
mv Release dists/stable/Release

GNUPGHOME=$(mktemp -d)
export GNUPGHOME
printf '%s\n' "$APT_SIGNING_KEY" | gpg --batch --quiet --import
gpg --batch --yes --armor --detach-sign -o dists/stable/Release.gpg dists/stable/Release
gpg --batch --yes --clearsign -o dists/stable/InRelease dists/stable/Release
# The repository must be signed by the key the package brings (users' apt checks it against that one).
gpg --export > signer.gpg
if ! cmp -s signer.gpg "$here/slate-archive-keyring.gpg"; then
    echo "APT_SIGNING_KEY isn't the key in packaging/linux/slate-archive-keyring.gpg" >&2
    exit 1
fi
mv signer.gpg slate-archive-keyring.gpg
gpgconf --kill gpg-agent
rm -rf "$GNUPGHOME"

# Pages' front page: the project
cat > "$out/index.html" <<'EOF'
<!doctype html>
<meta charset="utf-8">
<title>Slate</title>
<meta http-equiv="refresh" content="0; url=https://github.com/jamesccupps/Slate">
<p>Slate's apt repository is in <a href="apt/">apt/</a>. Slate is at
<a href="https://github.com/jamesccupps/Slate">github.com/jamesccupps/Slate</a>.</p>
EOF
find "$out" -type f | sort
