#!/usr/bin/env bash
# Guided demo of pg-gui, following scenario.md section by section.
#
# Drives an already running pg-gui through System Events (osascript), the
# same way the e2e harness does: keystrokes, menu clicks, native open
# panels, and a few clicks at fixed dialog offsets. Every step prints what
# it is about to show; by default the script waits for Enter before each
# step so a presenter can talk over it.
#
# GPUI draws its window itself and exposes nothing to accessibility, so
# steps that need a click on something whose position depends on your
# layout (a grid cell, a tree row, the gutter, a flame-graph frame) are
# handed to you: the script says what to do and waits for Enter.
#
# Requirements:
#   - pg-gui running (`cargo run`), at 100% zoom, nothing else in front of it
#   - the sample database up (`docker compose up -d --build --wait`)
#   - the terminal running this script has Accessibility permission
#     (System Settings > Privacy & Security > Accessibility)
#
# Usage:
#   scripts/demo.sh                 # every section, Enter between steps
#                                   # (r + Enter repeats the previous step,
#                                   #  s + Enter skips the one shown)
#   scripts/demo.sh 5               # start at section 5, go on to the end
#   scripts/demo.sh --only 5 7 8    # only those sections
#   scripts/demo.sh --auto          # no waiting; manual steps are skipped
#   scripts/demo.sh --auto --pace 3 # seconds between automatic steps
#   scripts/demo.sh --no-keys       # no on-screen shortcut overlay
#
# Each shortcut or menu command the script fires is printed in the terminal
# and flashed on screen over the bottom of the pg-gui window (a click-through
# panel that never takes the focus), so the audience sees what was pressed.
#
# The clipboard is used to paste SQL and is restored on exit.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SQL_DIR="$ROOT/sql"
PROCESS="pg-gui"
AUTO=0
PACE=2
SHOW_KEYS=1
SECTIONS=()
ONLY=0

while [ $# -gt 0 ]; do
    case "$1" in
        --auto) AUTO=1 ;;
        --pace) shift; PACE="$1" ;;
        --no-keys) SHOW_KEYS=0 ;;
        --only) ONLY=1 ;;
        -h|--help) sed -n '2,35p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) SECTIONS+=("$1") ;;
    esac
    shift
done

# --- terminal output ---------------------------------------------------------

BOLD=$'\033[1m'; DIM=$'\033[2m'; CYAN=$'\033[36m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'

section() { SKIPPING=0; printf '\n%s%s== %s ==%s\n' "$BOLD" "$CYAN" "$1" "$RESET"; }

# Announce a step, then wait for Enter (or PACE seconds under --auto). At
# the prompt, `r` replays the previous step's actions first, and `s` skips
# this one: its actions and hand-offs are ignored until the next step.
step() {
    SKIPPING=0
    advance "$1"
    printf '%s> %s%s\n' "$BOLD" "$1" "$RESET"
    if [ "$AUTO" = 1 ]; then
        command sleep "$PACE"
    elif ! prompt_replay "[Enter] go, r = repeat previous step, s = skip"; then
        SKIPPING=1
        printf '  %s(skipped)%s\n' "$DIM" "$RESET"
    fi
}

# --- repeating a step ----------------------------------------------------------
#
# Every action a section runs at top level (key, menu, set_editor, sleep, …)
# is recorded under the step it belongs to; actions those call internally
# are not, so a replay runs exactly what the section did once.

CUR_TITLE=""; CUR_ACTIONS=()
PREV_TITLE=""; PREV_ACTIONS=()
DEPTH=0; REPLAYING=0; SKIPPING=0

# Close the current step's recording and start one for <title>.
advance() {
    PREV_TITLE="$CUR_TITLE"
    PREV_ACTIONS=(${CUR_ACTIONS[@]+"${CUR_ACTIONS[@]}"})
    CUR_TITLE="$1"
    CUR_ACTIONS=()
}

# Run <fn> with the arguments, recording the call unless it is nested in
# another recorded action or part of a replay. Does nothing while the
# current step is skipped.
recorded() {
    local fn="$1"; shift
    if [ "$SKIPPING" = 1 ] && [ "$DEPTH" = 0 ] && [ "$REPLAYING" = 0 ]; then
        return
    fi
    if [ "$DEPTH" = 0 ] && [ "$REPLAYING" = 0 ]; then
        CUR_ACTIONS+=("$(printf '%q ' "${fn#_}" "$@")")
    fi
    DEPTH=$((DEPTH + 1))
    "$fn" "$@"
    DEPTH=$((DEPTH - 1))
}

replay() {
    if [ ${#PREV_ACTIONS[@]} -eq 0 ]; then
        printf '  %s(no previous step to repeat)%s\n' "$DIM" "$RESET"
        return
    fi
    printf '  %srepeating: %s%s\n' "$YELLOW" "$PREV_TITLE" "$RESET"
    REPLAYING=1
    local action
    for action in "${PREV_ACTIONS[@]}"; do
        eval "$action"
    done
    REPLAYING=0
}

# Wait for Enter; `r` replays the previous step and asks again. Fails when
# the answer is `s` (skip).
prompt_replay() {
    local answer
    while true; do
        read -r -p "${DIM}  $1${RESET} " answer </dev/tty
        case "$answer" in
            r|R) replay ;;
            s|S) return 1 ;;
            *) return 0 ;;
        esac
    done
}

# What the audience should now see.
expect() { printf '  %s-> %s%s\n' "$DIM" "$1" "$RESET"; }

# A step the presenter does by hand. Skipped under --auto.
manual() {
    if [ "$SKIPPING" = 1 ]; then
        return
    fi
    if [ "$AUTO" = 1 ]; then
        printf '  %s(skipped under --auto: %s)%s\n' "$DIM" "$1" "$RESET"
        return
    fi
    printf '  %sYOUR TURN:%s %s\n' "$YELLOW" "$RESET" "$1"
    read -r -p "${DIM}  [Enter when done]${RESET} " _ </dev/tty
}

# --- driving the app ---------------------------------------------------------

as_string() {
    local s="${1//\\/\\\\}"
    printf '"%s"' "${s//\"/\\\"}"
}

osa() { osascript -e "$1" >/dev/null; }

focus() {
    osa "tell application \"System Events\" to set frontmost of process \"$PROCESS\" to true"
    sleep 0.3
}

# --- showing what is pressed --------------------------------------------------

# A borderless, click-through panel that shows <label> centred on screen x
# <cx> with its bottom edge <bottom> points below the screen's top, for
# <seconds>. It runs as an accessory app and is ordered front without
# activating, so the keystrokes that follow still reach pg-gui.
OVERLAY_JS="$(mktemp -t pg-gui-demo-overlay)"
cat >"$OVERLAY_JS" <<'JS'
function run(argv) {
    ObjC.import('Cocoa');
    const [label, cx, bottom, seconds] = [argv[0], +argv[1], +argv[2], +argv[3]];
    $.NSApplication.sharedApplication.setActivationPolicy(1);
    const text = $.NSTextField.labelWithString($(label));
    text.font = $.NSFont.boldSystemFontOfSize(30);
    text.textColor = $.NSColor.whiteColor;
    text.sizeToFit;
    const tw = text.frame.size.width, th = text.frame.size.height;
    const w = tw + 48, h = th + 24;
    const screenHeight = $.NSScreen.screens.objectAtIndex(0).frame.size.height;
    // 128: NSWindowStyleMaskNonactivatingPanel, 2: buffered, 25: status level.
    const panel = $.NSPanel.alloc.initWithContentRectStyleMaskBackingDefer(
        $.NSMakeRect(cx - w / 2, screenHeight - bottom, w, h), 128, 2, false);
    panel.level = 25;
    panel.opaque = false;
    panel.backgroundColor = $.NSColor.clearColor;
    panel.ignoresMouseEvents = true;
    panel.hasShadow = true;
    const box = $.NSBox.alloc.initWithFrame($.NSMakeRect(0, 0, w, h));
    box.boxType = 4;
    box.borderWidth = 0;
    box.cornerRadius = 14;
    box.titlePosition = 0;
    box.fillColor = $.NSColor.colorWithWhiteAlpha(0.1, 0.82);
    text.frame = $.NSMakeRect(24, 12, tw, th);
    box.contentView.addSubview(text);
    panel.contentView = box;
    panel.orderFrontRegardless;
    $.NSRunLoop.currentRunLoop.runUntilDate($.NSDate.dateWithTimeIntervalSinceNow(seconds));
}
JS
OVERLAY_PID=""

# Print <label> in the terminal and flash it over pg-gui. Only for actions a
# section calls directly: the keys a helper presses on its way (the cmd-a,
# cmd-v of a paste) are plumbing, not something to show.
show_keys() {
    [ "${DEPTH:-0}" -le 1 ] || return 0
    printf '  %s⌨  %s%s\n' "$CYAN" "$1" "$RESET"
    [ "$SHOW_KEYS" = 1 ] || return 0
    if [ -n "$OVERLAY_PID" ]; then
        kill "$OVERLAY_PID" 2>/dev/null || true
    fi
    local wx wy ww wh
    read -r wx wy ww wh <<<"$(window_frame 2>/dev/null)" || return 0
    osascript -l JavaScript "$OVERLAY_JS" "$1" $((wx + ww / 2)) $((wy + wh - 90)) 1.8 \
        >/dev/null 2>&1 &
    OVERLAY_PID=$!
    # Out of the job table, so replacing it early (kill above) does not
    # print bash's "Terminated" notice.
    disown "$OVERLAY_PID" 2>/dev/null || true
}

# Modifier symbols for <modifier...>, in the order macOS menus print them.
mods_label() {
    local out="" m
    for m in ctrl alt shift cmd; do
        case " $* " in
            *" $m "*)
                case "$m" in
                    ctrl) out+="⌃" ;;
                    alt) out+="⌥" ;;
                    shift) out+="⇧" ;;
                    cmd) out+="⌘" ;;
                esac ;;
        esac
    done
    printf '%s' "$out"
}

# The name of virtual key code <kc>.
code_label() {
    case "$1" in
        36) printf '↩' ;;
        48) printf '⇥' ;;
        53) printf 'esc' ;;
        51) printf '⌫' ;;
        126) printf '↑' ;;
        125) printf '↓' ;;
        124) printf '→' ;;
        96) printf 'F5' ;;
        109) printf 'F10' ;;
        103) printf 'F11' ;;
        *) printf 'key %s' "$1" ;;
    esac
}

# key <char> [modifier...]   modifiers: cmd shift alt ctrl
key() {
    local char="$1"; shift
    show_keys "$(mods_label "$@")$(printf '%s' "$char" | tr '[:lower:]' '[:upper:]')"
    local mods=() using=""
    for m in "$@"; do
        case "$m" in
            cmd) mods+=("command down") ;;
            shift) mods+=("shift down") ;;
            alt) mods+=("option down") ;;
            ctrl) mods+=("control down") ;;
        esac
    done
    if [ ${#mods[@]} -gt 0 ]; then
        using=" using {$(IFS=,; echo "${mods[*]}" | sed 's/,/, /g')}"
    fi
    focus
    osa "tell application \"System Events\" to keystroke $(as_string "$char")$using"
    sleep 0.2
}

# code <virtual key code> [modifier...]
code() {
    local kc="$1"; shift
    show_keys "$(mods_label "$@")$(code_label "$kc")"
    local mods=() using=""
    for m in "$@"; do
        case "$m" in
            cmd) mods+=("command down") ;;
            shift) mods+=("shift down") ;;
        esac
    done
    if [ ${#mods[@]} -gt 0 ]; then
        using=" using {$(IFS=,; echo "${mods[*]}" | sed 's/,/, /g')}"
    fi
    focus
    osa "tell application \"System Events\" to key code $kc$using"
    sleep 0.2
}

RETURN=36; TAB=48; ESCAPE=53; DELETE=51; UP=126; DOWN=125; RIGHT=124
F5=96; F10=109; F11=103

repeat_code() {
    local n="$1"; shift
    show_keys "$(mods_label "${@:2}")$(code_label "$1") ×$n"
    for _ in $(seq "$n"); do code "$@"; done
}

# Type text one character at a time, so completions pop up as they would
# for a person typing.
type_slow() {
    focus
    osa "tell application \"System Events\"
        repeat with c in characters of $(as_string "$1")
            keystroke c
            delay 0.08
        end repeat
    end tell"
    sleep 0.3
}

# Replace the whole editor buffer with $1 and put the cursor at the top,
# so cmd-enter runs the first statement. Pasting avoids auto-closed
# brackets and completion popups getting in the way of typing.
set_editor() {
    printf '%s' "$1" | pbcopy
    key a cmd
    key v cmd
    sleep 0.3
    code $ESCAPE
    code $UP cmd
}

# menu <menu> <item>, matching an item with or without gpui's "✓ " prefix.
menu() {
    show_keys "$1 ▸ $2"
    focus
    osa "tell application \"System Events\" to tell process \"$PROCESS\"
        set m to menu 1 of menu bar item $(as_string "$1") of menu bar 1
        click (first menu item of m whose name is $(as_string "$2") or name is $(as_string "✓ $2"))
    end tell"
    sleep 0.5
}

# submenu <menu> <submenu> <item>
submenu() {
    show_keys "$1 ▸ $2 ▸ $3"
    focus
    osa "tell application \"System Events\" to tell process \"$PROCESS\"
        set m to menu 1 of menu item $(as_string "$2") of menu 1 of menu bar item $(as_string "$1") of menu bar 1
        click (first menu item of m whose name is $(as_string "$3") or name is $(as_string "✓ $3"))
    end tell"
    sleep 0.5
}

# The main window's frame as "x y width height".
window_frame() {
    osascript -e "tell application \"System Events\" to tell (first window of process \"$PROCESS\" whose name starts with \"pg-gui\")
        set {x, y} to position
        set {w, h} to size
        return (x as text) & \" \" & (y as text) & \" \" & (w as text) & \" \" & (h as text)
    end tell"
}

# Left-click at (x, y) points from the main window's top-left corner.
click_window() {
    focus
    local wx wy _w _h
    read -r wx wy _w _h <<<"$(window_frame)"
    local x=$((wx + ${1%.*})) y=$((wy + ${2%.*}))
    osascript -l JavaScript -e "
        ObjC.import('CoreGraphics');
        const at = \$.CGPointMake($x, $y);
        const post = (type) => \$.CGEventPost(\$.kCGHIDEventTap,
            \$.CGEventCreateMouseEvent(\$(), type, at, \$.kCGMouseButtonLeft));
        post(\$.kCGEventMouseMoved); delay(0.2);
        post(\$.kCGEventLeftMouseDown); delay(0.05);
        post(\$.kCGEventLeftMouseUp);" >/dev/null
    sleep 0.2
}

# The app's UI zoom (cmd-plus/minus) from its config.json, 1 when unset or
# unreadable. The app rewrites the file on every change, so it is current.
CONFIG_JSON="${PG_GUI_CONFIG_DIR:-$HOME/Library/Application Support/pg-gui}/config.json"
ui_zoom() {
    plutil -extract zoom raw -o - "$CONFIG_JSON" 2>/dev/null || echo 1
}

# Click at (dx, dy) in an open dialog: dx from the window's centre, dy from
# the dialog's top, which gpui-component puts a tenth of the way down. The
# offsets are measured at 100% zoom; the dialog grows with the zoom while
# its top stays where it is, so both scale by it.
click_dialog() {
    local _x _y w h zoom
    read -r _x _y w h <<<"$(window_frame)"
    zoom="$(ui_zoom)"
    click_window "$(awk -v w="$w" -v d="$1" -v z="$zoom" 'BEGIN { printf "%d", w / 2 + d * z }')" \
        "$(awk -v h="$h" -v d="$2" -v z="$zoom" 'BEGIN { printf "%d", h / 10 + d * z }')"
}

# Fill a field of the connection dialog: offsets from tests/e2e/harness.rs.
dialog_field() {
    click_dialog "$1" "$2"
    key a cmd
    type_slow "$3"
}

# Pick a path in a native open panel (File ▸ Open Folder…).
choose_path() {
    sleep 1
    key g cmd shift
    sleep 0.8
    key a cmd
    type_slow "$1"
    code $RETURN
    sleep 0.8
    code $RETURN
    sleep 1
}

# Open a working-folder file through the cmd-p palette: <query> must rank
# the file first (a file name holding it verbatim beats scattered matches).
# Needs the working folder open (section 2).
palette_open() {
    show_keys "⌘P  $1"
    key p cmd
    sleep 0.6
    type_slow "$1"
    sleep 1
    code $RETURN
    sleep 1
}


# Connect as <name> to database <db> through Connection ▸ New Connection…,
# always through the dialog so the demo shows it. Saving under a name that
# Recent already lists updates that entry (record_recent dedups by name),
# so re-running the demo never piles up duplicates.
connect_to() {
    local name="$1" db="$2"
    show_keys "Connection ▸ New Connection…"
    menu "Connection" "New Connection…"
    sleep 0.8
    dialog_field 0 87 "$name"         # Name
    dialog_field -66 161 "localhost"  # Host
    dialog_field 183 161 "5433"       # Port
    dialog_field -125 236 "$db"       # Database
    dialog_field 0 310 "pgui"         # Username
    dialog_field 0 385 "pgui"         # Password
    expect "The Connection string preview built itself from the fields."
    sleep 1.5
    click_dialog 202 523              # Connect
}

# Whether Connection ▸ Recent lists <name>.
recent_has() {
    local names
    names=$(osascript -e "tell application \"System Events\" to tell process \"$PROCESS\"
        return name of every menu item of menu 1 of menu item \"Recent\" of menu 1 of menu bar item \"Connection\" of menu bar 1
    end tell" 2>/dev/null || true)
    case ", $names, " in
        *", $1, "*|*", ✓ $1, "*) return 0 ;;
        *) return 1 ;;
    esac
}

# Switch to saved connection <name> from Connection ▸ Recent, creating it
# for database <db> through the dialog when Recent does not list it (a run
# started past section 1, or a Connect click that missed).
switch_to() {
    if recent_has "$1"; then
        show_keys "Connection ▸ Recent ▸ $1"
        submenu "Connection" "Recent" "$1"
    else
        show_keys "Connection ▸ New Connection…"
        connect_to "$1" "$2"
    fi
}

# Record the actions sections call: each becomes a wrapper around a copy of
# itself named _<fn>.
for fn in focus key code repeat_code type_slow set_editor menu submenu \
    click_window click_dialog dialog_field choose_path palette_open connect_to switch_to expect; do
    eval "_$fn() $(declare -f "$fn" | tail -n +2)"
    eval "$fn() { recorded _$fn \"\$@\"; }"
done
_sleep() { command sleep "$@"; }
sleep() { recorded _sleep "$@"; }

# --- preflight ---------------------------------------------------------------

if ! pgrep -x "$PROCESS" >/dev/null; then
    echo "pg-gui is not running; start it with \`cargo run\` first." >&2
    exit 1
fi
if ! osascript -e 'tell application "System Events" to return name of first process' >/dev/null 2>&1; then
    echo "osascript cannot reach System Events; grant this terminal Accessibility permission." >&2
    exit 1
fi

CLIPBOARD="$(pbpaste 2>/dev/null || true)"
trap 'printf "%s" "$CLIPBOARD" | pbcopy; rm -f "$OVERLAY_JS"' EXIT

# --- sections ----------------------------------------------------------------

s1() {
    section "1. Connect"
    step "Connection ▸ New Connection…: connect to the sample database as 'local'"
    connect_to local pgui_test
    expect "'local' in the title-bar combobox; soon the status bar reads 'SQL LSP: schema loaded'."
    manual "optionally open Connection ▸ Edit Connection… and press Test Connection to show the inline check"
}

s2() {
    section "2. Open the project folder"
    step "File ▸ Open Folder… → $SQL_DIR"
    menu "File" "Open Folder…"
    choose_path "$SQL_DIR"
    expect "Files panel shows app/, public/, grants/, examples/."
    manual "type 'order' in the files panel's filter box: matching files stay, folders open. Clear it after."
}

s3() {
    section "3. Browse the database"
    step "Panels toggle with cmd-1 (database), cmd-2 (files), cmd-3 (results)"
    key 1 cmd; sleep 1
    key 1 cmd; sleep 1
    key 2 cmd; sleep 1
    key 2 cmd; sleep 1
    key 3 cmd; sleep 1
    key 3 cmd
    expect "Each panel hides and comes back; the layout is remembered in config.json."

    step "cmd-1 shows the database browser"
    if [ "$(plutil -extract db_panel_visible raw -o - "$CONFIG_JSON" 2>/dev/null || echo true)" = true ]; then
        expect "Already shown (cmd-1 toggles it)."
    else
        key 1 cmd
    fi
    manual "expand app ▸ Tables ▸ orders: its CREATE TABLE DDL, columns with PK / not-null / default markers, the indexes (idx_orders_customer_id, idx_orders_created_at) and the foreign key"
}

s4() {
    section "4. The palette (cmd-p)"
    step "cmd-p, fuzzy 'plaord' → place_order.sql"
    key p cmd; sleep 0.6
    type_slow "plaord"
    expect "R.0.07.04.1__place_order.sql is the top hit."
    sleep 1.5
    code $RETURN

    step "cmd-p, '>size' → snippets mode"
    key p cmd; sleep 0.6
    type_slow ">size"
    expect "Size: database, Size: largest tables, …"
    sleep 2
    code $ESCAPE

    step "cmd-p, ':theme' → commands mode, pick View: Theme Dark"
    key p cmd; sleep 0.6
    type_slow ":theme dark"
    expect "Commands listed with their shortcuts."
    sleep 1.5
    code $RETURN

    step "cmd-shift-p, 'light' → back to the light theme"
    key p cmd shift; sleep 0.6
    type_slow "theme light"
    sleep 1
    code $RETURN

    step "cmd-shift-p, 'dark' → back to dark mode for the rest of the demo"
    key p cmd shift; sleep 0.6
    type_slow "theme dark"
    sleep 1
    code $RETURN
}

s5() {
    section "5. The editor and language support"
    step "New tab (cmd-t) and type 'select * from cu'"
    key t cmd
    type_slow "select * from cu"
    expect "Completion menu offers customers, labelled 'Table · app'."
    sleep 2
    # customers is the second entry: the first is the function
    # customer_open_order_total_amount.
    code $DOWN
    sleep 0.6
    code $RETURN
    type_slow " where c"
    expect "Columns listed as 'Column · text · app.customers'."
    sleep 2.5
    code $ESCAPE

    manual "hover the mouse over 'customers': the language server shows the table's documentation"

    step "Add a broken line 'selec 1;' and press cmd-j"
    set_editor $'select * from customers;\n\nselec 1;\n'
    sleep 2.5
    key j cmd
    expect "Cursor jumps to the syntax error. cmd-k goes back."
    sleep 1.5
    key k cmd

    step "Format a long one-line query with cmd-shift-f"
    set_editor "select c.id, c.name, count(o.id) as orders, sum(o.amount) as total from customers c left join orders o on o.customer_id = c.id where c.country = 'LV' group by c.id, c.name having count(o.id) > 1 order by total desc limit 10;"
    sleep 1
    key f cmd shift
    expect "Query formatted. 'fmt: on' in the status bar toggles format-on-save."

    step "Toggle format-on-save twice through Edit ▸ Format on Save"
    menu "Edit" "Format on Save"
    expect "Status bar now reads 'fmt: off'."
    sleep 1.5
    menu "Edit" "Format on Save"

    step "Select all, cmd-/ comments, cmd-/ again uncomments"
    key a cmd
    key / cmd
    sleep 1.5
    key / cmd

    step "cmd-f search for 'orders'"
    key f cmd
    sleep 0.5
    type_slow "orders"
    sleep 1.5
    code $ESCAPE

    step "Zoom: cmd-plus twice, cmd-minus, cmd-0"
    key = cmd; sleep 0.6
    key = cmd; sleep 1
    key - cmd; sleep 1
    key 0 cmd
}

s6() {
    section "6. Running queries"
    step "Two statements; cmd-enter runs only the one under the cursor"
    key t cmd
    set_editor $'select * from customers where country = \'LV\';\nselect count(*) from orders;\n'
    code $RETURN cmd
    expect "First statement selected; grid shows 500 rows (server-side cursor) and Fetch more."
    manual "press Fetch more under the grid: the next 500 rows are appended"

    step "Open examples/insert_customers.sql, select all, cmd-enter"
    palette_open "insert_customers.sql"
    key a cmd
    code $RETURN cmd
    expect "Statements run one by one; each RETURNING gets its own result-selector entry."
    manual "switch the results panel to Log to watch one line per statement arrive (pg_sleep makes it visible)"

    step "Open examples/insert_customers_fail.sql and run all of it"
    palette_open "customers_fail"
    key a cmd
    code $RETURN cmd
    expect "Third insert violates NOT NULL: log shows the two statements plus the error with detail and position; autocommit rolls the block back."
    manual "look at the Log view"

    step "Server notices: RAISE NOTICE lands in the log"
    key t cmd
    set_editor "do \$\$ begin raise notice 'hello from %', current_user; end \$\$;"
    code $RETURN cmd
    expect "No rows, but the log shows 'NOTICE: hello from pgui'."
}

s7() {
    section "7. Results: copy and export"
    step "Run 'select * from customers;'"
    set_editor "select * from customers;"
    code $RETURN cmd
    manual "click a cell and cmd-c; click a row header (row copied tab-joined), a column header (column); shift-/cmd-click row headers; cmd-a selects every row"

    step "Connection ▸ Export as CSV…"
    menu "Connection" "Export as CSV…"
    expect "Name suggested as customers_<date>; the export re-runs the query and writes every row through COPY."
    manual "save the file (or Cancel). Export as INSERT… writes a runnable seed script instead"
}

s8() {
    section "8. Transactions and cancel"
    step "Session ▸ Toggle Autocommit (off for this tab)"
    menu "Session" "Toggle Autocommit"

    step "update orders … where id = 1, then read it back"
    set_editor "update orders set status = 'cancelled' where id = 1;"
    code $RETURN cmd
    sleep 1.5
    set_editor "select status from orders where id = 1;"
    code $RETURN cmd
    expect "Reads 'cancelled' inside the open transaction."

    step "Session ▸ Rollback, then re-run the select"
    menu "Session" "Rollback"
    sleep 1
    code $RETURN cmd
    expect "Status back to its original value."

    step "Autocommit back on"
    menu "Session" "Toggle Autocommit"

    step "select pg_sleep(30), then Session ▸ Cancel Query"
    set_editor "select pg_sleep(30);"
    code $RETURN cmd
    sleep 3
    menu "Session" "Cancel Query"
    expect "Server cancels the statement; the log shows the cancellation."
}

PLAN_QUERY="select c.country, count(*), sum(o.amount)
from customers c
join orders o on o.customer_id = c.id
where o.created_at > now() - interval '30 days'
group by c.country
order by 3 desc;"

s9() {
    section "9. Query plans as a flame graph"
    step "cmd-e on the aggregate query"
    key t cmd
    set_editor "$PLAN_QUERY"
    key e cmd
    expect "Plan drawn as a flame graph, frame width = estimated cost."

    step "cmd-shift-e: EXPLAIN (ANALYZE, BUFFERS)"
    key e cmd shift
    expect "Frame widths now actual time × loops."
    manual "hover a frame for its numbers, click one to zoom, Reset zoom to come back"
}

s10() {
    section "10. Objects open from their files, Go to Definition"
    manual "in the database browser (cmd-1) expand app ▸ Functions and click place_order: the working folder's R.0.07.04.1__place_order.sql opens (it matches definition_file_mask), not a catalog dump. customer_order_summary opens the same way"

    step "Go to Definition from the editor"
    key t cmd
    set_editor "call place_order(1, 10);"
    manual "hold cmd and click 'place_order' (or right-click ▸ Go to Definition): R.0.07.04.1__place_order.sql opens; it is pg-gui's own catalog lookup, so it works with the language server down too"
}

s11() {
    section "11. Step-debugging PL/pgSQL"
    step "Open app/functions/R.0.07.01.1__add.sql"
    palette_open "__add"
    expect "Gutter numbers like Postgres: the 'AS \$\$' line is 1, lines above read 0, -1, …"
    manual "click the gutter next to 'total := total + 1;' to set a breakpoint"

    step "cmd-shift-d: Debug routine dialog, arguments 2, 3, stop on entry"
    code $UP cmd
    repeat_code 10 $DOWN
    key d cmd shift
    sleep 0.8
    expect "Routine 'app.add' filled in from the cursor."
    code $TAB
    type_slow "2, 3"
    sleep 1
    code $RETURN
    expect "Debug panel replaces the results; editor highlights the executing line; Variables: a, b, total, i."

    step "F5: continue to the breakpoint"
    code $F5
    expect "Stopped on the first pass through the loop, total = 2 (no breakpoint: runs to the end, returns 5)."

    manual "click 'total' in Variables and set it to 100"

    step "F10 steps one line at a time, watch total"
    repeat_code 3 $F10
    sleep 1

    step "F5 until the function returns"
    repeat_code 4 $F5
    expect "Returns 103 instead of 5 if total was edited."
}

s12() {
    section "12. Running scripts as a batch"
    manual "in the files panel click examples/insert_customers.sql, then cmd-click examples/long_script.sql: a 'Run 2 scripts' bar appears"

    step "cmd-shift-enter: confirmation names the connection and lists the files"
    code $RETURN cmd shift
    manual "confirm: files run in tree order from disk, each headed '▶ <path>' in the log; long_script streams ~120 lines with RAISE NOTICEs"

    manual "right-click a folder ▸ New Script…, name it 'demo': demo.sql opens. Rename… and Delete… are in the same menu"
}

s13() {
    section "13. Snippets with tab stops"
    step "Empty tab, cmd-p '>new table', Enter"
    key t cmd
    key p cmd; sleep 0.6
    type_slow ">new table"
    sleep 1
    code $RETURN
    sleep 0.5
    expect "CREATE TABLE template, table_name selected."
    type_slow "demo_items"
    code $TAB
    type_slow "item_id"

    step "Type 'create seq' on a new line"
    key a cmd
    code $DELETE
    type_slow "create seq"
    expect "Sequence template offered in completions as 'Snippet'."
    sleep 2.5
    code $ESCAPE
    expect "Own snippets: save .sql files into snippets/ next to config.json (cmd-, opens it)."
}

s14() {
    section "14. Connections own their tabs"
    step "Connect as 'admin' (same server, database postgres)"
    connect_to admin postgres
    sleep 2

    step "Untitled tab with some text on 'admin'"
    key t cmd
    set_editor "-- this tab belongs to admin
select datname from pg_database;"

    step "Switch back to 'local'"
    switch_to local pgui_test
    expect "Untitled tab parked with admin; results swap to local's; file-backed tabs stay."

    step "Switch to 'admin' again"
    switch_to admin postgres
    expect "Parked tab back in its slot."

    step "And back to 'local'"
    switch_to local pgui_test
}

s15() {
    section "15. Odds and ends"
    step "View ▸ Theme ▸ Dark, then System"
    submenu "View" "Theme" "Dark"
    sleep 2
    submenu "View" "Theme" "System"

    step "cmd-h: every keybinding in one dialog"
    key h cmd
    sleep 3
    code $ESCAPE

    step "cmd-, (pg-gui ▸ Preferences…): open the settings file in your editor"
    key , cmd
    sleep 2
    expect "config.json opens in the system's default editor for .json; the status bar names its path."
    manual "change a setting there (e.g. \"format_on_save\": false) and save: pg-gui picks it up live ('fmt: off'). Change it back, then return to pg-gui"
    focus

    expect "AI completion: export ANTHROPIC_API_KEY before cargo run, then cmd-i."
    expect "Changed on disk: edit an open file elsewhere and pg-gui offers to reload."
}

# --- run -------------------------------------------------------------------

LAST_SECTION=15
if [ ${#SECTIONS[@]} -eq 0 ]; then
    SECTIONS=(1)
fi
if [ "$ONLY" = 0 ]; then
    if [ ${#SECTIONS[@]} -gt 1 ]; then
        echo "give one section to start from, or --only with a list" >&2
        exit 1
    fi
    case "${SECTIONS[0]}" in
        ''|*[!0-9]*) echo "no section ${SECTIONS[0]} (1-$LAST_SECTION)" >&2; exit 1 ;;
    esac
    if [ "${SECTIONS[0]}" -lt 1 ] || [ "${SECTIONS[0]}" -gt "$LAST_SECTION" ]; then
        echo "no section ${SECTIONS[0]} (1-$LAST_SECTION)" >&2
        exit 1
    fi
    SECTIONS=($(seq "${SECTIONS[0]}" "$LAST_SECTION"))
fi

for s in "${SECTIONS[@]}"; do
    if ! declare -f "s$s" >/dev/null; then
        echo "no section $s (1-$LAST_SECTION)" >&2
        exit 1
    fi
done

for s in "${SECTIONS[@]}"; do
    "s$s"
done

if [ "$AUTO" = 0 ]; then
    advance ""
    prompt_replay "[Enter] finish, r = repeat last step" || true
fi

section "Done"
printf 'Reset the sample data (run it yourself):\n  docker compose down -v && docker compose up -d --build --wait\n'
