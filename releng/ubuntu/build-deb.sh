#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"

pkg_name="hintcontrol-gtk"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo_root/Cargo.toml" | head -n 1)"
arch="${DEB_ARCH:-amd64}"
out_dir="$repo_root/target/ubuntu"
stage_dir="$out_dir/${pkg_name}_${version}_${arch}"
deb_path="$out_dir/${pkg_name}_${version}_${arch}.deb"

if [[ -z "$version" ]]; then
  echo "Could not read package version from Cargo.toml" >&2
  exit 1
fi

require_command() {
  if ! command -v "$1" >/dev/null; then
    echo "Required command not found: $1" >&2
    exit 1
  fi
}

require_command cargo
require_command dpkg-deb

rm -rf "$stage_dir"
mkdir -p "$stage_dir/DEBIAN"

cd "$repo_root"
cargo build --release --locked

install -Dm755 "target/release/$pkg_name" "$stage_dir/usr/bin/$pkg_name"
install -Dm644 "dist/$pkg_name.desktop" "$stage_dir/usr/share/applications/$pkg_name.desktop"
install -Dm644 "LICENSE" "$stage_dir/usr/share/doc/$pkg_name/copyright"

installed_size="$(du -sk "$stage_dir/usr" | awk '{print $1}')"
sed \
  -e "s/@VERSION@/$version/g" \
  -e "s/@ARCH@/$arch/g" \
  -e "s/@INSTALLED_SIZE@/$installed_size/g" \
  "$script_dir/control.in" > "$stage_dir/DEBIAN/control"

dpkg-deb --build --root-owner-group "$stage_dir" "$deb_path"
echo "$deb_path"
