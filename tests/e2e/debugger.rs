//! PL/pgSQL step debugging through pldebugger. Needs `PG_GUI_E2E_DATABASE`
//! pointing at a server with `plugin_debugger` preloaded and the `pldbgapi`
//! extension created — the project's `docker compose` server has both.
//!
//! The debug panel cannot be read from outside the window, so the scenario
//! watches the routine's effects instead: the procedure commits after each
//! insert, so every statement's row turns visible to the scenario's own
//! connection the moment the debugger lets that statement run, and not
//! before.

use std::time::{Duration, Instant};

use crate::harness::{self, App, Key, Mod, Schema};

/// How long one Step Over gets to show its row before it is pressed again.
const STEP_WAIT: Duration = Duration::from_millis(1500);

#[test]
fn step_over_and_continue_procedure() {
    let Some(mut schema) = Schema::create("debugger::step_over_and_continue_procedure") else {
        return;
    };
    let name = schema.name().to_string();
    // The tab holds exactly what the server has, so the debugger can match
    // the body it steps through to the editor's lines.
    let procedure = format!(
        "create procedure {name}.debug_steps()\n\
         language plpgsql\n\
         as $$\n\
         begin\n\
         \x20   insert into {name}.steps values (1);\n\
         \x20   commit;\n\
         \x20   insert into {name}.steps values (2);\n\
         \x20   commit;\n\
         end;\n\
         $$;\n"
    );
    schema.execute("create table {schema}.steps (n int)");
    schema.execute(&procedure);
    let mut app = App::launch_with_database(&schema, &procedure);

    // cmd-shift-d opens the launch dialog with the signature taken from the
    // statement under the cursor (the buffer's start) and stop-on-entry
    // checked; Enter starts the session.
    app.send_keys(&[Mod::Cmd, Mod::Shift], 'd');
    std::thread::sleep(Duration::from_millis(500));
    app.press(&[], Key::Return);

    // Stopped on entry, nothing has run. Step Over until the first insert
    // has committed: the session takes a moment to attach, and the entry
    // stop sits before the first statement, so how many presses that takes
    // is not fixed — but each runs at most one statement.
    let start = Instant::now();
    'stepping: loop {
        app.press(&[], Key::F10);
        let pressed = Instant::now();
        while pressed.elapsed() < STEP_WAIT {
            if schema.row_count("steps") > 0 {
                break 'stepping;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            start.elapsed() < harness::DB_TIMEOUT,
            "Step Over never ran the first insert"
        );
    }
    // Held there: a routine that ran straight through (a breakpoint that
    // never trapped) could be caught between its two commits, so the second
    // insert has to stay out for a while, not just at one look.
    let held = Instant::now();
    while held.elapsed() < STEP_WAIT {
        assert_eq!(
            schema.row_count("steps"),
            1,
            "one step runs one statement; the second insert still waits"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Continue runs the rest of the routine.
    app.press(&[], Key::F5);
    app.wait_for("the routine to finish", harness::DB_TIMEOUT, |_| {
        schema.row_count("steps") == 2
    });

    app.quit();
}
