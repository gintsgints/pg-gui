# pg-gui demo scenario

A guided tour through every pg-gui feature, run against the sample database
that ships in this repository. Follow it top to bottom (about 20 minutes), or
jump to the section you want to show. Each step says what to do, then what you
should see (**→**).

On Linux, read `cmd` as `ctrl` throughout (`F1` replaces `cmd-h`).

## 0. Setup

Start the sample server. It runs Postgres 17 with `pg_stat_statements` and the
pldebugger extension, seeded from `sql/` with 10k customers and 100k orders in
the `app` schema:

```sh
docker compose up -d --build --wait   # host port 5433, pgui / pgui / pgui_test
cargo run
```

The seed puts `app` on the `pgui` role's `search_path`, so `customers` and
`orders` resolve without a schema prefix.

## 1. Connect

1. **Connection ▸ New Connection…**
2. Fill in Name `local`, Host `localhost`, Port `5433`, Database `pgui_test`,
   Username `pgui`, Password `pgui`. Leave Schema empty.
   **→** The *Connection string* preview builds itself as you type.
3. **Test Connection**.
   **→** The dialog reports success inline, so a typo is caught before the first query.
4. **Connect**.
   **→** `local` appears in the title-bar combobox. After a moment the status
   bar reads `SQL LSP: schema loaded`, which means schema-aware completions
   are live.

## 2. Open the project folder

1. **File ▸ Open Folder…** and choose this repo's `sql/` folder.
   **→** The files panel (`cmd-b` / `cmd-2`) shows the tree: `app/`, `public/`,
   `grants/`, `examples/`.
2. Type `order` in the panel's filter box.
   **→** Only matching files remain, and their folders open automatically.
   Clear the filter afterwards.

## 3. The palette (`cmd-p`)

1. `cmd-p`, type `plaord`.
   **→** `app/functions/R.0.07.04.1__place_order.sql` is the top hit, because
   fuzzy matching finds the scattered characters. Press Enter to open it.
2. `cmd-p`, type `>size`.
   **→** The list switches to snippets: `Size: database`, `Size: largest tables`, …
   Press Escape.
3. `cmd-p`, type `:theme`.
   **→** It now lists commands, each with its shortcut. Pick `View: Theme Dark`.
   Then `cmd-shift-p`, type `light`, and press Enter to switch back.

## 4. The editor and language support

Open a new tab with `cmd-t`.

1. Type `select * from cu`.
   **→** The completion menu offers `customers`, labelled `Table · app`.
   Accept it, then type ` where ` and start a column name: the columns are
   listed as `Column · text · app.customers`, and so on.
2. Hover over `customers`.
   **→** The language server shows documentation for the table.
3. Add a broken line: `selec 1;`, then press `cmd-j`.
   **→** The cursor jumps to the syntax error. `cmd-k` goes back through the problems.
   Delete the line.
4. Type a long query on one line and press `cmd-shift-f`.
   **→** The query is formatted. Click `fmt: on` in the status bar to toggle
   format-on-save. The setting is also under **Edit ▸ Format on Save**.
5. Select some lines and press `cmd-/`.
   **→** The lines are commented. Press `cmd-/` again to uncomment them.
6. `cmd-f` searches the script and `cmd-r` finds and replaces. `cmd-plus`,
   `cmd-minus` and `cmd-0` zoom.

## 5. Running queries

1. In the scratch tab, write two statements:

   ```sql
   select * from customers where country = 'LV';
   select count(*) from orders;
   ```

   Put the cursor on the first one and press `cmd-enter`.
   **→** Only that statement runs, and it gets selected so you can see what
   was sent. The grid shows the first 500 rows (a server-side cursor), with
   **Fetch more** under it.
2. Press **Fetch more**.
   **→** The next 500 rows are appended, and nothing beyond that is transferred.
3. Open `examples/insert_customers.sql` (from the palette or the files panel),
   press `cmd-a`, then `cmd-enter`.
   **→** The statements run one by one. `cmd-3` shows the results panel; switch
   it to **Log** to watch one line per statement arrive with its timing (the
   `pg_sleep` calls make this visible). Each `RETURNING` gets its own entry on
   the result selector.
4. Open `examples/insert_customers_fail.sql` and run all of it.
   **→** The third insert violates `NOT NULL`. The log shows the two statements
   that ran plus the error, with Postgres' detail and position. Autocommit
   rolls the whole block back.
5. Server notices:

   ```sql
   do $$ begin raise notice 'hello from %', current_user; end $$;
   ```

   **→** The query returns no rows, but the log shows `NOTICE: hello from pgui`.

## 6. Results: copy and export

1. Run `select * from customers;`. Click a cell and press `cmd-c`.
   **→** The cell's value is copied. Click a row header to copy the whole row
   (tab-joined), or a column header to copy the column.
2. Shift-click and cmd-click row headers.
   **→** You get a range or a toggled multi-row selection. `cmd-a` selects
   every row, across all pages.
3. **Connection ▸ Export as CSV…**
   **→** The file name is suggested as `customers_<date>`. The export re-runs
   the query and writes *every* row through `COPY`, not just the fetched 500.
   **Export as INSERT…** writes a runnable seed script instead.

## 7. Transactions and cancel

1. **Session ▸ Toggle Autocommit** to turn it off for this tab.
2. Run `update orders set status = 'cancelled' where id = 1;`, then
   `select status from orders where id = 1;`.
   **→** It reads `cancelled` inside the open transaction.
3. **Session ▸ Rollback**, then re-run the `select`.
   **→** The status is back to its original value. Turn autocommit back on.
4. Run `select pg_sleep(30);`, then **Session ▸ Cancel Query**.
   **→** The server cancels the statement, and the log shows the cancellation.

## 8. Query plans as a flame graph

1. Put the cursor on:

   ```sql
   select c.country, count(*), sum(o.amount)
   from customers c
   join orders o on o.customer_id = c.id
   where o.created_at > now() - interval '30 days'
   group by c.country
   order by 3 desc;
   ```

   and press `cmd-e`.
   **→** The plan is drawn as a flame graph, with each frame as wide as its
   estimated cost.
2. Press `cmd-shift-e`.
   **→** `EXPLAIN (ANALYZE, BUFFERS)` runs. Frame widths are now actual time ×
   loops. Hover a frame for its numbers and click one to zoom in. **Reset
   zoom** comes back out.

## 9. Object browser and Go to Definition

1. `cmd-1` shows the database browser. Expand `app ▸ Tables ▸ orders`.
   **→** You see its `CREATE TABLE` DDL, the columns with PK / not-null /
   default markers, the indexes (`idx_orders_customer_id`,
   `idx_orders_created_at`) and the foreign-key constraint.
2. Expand **Functions** and click `place_order`.
   **→** The working folder's `R.0.07.04.1__place_order.sql` opens, because
   it matches `definition_file_mask` (`*_{object}.sql`), not a catalog dump.
   Click `customer_order_summary`: it opens from its file the same way.
3. In a scratch tab, type `call place_order(1, 10);`. Hold `cmd` and click
   `place_order`.
   **→** The same file opens. Go to Definition is pg-gui's own catalog lookup,
   so it also works while the language server is down. Right-click ▸ **Go to
   Definition** does the same.

## 10. Step-debugging PL/pgSQL

1. Open `app/functions/R.0.07.01.1__add.sql`.
   **→** The gutter numbers lines the way Postgres does. The `AS $$` line is
   `1` and the lines above it read `0`, `-1`, …, so error messages and stack
   frames line up with the editor.
2. Click the gutter next to `total := total + 1;` to set a breakpoint.
3. `cmd-shift-d`.
   **→** The *Debug routine* dialog opens with `app.add` already filled in from
   the cursor. Enter arguments `2, 3`, keep **Stop on entry** checked, and
   press **Debug**.
   **→** The debug panel replaces the results. The editor highlights the
   executing line, and the Variables pane shows `a`, `b`, `total`, `i`.
4. Press `F5` to continue to the breakpoint.
   **→** The session stops on the first pass through the loop, with `total` = `2`.
5. Click `total` in the Variables pane and set it to `100`. Press `F5` until
   the function returns.
   **→** `total` counts up on each pass (`F10` steps one line at a time
   instead), and the call returns `103` instead of `5`, because you edited the
   running frame.
6. Try `place_order` with arguments `1, 99.50` and `F11` (step into).
   `shift-F5` stops a session at any time.

## 11. Running scripts as a batch

1. In the files panel, click `examples/insert_customers.sql`, then cmd-click
   `examples/long_script.sql`.
   **→** A "Run 2 scripts" bar appears above the tree.
2. Press `cmd-shift-enter` (or the bar).
   **→** A confirmation dialog names the target connection (`local`) and lists
   the files.
3. Confirm.
   **→** The files run in tree order, from disk. The log heads each one with
   `▶ <path>`, and the long script streams about 120 lines, including its
   `RAISE NOTICE`s. A failing file stops the batch, like `psql -v ON_ERROR_STOP=1`.
4. Right-click a folder ▸ **New Script…**, name it `demo`.
   **→** `demo.sql` is created inside that folder and opens in a tab.
   **Rename…** and **Delete…** are in the same menu.

## 12. Snippets with tab stops

1. In an empty tab, `cmd-p`, type `>new table`, and press Enter.
   **→** A `CREATE TABLE` template is inserted with `table_name` selected.
   Type `demo_items`, press Tab, and type the next field.
2. Type `create seq` in the editor.
   **→** The sequence template is offered in the completion menu as `Snippet`.
3. Save any `.sql` file into the `snippets/` folder next to `config.json`
   (`cmd-,` opens that file).
   **→** It appears under *Your snippets* the next time you open `>`.

## 13. Connections own their tabs

1. Create a second connection, `admin`: the same server with Database
   `postgres`. Open an untitled tab and type something into it.
2. Switch back to `local` from the title-bar combobox.
   **→** The untitled tab is parked with `admin` and every tab's results swap
   to `local`'s. File-backed tabs stay open. Switch to `admin` again and the
   parked tab returns to its slot.

## 14. Odds and ends

- **View ▸ Theme**: Catppuccin Latte, Catppuccin Mocha, or follow the system.
- `cmd-h`: every keybinding in one dialog.
- **AI completion** (optional): `export ANTHROPIC_API_KEY=…` before
  `cargo run`, then `cmd-i` completes the SQL at the cursor.
- **Changed on disk**: edit an open file in another editor and pg-gui offers
  to reload it.

## Reset

The tour inserts customers and runs the long script. To start over with a
fresh seed:

```sh
docker compose down -v && docker compose up -d --build --wait
```
