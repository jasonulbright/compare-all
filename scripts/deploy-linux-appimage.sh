#!/usr/bin/env bash
# Makes an AppDir self-contained. Runs as root on Arch Linux, in practice in
# the pinned archlinux container that package-linux-appimage.sh starts.
#
#   deploy-linux-appimage.sh APPDIR TOOLS_DIR
#
# APPDIR holds bin/compare-all, bin/ca, the desktop file, the icons and the
# AppStream file. TOOLS_DIR holds the files of fetch-appimage-tools.sh.
# quick-sharun.sh copies the C library, the dynamic loader, every library the
# two programs name or open at run time, Mesa with its software renderer,
# and the X keyboard data from the Arch packages into APPDIR, and makes
# sharun the AppRun. sharun starts each program on the bundled loader with a
# library path inside APPDIR. The Arch package versions are those of the day
# of the build; usr/share/doc/compare-all/bundled-packages.tsv records them.
set -euo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

[[ $# -eq 2 ]] || die "usage: $0 APPDIR TOOLS_DIR"
appdir="$(cd "$1" && pwd)"
tools="$(cd "$2" && pwd)"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[[ -f /etc/arch-release ]] || die "the deployment copies Arch Linux packages and runs only on Arch Linux"
[[ "$(id -u)" == 0 ]] || die "the deployment installs packages and runs as root"
bash "$here/fetch-appimage-tools.sh" --verify "$tools"
for pin in "SHARUN_SHA=$(sha256sum < "$tools/sharun+helper-libs-x86_64.tar" | cut -d ' ' -f 1)" \
  "CROSS_LIBC_DLOPEN_TAR_SHA=$(sha256sum < "$tools/cross-libc-dlopen-x86_64.tar" | cut -d ' ' -f 1)"; do
  grep -qx "[[:space:]]*$pin" "$tools/quick-sharun.sh" || die "quick-sharun.sh does not pin ${pin%%=*} to the fetched tarball"
done
for program in compare-all ca; do
  [[ -f "$appdir/bin/$program" ]] || die "$appdir/bin/$program is missing"
done

# winit, glutin and glow open these with dlopen by soname, so the dynamic
# sections of the two programs do not list them. cross-libc-dlopen.so opens
# libanl.so.1 and libcrypt.so.1 at every start and takes the host's copy
# when the AppDir has none.
runtime_packages=(
  glibc gcc-libs libxcrypt-compat libx11 libxcb libxkbcommon libxkbcommon-x11
  libxcursor libxi libxrender libxext libxfixes wayland libglvnd mesa
)
runtime_libraries=(
  libX11.so.6 libX11-xcb.so.1 libxcb.so.1 libxkbcommon.so.0
  libxkbcommon-x11.so.0 libXcursor.so.1 libXi.so.6 libXrender.so.1
  libwayland-client.so.0 libwayland-cursor.so.0 libwayland-egl.so.1
  libEGL.so.1 libGL.so.1 libanl.so.1 libcrypt.so.1
)
build_packages=(
  patchelf strace xorg-server-xvfb xorg-xauth binutils diffutils findutils file licenses
)

# The archlinux image skips locale, X11 locale and documentation files
# (NoExtract). The X11 compose tables are part of the deployment, so every
# package that misses files is installed again with every file.
packages_missing_files() {
  (pacman -Qkq 2> /dev/null || true) | cut -d ' ' -f 1 | sort -u
}
sed -i 's/^NoExtract/#NoExtract/' /etc/pacman.conf
pacman -Syu --noconfirm --needed "${runtime_packages[@]}" "${build_packages[@]}" > /dev/null
mapfile -t incomplete < <(packages_missing_files)
if ((${#incomplete[@]} > 0)); then
  pacman -S --noconfirm "${incomplete[@]}" > /dev/null
fi
left="$(packages_missing_files | tr '\n' ' ')"
[[ -z "$left" ]] || die "these packages still miss files: $left"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
originals="$work/originals"
mkdir -p "$originals" "$work/tmp" "$work/home"
cp -p "$appdir/bin/compare-all" "$appdir/bin/ca" "$originals/"
cp "$tools/sharun+helper-libs-x86_64.tar" "$tools/cross-libc-dlopen-x86_64.tar" "$work/tmp/"
install -m 0755 "$here/../assets/linux/AppRun.sh" "$appdir/AppRun.sh"

library_paths=()
for soname in "${runtime_libraries[@]}"; do
  [[ -e "/usr/lib/$soname" ]] || die "/usr/lib/$soname is missing after the package installation"
  library_paths+=("/usr/lib/$soname")
done

# STRACE_MODE runs compare-all on Xvfb with LD_DEBUG=libs and adds every
# library it opens. quick-sharun reports a step it skips and still exits 0,
# so its output is kept and searched.
status=0
(
  cd "$work"
  env APPDIR="$appdir" TMPDIR="$work/tmp" HOME="$work/home" OUTPATH="$work" \
    MAIN_BIN=compare-all STRACE_BINARY=compare-all STRACE_MODE=1 \
    NO_STRIP=1 DEPLOY_OPENGL=1 DEPLOY_DATADIR=0 DEPLOY_LOCALE=0 \
    timeout 1800 sh "$tools/quick-sharun.sh" \
    "$appdir/bin/compare-all" "$appdir/bin/ca" "${library_paths[@]}"
) > "$work/quick-sharun.log" 2>&1 || status=$?
cat "$work/quick-sharun.log"
((status == 0)) || die "quick-sharun.sh stopped with status $status"
if grep -E 'ERROR|Failed to add|cannot overwrite|No such file' "$work/quick-sharun.log"; then
  die "quick-sharun.sh reported a step that it did not complete"
fi
cmp -s "$here/../assets/linux/AppRun.sh" "$appdir/AppRun.sh" || die "quick-sharun.sh replaced AppRun.sh"

# quick-sharun.sh rewrites /usr/share and /usr/lib strings in the programs.
# The programs read the cursor theme, icon and time zone folders of the host
# through these strings, so the original files go back in place.
for program in compare-all ca; do
  install -m 0755 "$originals/$program" "$appdir/shared/bin/$program"
  cmp "$originals/$program" "$appdir/shared/bin/$program" || die "shared/bin/$program differs from the built program"
done

# The path-mapping hook links /tmp/<random name> to the AppDir at each start
# for files whose /usr strings were rewritten. With the programs restored, a
# bundled file that still holds such a name needs the hook and AppRun.lib,
# which AppRun.sh does not run.
hook="$appdir/bin/01-path-mapping-hardcoded.hook"
if [[ -f "$hook" ]]; then
  for variable in _tmp_bin _tmp_lib _tmp_share; do
    mapped="$(sed -n "s/^$variable=//p" "$hook" | tr -d '"')"
    [[ -n "$mapped" ]] || continue
    if grep -rlaF --exclude="${hook##*/}" "/tmp/$mapped" "$appdir"; then
      die "the files above hold the rewritten path /tmp/$mapped"
    fi
  done
  rm -f "$hook"
fi
rm -f "$appdir/AppRun.lib"

# anylinux.so removes the image's library and driver variables from the
# environment of a host program only while APPDIR is set. sharun sets APPDIR
# only when it runs as AppRun, so bin/ca and bin/compare-all started directly
# take it from .env, with the value that AppRun gives it.
# shellcheck disable=SC2016 # sharun expands ${SHARUN_DIR} when it reads .env
grep -qx 'APPDIR=${SHARUN_DIR}' "$appdir/.env" 2> /dev/null ||
  printf '%s\n' 'APPDIR=${SHARUN_DIR}' >> "$appdir/.env"
set -- "$appdir"/bin/*.hook
[[ ! -e "$1" ]] || die "quick-sharun.sh added start hooks that AppRun.sh does not run: $*"

# Every file under lib/, share/ and etc/ comes from an Arch package, except
# sharun's own files.
doc="$appdir/usr/share/doc/compare-all"
licenses="$doc/licenses"
mkdir -p "$licenses/spdx"
: > "$work/owners"
while IFS= read -r -d '' file; do
  relative="${file#"$appdir"/}"
  case "$relative" in
    lib/lib.path | lib/sharun-preload/*) continue ;;
    lib/*) source="/usr/$relative" ;;
    share/*) source="/usr/$relative" ;;
    etc/*) source="/$relative" ;;
    *) continue ;;
  esac
  owner="$(pacman -Qqo "$source" 2> /dev/null || true)"
  # localedef builds the compiled locales from the locale sources of glibc.
  if [[ -z "$owner" && "$relative" == lib/locale/* ]]; then
    owner=glibc
  fi
  [[ -n "$owner" ]] || die "$relative has no owning package ($source)"
  printf '%s\t%s\n' "$relative" "$owner" >> "$work/owners"
done < <(find "$appdir/lib" "$appdir/share" "$appdir/etc" \( -type f -o -type l \) -print0 2> /dev/null)

{
  printf 'file\tpackage\n'
  sort "$work/owners"
} > "$doc/bundled-files.tsv"
tarball_sum() {
  sha256sum < "$1" | cut -d ' ' -f 1
}
{
  printf 'name\tversion\tsource\tlicenses\tsha256\n'
  cut -f 2 "$work/owners" | sort -u > "$work/packages"
  while IFS= read -r package; do
    version="$(pacman -Q "$package" | cut -d ' ' -f 2)"
    # The package file, by its SHA-256, identifies the input exactly; the
    # Arch Linux Archive keeps every released file under its name.
    package_file="/var/cache/pacman/pkg/$(pacman -Sp --print-format '%f' "$package")"
    [[ -f "$package_file" ]] || pacman -Sw --noconfirm "$package" > /dev/null
    [[ -f "$package_file" && "$package_file" == *"-$version-"* ]] ||
      die "no package file of $package $version in the pacman cache"
    package_sum="$(tarball_sum "$package_file")"
    terms="$(LC_ALL=C pacman -Qi "$package" | sed -n 's/^Licenses *: *//p')"
    # The package names only "LGPL"; the libxcrypt sources state LGPL 2.1 or
    # later.
    if [[ "$package" == libxcrypt-compat && "$terms" == LGPL ]]; then
      terms=LGPL-2.1-or-later
    fi
    own=0
    if [[ -d "/usr/share/licenses/$package" ]]; then
      cp -RL "/usr/share/licenses/$package" "$licenses/$package"
      own=1
    fi
    # A standard SPDX term has its text in the licenses package; a
    # LicenseRef- or custom term has it only in the package's own folder.
    for term in $(tr '()' '  ' <<< "$terms"); do
      case "$term" in AND | OR | WITH | and | or | with) continue ;; esac
      if [[ -f "/usr/share/licenses/spdx/$term.txt" ]]; then
        cp "/usr/share/licenses/spdx/$term.txt" "$licenses/spdx/$term.txt"
      elif [[ -f "/usr/share/licenses/spdx/exceptions/$term.txt" ]]; then
        cp "/usr/share/licenses/spdx/exceptions/$term.txt" "$licenses/spdx/$term.txt"
      elif ((!own)); then
        die "no license text found for $term of $package"
      fi
    done
    printf '%s\t%s\tArch Linux package %s\t%s\t%s\n' "$package" "$version" "${package_file##*/}" "$terms" "$package_sum"
  done < "$work/packages"
  printf 'sharun\t3.5.0\tpkgforge-dev/Anylinux-sharun release\tMIT\t%s\n' \
    "$(tarball_sum "$tools/sharun+helper-libs-x86_64.tar")"
  printf 'cross-libc-dlopen\tv0.2.7\tpkgforge-dev/cross-libc-dlopen release\tMIT\t%s\n' \
    "$(tarball_sum "$tools/cross-libc-dlopen-x86_64.tar")"
  printf 'type2-runtime\t20251108\tAppImage/type2-runtime release (the ELF part of the AppImage file)\tMIT\t%s\n' \
    "$(tarball_sum "$tools/runtime-x86_64")"
} > "$doc/bundled-packages.tsv"

# anylinux.so in the sharun tarball contains code from
# linuxdeploy-plugin-checkrt.
install -D -m 0644 "$tools/LICENSE-sharun.txt" "$licenses/sharun/LICENSE"
install -D -m 0644 "$tools/LICENSE-linuxdeploy-plugin-checkrt.txt" "$licenses/sharun/LICENSE-linuxdeploy-plugin-checkrt"
tar -xOf "$tools/cross-libc-dlopen-x86_64.tar" LICENSE > "$work/LICENSE-cross-libc-dlopen"
install -D -m 0644 "$work/LICENSE-cross-libc-dlopen" "$licenses/cross-libc-dlopen/LICENSE"
install -D -m 0644 "$tools/LICENSE-type2-runtime.txt" "$licenses/type2-runtime/LICENSE"

if [[ -n "${HOST_UID:-}" && -n "${HOST_GID:-}" ]]; then
  chown -R "$HOST_UID:$HOST_GID" "$appdir"
fi
echo "Deployed $(wc -l < "$work/packages") Arch packages into $appdir"
