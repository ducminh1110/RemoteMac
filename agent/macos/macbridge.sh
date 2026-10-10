#!/bin/bash
# MacBridge launcher: checks what it can before starting — the program next to it, this
# Mac's macOS version, Gatekeeper's quarantine, and the two privacy permissions MacBridge
# needs — says plainly what is missing and how to allow it, then runs macbridge in its own
# place (exec: the same process, so its exit status and signals are the program's own).
#
# It never changes privacy settings itself: macOS decides, and asks the user. Every option
# is passed on to macbridge as it is:  ./macbridge.sh --password SECRET
#   ./macbridge.sh --check      only check, and say what is missing (exit 0 when all is well)
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
bin="$here/macbridge"
say() { printf '%s\n' "$*" >&2; }

if [[ ! -f "$bin" ]]; then
  say "macbridge.sh: the program is not next to this script (looked for $bin)."
  say "Keep macbridge and macbridge.sh in the same folder, as they come in MacBridge-macos.tar.gz."
  exit 2
fi
if [[ ! -x "$bin" ]]; then
  say "macbridge.sh: $bin cannot be run. Make it runnable with:"
  say "  chmod +x \"$bin\""
  exit 2
fi

# options that need no checks
case "${1:-}" in
  --stop|--version|-h|--help) exec "$bin" "$@" ;;
esac

ver="$(sw_vers -productVersion 2>/dev/null || echo 0)"
if (( ${ver%%.*} < 14 )); then
  say "MacBridge needs macOS 14 (Sonoma) or later; this Mac runs macOS $ver."
  exit 2
fi

# a downloaded program carries Gatekeeper's quarantine: macOS then refuses to start it
if xattr -p com.apple.quarantine "$bin" >/dev/null 2>&1; then
  say "macOS marked macbridge as downloaded (quarantine), so it may refuse to start it."
  say "If you trust this copy, clear the mark and run this again:"
  say "  xattr -d com.apple.quarantine \"$bin\""
  exit 2
fi

# the app macOS asks about: the terminal app this runs in (the first .app among the parents)
responsible() {
  local pid=$$ cmd
  while (( pid > 1 )); do
    cmd="$(ps -o comm= -p "$pid" 2>/dev/null)"
    if [[ "$cmd" == *.app/Contents/MacOS/* ]]; then
      local app="${cmd%%.app/Contents/MacOS/*}"
      printf '%s\n' "${app##*/}"
      return
    fi
    pid="$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')"
    [[ -n "$pid" ]] || break
  done
  printf 'your terminal app\n'
}

# the permissions, as macOS sees them for this terminal (macbridge asks the system itself)
report="$("$bin" --check-permissions 2>&1)"
status=$?
if (( status != 0 )); then
  app="$(responsible)"
  say "$report"
  say ""
  say "macOS gives these permissions to the app MacBridge runs in: $app."
  say "Open System Settings > Privacy & Security, turn $app on under each item listed above,"
  say "then quit $app completely and open it again (macOS applies the change at its next start)."
  if [[ "${1:-}" == "--check" ]]; then exit "$status"; fi
  say "Starting anyway: macOS shows its own request for what is missing."
  say ""
elif [[ "${1:-}" == "--check" ]]; then
  printf '%s\n' "$report"
  exit 0
fi

exec "$bin" "$@"
