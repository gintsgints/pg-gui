#!/bin/bash
# Runs the seed scripts laid out under sql/: one folder per object type, one
# file per object, versioned (V.<version>__<name>.sql) scripts for things
# created once and repeatable (R__<nnn>_<name>.sql) scripts for things that are
# safe to re-apply.
#
# The postgres entrypoint globs /docker-entrypoint-initdb.d/* and ignores
# directories, so this script is the single entry point it does see; it walks
# the folders itself, in dependency order.

set -euo pipefail

init_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Dependency order, not alphabetical: extensions first, then the schemas and
# tables the data is loaded into, then the routines over those tables, and
# finally role settings.
folders=(extensions schemas tables data functions roles)

run_file() {
    printf '%s: running %s\n' "$0" "${1#"$init_dir"/}"
    psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
        --no-password --no-psqlrc -f "$1"
}

for folder in "${folders[@]}"; do
    dir="$init_dir/$folder"
    [ -d "$dir" ] || continue

    # Versioned scripts before repeatable ones, each group in name order
    # (LC_ALL=C so the glob sorts the same everywhere).
    for pattern in 'V*.sql' 'R*.sql'; do
        while IFS= read -r -d '' file; do
            run_file "$file"
        done < <(LC_ALL=C find "$dir" -maxdepth 1 -name "$pattern" -print0 | LC_ALL=C sort -z)
    done
done
