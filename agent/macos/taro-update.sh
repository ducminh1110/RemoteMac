#!/bin/bash
# Taro updater for the Mac: fetches the newest Taro-macos.tar.gz from Taro's releases on
# GitHub, checks it (the SHA-256 GitHub lists for it, and that the program runs), and puts its
# taro, taro.sh, taro-update.sh and README.txt in place of the ones in this folder.
#
# A Taro running in the background goes on with the new version by itself, with the same ID
# and password (the copy that watches over it starts the new program; a connected PC
# reconnects on its own). Nothing else changes: settings, ID and permissions stay.
#
#   ./taro-update.sh           update when there is a newer version
#   ./taro-update.sh --check   only say whether there is one (exit 0: up to date, 10: newer)
#   ./taro-update.sh --force   install the newest release even when it is not newer
#   ./taro-update.sh --porcelain   for Taro itself: lines "latest V", "stage S", "progress N",
#                              "done V" on stdout, and no restart (the running Taro starts the new
#                              program at its next connection, so a connected PC is not cut off)
set -uo pipefail

repo="ducminh1110/RemoteMac"
asset="Taro-macos.tar.gz"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
say() { printf '%s\n' "$*" >&2; }
die() { say "taro-update: $*"; exit 1; }

check=0 force=0 porcelain=0
for a in "$@"; do
  case "$a" in
    --check) check=1 ;;
    --force) force=1 ;;
    --porcelain) porcelain=1 ;;
    -h|--help) sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown option $a (see --help)" ;;
  esac
done

command -v curl >/dev/null || die "curl is needed (it comes with macOS)"

# a provider's Taro: the official taro with provider/provider.json next to it. Only the
# program files below are ever replaced: the provider/ folder stays as it is. A provider that
# ships its own packages (updates.clients "off") is not updated from here at all.
product="Taro"
pj="$here/provider/provider.json"
if [[ -f "$pj" ]]; then
  product="$(plutil -extract product.name raw -o - "$pj" 2>/dev/null)" || product=""
  vendor="$(plutil -extract product.vendor raw -o - "$pj" 2>/dev/null)" || vendor=""
  clients="$(plutil -extract updates.clients raw -o - "$pj" 2>/dev/null)" || clients="notify"
  [[ -n "$product" ]] || die "$pj is not valid (no product.name): download the package again from your provider"
  if [[ "$clients" == "off" ]]; then
    say "Updates of $product are provided by ${vendor:-your provider}: get the new package from them."
    (( porcelain )) && exit 1
    exit 0
  fi
fi

# what this folder has
current=""
if [[ -x "$here/taro" ]]; then
  current="$("$here/taro" --version 2>/dev/null | awk '{print $2}')"
fi

# the newest release: its version, its file for the Mac, that file's SHA-256 (GitHub's API,
# read with macOS's own JavaScript)
json="$(curl -fsSL --retry 2 -H 'Accept: application/vnd.github+json' "https://api.github.com/repos/$repo/releases/latest" 2>/dev/null)" || json=""
latest="" url="" digest="" size=""
if [[ -n "$json" ]]; then
  read -r latest url digest size < <(osascript -l JavaScript -e '
    function run(argv) {
      const r = JSON.parse(argv[0]);
      const a = (r.assets || []).find(x => x.name === argv[1]) || {};
      const d = (a.digest || "").replace(/^sha256:/, "");
      return [(r.tag_name || "").replace(/^v/, ""), a.browser_download_url || "-", d || "-", a.size || "-"].join(" ");
    }' "$json" "$asset" 2>/dev/null)
fi
if [[ -z "$latest" ]]; then
  # (the API not reachable or rate-limited: the release page says the version)
  loc="$(curl -fsSI "https://github.com/$repo/releases/latest" 2>/dev/null | tr -d '\r' | awk 'tolower($1)=="location:"{print $2}' | tail -1)"
  latest="${loc##*/tag/}"; latest="${latest#v}"
  [[ -n "$latest" && "$latest" != "$loc" ]] || die "could not reach GitHub to find the newest Taro (is this Mac online?)"
  url="-" digest="-" size="-"
fi
[[ "$url" == "-" ]] && url="https://github.com/$repo/releases/download/$latest/$asset"
case "$url" in
  "https://github.com/$repo/releases/download/"*) ;;
  *) die "the newest release's file is not on Taro's releases page ($url): not installed" ;;
esac

# a.b.c (a pre-release: a.b.c-x, before a.b.c) as a number for comparing
num() { local v="${1%%-*}" a b c; IFS=. read -r a b c <<<"$v"; echo $(( ${a:-0} * 1000000 + ${b:-0} * 1000 + ${c:-0} )); }
newer=0
if [[ -z "$current" ]]; then newer=1
elif (( $(num "$latest") > $(num "$current") )); then newer=1
elif (( $(num "$latest") == $(num "$current") )) && [[ "$current" == *-* && "$latest" != *-* ]]; then newer=1
fi

out() { (( porcelain )) && printf '%s\n' "$*"; return 0; }
if (( check )); then
  out "latest $latest"
  if (( newer )); then say "Taro $latest is available (this folder has ${current:-no Taro}). Update: ./taro-update.sh"; exit 10; fi
  say "Taro is up to date ($current; the newest release is $latest)."
  exit 0
fi
if (( ! newer && ! force )); then
  say "Taro is up to date ($current; the newest release is $latest)."
  exit 0
fi

say "Downloading Taro ${latest}…"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/taro-update.XXXXXX")" || die "no temporary folder"
trap 'rm -rf "$tmp"' EXIT
out "stage downloading"
if (( porcelain )); then
  # (the share of the file that has arrived, for the viewer's progress bar)
  curl -fsSL --retry 2 -o "$tmp/$asset" "$url" & cpid=$!
  while kill -0 "$cpid" 2>/dev/null; do
    if [[ "$size" =~ ^[0-9]+$ && "$size" -gt 0 ]]; then
      got_b="$(stat -f%z "$tmp/$asset" 2>/dev/null || echo 0)"
      out "progress $(( got_b * 90 / size ))"
    fi
    sleep 0.3
  done
  wait "$cpid" || die "the download failed"
else
  curl -fL --retry 2 --progress-bar -o "$tmp/$asset" "$url" || die "the download failed"
fi
out "progress 90"; out "stage checking"
if [[ "$digest" != "-" ]]; then
  got="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  [[ "$got" == "$digest" ]] || die "the download does not match the release (SHA-256 $got, expected $digest): not installed"
fi
tar -xzf "$tmp/$asset" -C "$tmp" || die "the download is not a Taro bundle"
new="$tmp/Taro"
[[ -f "$new/taro" && -f "$new/taro.sh" ]] || die "the bundle has no taro and taro.sh"
chmod +x "$new/taro" "$new/taro.sh" "$new/taro-update.sh" 2>/dev/null
xattr -d com.apple.quarantine "$new/taro" 2>/dev/null
got="$("$new/taro" --version 2>/dev/null | awk '{print $2}')"
[[ -n "$got" ]] || die "the new taro does not run on this Mac: not installed"

out "progress 94"; out "stage installing"
# in place, each file at once (mv: a program or this script still running keeps its old copy)
# (the program files only: never provider/; a provider's Taro keeps its own README)
files="taro taro.sh taro-update.sh README.txt"
[[ -f "$pj" ]] && files="taro taro.sh taro-update.sh"
for f in $files; do
  [[ -f "$new/$f" ]] || continue
  cp -p "$new/$f" "$here/.$f.new" && mv -f "$here/.$f.new" "$here/$f" || die "could not replace $here/$f (is the folder writable?)"
done
say "$product $got is in $here."
out "progress 100"
if (( porcelain )); then out "done $got"; exit 0; fi

# a Taro running in the background: its watcher starts the new program (same ID, same password)
pidfile="$HOME/Library/Application Support/RemoteMac/macbridge.pid"
pid="$(tr -d '[:space:]' <"$pidfile" 2>/dev/null)"
if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
  worker="$(pgrep -P "$pid" | head -1)"
  if [[ -n "$worker" ]]; then
    kill -TERM "$worker" 2>/dev/null
    say "The Taro running in the background now goes on with $got (same ID and password; a connected PC reconnects by itself)."
  else
    say "Restart the Taro running in the background to use $got: ./taro --stop, then ./taro.sh --password YOUR-PASSWORD"
  fi
else
  say "Start it: ./taro.sh --password YOUR-PASSWORD"
fi
