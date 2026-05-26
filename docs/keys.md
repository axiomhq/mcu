# Key bindings

This file is the single source of truth for the in-app help modal.
Open it from anywhere in mcu with `?`. Any key dismisses it.

## Conventions

- **`:q` is the only quit.** There is no bare `q` shortcut anywhere
  — panes and overlays use Esc instead.
- **Esc closes / steps back.** It dismisses the current overlay or
  steps back one level toward the editor: error overlay → cleared;
  picker → closed; Solo view → back to Grid; sidebar pane → back
  to editor. Solo view is conceptually a full-screen overlay over
  the dashboard grid, so Esc returns to Grid.
- **`Enter` commits in pickers and forms**, and runs the query in
  the editor.
- **`!` is reserved for vim-standard "force".** `:q!`, `:e!`,
  `:wq!`, and `:w!` follow vim semantics. `:w!` on a server-loaded
  dashboard forces `overwrite=true` past the server's version
  check; plain `:w` refuses to stomp a concurrent edit. Other
  destructive commands (`:dash rm`, `:tile rm`) take no bang and
  mean what they say.

Format used by the in-app renderer:

- Lines starting with `## ` become a section heading.
- Other non-blank lines are split on a tab character: everything before
  the tab is the key binding column, everything after is the
  description. Use a literal `<TAB>` between the two; multiple spaces
  are kept as-is (the renderer only splits on tabs).
- Blank lines insert a vertical gap between rows.
- Lines starting with `#` (single hash) are treated as a comment and
  dropped — use them for notes that should not surface in the UI.

## Normal mode: motion
h j k l	left / down / up / right
w / b / e	word forward / back / end
0 / $	line start / end
gg / G	top / bottom of file
^	first non-blank char on line
f<c> / F<c>	find char on line forward / back (lands on)
t<c> / T<c>	till char on line forward / back (lands before)
; / ,	repeat last f/t (same / reverse direction)
[count] motion	e.g. 5j, 12w — repeat the motion

## Normal mode: insert
i / a	insert at / after cursor
I / A	insert at line start / end
o / O	open new line below / above

## Normal mode: edit
x	delete char under cursor
dd / cc / yy	delete / change / yank current line
d{motion}	delete to motion (e.g. dw, db, de, 3dd)
c{motion}	change to motion + Insert (cw, ciw, cb)
y{motion}	yank to motion (yw, yiw, Y = yy)
p / P	paste after / before (linewise opens a line)
>> / <<	indent / dedent current line
.	repeat last change
u / Ctrl-r	undo / redo

## Visual mode
v / V	enter charwise / linewise Visual
d / c / y / x	act on the selection, exit Visual
>> / <<	indent / dedent the selected lines
Esc	exit Visual without applying

## Text objects (after d / c / y)
iw / aw	inner / around word
i" i' i`	inside string literal
i( i[ i{ i<	inside bracket pair (a* includes brackets)

## Panes
Ctrl-w w	cycle focus (Editor ↔ Legend ↔ Params ↔ Dashboard)
Ctrl-w j / k / h / l	focus pane in the given direction
Esc	from any pane: return to editor

## Legend (when focused)
j / k	move selection
gg / G	first / last series
Space / Enter	toggle series visibility
a	toggle all (show all if any hidden, else hide all)
e	edit tag picker for the selected series
  j / k	move tag cursor (in picker)
  Space	toggle tag as legend label
  Esc / e	close picker
Esc / h	back to editor

## Params pane
j / k	move selection
a / i	add a new param (drops into `:p NAME=` cmdline)
e / Enter	edit selected param
x	clear the selected param
Esc / h	back to editor

## Dashboard pane (grid view)
h j k l / ←↓↑→	spatially move tile selection (accepts a count: `3j`)
Tab / Shift-Tab	cycle tile selection in layout order
Enter / v	zoom selected tile into solo view
m / s	enter Move / Resize sub-mode (Enter to confirm, Esc to cancel)
		  arrows now auto-shove neighbours instead of blocking;
		  cascade is right-first, falling through to a new row
		  below when the chain wraps past column 12.
		  Esc reverts every shoved tile to its pre-submode position.
a	add a new tile (kind picker overlay, placed at first free slot)
d	delete selected tile (y to confirm)
y	yank focused tile (count: `3y` yanks 3 tiles in row-major order)
x	cut focused tile (delete + yank, no confirm; count: `2x`)
p / P	paste yanked tile(s) below / above focused; preserves block shape
o / O	open a new tile in a new row below / above focused
		  (count: `3o` repeats; kind picker prompts once and is reused)
u	one-level dashboard undo (toggles with redo on second press)
Ctrl-d / Ctrl-u	scroll grid down / up by 10 rows
Ctrl-f / Ctrl-b	scroll grid down / up by 20 rows
g / G	jump to top / bottom of grid
:	open the ex-command line (returns to grid on Enter/Esc)
Esc	back to editor

## App-wide
r / Enter	run query
g a	open quick-fix picker for diagnostic under cursor
K	hover docs for symbol under cursor
D / M	refresh datasets / metrics for current dataset
:	command line
?	show this help
Esc	dismiss error overlay; otherwise return to dashboard grid when in solo view

## Insert mode
Tab / Ctrl-Space	open completion popup
Up/Down or Ctrl-p/n	select previous/next item (popup)
Tab / Enter	accept selected completion (popup)
Esc	close popup / return to Normal

## Command mode (`:`)
:w [path]	write buffer (to path, or current file)
:wq / :x	write and quit (always / when dirty)
:e <path> / :e!	edit file (force-reload current with `!`)
:q / :q!	quit / force-quit (discard changes)
:r / :run	run current context (editor query in Solo, focused tile in Grid)
:run tile	refetch the focused tile
:run dashboard	refetch every tile on the loaded dashboard
:ds / :datasets	refresh dataset list
:m / :metrics	refresh metrics for the current dataset
:refresh	refresh datasets and rerun
:p / :param	set/clear params (`:p NAME=VAL` set, `:p NAME=` clear, `:p!` clear all)
:viz <kind>	switch viz kind for the focused tile
:ax / :axiom	open the current query in the Axiom web UI
:trace	report the trace id of the focused panel
:time	open the time-range picker (presets + Custom calendar)
:time reset	restore the default time range (now-1h → now)
:time <start> [end]	set start (and end) directly — relative or RFC3339
:grid / :solo	switch between dashboard grid and single-panel view
:help / :h	show this help

## Dashboards
:dash ls	open the searchable dashboard picker
:open [uid]	open a dashboard by uid (or retry the last picked)
:w / :w!	save the loaded dashboard (PUT to server; `:w!` overwrites past version check)
:dash rm <uid>	delete a dashboard by uid
:dash new <name>	create a new empty dashboard
:dashinfo / :di	toggle the dashboard summary overlay
:grid / :solo	switch between grid and single-tile views
:tile add / rm / mv / size / title	per-tile commands (see :h)
:tile mv! <x> <y>	move with auto-shove (collisions cascade right/down)
:tile size! <w> <h>	resize with auto-shove
:tile yank [n] / cut [n]	capture / cut n tiles into the tile yank register
:tile paste [n]	paste below focused; `:tile paste! [n]` pastes above
:tile open <kind> [n]	open n new rows below focused; `:tile open! ...` above
:tile undo	one-level dashboard undo (toggles with redo)

## Dashboard picker (`:dash ls`)
type	filter as you type
j / k / ↑ / ↓	move cursor
Enter	open selected dashboard
Esc	close picker

## Time picker (`:time`)
j / k / ↑ / ↓	move cursor between presets
Enter	apply selected preset (or open Custom calendar)
g / G	jump to first / last entry
Esc	close picker

## Time picker — Custom calendar
Tab / Shift-Tab	switch focus between Start and End
h / l / ← / →	previous / next day
j / k / ↓ / ↑	previous / next week
< / >	previous / next month
Enter	apply (start = 00:00:00Z, end = 23:59:59Z)
Esc	back to preset list
