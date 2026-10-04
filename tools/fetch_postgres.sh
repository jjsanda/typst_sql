#!/usr/bin/env bash
# Fetch portable PostgreSQL binaries (zonky embedded-postgres, runs
# unprivileged) for tools/test_postgres.sh. Prints PG_ROOT on success.
set -euo pipefail
VERSION="${PG_VERSION:-17.2.0}"
DEST="${1:-$HOME/.local/quarry/opt/postgres-$VERSION}"
if [ -x "$DEST/bin/initdb" ]; then
  echo "$DEST"
  exit 0
fi
mkdir -p "$DEST"
JAR="$(mktemp)"
curl -sfL -o "$JAR" \
  "https://repo1.maven.org/maven2/io/zonky/test/postgres/embedded-postgres-binaries-linux-amd64/$VERSION/embedded-postgres-binaries-linux-amd64-$VERSION.jar"
TMP="$(mktemp -d)"
python3 -c "import zipfile,sys; zipfile.ZipFile('$JAR').extractall('$TMP')"
tar -xJf "$TMP"/postgres-linux-*.txz -C "$DEST"
rm -rf "$JAR" "$TMP"
echo "$DEST"
