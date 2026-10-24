#!/bin/sh
# First start of the stack: copies the reference instance (seed/) into empty
# volumes – the VizRS show with its backgrounds and the ErsatzTV channel
# database. Volumes that already hold data are left alone.
set -eu

# A new volume created here belongs to root; VizRS runs as 10001.
chown 10001:10001 /vizrs-data
if [ -e /vizrs-data/show.json ] || [ -d /vizrs-data/shows ]; then
  echo "seed: VizRS data present, left alone"
else
  cp -R /seed/vizrs-data/. /vizrs-data/
  chown -R 10001:10001 /vizrs-data
  echo "seed: VizRS show and assets copied"
fi

if [ -e /ersatztv-config/ersatztv.sqlite3 ]; then
  echo "seed: ErsatzTV database present, left alone"
else
  cp /seed/ersatztv-config/ersatztv.sqlite3 /ersatztv-config/
  echo "seed: ErsatzTV channel database copied"
fi
