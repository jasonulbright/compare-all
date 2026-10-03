#!/usr/bin/env bash
# Checks a built self-contained AppImage the way the AppImage catalog test
# does, in clean Ubuntu 22.04 and 20.04 containers that have no network and
# no X, GL or xkb package beyond Xvfb and the test tools. The 22.04 container
# runs every check; the 20.04 container (glibc 2.31) repeats the start. Both
# check the environment of a host program that the application starts. Exit
# status 0 means that every check passed; each failed check prints one line
# that starts with FAIL:.
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 APPIMAGE [OUTPUT_DIR]" >&2
  exit 2
fi
if [[ ! -f "$1" ]]; then
  echo "AppImage not found: $1" >&2
  exit 2
fi
if ! command -v docker > /dev/null; then
  echo "docker is required" >&2
  exit 2
fi

appimage="$(readlink -f "$1")"
file_name="$(basename "$appimage")"
if [[ $# -eq 2 ]]; then
  mkdir -p "$2"
  output="$(cd "$2" && pwd)"
else
  output="$(mktemp -d "${TMPDIR:-/tmp}/verify-linux-appimage.XXXXXXXX")"
fi
echo "Output folder: $output"

failures=0
fail() {
  echo "FAIL: $*"
  failures=$((failures + 1))
}

if [[ "$file_name" != *.AppImage ]]; then
  fail "file name '$file_name' does not end in .AppImage; rename the release file"
fi
without_anylinux="$(sed -E 's/anylinux//Ig' <<< "$file_name")"
if [[ "${without_anylinux,,}" == *linux* ]]; then
  fail "file name '$file_name' contains 'linux'; remove 'linux' from the release file name"
fi
if [[ "$file_name" =~ ^([A-Za-z0-9][A-Za-z0-9._+-]*)-([0-9][A-Za-z0-9._+]*)-x86_64\.AppImage$ ]]; then
  stem="${BASH_REMATCH[1]}"
else
  fail "file name '$file_name' does not have the form <name>-<version>-x86_64.AppImage; rename the release file"
  stem="$(sed -E 's/\.appimage$//I; s/[-_.]?(x86[-_]64|amd64|linux)//Ig; s/[-_.]v?[0-9].*$//' <<< "$file_name")"
fi

# The AppImage catalog converts the AppStream file with this build of
# appstreamcli, which is older than the one in Ubuntu 22.04.
appstreamcli_url=https://github.com/AppImage/appimage.github.io/releases/download/deps/appstreamcli-x86_64.AppImage
appstreamcli_sum=0b567ce75945bea2ba047533290ab85fb62749b025ef8bfbf8726e8b28c3e35f
cache="${XDG_CACHE_HOME:-$HOME/.cache}/compare-all-appimage-check"
mkdir -p "$cache"
catalog_appstreamcli="$cache/appstreamcli-x86_64.AppImage"
if ! printf '%s  %s\n' "$appstreamcli_sum" "$catalog_appstreamcli" | sha256sum --check --status 2> /dev/null; then
  curl --fail --location --retry 3 --silent --show-error "$appstreamcli_url" --output "$catalog_appstreamcli.part"
  if ! printf '%s  %s\n' "$appstreamcli_sum" "$catalog_appstreamcli.part" | sha256sum --check --status; then
    rm -f "$catalog_appstreamcli.part"
    echo "SHA-256 sum of $appstreamcli_url is not $appstreamcli_sum" >&2
    exit 1
  fi
  mv -f "$catalog_appstreamcli.part" "$catalog_appstreamcli"
fi

# Xvfb, a window manager, window tools, screenshot and OCR tools, and the
# lint tools. --no-install-recommends keeps Mesa drivers and X client
# libraries that these do not need out of the container.
tools=(
  xvfb icewm x11-utils xdotool imagemagick tesseract-ocr tesseract-ocr-eng
  desktop-file-utils appstream file binutils
)
check_image() {
  local release="$1" image="compare-all-appimage-check:$1" dockerfile recipe built
  dockerfile="FROM ubuntu:$release
RUN apt-get update \\
  && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ${tools[*]} \\
  && rm -rf /var/lib/apt/lists/* \\
  && mkdir -p /tmp/.X11-unix \\
  && chmod 1777 /tmp/.X11-unix"
  recipe="$(printf '%s' "$dockerfile" | sha256sum | cut -d ' ' -f 1)"
  built="$(docker image inspect --format '{{ index .Config.Labels "compare-all.recipe" }}' "$image" 2> /dev/null || true)"
  if [[ "$built" != "$recipe" ]]; then
    echo "Building the check image $image" >&2
    printf '%s\n' "$dockerfile" | docker build --quiet --label "compare-all.recipe=$recipe" --tag "$image" - > /dev/null
  fi
  printf '%s\n' "$image"
}

IFS= read -r -d '' check_script << 'CHECK' || true
set -euo pipefail

MIN_RUN_SECONDS=10
WINDOW_TIMEOUT_SECONDS=30
FLAT_COLOR_PERCENT=90
MIN_TEXT_WORDS=5
ERROR_WORDS='error|errors|failed|failure|panic|panicked|cannot|could not|unable to|fatal|exception|traceback|segmentation fault|core dumped|not found|permission denied|no such file'
LOADER=ld-linux-x86-64.so.2
APPSTREAM_ID=io.github.jasonulbright.compare_all

failures=0
fail() {
  echo "FAIL: $*"
  failures=$((failures + 1))
}
finish() {
  echo "Failed checks in the $CHECK_HOST container: $failures"
  if ((failures > 0)); then
    exit 1
  fi
  exit 0
}
hex() {
  od -An -tx1 -j "$2" -N "$3" "$1" | tr -d ' \n'
}
is_elf() {
  [[ "$(hex "$1" 0 4 2> /dev/null)" == 7f454c46 ]]
}
is_dynamic() {
  readelf -lW "$1" 2> /dev/null | grep -q 'Requesting program interpreter' ||
    readelf -dW "$1" 2> /dev/null | grep -q '(NEEDED)'
}
normalize() {
  tr '[:upper:]' '[:lower:]' <<< "$1" | tr -cd '[:lower:][:digit:]'
}
names_agree() {
  [[ -z "$1" || -z "$2" || "$1" == "$2" ]] && return 0
  [[ ${#1} -ge 3 && "$2" == *"$1"* ]] && return 0
  [[ ${#2} -ge 3 && "$1" == *"$2"* ]]
}
desktop_value() {
  awk -v key="$1" '
    /^\[/ { section = $0; next }
    section == "[Desktop Entry]" && $0 ~ "^" key "[ \t]*=" {
      sub("^" key "[ \t]*=[ \t]*", ""); print; exit
    }' "$2"
}
desktop_key_count() {
  awk -v key="$1" '
    /^\[/ { section = $0; next }
    section == "[Desktop Entry]" && $0 ~ "^" key "[ \t]*=" { n++ }
    END { print n + 0 }' "$2"
}

app_pid=""
icewm_pid=""
xvfb_pid=""
stop_processes() {
  for pid in $app_pid $icewm_pid $xvfb_pid; do
    kill "$pid" 2> /dev/null || true
  done
}
trap stop_processes EXIT

work=/tmp/check
mkdir -p "$work" "$HOME"
target="$work/test.AppImage"
# A bind mount from a Windows drive can drop the execute bit, and the runtime
# must be executable to extract the payload.
cp "/input/$APPIMAGE_NAME" "$target"
chmod 0755 "$target"

if ! is_elf "$target"; then
  fail "the file is not an ELF executable; build it with appimagetool"
  finish
fi
magic="$(hex "$target" 8 3)"
if [[ "$magic" != 414902 ]]; then
  fail "the AppImage magic at byte 8 is '$magic', not 414902 (type 2); build it with a type 2 runtime"
fi
elf_header="$(readelf -hW "$target")"
section_offset="$(awk -F: '/Start of section headers/ { print $2 + 0 }' <<< "$elf_header")"
section_size="$(awk -F: '/Size of section headers/ { print $2 + 0 }' <<< "$elf_header")"
section_count="$(awk -F: '/Number of section headers/ { print $2 + 0 }' <<< "$elf_header")"
payload_offset=$((section_offset + section_size * section_count))
if [[ "$(hex "$target" "$payload_offset" 4)" != 68737173 ]]; then
  fail "the payload at byte $payload_offset is not a SquashFS image; build it with appimagetool and a SquashFS runtime"
fi

if ! (cd "$work" && ./test.AppImage --appimage-extract > "$work/extract.log" 2>&1) \
  || [[ ! -d "$work/squashfs-root" ]]; then
  fail "--appimage-extract did not produce an AppDir: $(tail -n 1 "$work/extract.log")"
  finish
fi
appdir="$work/squashfs-root"

if [[ "$CHECK_ALL" == 1 ]]; then
  if [[ ! -e "$appdir/AppRun" ]]; then
    fail "AppRun is missing at the AppDir root; add an executable AppRun"
  elif [[ ! -x "$appdir/AppRun" ]]; then
    fail "AppRun is not executable; set mode 0755 on AppRun before packaging"
  fi

  dir_icon="$appdir/.DirIcon"
  if [[ ! -e "$dir_icon" && ! -L "$dir_icon" ]]; then
    fail ".DirIcon is missing at the AppDir root; add it as a PNG copy of the icon or a relative symlink to it"
  else
    if [[ -L "$dir_icon" ]]; then
      link="$(readlink "$dir_icon")"
      resolved="$(readlink -m "$dir_icon")"
      if [[ "$link" == /* ]]; then
        fail ".DirIcon is a symlink to the absolute path '$link'; make the link relative"
      elif [[ "$resolved" != "$appdir"/* ]]; then
        fail ".DirIcon is a symlink to '$link', outside the AppDir; point it at an icon inside the AppDir"
      elif [[ ! -f "$resolved" ]]; then
        fail ".DirIcon is a symlink to '$link', which does not exist; point it at the icon file"
      fi
    fi
    if [[ -f "$dir_icon" && "$(file -bL --mime-type "$dir_icon")" != image/png ]]; then
      fail ".DirIcon is $(file -bL --mime-type "$dir_icon"), not a PNG; use a PNG icon"
    fi
  fi

  shopt -s nullglob
  desktops=("$appdir"/*.desktop)
  shopt -u nullglob
  desktop=""
  if ((${#desktops[@]} != 1)); then
    fail "the AppDir root holds ${#desktops[@]} .desktop files, not exactly one; keep one .desktop file at the root"
  else
    desktop="${desktops[0]}"
  fi

  if [[ -n "$desktop" ]]; then
    validate_status=0
    validate_output="$(desktop-file-validate "$desktop" 2>&1)" || validate_status=$?
    validate_problems="$(grep -E ': (error|warning): ' <<< "$validate_output" | head -n 3 | tr '\n' ' ' || true)"
    if ((validate_status != 0)) || [[ -n "$validate_problems" ]]; then
      fail "desktop-file-validate reports problems in $(basename "$desktop"): ${validate_problems:-status $validate_status}"
    fi
    for key in Icon Categories; do
      count="$(desktop_key_count "$key" "$desktop")"
      if [[ "$count" != 1 ]]; then
        fail "$key appears $count times in [Desktop Entry] of $(basename "$desktop"); set it exactly once"
      fi
    done
    if [[ "$(desktop_value Terminal "$desktop")" == true ]]; then
      fail "the desktop file sets Terminal=true; this check starts graphical applications only"
    fi

    icon_name="$(desktop_value Icon "$desktop")"
    icon_file=""
    if [[ -n "$icon_name" ]]; then
      icon_file="$(find "$appdir" -path '*/scalable/*' -name "$icon_name.svg*" -print -quit)"
      for size in 128x128 256x256 512x512; do
        [[ -n "$icon_file" ]] && break
        icon_file="$(find "$appdir" -path "*/$size/*" -name "$icon_name.png" -print -quit)"
      done
      for extension in svg svgz png xpm; do
        [[ -n "$icon_file" ]] && break
        icon_file="$(find "$appdir" -maxdepth 1 -name "$icon_name.$extension" -print -quit)"
      done
    fi
    if [[ -z "$icon_file" && -e "$dir_icon" ]]; then
      icon_file="$dir_icon"
    fi
    if [[ -z "$icon_file" ]]; then
      fail "no icon file matches Icon=$icon_name; add $icon_name.png at the AppDir root"
    else
      icon_type="$(file -bL "$icon_file")"
      if [[ "$icon_type" == PNG* ]]; then
        if [[ ! "$icon_type" =~ [0-9]+\ x\ [0-9]+ ]]; then
          fail "the size of the icon ${icon_file#"$appdir"/} cannot be read; use a valid PNG"
        fi
      elif [[ "$icon_file" != *.svg && "$icon_file" != *.svgz ]]; then
        fail "the icon ${icon_file#"$appdir"/} is '$icon_type', not a PNG; use a PNG icon"
      fi
    fi

    app_name="$(desktop_value Name "$desktop")"
    stem_key="$(normalize "$APP_STEM")"
    name_key="$(normalize "$app_name")"
    desktop_key="$(normalize "$(basename "$desktop" .desktop)")"
    if ! names_agree "$stem_key" "$name_key" || ! names_agree "$stem_key" "$desktop_key"; then
      fail "the names disagree: file name '$APP_STEM', desktop Name '$app_name', desktop file '$(basename "$desktop")'; use one application name"
    fi
  fi

  # The catalog lint reads only usr/share/metainfo/*appdata.xml and runs
  # appstreamcli validate-tree once it finds one.
  shopt -s nullglob
  appdata=("$appdir"/usr/share/metainfo/*appdata.xml)
  shopt -u nullglob
  if ((${#appdata[@]} == 0)); then
    fail "usr/share/metainfo holds no *appdata.xml file; add $APPSTREAM_ID.appdata.xml"
  else
    validate_status=0
    appstreamcli validate-tree --no-net "$appdir" > "$work/appstream.log" 2>&1 || validate_status=$?
    appstream_problems="$(grep -E '^ *[EW]: ' "$work/appstream.log" | head -n 3 | tr -s ' ' | tr '\n' ' ' || true)"
    if ((validate_status != 0)) || [[ -n "$appstream_problems" ]]; then
      fail "appstreamcli validate-tree reports problems: ${appstream_problems:-status $validate_status}"
    else
      echo "AppStream: $(tail -n 1 "$work/appstream.log")"
    fi
    converter="$work/catalog-appstreamcli"
    mkdir -p "$converter"
    cp /tools/appstreamcli-x86_64.AppImage "$converter/"
    chmod 0755 "$converter/appstreamcli-x86_64.AppImage"
    if ! (cd "$converter" && ./appstreamcli-x86_64.AppImage --appimage-extract > /dev/null 2>&1) \
      || ! APPDIR="$converter/squashfs-root" "$converter/squashfs-root/AppRun" \
        convert "${appdata[0]}" "$work/appdata.yaml" > "$work/convert.log" 2>&1 \
      || ! grep -q "^ID: $APPSTREAM_ID\$" "$work/appdata.yaml"; then
      fail "the catalog's appstreamcli cannot convert $(basename "${appdata[0]}"): $(tail -n 1 "$work/convert.log" 2> /dev/null)"
    fi
  fi

  update_info=""
  read -r update_offset update_size < <(readelf -SW "$target" \
    | sed -E 's/^ *\[ *[0-9]+\] *//' | awk '$1 == ".upd_info" { print $4, $5 }') || true
  if [[ -n "${update_offset:-}" && -n "${update_size:-}" ]]; then
    update_end=$((16#$update_offset + 16#$update_size))
    update_info="$(head -c "$update_end" "$target" | tail -c "$((16#$update_size))" | tr -d '\000' \
      | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//')"
  fi
  if [[ -z "$update_info" ]]; then
    fail "the AppImage has no update information; pass -u 'gh-releases-zsync|<owner>|<repo>|latest|<name>-*-x86_64.AppImage.zsync' to appimagetool"
  else
    echo "Update information: $update_info"
    IFS='|' read -r -a update_fields <<< "$update_info"
    if [[ "${update_fields[0]}" != gh-releases-zsync || ${#update_fields[@]} -ne 5 ]]; then
      fail "the update information '$update_info' is not of the form gh-releases-zsync|<owner>|<repo>|<tag>|<file pattern>"
    fi
  fi

  # The catalog rates an AppImage self-contained when the payload ships a
  # dynamic loader and a C library (or holds no dynamic ELF file at all) and
  # the runtime is static.
  elf_files=0
  dynamic_files=0
  while IFS= read -r -d '' file; do
    is_elf "$file" || continue
    elf_files=$((elf_files + 1))
    is_dynamic "$file" && dynamic_files=$((dynamic_files + 1))
  done < <(find "$appdir" -type f -print0)
  loader_found="$(find "$appdir" \( -name 'ld-linux*.so*' -o -name 'ld-2.*.so' -o -name 'ld-musl-*.so.1' \) -print -quit)"
  libc_found="$(find "$appdir" \( -name 'libc.so.6' -o -name 'libc.musl-*.so.1' -o -name 'ld-musl-*.so.1' \) -print -quit)"
  if ((elf_files > 0 && dynamic_files == 0)); then
    libc_mode=none
  elif [[ -n "$loader_found" && -n "$libc_found" ]]; then
    libc_mode=bundled
  else
    libc_mode=host
  fi
  if is_dynamic "$target"; then
    runtime_mode=dynamic
  else
    runtime_mode=static
  fi
  self_contained=false
  if [[ "$libc_mode" != host && "$runtime_mode" == static ]]; then
    self_contained=true
  fi
  echo "X-AppImage-Libc=$libc_mode X-AppImage-Runtime=$runtime_mode X-AppImage-Self-Contained=$self_contained"
  if [[ "$self_contained" != true ]]; then
    fail "the catalog rates the AppImage X-AppImage-Self-Contained=false (libc $libc_mode, runtime $runtime_mode); bundle the loader and libc.so.6 and use the static type2 runtime"
  fi

  # The files a start on a host without X, GL or xkb libraries needs.
  required=(
    AppRun AppRun.sh sharun lib/lib.path "lib/$LOADER" lib/libc.so.6 lib/libm.so.6
    lib/libgcc_s.so.1 lib/libanl.so.1 lib/libcrypt.so.1
    lib/libX11.so.6 lib/libX11-xcb.so.1 lib/libxcb.so.1 lib/libXcursor.so.1 lib/libXi.so.6
    lib/libXrender.so.1 lib/libxkbcommon.so.0 lib/libxkbcommon-x11.so.0
    lib/libwayland-client.so.0 lib/libwayland-cursor.so.0 lib/libwayland-egl.so.1
    lib/libGL.so.1 lib/libGLX.so.0 lib/libGLX_mesa.so.0 lib/libEGL.so.1 lib/libEGL_mesa.so.0
    lib/libGLdispatch.so.0 share/glvnd/egl_vendor.d/50_mesa.json share/X11/xkb/rules/evdev
    lib/sharun-preload/anylinux.so lib/sharun-preload/cross-libc-dlopen.so
    shared/bin/compare-all shared/bin/ca bin/compare-all bin/ca
    usr/share/doc/compare-all/LICENSE usr/share/doc/compare-all/THIRD-PARTY-NOTICES.md
    usr/share/doc/compare-all/bundled-packages.tsv usr/share/doc/compare-all/bundled-files.tsv
    usr/share/doc/compare-all/licenses/sharun/LICENSE
    usr/share/doc/compare-all/licenses/sharun/LICENSE-linuxdeploy-plugin-checkrt
    usr/share/doc/compare-all/licenses/cross-libc-dlopen/LICENSE
    "usr/share/metainfo/$APPSTREAM_ID.appdata.xml" usr/share/applications/compare-all.desktop
  )
  for path in "${required[@]}"; do
    [[ -e "$appdir/$path" ]] || fail "the AppDir lacks $path, which a start on a bare host needs"
  done
  shopt -s nullglob
  gallium=("$appdir"/lib/libgallium-*.so)
  hooks=("$appdir"/bin/*.hook)
  shopt -u nullglob
  ((${#gallium[@]} > 0)) || fail "the AppDir lacks lib/libgallium-*.so, the Mesa driver with the software renderer"
  ((${#hooks[@]} == 0)) || fail "the AppDir holds start hooks that AppRun.sh does not run: ${hooks[*]#"$appdir"/}"
  for link in AppRun bin/compare-all bin/ca; do
    cmp -s "$appdir/sharun" "$appdir/$link" || fail "$link is not sharun; sharun starts the programs on the bundled loader"
  done
  absolute="$(find "$appdir" -type l -lname '/*' -printf '%P ' | head -c 300)"
  [[ -z "$absolute" ]] || fail "symlinks with absolute targets: $absolute"
  dangling="$(find "$appdir" -xtype l -printf '%P ' | head -c 300)"
  [[ -z "$dangling" ]] || fail "dangling symlinks: $dangling"

  # Every bundled package has its license texts in the AppDir.
  manifest="$appdir/usr/share/doc/compare-all/bundled-packages.tsv"
  licenses="$appdir/usr/share/doc/compare-all/licenses"
  if [[ -f "$manifest" ]]; then
    for package in glibc mesa libx11 libxkbcommon; do
      grep -q "^$package	" "$manifest" || fail "the package manifest lists no $package"
    done
    while IFS=$'\t' read -r package version source terms; do
      [[ "$package" == name ]] && continue
      [[ -d "$licenses/$package" ]] && continue
      for term in $(tr '()' '  ' <<< "$terms"); do
        case "$term" in AND | OR | WITH | and | or | with) continue ;; esac
        [[ -f "$licenses/spdx/$term.txt" ]] || fail "no license text for $term of $package $version"
      done
    done < "$manifest"
    glibc_version="$(awk -F '\t' '$1 == "glibc" { print $2 }' "$manifest")"
    echo "Bundled packages: $(($(wc -l < "$manifest") - 1)); glibc $glibc_version; $(awk -F '\t' '$1 == "mesa" { print "mesa " $2 }' "$manifest")"
  fi

  # Every dynamic ELF file loads through the bundled loader on sharun's
  # library path, with every library found inside the AppDir.
  library_path="$appdir/lib"
  if [[ -f "$appdir/lib/lib.path" ]]; then
    while IFS= read -r entry; do
      case "$entry" in
        +/*) library_path="$library_path:$appdir/lib${entry#+}" ;;
      esac
    done < "$appdir/lib/lib.path"
  fi
  checked=0
  if [[ -x "$appdir/lib/$LOADER" ]]; then
    while IFS= read -r -d '' file; do
      is_elf "$file" || continue
      is_dynamic "$file" || continue
      [[ "$file" == "$appdir/lib/$LOADER" ]] && continue
      checked=$((checked + 1))
      listing="$("$appdir/lib/$LOADER" --inhibit-cache --library-path "$library_path" --list "$file" 2>&1 || true)"
      problems="$(awk -v root="$appdir/" '
        /=> not found/ { print $1; next }
        /=>/ && $3 ~ /^\// && index($3, root) != 1 { print $1 " from " $3; next }
        /not found|error while loading|cannot open/ { print }
      ' <<< "$listing" | sort -u | tr '\n' ' ')"
      [[ -z "$problems" ]] || fail "${file#"$appdir"/} does not load from the AppDir alone: $problems"
    done < <(find "$appdir" -type f -print0)
    echo "ELF files that load through the bundled loader with every library inside the AppDir: $checked"
  else
    fail "lib/$LOADER is missing or not executable, so no ELF file can load through the bundled loader"
  fi
fi

export DISPLAY=:99 LANG=C LC_ALL=C
unset WAYLAND_DISPLAY
mkdir -p "$HOME/.icewm" "$HOME/.local/share/appimagekit"
printf '%s\n' 'ShowTaskBar = 0' > "$HOME/.icewm/preferences"
touch "$HOME/.local/share/appimagekit/no_desktopintegration"
# --network none gives the container its own abstract socket namespace, so
# display :99 here never reaches a display of the host.
Xvfb :99 -screen 0 800x600x24 -nolisten tcp > "$work/xvfb.log" 2>&1 &
xvfb_pid=$!
display_ready=false
for _ in $(seq 1 20); do
  if xdpyinfo > /dev/null 2>&1; then
    display_ready=true
    break
  fi
  sleep 0.5
done
if [[ "$display_ready" != true ]]; then
  fail "Xvfb did not start in the container: $(tail -n 1 "$work/xvfb.log")"
  finish
fi

# The application runs `date +%z` through PATH once at start (the zone
# offset), and bin/ca does so for a script run. A `date` first on the host
# PATH records the environment that the host program receives. The
# environment of the application itself (/proc/PID/environ) does not show the
# variables that sharun sets after the start.
spy=/tmp/spy
mkdir -p "$spy"
cat > "$spy/date" << 'SPY'
#!/bin/sh
out="$(cat /tmp/spy/.outdir)"
mkdir -p "$out"
tr '\0' '\n' < /proc/$$/environ > "$out/date.$$.env"
printf '%s\n' "$0 $*" > "$out/date.$$.argv"
echo +0000
SPY
chmod 0755 "$spy/date"
host_path="$spy:$PATH"
# Variables that point a host program into the image, or that tell it about
# the image. Any other variable whose value names an image path also counts.
IMAGE_VARIABLES=(
  GCONV_PATH LIBGL_DRIVERS_PATH LIBVA_DRIVERS_PATH GBM_BACKENDS_PATH __EGL_VENDOR_LIBRARY_DIRS
  TERMINFO AMDGPU_ASIC_ID_TABLE_PATHS CROSS_LIBC_DLOPEN_ROOT SHARUN_DIR APPDIR APPIMAGE ARGV0
  OWD LD_LIBRARY_PATH LD_PRELOAD
)
# wait_child LABEL SECONDS: prints the environment file of the first date
# child that the entry point LABEL started.
wait_child() {
  local file="" waited=0
  while ((waited < $2)); do
    file="$(find "/output/child-env/$1" -name 'date.*.env' -print -quit 2> /dev/null)"
    [[ -n "$file" ]] && break
    sleep 1
    waited=$((waited + 1))
  done
  printf '%s\n' "$file"
}
# check_child LABEL ROOT...: the date child of entry point LABEL received no
# image variable; each ROOT is a path prefix of the image at this start.
check_child() {
  local label="$1" file found="" name value root first
  shift
  file="$(wait_child "$label" 20)"
  if [[ -z "$file" ]]; then
    fail "the $label start ran no 'date' through PATH within 20 seconds, so its child environment is unchecked"
    return
  fi
  if [[ -e "$appdir/bin/date" ]]; then
    fail "the AppDir holds bin/date, which comes before the host's date on the child PATH"
  fi
  for name in "${IMAGE_VARIABLES[@]}"; do
    if grep -q "^$name=" "$file"; then
      found="$found $name"
    fi
  done
  while IFS= read -r line; do
    name="${line%%=*}"
    value="${line#*=}"
    [[ " ${IMAGE_VARIABLES[*]} " == *" $name "* ]] && continue
    for root; do
      if [[ "$value" == *"$root"* ]]; then
        case "$name" in
          PATH)
            first="${value%%:*}"
            if [[ "$first" == "$root"* ]]; then
              found="$found PATH(starts with $first)"
            else
              found="$found PATH(holds $root)"
            fi
            ;;
          *) found="$found $name" ;;
        esac
        break
      fi
    done
  done < "$file"
  if [[ -n "$found" ]]; then
    fail "the host program 'date' started by the $label start receives image variables:$found"
  else
    echo "Child environment of the $label start: no image variable ($(grep -c . "$file") variables)"
  fi
}
# stop_image_processes PREFIX: ends every process whose executable lies under
# PREFIX.
stop_image_processes() {
  local entry signal
  for signal in TERM KILL; do
    for entry in /proc/[0-9]*; do
      if [[ "$(readlink "$entry/exe" 2> /dev/null)" == "$1"* ]]; then
        kill "-$signal" "${entry#/proc/}" 2> /dev/null || true
      fi
    done
    sleep 1
  done
}

echo /output/child-env/AppRun > "$spy/.outdir"
(cd "$work" && PATH="$host_path" APPIMAGE="/input/$APPIMAGE_NAME" APPDIR="$appdir" OWD="$work" \
  ARGV0="$APPIMAGE_NAME" exec "$appdir/AppRun") > /output/app.log 2>&1 &
app_pid=$!
sleep "$MIN_RUN_SECONDS"
waited=$MIN_RUN_SECONDS
window=""
while kill -0 "$app_pid" 2> /dev/null; do
  window="$(timeout 5 xdotool search --onlyvisible --name '.' 2> /dev/null | head -n 1 || true)"
  if [[ -n "$window" ]] || ((waited >= WINDOW_TIMEOUT_SECONDS)); then
    break
  fi
  sleep 1
  waited=$((waited + 1))
done

startup_error=""
if ! kill -0 "$app_pid" 2> /dev/null; then
  exit_status=0
  wait "$app_pid" || exit_status=$?
  app_pid=""
  startup_error="the application exited with status $exit_status within $waited seconds instead of showing a window"
elif [[ -z "$window" ]]; then
  startup_error="no visible window appeared within $WINDOW_TIMEOUT_SECONDS seconds"
else
  sleep 2
  if ! kill -0 "$app_pid" 2> /dev/null; then
    app_pid=""
    startup_error="the application exited right after it showed its window"
  fi
fi
if [[ -n "$startup_error" ]]; then
  import -window root /output/screen.png 2> /dev/null || true
  echo "Last lines of the application output (app.log in the output folder):"
  tail -n 20 /output/app.log | sed 's/^/  | /'
  cause="$(grep -m 1 -E 'error while loading shared libraries|version .GLIBC_[0-9.]+. not found' /output/app.log || true)"
  if [[ -z "$cause" ]]; then
    cause="$(grep -iE -m 1 '(xkbcommon|xcb|libX[A-Za-z0-9-]*|libEGL|libGL)[^ ]*\.so' /output/app.log || true)"
  fi
  if [[ -n "$cause" ]]; then
    fail "$startup_error; bundle or stop needing the library in: $cause"
  else
    fail "$startup_error; read app.log in the output folder"
  fi
  finish
fi
echo "A visible window appeared after $waited seconds on $CHECK_HOST (glibc $(getconf GNU_LIBC_VERSION | awk '{ print $NF }'))"

# sharun keeps its own file as the executable of the program it starts.
process=""
for entry in /proc/[0-9]*; do
  if [[ "$(readlink "$entry/exe" 2> /dev/null)" == "$appdir/bin/compare-all" ]]; then
    process="${entry#/proc/}"
  fi
done
if [[ -z "$process" ]]; then
  fail "no running process has $appdir/bin/compare-all as its executable"
else
  mapped="$(awk '$6 ~ /\.so/ { print $6 }' "/proc/$process/maps" | sort -u)"
  printf '%s\n' "$mapped" > /output/mapped-libraries.txt
  host_mapped="$(grep -v "^$appdir/" <<< "$mapped" | tr '\n' ' ' || true)"
  if [[ -n "$host_mapped" ]]; then
    fail "the running application maps libraries of the host: $host_mapped; bundle them"
  else
    echo "Libraries mapped by the running application: $(grep -c . <<< "$mapped"), all from the AppDir"
  fi
fi
check_child AppRun "$appdir"

if [[ "$CHECK_ALL" == 1 ]]; then
  icewm > "$work/icewm.log" 2>&1 &
  icewm_pid=$!
  sleep 2
  screen_width=800
  screen_height=600
  read -r screen_width screen_height < <(timeout 5 xdotool getdisplaygeometry 2> /dev/null) || true
  largest=""
  largest_area=0
  largest_geometry=""
  for id in $(timeout 5 xdotool search --onlyvisible --name '.' 2> /dev/null || true); do
    geometry="$(timeout 5 xdotool getwindowgeometry --shell "$id" 2> /dev/null || true)"
    width="$(sed -n 's/^WIDTH=//p' <<< "$geometry")"
    height="$(sed -n 's/^HEIGHT=//p' <<< "$geometry")"
    [[ "$width" =~ ^[0-9]+$ && "$height" =~ ^[0-9]+$ ]] || continue
    if ((width * height > largest_area)); then
      largest="$id"
      largest_area=$((width * height))
      largest_geometry="$width $height"
    fi
  done
  if [[ -n "$largest" ]]; then
    read -r width height <<< "$largest_geometry"
    timeout 5 xdotool windowmove "$largest" 0 0 2> /dev/null || true
    fit_width=$width
    fit_height=$height
    ((width > screen_width - 4)) && fit_width=$((screen_width - 4))
    ((height > screen_height - 30)) && fit_height=$((screen_height - 30))
    if ((fit_width != width || fit_height != height)); then
      echo "Resizing the window from ${width}x${height} to ${fit_width}x${fit_height} to fit the screen"
      timeout 5 xdotool windowsize "$largest" "$fit_width" "$fit_height" 2> /dev/null || true
      sleep 2
    fi
  fi

  screenshot=/output/screenshot.png
  rm -f "$screenshot"
  active="$(timeout 5 xdotool getactivewindow 2> /dev/null || true)"
  if [[ -n "$active" ]] && timeout 30 import -window "$active" "$screenshot" 2> /dev/null; then
    echo "Screenshot of the active window $active"
  elif [[ -n "$largest" ]] && timeout 30 import -window "$largest" "$screenshot" 2> /dev/null; then
    echo "Screenshot of the largest window $largest"
  else
    timeout 30 import -window root "$screenshot" 2> /dev/null || true
    echo "Screenshot of the whole screen"
  fi
  import -window root /output/screen.png 2> /dev/null || true

  if ! kill -0 "$app_pid" 2> /dev/null; then
    app_pid=""
    fail "the application exited while the screenshot was taken; read app.log in the output folder"
  fi

  if [[ "$(file -b --mime-type "$screenshot" 2> /dev/null)" != image/png ]]; then
    fail "no PNG screenshot of the window could be taken"
    finish
  fi
  read -r shot_width shot_height < <(identify -format '%w %h\n' "$screenshot")
  top_count="$(convert "$screenshot" -alpha off -depth 8 -format %c histogram:info:- \
    | awk -F: '{ gsub(/ /, "", $1); if ($1 + 0 > top) top = $1 + 0 } END { print top + 0 }')"
  share=$((100 * top_count / (shot_width * shot_height)))
  text="$(convert "$screenshot" -resize 200% -colorspace Gray png:- \
    | OMP_THREAD_LIMIT=1 timeout 60 tesseract stdin stdout -l eng --psm 11 2> /dev/null \
    | tr -s '[:space:]' ' ' || true)"
  printf '%s\n' "$text" > /output/screenshot.txt
  words="$(tr '[:upper:]' '[:lower:]' <<< "$text" | tr -cs '[:lower:]' '\n' | awk 'length($0) >= 3' \
    | grep -cvxE 'file|edit|view|help|tools|window|options|settings|search|about|preferences|menu' || true)"
  echo "Screenshot: ${shot_width}x${shot_height}, ${share}% one color, ${words} words of text"
  if ((share >= FLAT_COLOR_PERCENT && words < MIN_TEXT_WORDS)); then
    fail "the window is ${share}% one color with $words words of text; show content at start without arguments and without network"
  fi
  error_text="$(grep -oiE ".{0,40}\\b(${ERROR_WORDS})\\b.{0,40}" <<< "$text" | head -n 1 || true)"
  if [[ -n "$error_text" ]]; then
    fail "the screenshot shows error text: '$error_text'; remove the message from the start window"
  fi

  # The other entry points: the two program files started directly, as from
  # a mounted or extracted image, and the runtime's extract-and-run start.
  stop_image_processes "$appdir/"
  app_pid=""
  echo /output/child-env/bin-compare-all > "$spy/.outdir"
  (cd "$work" && PATH="$host_path" exec "$appdir/bin/compare-all") > /output/app-bin-compare-all.log 2>&1 &
  check_child bin-compare-all "$appdir"
  stop_image_processes "$appdir/"

  printf '# reads the zone offset and does nothing else\n' > "$work/zone.script"
  echo /output/child-env/bin-ca-script > "$spy/.outdir"
  ca_status=0
  (cd "$work" && PATH="$host_path" timeout 60 "$appdir/bin/ca" "@$work/zone.script") \
    > /output/ca-script.log 2>&1 || ca_status=$?
  echo "bin/ca @script exit status: $ca_status"
  check_child bin-ca-script "$appdir"

  echo /output/child-env/extract-and-run > "$spy/.outdir"
  (cd "$work" && PATH="$host_path" APPIMAGE_EXTRACT_AND_RUN=1 exec "$target") > /output/app-extract-and-run.log 2>&1 &
  check_child extract-and-run /tmp/appimage_extracted_ "$target"
  stop_image_processes /tmp/appimage_extracted_
fi

finish
CHECK

container=""
trap 'if [[ -n "$container" ]]; then docker rm -f "$container" > /dev/null 2>&1 || true; fi' EXIT
# run_check RELEASE CHECK_ALL: runs the check script in the Ubuntu RELEASE
# container; its log and files land in OUTPUT/ubuntu-RELEASE.
run_check() {
  local release="$1" check_all="$2" image folder status=0 found
  image="$(check_image "$release")"
  folder="$output/ubuntu-$release"
  mkdir -p "$folder"
  container="compare-all-appimage-check-$$-${release//./}"
  echo "== Ubuntu $release"
  # --mount instead of -v: -v splits its argument on ':'.
  docker run --rm --name "$container" --network none \
    --user "$(id -u):$(id -g)" \
    --env HOME=/tmp/home \
    --env APPIMAGE_NAME="$file_name" \
    --env APP_STEM="$stem" \
    --env CHECK_ALL="$check_all" \
    --env CHECK_HOST="Ubuntu $release" \
    --mount "type=bind,source=$appimage,target=/input/$file_name,readonly" \
    --mount "type=bind,source=$catalog_appstreamcli,target=/tools/appstreamcli-x86_64.AppImage,readonly" \
    --mount "type=bind,source=$folder,target=/output" \
    "$image" bash -c "$check_script" check 2>&1 | tee "$folder/check.log" || status=$?
  container=""
  found="$(grep -c '^FAIL:' "$folder/check.log" || true)"
  failures=$((failures + found))
  if ((status != 0 && found == 0)); then
    fail "the Ubuntu $release check container stopped with status $status before it reported a result; read $folder/check.log"
  fi
  if [[ -f "$folder/screenshot.png" ]]; then
    echo "Screenshot: $folder/screenshot.png"
  fi
}
run_check 22.04 1
run_check 20.04 0

if ((failures > 0)); then
  echo "Result: fail. Failed checks: $failures. AppImage: $file_name"
  exit 1
fi
echo "Result: pass. Every check passed. AppImage: $file_name"
