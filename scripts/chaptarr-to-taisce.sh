#!/bin/sh
# chaptarr-to-taisce.sh: a Readarr / Chaptarr "Custom Script" connection that
# sends every imported EPUB to a Taisce library.
#
# Setup:
#   1. Taisce web UI: Settings > Upload tokens > add. Copy the tbu_... token
#      (shown once). It can upload books and do nothing else.
#   2. Make the token and URL visible to the *arr process, e.g. in the
#      container's environment:
#        TAISCE_URL=https://taisce.example
#        TAISCE_UPLOAD_TOKEN=tbu_...
#   3. Settings > Connect > + > Custom Script: path to this file, triggers
#      "On Release Import" and "On Upgrade". Test runs it with the Test event.
#
# The event contract (Readarr's CustomScript.cs, src/NzbDrone.Core/
# Notifications/CustomScript; the Servarr wiki page is retired):
#   Readarr_EventType       Test | Download (import and upgrade both) | ...
#   Readarr_AddedBookPaths  the imported files' paths, '|'-separated
#   Readarr_DeletedPaths    on an upgrade, the replaced files (ignored here)
# Readarr builds these in a .NET StringDictionary, which lowercases keys, so
# the process actually sees readarr_eventtype and readarr_addedbookpaths.
# Both spellings are read. Chaptarr is a Readarr fork, so its variables are
# assumed to follow the same contract under a chaptarr_ prefix; both prefixes
# are read, readarr_ first.
#
# Manual use: pass paths as arguments to upload them without an event:
#   TAISCE_URL=... TAISCE_UPLOAD_TOKEN=... ./chaptarr-to-taisce.sh a.epub b.epub
#
# Taisce answers {"results":[{"filename","status","message"?}]}, one per
# file: added | updated | already_here are fine; clash | error are failures.
# Exits non-zero if any upload failed. Non-EPUB files are skipped.

set -u

log() { printf '%s\n' "chaptarr-to-taisce: $*"; }

# The first set variable among readarr_/Readarr_/chaptarr_/Chaptarr_ + name.
arr_var() {
    for prefix in readarr_ Readarr_ chaptarr_ Chaptarr_; do
        for name in "$1" "$2"; do
            eval "val=\${${prefix}${name}:-}"
            if [ -n "$val" ]; then
                printf '%s' "$val"
                return 0
            fi
        done
    done
    return 0
}

event=$(arr_var eventtype EventType)
paths=$(arr_var addedbookpaths AddedBookPaths)

if [ "$#" -gt 0 ]; then
    event=manual
fi

case "$event" in
    Test)
        if [ -z "${TAISCE_URL:-}" ] || [ -z "${TAISCE_UPLOAD_TOKEN:-}" ]; then
            log "test: TAISCE_URL or TAISCE_UPLOAD_TOKEN is not set; imports will fail until they are"
        else
            log "test: ok, uploads go to ${TAISCE_URL%/}/api/books"
        fi
        exit 0
        ;;
    Download | download | manual) ;;
    "")
        log "no event type and no paths: nothing to do"
        exit 0
        ;;
    *)
        log "event $event: nothing to upload"
        exit 0
        ;;
esac

if [ -z "${TAISCE_URL:-}" ] || [ -z "${TAISCE_UPLOAD_TOKEN:-}" ]; then
    log "TAISCE_URL and TAISCE_UPLOAD_TOKEN must both be set"
    exit 2
fi
endpoint="${TAISCE_URL%/}/api/books"

if [ "$event" != manual ]; then
    if [ -z "$paths" ]; then
        log "event $event carried no book paths: nothing to upload"
        exit 0
    fi
    # split on '|' (paths may contain spaces; no globbing)
    set -f
    old_ifs=$IFS
    IFS='|'
    # shellcheck disable=SC2086
    set -- $paths
    IFS=$old_ifs
    set +f
fi

# one file's result line from the JSON reply: jq, else python3, else raw
result_lines() {
    if command -v jq >/dev/null 2>&1; then
        jq -r '.results[] | "\(.status)\t\(.filename)\t\(.message // "")"'
    elif command -v python3 >/dev/null 2>&1; then
        python3 -c 'import json,sys
for r in json.load(sys.stdin).get("results", []):
    print("%s\t%s\t%s" % (r.get("status",""), r.get("filename",""), r.get("message","")))'
    else
        sed 's/.*"status":"\([a-z_]*\)".*/\1\tresponse\t/'
    fi
}

uploaded=0
failed=0
skipped=0
tmp=$(mktemp) || exit 1
trap 'rm -f "$tmp"' EXIT

for path in "$@"; do
    case "$path" in
        *.epub | *.EPUB | *.Epub) ;;
        *)
            log "skip (not an EPUB): $path"
            skipped=$((skipped + 1))
            continue
            ;;
    esac
    if [ ! -f "$path" ]; then
        log "FAILED (no such file): $path"
        failed=$((failed + 1))
        continue
    fi
    code=$(curl -sS -o "$tmp" -w '%{http_code}' --max-time 600 \
        -H "Authorization: Bearer $TAISCE_UPLOAD_TOKEN" \
        -F "files=@\"$path\";type=application/epub+zip" \
        "$endpoint") || code=000
    if [ "$code" != 200 ]; then
        log "FAILED (HTTP $code): $path $(head -c 300 "$tmp" 2>/dev/null)"
        failed=$((failed + 1))
        continue
    fi
    lines=$(result_lines <"$tmp")
    if [ -z "$lines" ]; then
        log "FAILED (unreadable reply): $path $(head -c 300 "$tmp")"
        failed=$((failed + 1))
        continue
    fi
    tab=$(printf '\t')
    while IFS="$tab" read -r status name message; do
        case "$status" in
            added | updated | already_here)
                log "$status: $name ($path)"
                uploaded=$((uploaded + 1))
                ;;
            *)
                log "FAILED ($status${message:+: $message}): $name ($path)"
                failed=$((failed + 1))
                ;;
        esac
    done <<EOF
$lines
EOF
done

log "done: $uploaded uploaded, $failed failed, $skipped skipped"
[ "$failed" -eq 0 ]
