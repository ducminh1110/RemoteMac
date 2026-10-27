#!/usr/bin/env bash
# The PostgreSQL the relay runs for the portal (Taro Enterprise 0.2: one server): relocatable
# Linux builds from Maven Central (io.zonky.test.postgres, the build embedded-postgres uses),
# pinned by version and checksum, made ready for the relay's core:
#   scripts/fetch-postgres.sh ARCH OUT-DIR      ARCH: x86_64 | aarch64
# -> OUT-DIR/bin (initdb, pg_ctl, postgres), OUT-DIR/lib, OUT-DIR/share. Symbolic links are
# replaced by the files they name (an Enterprise update carries no links) and what the server
# does not use (client libraries, headers) is left out. Run from enterprise/.
set -euo pipefail
A="$1" OUT="$2"
V=16.15.0
case "$A" in
  x86_64) ART=embedded-postgres-binaries-linux-amd64; SUM=653abc065c682b85d3da50168fb95dc524bd85426cdec9ce3695a550ef431df2 ;;
  aarch64) ART=embedded-postgres-binaries-linux-arm64v8; SUM=f846a9989d686b7977d6eca9bc9d8b2b69e3f70c7eb60e032197d9c574a6136c ;;
  *) echo "unknown architecture $A" >&2; exit 2 ;;
esac
W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
curl -fsSL --retry 3 -o "$W/pg.jar" "https://repo1.maven.org/maven2/io/zonky/test/postgres/$ART/$V/$ART-$V.jar"
echo "$SUM  $W/pg.jar" | sha256sum -c -
(cd "$W" && unzip -q pg.jar '*.txz')
mkdir -p "$W/pg"
tar -xJf "$W"/*.txz -C "$W/pg"
python3 - "$W/pg" <<'PY'
import os, re, shutil, sys
root = sys.argv[1]
lib = os.path.join(root, "lib")
soname = re.compile(r"\.so\.\d+$")
links = [n for n in os.listdir(lib) if os.path.islink(os.path.join(lib, n))]
targets = set()
for n in links:
    p = os.path.join(lib, n)
    real = os.path.realpath(p)
    if soname.search(n):
        os.unlink(p)
        shutil.copy2(real, p)
        targets.add(os.path.basename(real))
for n in links:
    p = os.path.join(lib, n)
    if os.path.islink(p):
        os.unlink(p)
# the versioned files the sonames were copied from
for n in targets:
    p = os.path.join(lib, n)
    if os.path.exists(p) and not soname.search(n):
        os.unlink(p)
# client-side libraries and development files: not used by the server
for n in os.listdir(lib):
    if re.match(r"lib(ecpg|ecpg_compat|pgtypes|pq)\.", n) or n.endswith(".a"):
        os.unlink(os.path.join(lib, n))
for d in ("include",):
    shutil.rmtree(os.path.join(root, d), ignore_errors=True)
for dp, dn, fn in os.walk(root):
    for n in dn + fn:
        assert not os.path.islink(os.path.join(dp, n)), os.path.join(dp, n)
PY
rm -rf "$OUT" && mkdir -p "$OUT"
cp -a "$W/pg/." "$OUT/"
echo "PostgreSQL $V for $A in $OUT ($(du -sh "$OUT" | cut -f1), $(find "$OUT" -type f | wc -l) files)"
