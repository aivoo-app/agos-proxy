#!/bin/sh
# Container entrypoint: seed the store on first run, then exec the command.
set -e

SETUP_FILE="/data/setup.json"

# Compose passes the setup document via AGOS_SETUP; persist it so the seed
# happens exactly once (the store itself is the "already seeded" marker).
if [ -n "$AGOS_SETUP" ] && [ ! -f "$SETUP_FILE" ]; then
    printf '%s\n' "$AGOS_SETUP" > "$SETUP_FILE"
fi

# `profile list` prints a single "No profiles yet..." line on an empty
# store and one row per profile otherwise — seed while it stays that thin.
if [ -f "$SETUP_FILE" ] && [ "$(agos-proxy profile list 2>/dev/null | wc -l)" -le 1 ]; then
    echo "[entrypoint] seeding store from $SETUP_FILE"
    agos-proxy bootstrap from-file "$SETUP_FILE"
fi

exec agos-proxy "$@"
