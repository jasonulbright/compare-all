#!/usr/bin/env bash
# Downloads the pinned appimagetool and type2 runtime into the folder named on
# the command line. A file whose SHA-256 sum differs stops the script.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 DIRECTORY" >&2
  exit 2
fi
directory="$1"
mkdir -p "$directory"

fetch() {
  local url="$1" sum="$2" file="$directory/$3"
  curl --fail --location --retry 3 "$url" --output "$file"
  printf '%s  %s\n' "$sum" "$file" | sha256sum --check
}

fetch https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage \
  ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0 \
  appimagetool-x86_64.AppImage
chmod 0755 "$directory/appimagetool-x86_64.AppImage"
fetch https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64 \
  2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d \
  runtime-x86_64
