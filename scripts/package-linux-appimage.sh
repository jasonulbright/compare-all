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

repository="${GITHUB_REPOSITORY:-jasonulbright/compare-all}"
if [[ ! "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "GITHUB_REPOSITORY must have the form OWNER/NAME: $repository" >&2
  exit 2
fi
update_information="gh-releases-zsync|${repository%%/*}|${repository#*/}|latest|compare-all-*-x86_64.AppImage.zsync"
allow_missing_zsync="${COMPARE_ALL_ALLOW_MISSING_ZSYNC:-0}"

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
for tool in readelf dpkg-query; do
  if ! command -v "$tool" > /dev/null; then
    echo "required program is missing: $tool" >&2
    exit 1
  fi
done

# winit and glow load these with dlopen by soname, so the dynamic section of
# the binary does not list them.
runtime_libraries=(
  libX11.so.6 libX11-xcb.so.1 libxcb.so.1 libxkbcommon.so.0
  libxkbcommon-x11.so.0 libXcursor.so.1 libXi.so.6 libXrender.so.1
  libwayland-client.so.0 libwayland-egl.so.1 libEGL.so.1 libGL.so.1
)

# The AppImage excludelist: the host supplies these, and a bundled copy can
# conflict with the C library, graphics driver, or sound server of the host.
excluded_libraries=(
  ld-linux.so.2 ld-linux-x86-64.so.2 libanl.so.1 libBrokenLocale.so.1
  libcidn.so.1 libc.so.6 libdl.so.2 libm.so.6 libmvec.so.1 libnss_compat.so.2
  libnss_dns.so.2 libnss_files.so.2 libnss_hesiod.so.2 libnss_nisplus.so.2
  libnss_nis.so.2 libpthread.so.0 libresolv.so.2 librt.so.1 libthread_db.so.1
  libutil.so.1 libstdc++.so.6 libGL.so.1 libEGL.so.1 libGLdispatch.so.0
  libGLX.so.0 libOpenGL.so.0 libdrm.so.2 libglapi.so.0 libgbm.so.1 libxcb.so.1
  libX11.so.6 libX11-xcb.so.1 libwayland-client.so.0 libasound.so.2
  libfontconfig.so.1 libfreetype.so.6 libharfbuzz.so.0 libcom_err.so.2
  libexpat.so.1 libgcc_s.so.1 libgpg-error.so.0 libICE.so.6 libSM.so.6
  libusb-1.0.so.0 libuuid.so.1 libz.so.1 libjack.so.0 libpipewire-0.3.so.0
  libxcb-dri3.so.0 libxcb-dri2.so.0 libfribidi.so.0 libgmp.so.10
)

system_library_dirs=(
  /usr/lib/x86_64-linux-gnu /lib/x86_64-linux-gnu /usr/lib64 /lib64 /usr/lib /lib
)
extra_library_dirs=()
if [[ -n "${COMPARE_ALL_EXTRA_LIBRARY_DIRS:-}" ]]; then
  IFS=: read -r -a extra_library_dirs <<< "$COMPARE_ALL_EXTRA_LIBRARY_DIRS"
fi

is_excluded() {
  local name
  for name in "${excluded_libraries[@]}"; do
    if [[ "$name" == "$1" ]]; then
      return 0
    fi
  done
  return 1
}

needed_libraries() {
  readelf -d "$1" | sed -n 's/^.*(NEEDED).*\[\(.*\)\]$/\1/p'
}

is_x86_64_elf() {
  readelf -h "$1" 2> /dev/null | grep -q 'Machine:.*X86-64'
}

owner_package() {
  local candidate output line
  for candidate in "$1" "$(readlink -f "$1")" "/usr$1"; do
    if output="$(dpkg-query -S "$candidate" 2> /dev/null)"; then
      while IFS= read -r line; do
        if [[ "$line" != diversion* ]]; then
          printf '%s\n' "${line%%:*}"
          return 0
        fi
      done <<< "$output"
    fi
  done
  return 1
}

appimagetool="$(readlink -f "$appimagetool")"
runtime="$(readlink -f "$runtime")"
dist="${DIST_DIR:-$root/dist}"
mkdir -p "$dist"
dist="$(cd "$dist" && pwd)"
stage="$(mktemp -d "$dist/.compare-all-appimage.XXXXXXXX")"
trap 'rm -rf "$stage"' EXIT

appdir="$stage/CompareAll.AppDir"
licenses="$appdir/usr/share/doc/compare-all/licenses"
install -D -m 0755 "$binary_dir/compare-all" "$appdir/usr/bin/compare-all"
install -D -m 0755 "$binary_dir/ca" "$appdir/usr/bin/ca"
install -D -m 0644 "$root/assets/icon/compare-all-512.png" "$appdir/compare-all.png"
# An absolute link target does not resolve inside the mounted AppImage.
ln -s compare-all.png "$appdir/.DirIcon"
install -D -m 0644 "$root/assets/icon/compare-all-512.png" \
  "$appdir/usr/share/icons/hicolor/512x512/apps/compare-all.png"
install -D -m 0644 "$root/LICENSE" "$appdir/usr/share/doc/compare-all/LICENSE"
install -D -m 0644 "$root/THIRD-PARTY-NOTICES.md" "$appdir/usr/share/doc/compare-all/THIRD-PARTY-NOTICES.md"
install -D -m 0644 "$root/README.md" "$appdir/usr/share/doc/compare-all/README.md"

binary_needs="$(needed_libraries "$binary_dir/compare-all"; needed_libraries "$binary_dir/ca")"
# shellcheck disable=SC2206 # sonames contain no blanks or glob characters
queue=("${runtime_libraries[@]}" $binary_needs)
declare -A seen=()
index=0
while ((index < ${#queue[@]})); do
  soname="${queue[index]}"
  index=$((index + 1))
  if [[ -n "${seen[$soname]:-}" ]]; then
    continue
  fi
  seen[$soname]=1
  # The dependencies of an excluded library are the host's own.
  if is_excluded "$soname"; then
    continue
  fi

  path=""
  from_extra=0
  for dir in "${extra_library_dirs[@]}"; do
    if [[ -n "$dir" && -e "$dir/$soname" ]] && is_x86_64_elf "$dir/$soname"; then
      path="$dir/$soname"
      from_extra=1
      break
    fi
  done
  if [[ -z "$path" ]]; then
    for dir in "${system_library_dirs[@]}"; do
      if [[ -e "$dir/$soname" ]] && is_x86_64_elf "$dir/$soname"; then
        path="$dir/$soname"
        break
      fi
    done
  fi
  if [[ -z "$path" ]]; then
    echo "required library is missing: $soname" >&2
    echo "install the package that provides it, or name its folder in COMPARE_ALL_EXTRA_LIBRARY_DIRS" >&2
    exit 1
  fi
  install -D -m 0644 "$(readlink -f "$path")" "$appdir/usr/lib/$soname"

  if ((from_extra)); then
    echo "warning: $soname comes from $path; the AppImage holds no license file for it" >&2
  else
    if ! package="$(owner_package "$path")"; then
      echo "no installed package owns $path, so its license file cannot be found" >&2
      exit 1
    fi
    copyright="/usr/share/doc/$package/copyright"
    if [[ ! -f "$copyright" ]]; then
      echo "license file is missing: $copyright (for $soname)" >&2
      exit 1
    fi
    install -D -m 0644 "$copyright" "$licenses/$package.copyright"
    echo "bundled $soname from package $package" >&2
  fi

  library_needs="$(needed_libraries "$path")"
  # shellcheck disable=SC2206 # sonames contain no blanks or glob characters
  queue+=($library_needs)
done

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
LD_LIBRARY_PATH="$HERE/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export LD_LIBRARY_PATH
exec "$HERE/usr/bin/compare-all" "$@"
EOF
chmod 0755 "$appdir/AppRun"

name="compare-all-${version}-x86_64"
output="$dist/$name.AppImage"
staged_output="$stage/$name.AppImage"
# zsyncmake writes the .zsync file to the working folder.
(
  cd "$stage"
  APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 VERSION="$version" \
    "$appimagetool" --no-appstream --runtime-file "$runtime" \
    --updateinformation "$update_information" "$appdir" "$staged_output"
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
