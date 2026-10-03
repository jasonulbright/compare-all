#!/usr/bin/env bash
# Builds the self-contained AppImage from the release binaries.
#
#   package-linux-appimage.sh VERSION TOOLS_DIR
#
# TOOLS_DIR holds the files of fetch-appimage-tools.sh. The script stages an
# AppDir, runs deploy-linux-appimage.sh in the pinned archlinux container
# (Docker), and packs the AppDir as SquashFS with the pinned appimagetool and
# static type2 runtime. The binaries stay byte for byte as built.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 VERSION TOOLS_DIR" >&2
  exit 2
fi

version="$1"
tools="$2"
if [[ ! "$version" =~ ^[0-9]{4}\.[0-9]{2}\.[0-9]{2}\.[0-9]{4}$ ]]; then
  echo "version must have the form YYYY.MM.DD.NNNN" >&2
  exit 2
fi

repository="${GITHUB_REPOSITORY:-jasonulbright/compare-all}"
if [[ ! "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "GITHUB_REPOSITORY must have the form OWNER/NAME: $repository" >&2
  exit 2
fi
update_information="gh-releases-zsync|${repository%%/*}|${repository#*/}|latest|compare-all-*-x86_64.AppImage.zsync"
allow_missing_zsync="${COMPARE_ALL_ALLOW_MISSING_ZSYNC:-0}"

# The image is pinned by digest; its packages are upgraded at deployment.
arch_image="archlinux@sha256:b21322c663be387c0ed9cbc7bbbfe18e41633ad4e7b7c77cfad45f128be20040"
appstream_id="io.github.jasonulbright.compare_all"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
binary_dir="$target_dir/release"
for file in "$binary_dir/compare-all" "$binary_dir/ca" "$root/README.md" \
  "$root/LICENSE" "$root/THIRD-PARTY-NOTICES.md" "$root/assets/icon/compare-all-512.png" \
  "$root/assets/linux/AppRun.sh" "$root/assets/linux/$appstream_id.appdata.xml"; do
  if [[ ! -f "$file" ]]; then
    echo "required file is missing: $file" >&2
    exit 1
  fi
done
if ! command -v docker > /dev/null; then
  echo "required program is missing: docker" >&2
  exit 1
fi
mkdir -p "$tools"
tools="$(cd "$tools" && pwd)"
bash "$root/scripts/fetch-appimage-tools.sh" --verify "$tools"

dist="${DIST_DIR:-$root/dist}"
mkdir -p "$dist"
dist="$(cd "$dist" && pwd)"
stage="$(mktemp -d "$dist/.compare-all-appimage.XXXXXXXX")"
cleanup() {
  # The container writes as root; it hands the AppDir back before it exits,
  # but a failed deployment can leave root-owned files.
  rm -rf "$stage" 2> /dev/null ||
    docker run --rm --mount "type=bind,source=$stage,target=/stage" "$arch_image" \
      rm -rf /stage/AppDir > /dev/null 2>&1 || true
  rm -rf "$stage" 2> /dev/null || true
}
trap cleanup EXIT

appdir="$stage/AppDir"
doc="$appdir/usr/share/doc/compare-all"
install -D -m 0755 "$binary_dir/compare-all" "$appdir/bin/compare-all"
install -D -m 0755 "$binary_dir/ca" "$appdir/bin/ca"
install -D -m 0644 "$root/assets/icon/compare-all-512.png" "$appdir/compare-all.png"
# An absolute link target does not resolve inside the mounted AppImage.
ln -s compare-all.png "$appdir/.DirIcon"
install -D -m 0644 "$root/assets/icon/compare-all-512.png" \
  "$appdir/usr/share/icons/hicolor/512x512/apps/compare-all.png"
install -D -m 0644 "$root/assets/linux/$appstream_id.appdata.xml" \
  "$appdir/usr/share/metainfo/$appstream_id.appdata.xml"
install -D -m 0644 "$root/LICENSE" "$doc/LICENSE"
install -D -m 0644 "$root/THIRD-PARTY-NOTICES.md" "$doc/THIRD-PARTY-NOTICES.md"
install -D -m 0644 "$root/README.md" "$doc/README.md"

cat > "$appdir/compare-all.desktop" << 'EOF'
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

# --mount instead of -v: -v splits its argument on ':'.
docker run --rm \
  --env HOST_UID="$(id -u)" --env HOST_GID="$(id -g)" \
  --mount "type=bind,source=$root/scripts,target=/src/scripts,readonly" \
  --mount "type=bind,source=$root/assets/linux,target=/src/assets/linux,readonly" \
  --mount "type=bind,source=$tools,target=/tools,readonly" \
  --mount "type=bind,source=$stage,target=/stage" \
  "$arch_image" bash /src/scripts/deploy-linux-appimage.sh /stage/AppDir /tools

for program in compare-all ca; do
  if ! cmp "$binary_dir/$program" "$appdir/shared/bin/$program"; then
    echo "shared/bin/$program differs from $binary_dir/$program" >&2
    exit 1
  fi
done

name="compare-all-${version}-x86_64"
output="$dist/$name.AppImage"
staged_output="$stage/$name.AppImage"
# zsyncmake writes the .zsync file to the working folder.
(
  cd "$stage"
  APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 VERSION="$version" \
    "$tools/appimagetool-x86_64.AppImage" --no-appstream --runtime-file "$tools/runtime-x86_64" \
    --comp zstd --updateinformation "$update_information" "$appdir" "$staged_output"
)
chmod 0755 "$staged_output"
if [[ -f "$staged_output.zsync" ]]; then
  mv -f "$staged_output.zsync" "$output.zsync"
  mv -f "$staged_output" "$output"
  printf '%s\n' "$output" "$output.zsync"
elif [[ "$allow_missing_zsync" == 1 ]]; then
  rm -f "$output.zsync"
  mv -f "$staged_output" "$output"
  echo "warning: appimagetool wrote no $name.AppImage.zsync" >&2
  printf '%s\n' "$output"
else
  echo "appimagetool wrote no $name.AppImage.zsync: install zsync, or set COMPARE_ALL_ALLOW_MISSING_ZSYNC=1" >&2
  exit 1
fi
