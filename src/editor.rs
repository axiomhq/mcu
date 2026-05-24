use tui_textarea::TextArea;

const SAMPLE_QUERY: &str = "\
`<dataset>`:`<metric>`
| align to 5m using avg";

pub fn new_editor() -> TextArea<'static> {
    editor_with_text(SAMPLE_QUERY)
}

/// Build a textarea preloaded with `text`. Newlines split lines; an empty
/// string yields a single empty line (matching `TextArea::default`).
pub fn editor_with_text(text: &str) -> TextArea<'static> {
    if text.is_empty() {
        return TextArea::default();
    }
    let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    TextArea::new(lines)
}
