# mcu

A terminal interface for querying [Axiom](https://axiom.co). You write
MPL in a vim-style editor, charts render alongside, and you can browse
or edit Axiom dashboards inline.

It uses the same `~/.axiom.toml` config file as the official Axiom
CLI, so if you've already authenticated there, you're done.

## Installing

There's no published binary yet. Build from source:

```sh
git clone <this-repo>
cd mcu
cargo install --path .
```

Rust 1.85 or newer.

## Configuration

If you don't already have an `~/.axiom.toml`:

```toml
active_deployments = "prod"

[deployments.prod]
url    = "https://api.axiom.co"
token  = "xaat-..."
org_id = "..."
```

`active_deployments` is optional when you only have one entry.

## Running

`mcu` opens an empty editor; your last query comes back from
the cache. To open a file:

```sh
mcu my-query.mpl
```

To jump straight into a dashboard:

```sh
mcu -d <dashboard-uid>
```

If your MPL declares parameters (`param $host: string;`) you can
supply values from the command line:

```sh
mcu -p host=db-01 -p region=us-east
```

`--help` prints the full list.

## Using it

Type MPL, hit Enter, see the chart. Use `:` for commands (`:w` to save
the file, `:q` to quit, `:open <uid>` to load a dashboard). Use `?`
for the full key reference at any time.

The chart kind is picked by a comment at the top of the buffer:

```
// @viz line
```

Change `line` to `bar`, `area`, `scatter`, `pie`, `heatmap`, `table`,
`top_list`, `statistic`, or `note` to switch chart kinds without
touching the query.

### Dashboards

`:dash ls` opens a searchable picker over every dashboard in your
workspace. Pick one and you land in a grid view; press Enter on a
tile to zoom into a single chart with the editor showing its query.
Edits save back to Axiom — change the query, move tiles around with
`m`/`s` (auto-shove cascades) or `:tile mv!` / `:tile size!`, then
`:w` (refuses if someone else bumped the version) or `:w!` to
overwrite.

### Time range

`:time` opens a preset menu (last 5 minutes, last hour, today,
yesterday, …) or a calendar for custom ranges. The range applies to
whichever query or dashboard is in front of you.

### Parameters

If your query has `param $host: string;` at the top, the param panel
on the right shows it; press `i` in the panel to fill in a value, or
set it from the command line with `:p host=db-01`. The same panel is
how you clear a value (`x`) or wipe everything (`:p!`).

## Keys

Full reference: [`docs/keys.md`](docs/keys.md). Same file the in-app
`?` renders. The keymap is vim-flavoured: `hjkl`, `gg`/`G`,
`dd`/`yy`/`p`, `:` for commands, visual mode, operators on text
objects, the usual.

A few things to know up front:

- `:q` is the only quit. There's no bare `q` shortcut anywhere.
- `Esc` always closes the current thing — an overlay, a picker, a
  zoomed-in chart back to the grid, or a sidebar pane back to the
  editor.
- Ex-command completion is fuzzy: `:dl<Tab>` finds `:dash ls`,
  `:tm<Tab>` finds `:time`. Plain prefix matches still come first.

## Where state lives

Datasets, metric lists, dashboard listings, and the last query you
edited are cached under `$XDG_CACHE_HOME/mcu/` (or the
platform equivalent). Deleting the directory resets the app; nothing
on the Axiom server is affected.

## Limitations

APL queries inside dashboards are shown but not executed — only MPL
tiles fetch live data.
