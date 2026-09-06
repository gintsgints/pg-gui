#!/bin/bash
# Runs the seed scripts laid out under sql/: one folder per object type, one
# file per object, named
# <V|R>.<reserved>.<object_order>.<dependency_order>.<n>__<name>.sql — V for
# things created once, R for things that are safe to re-apply. Files in
# upgrade/ are named V.<YYYY>.<MM>.<DD>.<HH>.<MI>__<name>.sql instead, so
# branches never claim the same number; they still sort in run order.
#
# The postgres entrypoint globs /docker-entrypoint-initdb.d/* and ignores
# directories, so this script is the single entry point it does see; it walks
# the folders itself, in dependency order.

set -euo pipefail

init_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Dependency order, not alphabetical: extensions first, then the roles and
# schemas everything else is owned by, the types and tables the data is loaded
# into, the routines and views over those tables, and finally the privileges.
# A folder's position here is the `<object_order>` field of its V scripts'
# names; keep the two in step when adding one. Missing folders are skipped, so
# a folder only has to exist once it holds a file.
folders=(
    extensions  # 01
    roles       # 02
    schemas     # 03
    types       # 04
    sequences   # 05
    tables      # 06
    functions   # 07
    views       # 08
    matviews    # 09
    constraints # 10
    triggers    # 11
    upgrade     # 12
    indexes     # 13
    refresh     # 14
    grants      # 15
)

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
