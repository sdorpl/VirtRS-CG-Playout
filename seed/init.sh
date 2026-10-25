#!/bin/sh
# Every start of the stack (the image of seed/Dockerfile): copies the reference
# instance into empty volumes – the VizRS show with its backgrounds, the
# ErsatzTV channel database and the test media. Volumes that already hold data
# are left alone. The ErsatzTV programs and the graphics element are ours and
# are always replaced, so a new version of the stack brings them along.
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

if [ -n "$(ls -A /media)" ]; then
  echo "seed: test media present, left alone"
else
  cp -R /seed/media/. /media/
  echo "seed: test media copied"
fi

rm -rf /vizrs-client/*
cp -R /seed/client/. /vizrs-client/
chmod 755 /vizrs-client/*
mkdir -p /ersatztv-config/templates/graphics-elements/script
# Copy all templates (*.yml) to ErsatzTV script templates directory
cp -R /seed/templates/. /ersatztv-config/templates/graphics-elements/script/
echo "seed: ErsatzTV clients and VizRS graphics elements updated"
