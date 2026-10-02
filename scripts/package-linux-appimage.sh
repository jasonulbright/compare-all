#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 VERSION APPIMAGETOOL RUNTIME" >&2
  exit 2
fi

version="$1"
appimagetool="$2"
runtime="$3"
if [[ ! "$version" =~ ^[0-9]{4}\.[0-9]{2}\.[0-9]{2}\.[0-9]{4}$ ]]; then
  echo "version must have the form YYYY.MM.DD.NNNN" >&2
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
binary_dir="$target_dir/release"
for file in "$binary_dir/compare-all" "$binary_dir/ca" "$root/README.md" \
  "$root/CHANGELOG.md" "$root/LICENSE" "$root/THIRD-PARTY-NOTICES.md" \
  "$root/assets/icon/compare-all-512.png" "$appimagetool" "$runtime"; do
  if [[ ! -f "$file" ]]; then
    echo "required file is missing: $file" >&2
    exit 1
  fi
done

dist="${DIST_DIR:-$root/dist}"
mkdir -p "$dist"
stage="$(mktemp -d "$dist/.compare-all-appimage.XXXXXXXX")"
trap 'rm -rf "$stage"' EXIT

appdir="$stage/CompareAll.AppDir"
install -D -m 0755 "$binary_dir/compare-all" "$appdir/usr/bin/compare-all"
install -D -m 0755 "$binary_dir/ca" "$appdir/usr/bin/ca"
install -D -m 0644 "$root/assets/icon/compare-all-512.png" "$appdir/compare-all.png"
install -D -m 0644 "$root/assets/icon/compare-all-512.png" \
  "$appdir/usr/share/icons/hicolor/512x512/apps/compare-all.png"
install -D -m 0644 "$root/LICENSE" "$appdir/usr/share/doc/compare-all/LICENSE"
install -D -m 0644 "$root/THIRD-PARTY-NOTICES.md" "$appdir/usr/share/doc/compare-all/THIRD-PARTY-NOTICES.md"
install -D -m 0644 "$root/README.md" "$appdir/usr/share/doc/compare-all/README.md"

cat > "$appdir/compare-all.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Compare All
Comment=Compare and merge files and folders
Exec=compare-all
Icon=compare-all
Terminal=false
Categories=Utility;FileTools;
StartupNotify=true
EOF
install -D -m 0644 "$appdir/compare-all.desktop" \
  "$appdir/usr/share/applications/compare-all.desktop"
if command -v desktop-file-validate > /dev/null; then
  desktop-file-validate "$appdir/compare-all.desktop"
fi

cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
set -eu
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/compare-all" "$@"
EOF
chmod 0755 "$appdir/AppRun"

name="compare-all-${version}-linux-x86_64"
output="$dist/$name.AppImage"
staged_output="$stage/$name.AppImage"
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 VERSION="$version" \
  "$appimagetool" --no-appstream --runtime-file "$runtime" "$appdir" "$staged_output"
chmod 0755 "$staged_output"
mv -f "$staged_output" "$output"
printf '%s\n' "$output"
