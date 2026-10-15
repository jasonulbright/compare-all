# Minimal helpers used by appimage-catalog-gate.sh. Downloads are cached only
# for the lifetime of the gate container.
set -eu
FETCH_CACHE="${GATE_FETCH_CACHE:-/root/gate-cache}"
FETCH_ATTEMPTS=4

die() {
  echo "error: $*" >&2
  exit 1
}

sha256_of() {
  sha256sum "$1" | cut -d' ' -f1
}

# fetch_verified URL SHA256 NAME -> prints the cached path.
fetch_verified() {
  url="$1"; want="$2"; name="$3"
  mkdir -p "$FETCH_CACHE"
  path="$FETCH_CACHE/$name"
  if [ -f "$path" ] && [ "$(sha256_of "$path")" = "$want" ]; then
    echo "$path"
    return 0
  fi
  rm -f "$path" "$path.part"
  attempt=1
  while :; do
    if curl -fL --retry 0 --connect-timeout 30 --max-time 1800 -o "$path.part" "$url" >&2; then
      break
    fi
    [ "$attempt" -ge "$FETCH_ATTEMPTS" ] && die "download failed after $attempt attempts: $url"
    sleep $((attempt * 3))
    attempt=$((attempt + 1))
  done
  got="$(sha256_of "$path.part")"
  if [ "$got" != "$want" ]; then
    rm -f "$path.part"
    die "$name has SHA-256 $got; the pin is $want"
  fi
  mv "$path.part" "$path"
  echo "$path"
}
