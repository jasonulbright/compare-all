#!/bin/sh
# sharun (the AppRun) runs this script with the shell of the host and sets
# APPDIR. bin/compare-all is a link to sharun, which starts
# shared/bin/compare-all on the bundled dynamic loader.
set -eu
exec "$APPDIR/bin/compare-all" "$@"
