//! PL/pgSQL step debugging through pldebugger. Needs `PG_GUI_E2E_DATABASE`
//! pointing at a server with `plugin_debugger` preloaded and the `pldbgapi`
//! extension created — the project's `docker compose` server has both.
//!
//! The debug panel cannot be read from outside the window, so the scenarios
//! watch the routine's effects instead: every statement of the procedure
//! calls `nextval` on a sequence, and sequences are not transactional, so
//! the scenario's own connection sees each call the moment the debugger
//! lets that statement run, and not before. The routine holds no `COMMIT`:
//! an earlier version committed after each statement, and continuing from a
//! breakpoint after such a commit hung, leaving the target backend spinning
//! in pldebugger, deaf even to `pg_terminate_backend`, until the server was
//! restarted.

use std::time::{Duration, Instant};

use crate::harness::{self, App, Key, Schema};

/// How long one Step Over gets to show its call before it is pressed again,
/// and how long a stopped session must hold off the next one.
const STEP_WAIT: Duration = Duration::from_millis(1500);
/// Height of the editor panel, in points: room for all eleven lines of
/// [`create_steps_procedure`]'s procedure at 20 each.
const EDITOR_HEIGHT: f64 = 260.;

/// Wait until exactly `steps` of the procedure's statements have run.
fn wait_steps(app: &mut App, schema: &mut Schema, steps: i64) {
    app.wait_for(
        &format!("{steps} statement(s) run"),
        harness::DB_TIMEOUT,
        |_| schema.sequence_calls("steps") == steps,
    );
}

/// Check that the session stays stopped after `steps` statements. A
/// routine that ran straight through (a breakpoint that never trapped)
/// could be caught between two statements, so the next one has to stay
/// off for a while, not just at one look.
fn hold_steps(schema: &mut Schema, steps: i64) {
    let held = Instant::now();
    while held.elapsed() < STEP_WAIT {
        assert_eq!(
            schema.sequence_calls("steps"),
            steps,
            "the session runs no further than it was told to"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Create the sequence `steps` and a procedure `debug_steps()` of four
/// statements, each calling `nextval` on it, and return the procedure's
/// `CREATE`. Each statement has a line of its own: see [`step_row`].
fn create_steps_procedure(schema: &mut Schema) -> String {
    let name = schema.name().to_string();
    let statements = format!("    perform nextval('{name}.steps');\n").repeat(4);
    let procedure = format!(
        "create procedure {name}.debug_steps()\n\
         language plpgsql\n\
         as $$\n\
         begin\n\
         {statements}\
         end;\n\
         $$;\n"
    );
    schema.execute("create sequence {schema}.steps");
    schema.execute(&procedure);
    procedure
}

/// Buffer row of statement `n` (1 = first) in [`create_steps_procedure`]'s
/// procedure.
fn step_row(n: u32) -> u32 {
    n + 3
}

/// Start the app on `schema`'s database with the procedure in the tab — it
/// holds exactly what the server has, so the debugger can match the body it
/// steps through to the editor's lines — laid out so the editor's gutter
/// sits where [`App::toggle_breakpoint`] expects it, and tall enough to
/// show every line of the procedure: the default height cuts the last
/// statement's line in half, and its breakpoint dot with it.
fn launch(schema: &Schema, procedure: &str) -> App {
    App::launch_for_results_with(Some(schema.url()), procedure, |seed| {
        seed["editor_height"] = serde_json::Value::from(EDITOR_HEIGHT);
    })
}

#[test]
fn step_over_and_continue_procedure() {
    let Some(mut schema) = Schema::create("debugger::step_over_and_continue_procedure") else {
        return;
    };
    let procedure = create_steps_procedure(&mut schema);
    let mut app = launch(&schema, &procedure);
    app.start_debug(true);

    // Stopped on entry, nothing has run. Step Over until the first
    // statement has run: the session takes a moment to attach, and the entry
    // stop sits before the first statement, so how many presses that takes
    // is not fixed — but each runs at most one statement.
    let start = Instant::now();
    'stepping: loop {
        app.press(&[], Key::F10);
        let pressed = Instant::now();
        while pressed.elapsed() < STEP_WAIT {
            if schema.sequence_calls("steps") > 0 {
                break 'stepping;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            start.elapsed() < harness::DB_TIMEOUT,
            "Step Over never ran the first statement"
        );
    }
    hold_steps(&mut schema, 1);

    // Continue runs the rest of the routine.
    app.press(&[], Key::F5);
    wait_steps(&mut app, &mut schema, 4);

    app.quit();
}

#[test]
fn breakpoints_stop_where_set() {
    let Some(mut schema) = Schema::create("debugger::breakpoints_stop_where_set") else {
        return;
    };
    let procedure = create_steps_procedure(&mut schema);
    let mut app = launch(&schema, &procedure);

    // Breakpoints on the second and third statements, and no stop on entry:
    // the session runs by itself up to the first of them.
    app.toggle_breakpoint(step_row(2));
    app.toggle_breakpoint(step_row(3));
    app.start_debug(false);
    wait_steps(&mut app, &mut schema, 1);
    hold_steps(&mut schema, 1);

    // Continue stops again at the next one.
    app.press(&[], Key::F5);
    wait_steps(&mut app, &mut schema, 2);
    hold_steps(&mut schema, 2);

    // And past the last one runs to the end.
    app.press(&[], Key::F5);
    wait_steps(&mut app, &mut schema, 4);

    app.quit();
}

#[test]
fn breakpoints_toggled_mid_session() {
    let Some(mut schema) = Schema::create("debugger::breakpoints_toggled_mid_session") else {
        return;
    };
    let procedure = create_steps_procedure(&mut schema);
    let mut app = launch(&schema, &procedure);

    app.toggle_breakpoint(step_row(2));
    app.toggle_breakpoint(step_row(3));
    app.start_debug(false);
    wait_steps(&mut app, &mut schema, 1);
    hold_steps(&mut schema, 1);

    // While stopped: drop the third statement's breakpoint and arm the
    // fourth's. Continue then runs through the third and stops at the
    // fourth.
    app.toggle_breakpoint(step_row(3));
    app.toggle_breakpoint(step_row(4));
    app.press(&[], Key::F5);
    wait_steps(&mut app, &mut schema, 3);
    hold_steps(&mut schema, 3);

    app.press(&[], Key::F5);
    wait_steps(&mut app, &mut schema, 4);

    app.quit();
}
