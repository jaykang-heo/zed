use crate::{RelatedFile, ZetaPromptInput, write_event};
use anyhow::{Result, anyhow};
use std::fmt::Write;

const EDITABLE_REGION_START: &str = "<|editable_region_start|>\n";
const EDITABLE_REGION_END: &str = "\n<|editable_region_end|>";
const USER_CURSOR_MARKER: &str = "<|user_cursor|>";
const NO_EDITS: &str = "NO_EDITS";

/// Maximum number of udiff content lines to include from the edit history.
const MAX_HISTORY_LINES: usize = 128;

const TEACHER_PROMPT_TEMPLATE: &str = include_str!("prompts/teacher.md");

/// Build a teacher prompt from a `ZetaPromptInput`.
///
/// `editable_range` and `context_range` are byte-offset ranges within
/// `input.cursor_excerpt`. `context_range` must fully contain `editable_range`.
pub fn format_teacher_prompt(
    input: &ZetaPromptInput,
    editable_range: std::ops::Range<usize>,
    context_range: std::ops::Range<usize>,
) -> String {
    let edit_history = format_edit_history(input);
    let context = format_related_files(&input.related_files);
    let cursor_excerpt = format_cursor_excerpt(input, editable_range, context_range);

    TEACHER_PROMPT_TEMPLATE
        .replace("{{context}}", &context)
        .replace("{{edit_history}}", &edit_history)
        .replace("{{cursor_excerpt}}", &cursor_excerpt)
}

/// Extract the editable region text from a teacher model response.
///
/// Returns the content between the last `<|editable_region_start|>` and
/// `<|editable_region_end|>` markers in the last code block of the response,
/// with cursor/selection markers stripped. Returns an empty string when
/// the model outputs `NO_EDITS`.
pub fn extract_teacher_editable_region(response: &str) -> Result<String> {
    let code_block = extract_last_codeblock(response);

    if code_block.trim() == NO_EDITS {
        return Ok(String::new());
    }

    let region = extract_editable_region(&code_block)?;

    let cleaned = region
        .replace("<|selection_start|>", "")
        .replace(USER_CURSOR_MARKER, "");

    Ok(cleaned)
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn format_edit_history(input: &ZetaPromptInput) -> String {
    let mut raw = String::new();
    for event in &input.events {
        write_event(&mut raw, event);
        raw.push('\n');
    }

    let lines: Vec<&str> = raw.lines().filter(|s| is_udiff_content_line(s)).collect();

    let history_lines = if lines.len() > MAX_HISTORY_LINES {
        &lines[lines.len() - MAX_HISTORY_LINES..]
    } else {
        &lines
    };

    if history_lines.is_empty() {
        return "(No edit history)".to_string();
    }

    history_lines.join("\n")
}

fn format_related_files(related_files: &[RelatedFile]) -> String {
    if related_files.is_empty() {
        return "(No context)".to_string();
    }

    let mut prompt = String::new();
    for file in related_files {
        let path_str = file.path.to_string_lossy();
        writeln!(&mut prompt, "`````{path_str}").ok();

        let mut prev_row = 0;
        for excerpt in &file.excerpts {
            if excerpt.row_range.start > prev_row {
                prompt.push_str("…\n");
            }
            prompt.push_str(&excerpt.text);
            prompt.push('\n');
            prev_row = excerpt.row_range.end;
        }
        if prev_row < file.max_row {
            prompt.push_str("…\n");
        }
        prompt.push_str("\n`````\n");
    }

    prompt
}

fn format_cursor_excerpt(
    input: &ZetaPromptInput,
    editable_range: std::ops::Range<usize>,
    context_range: std::ops::Range<usize>,
) -> String {
    let excerpt = input.cursor_excerpt.as_ref();
    let cursor_offset = input.cursor_offset_in_excerpt;

    let mut result = String::new();

    let path_str = input.cursor_path.to_string_lossy();
    write!(&mut result, "`````{path_str}\n").ok();
    result.push_str(&excerpt[context_range.start..editable_range.start]);
    result.push_str(EDITABLE_REGION_START);
    result.push_str(&excerpt[editable_range.start..cursor_offset]);
    result.push_str(USER_CURSOR_MARKER);
    result.push_str(&excerpt[cursor_offset..editable_range.end]);
    result.push_str(EDITABLE_REGION_END);
    result.push_str(&excerpt[editable_range.end..context_range.end]);
    result.push_str("\n`````");

    result
}

fn is_udiff_content_line(s: &str) -> bool {
    s.starts_with('-')
        || s.starts_with('+')
        || s.starts_with(' ')
        || s.starts_with("---")
        || s.starts_with("+++")
        || s.starts_with("@@")
}

fn extract_editable_region(text: &str) -> Result<String> {
    let start = text
        .rfind(EDITABLE_REGION_START)
        .map_or(0, |pos| pos + EDITABLE_REGION_START.len());
    let end = text.rfind(EDITABLE_REGION_END).unwrap_or(text.len());

    if start >= end {
        return Err(anyhow!("Invalid editable region markers"));
    }

    let region = &text[start..end];
    Ok(region.strip_suffix('\n').unwrap_or(region).to_string())
}

/// Extract the content of the last fenced code block in `text`.
/// Falls back to `text` itself if no fenced block is found.
fn extract_last_codeblock(text: &str) -> String {
    let mut last_block = None;
    let mut search_start = 0;

    while let Some(start) = text[search_start..].find("```") {
        let start = start + search_start;
        let bytes = text.as_bytes();
        let mut backtick_end = start;

        while backtick_end < bytes.len() && bytes[backtick_end] == b'`' {
            backtick_end += 1;
        }

        let backtick_count = backtick_end - start;
        let closing_pattern = format!("\n{}", "`".repeat(backtick_count));

        // Skip the optional language tag on the opening fence line.
        while backtick_end < bytes.len() && bytes[backtick_end] != b'\n' {
            backtick_end += 1;
        }

        if let Some(end_pos) = text[backtick_end..].find(&closing_pattern) {
            let code_block = &text[backtick_end + 1..backtick_end + end_pos + 1];
            last_block = Some(code_block.to_string());
            search_start = backtick_end + end_pos + closing_pattern.len();
        } else {
            break;
        }
    }

    last_block.unwrap_or_else(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    fn make_input(
        content: &str,
        cursor_offset: usize,
        editable_range: std::ops::Range<usize>,
    ) -> ZetaPromptInput {
        ZetaPromptInput {
            cursor_path: Arc::from(Path::new("src/main.rs")),
            cursor_excerpt: Arc::from(content),
            editable_range_in_excerpt: editable_range,
            cursor_offset_in_excerpt: cursor_offset,
            excerpt_start_row: Some(0),
            events: vec![],
            related_files: vec![],
            excerpt_ranges: None,
            preferred_model: None,
            in_open_source_repo: false,
            force: true,
        }
    }

    #[test]
    fn test_format_teacher_prompt_basic() {
        let content = "fn main() {\n    println!(\"hello\");\n}";
        let input = make_input(content, 20, 12..33);
        let prompt = format_teacher_prompt(&input, 12..33, 0..content.len());

        assert!(prompt.contains("<|editable_region_start|>"));
        assert!(prompt.contains("<|editable_region_end|>"));
        assert!(prompt.contains("<|user_cursor|>"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("(No edit history)"));
        assert!(prompt.contains("(No context)"));
    }

    #[test]
    fn test_format_teacher_prompt_with_events() {
        let content = "fn main() {\n    println!(\"hello\");\n}";
        let mut input = make_input(content, 20, 12..33);
        input.events = vec![Arc::new(crate::Event::BufferChange {
            path: Arc::from(Path::new("src/main.rs")),
            old_path: Arc::from(Path::new("src/main.rs")),
            diff: "@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"world\");\n+    println!(\"hello\");\n }\n".to_string(),
            predicted: false,
            in_open_source_repo: false,
        })];

        let prompt = format_teacher_prompt(&input, 12..33, 0..content.len());

        assert!(prompt.contains("+    println!(\"hello\");"));
        assert!(prompt.contains("-    println!(\"world\");"));
    }

    #[test]
    fn test_format_teacher_prompt_with_related_files() {
        let content = "fn main() {\n    greet();\n}";
        let mut input = make_input(content, 16, 12..24);
        input.related_files = vec![crate::RelatedFile {
            path: Arc::from(Path::new("src/greet.rs")),
            max_row: 5,
            excerpts: vec![crate::RelatedExcerpt {
                row_range: 0..3,
                text: Arc::from("fn greet() {\n    println!(\"hi\");\n}"),
            }],
            in_open_source_repo: false,
        }];

        let prompt = format_teacher_prompt(&input, 12..24, 0..content.len());

        assert!(prompt.contains("src/greet.rs"));
        assert!(prompt.contains("fn greet()"));
    }

    #[test]
    fn test_extract_teacher_editable_region_basic() {
        let response = indoc::indoc! {"
            The user is adding a print statement.

            `````
            <|editable_region_start|>
                println!(\"hello world\");
            <|editable_region_end|>
            `````
        "};

        let result = extract_teacher_editable_region(response).unwrap();
        assert_eq!(result, "    println!(\"hello world\");");
    }

    #[test]
    fn test_extract_teacher_editable_region_no_edits() {
        let response = indoc::indoc! {"
            No changes needed.

            `````
            NO_EDITS
            `````
        "};

        let result = extract_teacher_editable_region(response).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_extract_teacher_editable_region_strips_markers() {
        let response = indoc::indoc! {"
            Completing the function call.

            `````
            <|editable_region_start|>
                total += product.<|selection_start|>price<|user_cursor|>;
            <|editable_region_end|>
            `````
        "};

        let result = extract_teacher_editable_region(response).unwrap();
        assert_eq!(result, "    total += product.price;");
    }

    #[test]
    fn test_extract_last_codeblock_returns_last() {
        let text = indoc::indoc! {"
            First block:
            ```
            first
            ```
            Second block:
            ```
            second
            ```
        "};

        assert_eq!(extract_last_codeblock(text), "second\n");
    }

    #[test]
    fn test_extract_last_codeblock_no_block() {
        let text = "just plain text";
        assert_eq!(extract_last_codeblock(text), "just plain text");
    }

    #[test]
    fn test_extract_last_codeblock_with_language_tag() {
        let text = "````rust\nfn main() {}\n````";
        assert_eq!(extract_last_codeblock(text), "fn main() {}\n");
    }
}
