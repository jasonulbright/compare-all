#!/usr/bin/env bash
# Runs the checks of the AppImage catalog test against a built AppImage inside
# a clean Ubuntu 22.04 container that has no network. Exit status 0 means that
# every check passed; each failed check prints one line that starts with FAIL:.
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

image=compare-all-appimage-check:22.04
container="compare-all-appimage-check-$$"
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

IFS= read -r -d '' dockerfile <<'EOF' || true
FROM ubuntu:22.04
RUN apt-get update \
  && DEBIAN_FRONTEND=noninteractive apt-get install -y \
    xvfb icewm x11-utils x11-apps netpbm xdotool xterm xsel stalonetray \
    libgl1-mesa-dri libgl1-mesa-dev mesa-utils libosmesa6 libsdl1.2-dev libsdl2-2.0-0 \
    libfuse2 desktop-file-utils libfile-mimeinfo-perl xmlstarlet imagemagick \
    fonts-wqy-microhei tesseract-ocr tesseract-ocr-eng \
    libasound2-dev pulseaudio-utils alsa-utils alsa-oss libjack0 \
    file binutils \
  && rm -rf /var/lib/apt/lists/* \
  && mkdir -p /tmp/.X11-unix \
  && chmod 1777 /tmp/.X11-unix
EOF
recipe="$(printf '%s' "$dockerfile" | sha256sum | cut -d ' ' -f 1)"
built="$(docker image inspect --format '{{ index .Config.Labels "compare-all.recipe" }}' "$image" 2> /dev/null || true)"
if [[ "$built" != "$recipe" ]]; then
  echo "Building the check image $image"
  printf '%s\n' "$dockerfile" | docker build --quiet --label "compare-all.recipe=$recipe" --tag "$image" - > /dev/null
fi

IFS= read -r -d '' check_script <<'CHECK' || true
set -euo pipefail

MIN_RUN_SECONDS=10
WINDOW_TIMEOUT_SECONDS=30
FLAT_COLOR_PERCENT=90
MIN_TEXT_WORDS=5
ERROR_WORDS='error|errors|failed|failure|panic|panicked|cannot|could not|unable to|fatal|exception|traceback|segmentation fault|core dumped|not found|permission denied|no such file'
EXCLUDED_LIBRARIES=(
  ld-linux.so.2 ld-linux-x86-64.so.2 libanl.so.1 libBrokenLocale.so.1 libcidn.so.1
  libc.so.6 libdl.so.2 libm.so.6 libmvec.so.1 libnss_compat.so.2 libnss_dns.so.2
  libnss_files.so.2 libnss_hesiod.so.2 libnss_nisplus.so.2 libnss_nis.so.2
  libpthread.so.0 libresolv.so.2 librt.so.1 libthread_db.so.1 libutil.so.1
  libstdc++.so.6 libGL.so.1 libEGL.so.1 libGLdispatch.so.0 libGLX.so.0
  libOpenGL.so.0 libdrm.so.2 libglapi.so.0 libgbm.so.1 libxcb.so.1 libX11.so.6
  libX11-xcb.so.1 libwayland-client.so.0 libasound.so.2 libfontconfig.so.1
  libfreetype.so.6 libharfbuzz.so.0 libcom_err.so.2 libexpat.so.1 libgcc_s.so.1
  libgpg-error.so.0 libICE.so.6 libSM.so.6 libusb-1.0.so.0 libuuid.so.1 libz.so.1
  libjack.so.0 libpipewire-0.3.so.0 libxcb-dri3.so.0 libxcb-dri2.so.0
  libfribidi.so.0 libgmp.so.10
)

failures=0
fail() {
  echo "FAIL: $*"
  failures=$((failures + 1))
}
finish() {
  echo "Failed checks in the container: $failures"
  if ((failures > 0)); then
    exit 1
  fi
  exit 0
}
hex() {
  od -An -tx1 -j "$2" -N "$3" "$1" | tr -d ' \n'
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

if [[ "$(hex "$target" 0 4)" != 7f454c46 ]]; then
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

bundled="$(find "$appdir" -printf '%f\n' | sort -u)"
for library in "${EXCLUDED_LIBRARIES[@]}"; do
  if grep -qxF "$library" <<< "$bundled"; then
    fail "the AppDir bundles $library, which is on the AppImage excludelist; remove it from the AppDir"
  fi
done

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

container_glibc="$(ldd --version | awk 'NR == 1 { print $NF }')"
library_path=""
if grep -q LD_LIBRARY_PATH "$appdir/AppRun" 2> /dev/null; then
  for folder in usr/lib/x86_64-linux-gnu usr/lib usr/lib64 lib/x86_64-linux-gnu lib lib64; do
    if [[ -d "$appdir/$folder" ]]; then
      library_path="${library_path:+$library_path:}$appdir/$folder"
    fi
  done
fi
: > "$work/glibc-versions"
elf_count=0
while IFS= read -r -d '' elf; do
  [[ "$(hex "$elf" 0 4)" == 7f454c46 ]] || continue
  [[ "$(readelf -dW "$elf" 2> /dev/null)" == *"(NEEDED)"* ]] || continue
  elf_count=$((elf_count + 1))
  linked="$(LD_LIBRARY_PATH="$library_path" ldd "$elf" 2>&1 || true)"
  missing="$(awk '/=> not found/ { print $1 }' <<< "$linked" | sort -u | tr '\n' ' ')"
  if [[ -n "$missing" ]]; then
    fail "${elf#"$appdir"/} needs ${missing% } that neither the AppDir nor a clean Ubuntu 22.04 provides; bundle it"
  fi
  objdump -T "$elf" 2> /dev/null | awk '/\*UND\*/' | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' >> "$work/glibc-versions" || true
done < <(find "$appdir" -type f -print0)
if [[ "$(readelf -lW "$target" 2> /dev/null)" == *"program interpreter"* ]]; then
  objdump -T "$target" 2> /dev/null | awk '/\*UND\*/' | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' >> "$work/glibc-versions" || true
fi
newest_glibc="$(sort -uV "$work/glibc-versions" | tail -n 1)"
echo "Dynamically linked ELF files: $elf_count; newest glibc symbol: ${newest_glibc:-none}; container glibc: $container_glibc"
if [[ -n "$newest_glibc" ]]; then
  required="${newest_glibc#GLIBC_}"
  if [[ "$(printf '%s\n%s\n' "$required" "$container_glibc" | sort -V | tail -n 1)" != "$container_glibc" ]]; then
    fail "the AppImage needs $newest_glibc, newer than glibc $container_glibc of Ubuntu 22.04; build on an older distribution"
  fi
fi

export DISPLAY=:99 LANG=C LC_ALL=C LIBGL_ALWAYS_SOFTWARE=1
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

(cd "$work" && APPIMAGE="/input/$APPIMAGE_NAME" APPDIR="$appdir" OWD="$work" ARGV0="$APPIMAGE_NAME" \
  exec "$appdir/AppRun") > /output/app.log 2>&1 &
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
  cause="$(grep -m 1 'error while loading shared libraries' /output/app.log || true)"
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
echo "A visible window appeared after $waited seconds"

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

finish
CHECK

trap 'docker rm -f "$container" > /dev/null 2>&1 || true' EXIT
status=0
# --mount instead of -v: -v splits its argument on ':'.
docker run --rm --name "$container" --network none \
  --user "$(id -u):$(id -g)" \
  --env HOME=/tmp/home \
  --env APPIMAGE_NAME="$file_name" \
  --env APP_STEM="$stem" \
  --mount "type=bind,source=$appimage,target=/input/$file_name,readonly" \
  --mount "type=bind,source=$output,target=/output" \
  "$image" bash -c "$check_script" check 2>&1 | tee "$output/check.log" || status=$?

container_failures="$(grep -c '^FAIL:' "$output/check.log" || true)"
failures=$((failures + container_failures))
if ((status != 0 && container_failures == 0)); then
  fail "the check container stopped with status $status before it reported a result; read check.log in the output folder"
fi
if [[ -f "$output/screenshot.png" ]]; then
  echo "Screenshot: $output/screenshot.png"
fi
if ((failures > 0)); then
  echo "Result: fail. Failed checks: $failures. AppImage: $file_name"
  exit 1
fi
echo "Result: pass. Every check passed. AppImage: $file_name"
