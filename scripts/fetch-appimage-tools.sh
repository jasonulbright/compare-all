#!/usr/bin/env bash
# Downloads the pinned AppImage build tools and license texts into the folder
# named on the command line. A file whose SHA-256 sum differs stops the script.
# A file that is already in the folder with the pinned sum is not downloaded
# again. --verify checks the folder and downloads nothing.
set -euo pipefail

if [[ $# -eq 2 && "$1" == --verify ]]; then
  verify_only=1
  directory="$2"
elif [[ $# -eq 1 ]]; then
  verify_only=0
  directory="$1"
else
  echo "usage: $0 [--verify] DIRECTORY" >&2
  exit 2
fi
mkdir -p "$directory"

# file name, URL, SHA-256. quick-sharun.sh refuses a sharun or
# cross-libc-dlopen tarball whose sum differs from the sums it holds, so the
# two tarballs here must be the ones that the pinned quick-sharun.sh names.
pins=(
  appimagetool-x86_64.AppImage
  https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
  ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0

  runtime-x86_64
  https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
  2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

  quick-sharun.sh
  https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/5d00649d56d4196e59a632bd47d1660d1ee4acfa/useful-tools/quick-sharun.sh
  8026711b271c0d67d37075cd7e8c50dbd9fa635c012f9edb92a2c0d41573664b

  sharun+helper-libs-x86_64.tar
  https://github.com/pkgforge-dev/Anylinux-sharun/releases/download/3.5.0/sharun+helper-libs-x86_64.tar
  a84935e91826cc38834f35eee099269ab643f97a47d3aaf5c171d87fda3d3c1f

  cross-libc-dlopen-x86_64.tar
  https://github.com/pkgforge-dev/cross-libc-dlopen/releases/download/v0.2.7/cross-libc-dlopen-x86_64.tar
  5b4a9c799b4875e7687b9b4158105a64056031c6c8a8719cbf00fa20839c9cf3

  LICENSE-sharun.txt
  https://raw.githubusercontent.com/pkgforge-dev/Anylinux-sharun/b6be3cc56cc8dbe78b8fd0381d67f86fbd25120f/LICENSE
  ed1795c447be9b4ae96262f583b559f733a82f627b0265f860f22488c7f8b2ff

  LICENSE-linuxdeploy-plugin-checkrt.txt
  https://raw.githubusercontent.com/darealshinji/linuxdeploy-plugin-checkrt/ff00c4e27fcf6a30b328d31dbb1a9fe0938569b1/COPYING
  511f1eb6929dab1bcd4fbe08fcda50b010b93187c2d383179ead2572ea91ac83
)

matches() {
  [[ -f "$1" ]] && printf '%s  %s\n' "$2" "$1" | sha256sum --check --status
}

for ((i = 0; i < ${#pins[@]}; i += 3)); do
  file="$directory/${pins[i]}"
  url="${pins[i + 1]}"
  sum="${pins[i + 2]}"
  if matches "$file" "$sum"; then
    continue
  fi
  if ((verify_only)); then
    echo "missing or changed: $file (expected SHA-256 $sum)" >&2
    exit 1
  fi
  curl --fail --location --retry 3 --silent --show-error "$url" --output "$file.part"
  if ! matches "$file.part" "$sum"; then
    rm -f "$file.part"
    echo "SHA-256 sum of $url is not $sum" >&2
    exit 1
  fi
  mv -f "$file.part" "$file"
  echo "$file: OK"
done
if ((!verify_only)); then
  chmod 0755 "$directory/appimagetool-x86_64.AppImage"
fi
