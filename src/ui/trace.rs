//! Trace view renderer (`ViewMode::Trace`).
//!
//! Step 23: tree + waterfall on the left, span-detail pane on
//! the right. The body splits 65/35 with a soft 20-column floor
//! on the detail pane; tighter terminals collapse to "tree only"
//! to avoid rendering a vestigial 5-column detail strip.
//!
//! ## Layout
//!
//! ```text
//! ┌──────────────────────────────────────┬──────────────────────────────┐
//! │ trace 7ab6afba… · /POST checkout …   │ identity                     │
//! ├──────────────────────────────────────┤   trace_id  7ab6afba…        │
//! │ ▸ [api]    http.handle ████  130ms   │   span_id   89f0…            │
//! │   ▸ [db]   query.exec   ▌▌    52ms   │   …                          │
//! └──────────────────────────────────────┴──────────────────────────────┘
//! ```
//!
//! ## Virtualization
//!
//! Both panes virtualize: the tree iterates only rows in
//! `[scroll, scroll + body_h)`, and the detail pane materialises
//! section lines lazily — long attribute / event maps don't pay
//! a render cost for rows that scroll off-screen. The 1,498-span
//! production fixture renders cleanly inside the same per-frame
//! budget as the 47-span fixture.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use serde_json::Value as Json;

use crate::app::{App, Pane, TraceInputMode, TraceView};
use crate::trace::{Span as TraceSpan, SpanEvent, TreeRow};

// Body split: 65% tree, remainder detail; floor 20 cols on detail.
const DETAIL_MIN_COLS: u16 = 20;
const TREE_DEFAULT_PCT: u16 = 65;
const DUR_COL_WIDTH: u16 = 9;
// The waterfall bar is no longer a fixed width — it claims whatever
// space is left after the (capped) label column and the duration
// column, so the timeline is as wide as the pane allows. The label
// column is capped so a wide terminal grows the *bar*, not the name
// column, and floored so names stay readable on narrow ones.
const LABEL_MAX_COLS: usize = 60;
const LABEL_MIN_COLS: usize = 16;

/// Entry point — called by `src/ui/mod.rs` when
/// `app.view_mode == ViewMode::Trace`.
pub fn draw_trace(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(_) = app.trace_view.as_ref() else {
        let line = Paragraph::new(Line::from(Span::styled(
            "loading trace…",
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(line, area);
        return;
    };

    // ---- Header (1 row + 1-row separator) ----------------------------
    let header_h: u16 = 1;
    let sep_h: u16 = 1;
    let header_rect = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: header_h.min(area.height),
    };
    {
        let view = app.trace_view.as_ref().expect("checked above");
        f.render_widget(Paragraph::new(build_header(view)), header_rect);
    }
    if area.height > header_h {
        let sep_rect = Rect {
            x: area.x,
            y: area.y + header_h,
            width: area.width,
            height: sep_h,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(area.width as usize),
                Style::default().fg(Color::DarkGray),
            ))),
            sep_rect,
        );
    }

    // ---- Body split ---------------------------------------------------
    let body_h = area.height.saturating_sub(header_h + sep_h);
    if body_h == 0 {
        return;
    }
    let body_rect = Rect {
        x: area.x,
        y: area.y + header_h + sep_h,
        width: area.width,
        height: body_h,
    };
    let detail_visible = area.width >= DETAIL_MIN_COLS * 2;
    let (tree_rect, detail_rect_opt) = if detail_visible {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(TREE_DEFAULT_PCT),
                Constraint::Min(DETAIL_MIN_COLS),
            ])
            .split(body_rect);
        (split[0], Some(split[1]))
    } else {
        (body_rect, None)
    };

    draw_tree(f, app, tree_rect);
    if let Some(detail_rect) = detail_rect_opt {
        draw_detail(f, app, detail_rect);
    }

    // Stash heights so the keymap's half-page math is exact next
    // frame. Use the inner rect (post-border) for the detail pane.
    app.last_trace_body_height = tree_rect.height;
    app.last_trace_detail_height = detail_rect_opt
        .map(|r| r.height.saturating_sub(2))
        .unwrap_or(0);
    // Stash the detail rect for mouse focus / scroll routing (step
    // 27). The tree body rect + scroll origin are stashed inside
    // `draw_tree`, which has the post-prompt body geometry.
    app.mouse_geom.trace_detail = detail_rect_opt.unwrap_or_default();
}

// ============================================================
//                          Header
// ============================================================

fn build_header(view: &TraceView) -> Line<'static> {
    let trace_id = short_id(&view.model.trace_id);
    let root_label = view
        .model
        .roots
        .first()
        .and_then(|&i| view.model.spans.get(i))
        .map(|s| {
            format!(
                "{} [{}]",
                display_name(&s.name),
                display_service(&s.service)
            )
        })
        .unwrap_or_else(|| "no root".to_string());
    let dur = humanize_duration_ns(view.model.duration_ns());
    let span_count = view.model.spans.len();
    let err_count = view.model.spans.iter().filter(|s| s.is_error).count();
    let err_segment = if err_count == 0 {
        String::new()
    } else {
        format!("  ·  {err_count} err")
    };
    let mut parts: Vec<Span<'static>> = vec![
        Span::styled(
            format!("trace {trace_id}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  ·  "),
        Span::styled(root_label, Style::default().fg(Color::White)),
        Span::raw("  ·  "),
        Span::styled(dur, Style::default().fg(Color::LightGreen)),
        Span::raw("  ·  "),
        Span::styled(
            format!("{span_count} spans"),
            Style::default().fg(Color::Gray),
        ),
        Span::styled(err_segment, Style::default().fg(Color::Red)),
    ];
    // Filter badge: stays visible after the user commits with
    // Enter. The keymap clears `filter` on Esc, which makes the
    // badge disappear next frame.
    if !view.filter.is_empty() {
        let match_count = view.filter_matches.as_ref().map(Vec::len).unwrap_or(0);
        parts.push(Span::raw("  ·  "));
        parts.push(Span::styled(
            format!("[/ {}]", view.filter),
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightYellow)
                .add_modifier(Modifier::BOLD),
        ));
        parts.push(Span::styled(
            format!(" {match_count} hit"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(parts)
}

// ============================================================
//                         Tree pane
// ============================================================

fn draw_tree(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    // Filter input prompt steals one row at the bottom of the
    // pane, vim-style. The rest of the body is the waterfall.
    let in_filter_input = app
        .trace_view
        .as_ref()
        .map(|v| v.input_mode == TraceInputMode::Filter)
        .unwrap_or(false);
    let prompt_h: u16 = if in_filter_input && area.height > 0 {
        1
    } else {
        0
    };
    let body_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height.saturating_sub(prompt_h),
    };
    // Stash the tree body rect for mouse hit-testing (step 27). The
    // scroll origin is stashed below once it's been re-clamped.
    app.mouse_geom.trace_tree_body = body_area;

    // Compute visible rows + cursor position within them, then
    // reclamp scroll. `cursor` lives in `model.tree` index space
    // so its identity survives fold/filter operations — we just
    // look up its position in the visible set per frame.
    let visible: Vec<usize> = {
        let Some(view) = app.trace_view.as_ref() else {
            return;
        };
        if view.model.tree.is_empty() {
            return;
        }
        view.visible_rows()
    };

    if visible.is_empty() {
        // Filter hid everything. Draw a placeholder so the user
        // doesn't think the pane crashed.
        let line = Paragraph::new(Line::from(vec![
            Span::styled("no matches", Style::default().fg(Color::DarkGray)),
            Span::raw("  ("),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" clears the filter)"),
        ]));
        f.render_widget(line, body_area);
        if prompt_h > 0 {
            draw_filter_prompt(
                f,
                Rect {
                    x: area.x,
                    y: area.y + body_area.height,
                    width: area.width,
                    height: prompt_h,
                },
                app.trace_view.as_ref().unwrap(),
            );
        }
        return;
    }

    // Find cursor's position in the visible window; if hidden,
    // snap to the nearest visible row. Renderer-side fallback for
    // edge cases the keymap might miss (e.g. filter just cleared
    // the cursor's row).
    let (cursor_tree, cursor_vis, scroll) = {
        let view = app.trace_view.as_ref().expect("checked above");
        let cursor_tree = view.cursor.min(view.model.tree.len() - 1);
        let cursor_vis = visible.iter().position(|&i| i == cursor_tree).unwrap_or(0);
        let body_h = body_area.height as usize;
        let max_scroll = visible.len().saturating_sub(body_h);
        let mut scroll = (view.scroll as usize).min(max_scroll);
        if cursor_vis < scroll {
            scroll = cursor_vis;
        } else if body_h > 0 && cursor_vis >= scroll + body_h {
            scroll = (cursor_vis + 1).saturating_sub(body_h);
        }
        (visible[cursor_vis], cursor_vis, scroll)
    };
    if let Some(v) = app.trace_view.as_mut() {
        // Snap-back: if the cursor's original row was hidden, the
        // tree index we settled on (visible[cursor_vis]) replaces
        // it so subsequent keystrokes step from a visible row.
        v.cursor = cursor_tree;
        v.scroll = scroll as u16;
    }
    // Stash the re-clamped scroll origin so a click row maps to
    // `visible[scroll + dy]` next frame (step 27).
    app.mouse_geom.trace_tree_scroll = scroll;

    let view = app.trace_view.as_ref().expect("checked above");
    let model = &view.model;
    let body_h = body_area.height as usize;
    let focused_tree = app.focus == Pane::TraceTree;
    let t0 = model.t0_ns;
    let t1 = model.t1_ns;

    // Tree guide prefixes over the *visible* sequence, so folds and
    // filters are reflected (a collapsed subtree's children simply
    // aren't in the list). Computed once per frame in O(visible).
    let depths: Vec<u16> = visible.iter().map(|&r| model.tree[r].depth).collect();
    let guides = tree_guides(&depths);

    let visible_end = (scroll + body_h).min(visible.len());
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(body_h);
    for (vis_idx, &row_idx) in visible.iter().enumerate().take(visible_end).skip(scroll) {
        let row = model.tree[row_idx];
        let span = &model.spans[row.span_idx];
        let selected = vis_idx == cursor_vis;
        let collapsed = view.collapsed.contains(&row.span_idx) && row.has_children;
        // Show the service tag only at a service boundary (the span's
        // service differs from its parent's). Roots and orphans count
        // as boundaries. Within one service the tag is redundant — the
        // bar colour already encodes it — so we drop it and give the
        // operation name the whole column.
        let parent_service = span
            .parent_span_id
            .as_deref()
            .and_then(|p| model.by_id.get(p))
            .map(|&pi| model.spans[pi].service.as_str());
        let show_service = parent_service != Some(span.service.as_str());
        lines.push(build_tree_row(
            span,
            row,
            collapsed,
            &guides[vis_idx],
            show_service,
            t0,
            t1,
            body_area.width as usize,
            selected,
            focused_tree,
        ));
    }
    f.render_widget(Paragraph::new(lines), body_area);

    if prompt_h > 0 {
        draw_filter_prompt(
            f,
            Rect {
                x: area.x,
                y: area.y + body_area.height,
                width: area.width,
                height: prompt_h,
            },
            view,
        );
    }
}

/// One-row vim-style search prompt: `/<query>█`. Yellow on the
/// `/` so the user knows they're in input mode. The cursor cell
/// is rendered as a literal `█` (we don't move the terminal
/// cursor; the trace pane never showed it). `Esc` cancels and
/// `Enter` commits in the keymap.
fn draw_filter_prompt(f: &mut Frame, area: Rect, view: &TraceView) {
    let line = Line::from(vec![
        Span::styled("/", Style::default().fg(Color::Yellow)),
        Span::raw(view.filter.clone()),
        Span::styled(
            "█",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// Build box-drawing tree-guide prefixes for a flat DFS row
/// sequence given each row's `depth`. Operates on whatever sequence
/// it's handed — pass the *visible* rows so folds/filters are
/// reflected automatically.
///
/// Each returned prefix is the indentation + connector for that
/// row, e.g.
///
/// ```text
/// (root)
/// ├╴ child
/// │ └╴ last grandchild
/// └╴ last child
/// ```
///
/// Ancestors with a later sibling get a `│ ` rail; exhausted
/// branches get blank space. The node's own connector is `├╴`
/// (has a following sibling) or `└╴` (last child). Roots (depth 0)
/// get no connector. Pure + O(n) (the inner clear loop is bounded
/// by max depth), so it's cheap to call every frame.
fn tree_guides(depths: &[u16]) -> Vec<String> {
    let n = depths.len();
    // `last_child[k]`: is row k the last among its siblings?
    let mut last_child = vec![true; n];
    let max_depth = depths.iter().copied().max().unwrap_or(0) as usize;
    // `prev_at_depth[d]` = most recent row index seen at depth d that
    // hasn't yet been proven non-last.
    let mut prev_at_depth: Vec<Option<usize>> = vec![None; max_depth + 1];
    for (k, &d) in depths.iter().enumerate() {
        let d = d as usize;
        // Descending to depth d closes every deeper open branch.
        for slot in prev_at_depth.iter_mut().skip(d + 1) {
            *slot = None;
        }
        if let Some(p) = prev_at_depth[d] {
            // A sibling now follows `p`, so `p` wasn't the last child.
            last_child[p] = false;
        }
        prev_at_depth[d] = Some(k);
    }

    let mut out = Vec::with_capacity(n);
    // Stack of ancestor `last_child` flags, indexed by depth.
    let mut rail: Vec<bool> = Vec::with_capacity(max_depth + 1);
    for (k, &d) in depths.iter().enumerate() {
        let d = d as usize;
        rail.truncate(d);
        let mut s = String::with_capacity(d * 2 + 2);
        // Skip the depth-0 root's column: top-level roots are
        // independent trees, so their children's connectors sit at
        // column 0 (matching the `tree` command).
        for &ancestor_last in rail.iter().skip(1) {
            s.push_str(if ancestor_last { "  " } else { "│ " });
        }
        if d > 0 {
            s.push_str(if last_child[k] { "└╴" } else { "├╴" });
        }
        out.push(s);
        rail.push(last_child[k]);
    }
    out
}

/// Compose one span row:
/// `<guides>▸ <name>  <bar>  <duration>`
///
/// Layout (left-to-right): a capped label column, a single space,
/// the waterfall bar (claims the remaining width), a single space,
/// then the right-aligned `DUR_COL_WIDTH` duration column.
#[allow(clippy::too_many_arguments)]
fn build_tree_row(
    span: &TraceSpan,
    row: TreeRow,
    collapsed: bool,
    guides: &str,
    show_service: bool,
    t0: i64,
    t1: i64,
    width: usize,
    selected: bool,
    focused: bool,
) -> Line<'static> {
    // Marker semantics:
    //   ⚠   orphan (takes precedence — most important signal)
    //   ▾   collapsed parent (fold closed)
    //   ▸   expanded parent (fold open)
    //   ·   leaf (no children)
    let marker = if row.is_orphan {
        "⚠ "
    } else if row.has_children {
        if collapsed { "▾ " } else { "▸ " }
    } else {
        "· "
    };
    let name = display_name(&span.name);

    // Service tag only at boundaries; elsewhere the name gets the
    // whole column (the bar colour still encodes the service).
    let label_text = if show_service {
        format!(
            "{guides}{marker}[{}] {name}",
            display_service(&span.service)
        )
    } else {
        format!("{guides}{marker}{name}")
    };
    let dur_text = humanize_duration_ns(span.duration_ns);

    // Column budget: `<label> <bar> <dur>` with a single space
    // between each. The label is capped (so wide terminals grow the
    // bar, not the name) and floored (so it stays legible); the bar
    // takes everything left over.
    let avail = width.saturating_sub(DUR_COL_WIDTH as usize + 2);
    let label_budget = (avail / 2).clamp(LABEL_MIN_COLS.min(avail), LABEL_MAX_COLS);
    let bar_cells = avail.saturating_sub(label_budget).min(u16::MAX as usize) as u16;
    let label_display = truncate_for_display(&sanitize_inline(&label_text), label_budget);
    let label_pad = label_budget.saturating_sub(visual_width(&label_display));

    // Bar geometry: project the span onto `bar_cells` cells at
    // 1/8-cell resolution on the trailing edge.
    let (start_e, end_e) = bar_eighths(span.start_ns, span.end_ns, t0, t1, bar_cells);
    let bar_string = render_bar_string(start_e, end_e, bar_cells);

    // Style resolution.
    let row_style = if selected && focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if selected {
        // Focus is elsewhere (detail pane) but show a dim marker.
        Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM)
    } else if span.is_error {
        Style::default().fg(Color::Red)
    } else if row.is_orphan {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::DIM)
    } else {
        Style::default()
    };
    // Bar colour: red on error, palette-hashed on service otherwise.
    let bar_colour = if span.is_error {
        Color::Red
    } else {
        service_colour(&span.service)
    };

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(5);
    spans.push(Span::styled(
        format!("{label_display}{}", " ".repeat(label_pad)),
        row_style,
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        bar_string,
        Style::default().fg(bar_colour).patch(
            row_style
                .bg
                .map_or(Style::default(), |bg| Style::default().bg(bg)),
        ),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("{dur_text:>width$}", width = DUR_COL_WIDTH as usize),
        if selected && focused {
            row_style
        } else {
            Style::default().fg(Color::DarkGray)
        },
    ));
    Line::from(spans)
}

/// Project a span's `[start_ns, end_ns]` onto a `bar_cells`-wide
/// column at **1/8-cell** resolution, returning `(start_eighth,
/// end_eighth)` with `0 <= start_e <= end_e <= bar_cells * 8`.
///
/// The leading edge is floored to a whole cell: terminal block
/// glyphs fill from the left, so a left-anchored fill renders as a
/// clean solid bar without reverse-video tricks (which break on
/// unknown terminal backgrounds). The *trailing* edge keeps 1/8
/// precision — with the bar now claiming most of the pane width,
/// the cell-aligned start is a sub-2% positional error while the
/// duration reads precisely.
///
/// Guarantees:
/// * `bar_cells == 0` or `end_ns < start_ns` (skew) → `(0, 0)`.
/// * Degenerate trace (`t0 == t1`) → `(0, 1)` (a 1/8 tick).
/// * Any non-degenerate span yields `end_e - start_e >= 1` so a
///   sub-cell span still shows a 1/8 tick rather than vanishing.
/// * `end_ns > t1` (clock skew) clamps to `bar_cells * 8`.
pub(crate) fn bar_eighths(
    start_ns: i64,
    end_ns: i64,
    t0: i64,
    t1: i64,
    bar_cells: u16,
) -> (u32, u32) {
    if bar_cells == 0 || end_ns < start_ns {
        return (0, 0);
    }
    let total_e = bar_cells as i64 * 8;
    let total = (t1.saturating_sub(t0)).max(0);
    if total == 0 {
        // Degenerate trace duration — a single 1/8 tick at the start.
        return (0, 1);
    }
    let total_f = total as f64;
    let s_rel = (start_ns.saturating_sub(t0)).max(0) as f64;
    let e_rel = (end_ns.saturating_sub(t0)).max(0) as f64;
    // Floor the start to a whole cell, capped at the last cell so
    // there's always room for at least a 1/8 tick.
    let start_cell = (s_rel / total_f * bar_cells as f64)
        .floor()
        .clamp(0.0, (bar_cells - 1) as f64) as i64;
    let start_e = start_cell * 8;
    // Round the end to the nearest 1/8, clamped into the column.
    let mut end_e = (e_rel / total_f * total_e as f64).round() as i64;
    end_e = end_e.clamp(start_e, total_e);
    if end_e <= start_e {
        end_e = start_e + 1; // guarantee a visible tick
    }
    (start_e as u32, end_e as u32)
}

/// Render the bar as a solid run of block glyphs. Because the start
/// is cell-aligned, every filled cell is a left-anchored fill: a
/// full `█` for interior cells and a left-eighth block
/// (`▏▎▍▌▋▊▉`) for the trailing partial cell. Empty cells are
/// spaces. One colour throughout — the caller styles the whole
/// string.
fn render_bar_string(start_e: u32, end_e: u32, bar_cells: u16) -> String {
    // Index `n-1` → a cell filled `n` eighths from the left.
    const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
    let mut out = String::with_capacity(bar_cells as usize);
    for i in 0..bar_cells as u32 {
        let c0 = i * 8;
        let lo = start_e.max(c0);
        let hi = end_e.min(c0 + 8);
        if hi <= lo {
            out.push(' ');
            continue;
        }
        // Start is cell-aligned, so `lo == c0` for every filled
        // cell — the fill is always left-anchored. `hi - c0` is the
        // eighths filled in this cell (1..=8).
        let filled = (hi - c0).clamp(1, 8) as usize;
        out.push(EIGHTHS[filled - 1]);
    }
    out
}

/// Stable colour from `service.name` over a curated palette
/// (red excluded — reserved for errors). Empty service falls to a
/// muted grey so unlabelled spans don't visually compete with
/// real services.
pub(crate) fn service_colour(service: &str) -> Color {
    if service.is_empty() {
        return Color::DarkGray;
    }
    // FNV-1a hash — small, deterministic, no extra deps.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in service.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x00000100000001B3);
    }
    SERVICE_PALETTE[(h as usize) % SERVICE_PALETTE.len()]
}

/// Curated palette: avoids red (errors) and pure white / black
/// (used by selection / unfocused chrome). Twelve hues give
/// reasonable spread across typical service-counts in a trace
/// (most are <8).
const SERVICE_PALETTE: &[Color] = &[
    Color::Cyan,
    Color::LightCyan,
    Color::Blue,
    Color::LightBlue,
    Color::Magenta,
    Color::LightMagenta,
    Color::Green,
    Color::LightGreen,
    Color::Yellow,
    Color::LightYellow,
    Color::Gray,
    Color::Rgb(180, 120, 60), // amber
];

// ============================================================
//                       Detail pane
// ============================================================

/// Plan a detail-pane row without materialising its text. The
/// `Section`-by-row planner lets the renderer compute the total
/// height in O(spans/attrs) and then only build strings for the
/// visible window — important for spans with hundreds of
/// attributes. Owned strings so the plan can outlive the borrow
/// on `App.trace_view`.
#[derive(Debug)]
enum DetailRow {
    SectionHeader(&'static str),
    /// Plain key/value row. Long values are truncated to viewport
    /// width minus key column.
    KV(String, String),
    /// One event: timestamp + name on the row, attributes follow
    /// as KV rows.
    EventHeader(String),
    /// Blank line between sections.
    Blank,
}

fn draw_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::TraceDetail;
    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(" detail ", Style::default().fg(Color::Gray)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Read the currently-selected span.
    let (rows, scroll_request) = {
        let Some(view) = app.trace_view.as_ref() else {
            return;
        };
        let model = &view.model;
        let cursor = view.cursor.min(model.tree.len().saturating_sub(1));
        let span_idx = model.tree[cursor].span_idx;
        let span = &model.spans[span_idx];
        let rows = plan_detail_rows(model, span);
        (rows, view.detail_scroll)
    };
    let total_lines = rows.len();
    let visible_h = inner.height as usize;
    let max_scroll = total_lines.saturating_sub(visible_h) as u16;
    let scroll = scroll_request.min(max_scroll);
    if let Some(v) = app.trace_view.as_mut() {
        v.detail_scroll = scroll;
    }

    // Materialise only the visible slice.
    let from = scroll as usize;
    let to = (from + visible_h).min(total_lines);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(to - from);
    for row in &rows[from..to] {
        lines.push(render_detail_row(row, inner.width as usize));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Build the section descriptors for one span. Returned as a
/// `Vec<DetailRow>` so the renderer can index by row and skip the
/// off-screen slice.
fn plan_detail_rows(model: &crate::trace::TraceModel, span: &TraceSpan) -> Vec<DetailRow> {
    let mut rows: Vec<DetailRow> = Vec::with_capacity(32);

    // ---- identity ----
    rows.push(DetailRow::SectionHeader("identity"));
    rows.push(DetailRow::KV(
        "trace_id".to_string(),
        model.trace_id.clone(),
    ));
    rows.push(DetailRow::KV("span_id".to_string(), span.span_id.clone()));
    if let Some(p) = span.parent_span_id.as_deref() {
        // Look the parent up by id so the row can show its
        // name alongside the raw span_id — huge UX win for
        // traces with opaque hex ids. Orphans (parent not in
        // the loaded trace) fall through to the bare id with
        // an `(orphan)` marker.
        let parent_label = match model.by_id.get(p) {
            Some(&idx) => {
                let parent_span = &model.spans[idx];
                format!("{p}  ({})", display_name(&parent_span.name))
            }
            None => format!("{p}  (orphan)"),
        };
        rows.push(DetailRow::KV("parent".to_string(), parent_label));
    }
    rows.push(DetailRow::KV("name".to_string(), display_name(&span.name)));
    rows.push(DetailRow::KV(
        "kind".to_string(),
        span.kind.as_str().to_string(),
    ));
    rows.push(DetailRow::Blank);

    // ---- timing ----
    rows.push(DetailRow::SectionHeader("timing"));
    let start_rel = span.start_ns.saturating_sub(model.t0_ns);
    let trace_dur = model.duration_ns().max(1);
    let pct = (span.duration_ns as f64 / trace_dur as f64 * 100.0).clamp(0.0, 999.9);
    rows.push(DetailRow::KV(
        "start".to_string(),
        format!("+{}", humanize_duration_ns(start_rel)),
    ));
    rows.push(DetailRow::KV(
        "duration".to_string(),
        format!("{} ({:.1}%)", humanize_duration_ns(span.duration_ns), pct),
    ));
    rows.push(DetailRow::Blank);

    // ---- status ----
    rows.push(DetailRow::SectionHeader("status"));
    let status_text = match span.status_code.as_deref() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            if span.is_error {
                "ERROR".to_string()
            } else {
                "OK".to_string()
            }
        }
    };
    rows.push(DetailRow::KV("code".to_string(), status_text));
    if span.is_error {
        rows.push(DetailRow::KV("error".to_string(), "true".to_string()));
    }
    rows.push(DetailRow::Blank);

    // ---- service / resource ----
    rows.push(DetailRow::SectionHeader("service"));
    rows.push(DetailRow::KV(
        "service.name".to_string(),
        display_service(&span.service),
    ));
    for (k, v) in span.resource.iter() {
        if k == "service.name" {
            continue;
        }
        rows.push(DetailRow::KV(k.clone(), render_json(v)));
    }
    rows.push(DetailRow::Blank);

    // ---- attributes ----
    if !span.attributes.is_empty() {
        rows.push(DetailRow::SectionHeader("attributes"));
        for (k, v) in span.attributes.iter() {
            rows.push(DetailRow::KV(k.clone(), render_json(v)));
        }
        rows.push(DetailRow::Blank);
    }

    // ---- events ----
    if !span.events.is_empty() {
        rows.push(DetailRow::SectionHeader("events"));
        for ev in &span.events {
            rows.push(DetailRow::EventHeader(format_event_header(ev, model.t0_ns)));
            for (k, v) in &ev.attributes {
                rows.push(DetailRow::KV(k.clone(), render_json(v)));
            }
        }
    }
    rows
}

fn render_detail_row(row: &DetailRow, width: usize) -> Line<'static> {
    match row {
        DetailRow::SectionHeader(name) => Line::from(Span::styled(
            (*name).to_string(),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )),
        DetailRow::KV(k, v) => {
            // Key column: 14 chars, right-padded; value collapsed to
            // a single line then truncated to the remaining width.
            let key_w = 14usize;
            let key = truncate_for_display(&sanitize_inline(k), key_w);
            let key_pad = key_w.saturating_sub(visual_width(&key));
            let value_budget = width.saturating_sub(key_w + 1);
            let value = truncate_for_display(&sanitize_inline(v), value_budget);
            Line::from(vec![
                Span::styled(
                    format!("{key}{} ", " ".repeat(key_pad)),
                    Style::default().fg(Color::Gray),
                ),
                Span::raw(value),
            ])
        }
        DetailRow::EventHeader(text) => Line::from(Span::styled(
            truncate_for_display(&sanitize_inline(text), width),
            Style::default().fg(Color::LightYellow),
        )),
        DetailRow::Blank => Line::from(Span::raw("")),
    }
}

fn format_event_header(ev: &SpanEvent, t0_ns: i64) -> String {
    let rel = ev.time_ns.saturating_sub(t0_ns);
    format!("• +{}  {}", humanize_duration_ns(rel), ev.name)
}

/// Render a `serde_json::Value` for inline display. Strings are
/// shown unquoted (they're the common case); numbers, booleans,
/// nulls round-trip via `Display`; arrays / objects compact via
/// `serde_json::to_string`.
fn render_json(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Null => "null".to_string(),
        Json::Bool(b) => b.to_string(),
        Json::Number(n) => n.to_string(),
        Json::Array(_) | Json::Object(_) => serde_json::to_string(v).unwrap_or_default(),
    }
}

// ============================================================
//                         Helpers
// ============================================================

fn display_name(n: &str) -> String {
    if n.is_empty() {
        "(unnamed)".to_string()
    } else {
        n.to_string()
    }
}

fn display_service(s: &str) -> String {
    if s.is_empty() {
        "?".to_string()
    } else {
        s.to_string()
    }
}

fn visual_width(s: &str) -> usize {
    s.chars().count()
}

/// Collapse a value to a single display line: newlines, carriage
/// returns, tabs, and other control characters become spaces. A
/// multi-line attribute value (SQL, a stack trace, a JSON blob with
/// embedded `\n`) would otherwise render across several buffer rows
/// and shove the rest of the detail pane out of place. Truncation
/// (by [`truncate_for_display`]) then caps the width.
fn sanitize_inline(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn truncate_for_display(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut = max.saturating_sub(1);
    let mut out: String = s.chars().take(cut).collect();
    out.push('…');
    out
}

/// Format a nanosecond duration in the largest unit ≥1.0. Returns
/// `0ns` for zero / negative input.
pub(crate) fn humanize_duration_ns(ns: i64) -> String {
    if ns <= 0 {
        return "0ns".to_string();
    }
    let ns = ns as f64;
    if ns >= 1e9 {
        format!("{:.2}s", ns / 1e9)
    } else if ns >= 1e6 {
        format!("{:.2}ms", ns / 1e6)
    } else if ns >= 1e3 {
        format!("{:.2}µs", ns / 1e3)
    } else {
        format!("{ns:.0}ns")
    }
}

fn short_id(id: &str) -> String {
    if id.chars().count() <= 16 {
        id.to_string()
    } else {
        format!("{}…", crate::util::take_chars(id, 12))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_picks_largest_unit() {
        assert_eq!(humanize_duration_ns(0), "0ns");
        assert_eq!(humanize_duration_ns(-5), "0ns");
        assert_eq!(humanize_duration_ns(500), "500ns");
        assert_eq!(humanize_duration_ns(1_500), "1.50µs");
        assert_eq!(humanize_duration_ns(2_300_000), "2.30ms");
        assert_eq!(humanize_duration_ns(3_500_000_000), "3.50s");
    }

    #[test]
    fn bar_eighths_full_span_covers_column() {
        // Whole trace → fills all 20 cells = 160 eighths.
        assert_eq!(bar_eighths(0, 100, 0, 100, 20), (0, 160));
    }

    #[test]
    fn bar_eighths_mid_half() {
        // start 25% floors to cell 5 (eighth 40); end 75% → eighth 120.
        assert_eq!(bar_eighths(25, 75, 0, 100, 20), (40, 120));
    }

    #[test]
    fn bar_eighths_at_t0() {
        let (s, e) = bar_eighths(0, 10, 0, 100, 20);
        assert_eq!(s, 0);
        assert!(e >= 1, "non-zero span must produce a visible tick");
    }

    #[test]
    fn bar_eighths_at_t1() {
        // 90%..100% → start floors to cell 18 (eighth 144), end 160.
        let (s, e) = bar_eighths(90, 100, 0, 100, 20);
        assert_eq!(s, 144);
        assert_eq!(e, 160, "right edge clamps to bar_cells*8");
    }

    #[test]
    fn bar_eighths_zero_duration_span() {
        // start == end mid-trace: start floors to its cell, end gets
        // a guaranteed 1/8 tick beyond it.
        let (s, e) = bar_eighths(50, 50, 0, 100, 20);
        assert_eq!(s, 80); // cell 10
        assert_eq!(e, 81); // +1 eighth tick
    }

    #[test]
    fn bar_eighths_sub_cell_rounds_to_tick() {
        // 1ns out of 1e9 across 20 cells: end rounds to 0 eighths,
        // bumped to a 1/8 tick.
        let (s, e) = bar_eighths(100, 101, 0, 1_000_000_000, 20);
        assert_eq!((s, e), (0, 1));
    }

    #[test]
    fn bar_eighths_degenerate_total() {
        // t0 == t1 — single 1/8 tick.
        assert_eq!(bar_eighths(0, 0, 100, 100, 20), (0, 1));
    }

    #[test]
    fn bar_eighths_zero_bar_width() {
        // Tiny terminal: no bar column at all.
        assert_eq!(bar_eighths(0, 100, 0, 100, 0), (0, 0));
    }

    #[test]
    fn bar_eighths_inverted_bounds_collapse() {
        // end_ns < start_ns is anomalous; expect (0, 0).
        assert_eq!(bar_eighths(100, 50, 0, 200, 20), (0, 0));
    }

    #[test]
    fn bar_eighths_overflow_t1_clamps() {
        // Caller may pass end_ns > t1 from clock skew; helper trusts
        // t1 as the right edge.
        let (_, e) = bar_eighths(0, 200, 0, 100, 20);
        assert_eq!(e, 160);
    }

    #[test]
    fn service_colour_is_deterministic() {
        let a1 = service_colour("checkout");
        let a2 = service_colour("checkout");
        let b1 = service_colour("payments");
        assert_eq!(a1, a2, "same input must produce same colour");
        // Probabilistic: different services may collide, but
        // checkout vs payments must hash to different cells with
        // a 12-colour palette. If this ever flakes the palette
        // grew or shrank and the assertion needs to change.
        assert_ne!(a1, b1);
    }

    #[test]
    fn service_colour_never_red() {
        // Sweep 1000 ASCII names; none should hash to red.
        for i in 0..1000 {
            let name = format!("svc-{i}");
            let c = service_colour(&name);
            assert_ne!(c, Color::Red, "palette must exclude red ({name})");
        }
        // Empty service falls to a muted grey.
        assert_eq!(service_colour(""), Color::DarkGray);
    }

    #[test]
    fn tree_guides_linear_chain() {
        // 0→1→2→3 each the only child → all last-child. Depth-1
        // connector sits at column 0 (root contributes no rail).
        let g = tree_guides(&[0, 1, 2, 3]);
        assert_eq!(g[0], "");
        assert_eq!(g[1], "└╴");
        assert_eq!(g[2], "  └╴");
        assert_eq!(g[3], "    └╴");
    }

    #[test]
    fn tree_guides_siblings_get_rail() {
        // root(0) with children a(1), b(1); a has child(2).
        //   (root)
        //   ├╴ a       (has sibling b → ├)
        //   │ └╴ a.c    (rail under a because b follows)
        //   └╴ b       (last)
        let g = tree_guides(&[0, 1, 2, 1]);
        assert_eq!(g[0], "");
        assert_eq!(g[1], "├╴");
        assert_eq!(g[2], "│ └╴");
        assert_eq!(g[3], "└╴");
    }

    #[test]
    fn tree_guides_two_roots_are_independent() {
        // Top-level roots are independent trees — no rail spans
        // between them, so each root's children connect at column 0.
        let g = tree_guides(&[0, 1, 0, 1]);
        assert_eq!(g[0], ""); // root 0
        assert_eq!(g[1], "└╴"); // child of root0
        assert_eq!(g[2], ""); // root 1
        assert_eq!(g[3], "└╴"); // child of root1
    }

    #[test]
    fn tree_guides_empty() {
        assert!(tree_guides(&[]).is_empty());
    }

    #[test]
    fn render_bar_string_full_run() {
        assert_eq!(render_bar_string(0, 24, 3), "███");
    }

    #[test]
    fn render_bar_string_offset_and_gap() {
        // Empty cell 0, full cell 1, empty cell 2.
        assert_eq!(render_bar_string(8, 16, 3), " █ ");
    }

    #[test]
    fn render_bar_string_trailing_partial() {
        // One cell filled 4/8 → the 4-eighth left block.
        assert_eq!(render_bar_string(0, 4, 1), "▌");
        // 1/8 tick → the thinnest left block.
        assert_eq!(render_bar_string(0, 1, 1), "▏");
        // Two full cells + a 3/8 trailing partial.
        assert_eq!(render_bar_string(0, 19, 3), "██▍");
    }

    #[test]
    fn render_bar_string_empty_when_no_fill() {
        assert_eq!(render_bar_string(0, 0, 3), "   ");
    }

    #[test]
    fn sanitize_inline_replaces_control_chars() {
        assert_eq!(sanitize_inline("a\nb\tc\rd"), "a b c d");
        assert_eq!(sanitize_inline("plain"), "plain");
        // A multi-line JSON-ish blob collapses to one row.
        assert_eq!(sanitize_inline("{\n  \"k\": 1\n}"), "{   \"k\": 1 }");
    }

    #[test]
    fn detail_kv_value_with_newlines_renders_single_line() {
        // Regression: a long multi-line attribute value must not
        // bleed across rows / into the tree pane.
        let row = DetailRow::KV(
            "db.statement".to_string(),
            "SELECT *\nFROM t\nWHERE x = 1".to_string(),
        );
        let line = render_detail_row(&row, 60);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !text.contains('\n') && !text.contains('\t') && !text.contains('\r'),
            "rendered detail row leaked a control char: {text:?}"
        );
    }

    #[test]
    fn detail_kv_value_truncates_to_width() {
        let row = DetailRow::KV("k".to_string(), "x".repeat(500));
        let line = render_detail_row(&row, 40);
        let width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert!(width <= 40, "detail row {width} cols exceeds 40");
    }

    #[test]
    fn truncate_for_display_handles_short_input() {
        assert_eq!(truncate_for_display("hi", 10), "hi");
        assert_eq!(truncate_for_display("", 5), "");
        assert_eq!(truncate_for_display("hello", 0), "");
    }

    #[test]
    fn truncate_for_display_uses_ellipsis_at_max() {
        let out = truncate_for_display("hello world", 6);
        assert_eq!(out.chars().count(), 6);
        assert!(out.ends_with('…'));
    }
}
