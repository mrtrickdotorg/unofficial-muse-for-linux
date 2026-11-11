//! Markdown-lite rendering for assistant replies.
//!
//! The web client renders replies as rich text; a plain label shows the
//! raw machinery (`**`, backticks, `#`) instead. This module converts the
//! markdown the service actually sends into a small block model whose text
//! is Pango markup. It is deliberately conservative: anything it does not
//! recognise stays literal, all source text is XML-escaped before any tag
//! is added, and the caller validates the finished markup with Pango and
//! falls back to the plain label when validation fails — a formatting bug
//! must never hide or corrupt the reply itself.

/// One rendered block. Text payloads are Pango markup produced by
/// [`inline_markup`], except `Code`, which is raw source shown verbatim in
/// a monospace block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(String),
    Heading(u8, String),
    Bullets(Vec<String>),
    Numbers(Vec<String>),
    Code(String),
    Quote(String),
    Rule,
}

/// True when the text carries markdown structure worth rendering richly.
/// Plain prose stays on the plain-label path, byte for byte.
pub fn looks_like_markdown(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_end();
        line.starts_with("```")
            || line.starts_with("# ")
            || line.starts_with("## ")
            || line.starts_with("### ")
            || line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with("> ")
            || line.starts_with('|')
    }) || text.contains("**")
        || text.contains("`")
        || text.contains("](")
}

/// Split markdown source into blocks. Line-oriented and forgiving: an
/// unterminated fence runs to the end, list items absorb indented
/// continuation lines, and table rows are kept together as a code block
/// so their columns survive in monospace.
pub fn markdown_blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    // str::lines already tolerates CRLF endings.
    let raw_lines: Vec<&str> = text.lines().collect();
    let mut index = 0;
    let mut paragraph: Vec<&str> = Vec::new();
    let flush_paragraph = |paragraph: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(inline_markup(&paragraph.join(" "))));
            paragraph.clear();
        }
    };
    while index < raw_lines.len() {
        let line = raw_lines[index];
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            flush_paragraph(&mut paragraph, &mut blocks);
            let mut code = Vec::new();
            index += 1;
            while index < raw_lines.len() && !raw_lines[index].trim().starts_with("```") {
                code.push(raw_lines[index]);
                index += 1;
            }
            index += 1; // closing fence (or one past the end)
            blocks.push(Block::Code(code.join("\n")));
            continue;
        }
        if trimmed.is_empty() {
            flush_paragraph(&mut paragraph, &mut blocks);
            index += 1;
            continue;
        }
        if let Some(heading) = parse_heading(trimmed) {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(heading);
            index += 1;
            continue;
        }
        if is_rule(trimmed) {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block::Rule);
            index += 1;
            continue;
        }
        if trimmed.starts_with('|') {
            flush_paragraph(&mut paragraph, &mut blocks);
            let mut table = Vec::new();
            while index < raw_lines.len() && raw_lines[index].trim().starts_with('|') {
                table.push(raw_lines[index].trim());
                index += 1;
            }
            blocks.push(Block::Code(table.join("\n")));
            continue;
        }
        if trimmed.starts_with("> ") || trimmed == ">" {
            flush_paragraph(&mut paragraph, &mut blocks);
            let mut quote = Vec::new();
            while index < raw_lines.len() {
                let q = raw_lines[index].trim();
                if let Some(rest) = q.strip_prefix("> ") {
                    quote.push(rest);
                } else if q == ">" {
                    quote.push("");
                } else {
                    break;
                }
                index += 1;
            }
            blocks.push(Block::Quote(inline_markup(&quote.join(" "))));
            continue;
        }
        if let Some(item) = bullet_item(trimmed) {
            flush_paragraph(&mut paragraph, &mut blocks);
            let mut items = vec![item];
            index += 1;
            while index < raw_lines.len() {
                let next = raw_lines[index].trim();
                if let Some(item) = bullet_item(next) {
                    items.push(item);
                    index += 1;
                } else if is_continuation(raw_lines[index]) {
                    let last = items.last_mut().expect("items is non-empty");
                    *last = format!("{last} {}", next);
                    index += 1;
                } else {
                    break;
                }
            }
            blocks.push(Block::Bullets(
                items.iter().map(|item| inline_markup(item)).collect(),
            ));
            continue;
        }
        if let Some(item) = numbered_item(trimmed) {
            flush_paragraph(&mut paragraph, &mut blocks);
            let mut items = vec![item];
            index += 1;
            while index < raw_lines.len() {
                let next = raw_lines[index].trim();
                if let Some(item) = numbered_item(next) {
                    items.push(item);
                    index += 1;
                } else if is_continuation(raw_lines[index]) {
                    let last = items.last_mut().expect("items is non-empty");
                    *last = format!("{last} {}", next);
                    index += 1;
                } else {
                    break;
                }
            }
            blocks.push(Block::Numbers(
                items.iter().map(|item| inline_markup(item)).collect(),
            ));
            continue;
        }
        paragraph.push(trimmed);
        index += 1;
    }
    flush_paragraph(&mut paragraph, &mut blocks);
    blocks
}

fn parse_heading(line: &str) -> Option<Block> {
    for (marker, level) in [("### ", 3u8), ("## ", 2), ("# ", 1)] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(Block::Heading(level, inline_markup(rest.trim())));
        }
    }
    None
}

fn is_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| *c != ' ').collect();
    compact.len() >= 3
        && (compact.chars().all(|c| c == '-')
            || compact.chars().all(|c| c == '*')
            || compact.chars().all(|c| c == '_'))
}

fn bullet_item(line: &str) -> Option<String> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

fn numbered_item(line: &str) -> Option<String> {
    let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    let rest = &line[digits.len()..];
    rest.strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))
        .map(|item| item.trim().to_string())
}

fn is_continuation(line: &str) -> bool {
    (line.starts_with("  ") || line.starts_with('\t')) && !line.trim().is_empty()
}

/// Convert inline markdown to Pango markup. The source is consumed in one
/// pass; code spans are taken verbatim first at each position, then links,
/// then strong/emphasis/strike pairs. Unmatched markers stay literal.
pub fn inline_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    render_inline(text, &mut out, 0);
    out
}

fn render_inline(text: &str, out: &mut String, depth: u8) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Code span: verbatim until the closing backtick.
        if bytes[i] == b'`'
            && let Some(end) = find_byte(bytes, i + 1, b'`')
        {
            out.push_str("<tt>");
            escape_into(&text[i + 1..end], out);
            out.push_str("</tt>");
            i = end + 1;
            continue;
        }
        // Link: [label](url)
        if bytes[i] == b'['
            && depth < 3
            && let Some((label, url, next)) = parse_link(text, i)
        {
            out.push_str("<a href=\"");
            escape_attr_into(url, out);
            out.push_str("\">");
            render_inline(label, out, depth + 1);
            out.push_str("</a>");
            i = next;
            continue;
        }
        // Strong: **text** or __text__
        if depth < 3
            && (text[i..].starts_with("**") || text[i..].starts_with("__"))
            && let Some(end) = text[i + 2..].find(&text[i..i + 2])
            && let inner = &text[i + 2..i + 2 + end]
            && !inner.is_empty()
            && !inner.starts_with(' ')
            && !inner.ends_with(' ')
        {
            out.push_str("<b>");
            render_inline(inner, out, depth + 1);
            out.push_str("</b>");
            i += 2 + end + 2;
            continue;
        }
        // Strike: ~~text~~
        if depth < 3
            && text[i..].starts_with("~~")
            && let Some(end) = text[i + 2..].find("~~")
            && let inner = &text[i + 2..i + 2 + end]
            && !inner.is_empty()
        {
            out.push_str("<s>");
            render_inline(inner, out, depth + 1);
            out.push_str("</s>");
            i += 2 + end + 2;
            continue;
        }
        // Emphasis: *text* or _text_, with word-boundary care so that
        // snake_case names and multiplication stay literal.
        if depth < 3
            && (bytes[i] == b'*' || bytes[i] == b'_')
            && (i == 0 || !is_word_byte(bytes[i - 1]))
            && let Some(end) = find_emphasis_end(bytes, i + 1, bytes[i])
            && let inner = &text[i + 1..end]
            && !inner.is_empty()
            && !inner.starts_with(' ')
            && !inner.ends_with(' ')
        {
            out.push_str("<i>");
            render_inline(inner, out, depth + 1);
            out.push_str("</i>");
            i = end + 1;
            continue;
        }
        // Plain character (UTF-8 safe: copy the whole char).
        let ch_len = utf8_len(bytes[i]);
        escape_into(&text[i..i + ch_len], out);
        i += ch_len;
    }
}

fn parse_link(text: &str, start: usize) -> Option<(&str, &str, usize)> {
    let bytes = text.as_bytes();
    let close = find_byte(bytes, start + 1, b']')?;
    if bytes.get(close + 1) != Some(&b'(') {
        return None;
    }
    let end = find_byte(bytes, close + 2, b')')?;
    let url = &text[close + 2..end];
    if url.is_empty() || url.contains(char::is_whitespace) {
        return None;
    }
    Some((&text[start + 1..close], url, end + 1))
}

fn find_byte(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|b| *b == needle)
        .map(|pos| from + pos)
}

fn find_emphasis_end(bytes: &[u8], from: usize, marker: u8) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == marker {
            // The closing marker must not be followed by a word char when
            // it is an underscore (snake_case), and must close real text.
            if marker == b'_' && i + 1 < bytes.len() && is_word_byte(bytes[i + 1]) {
                i += 1;
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

fn escape_into(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn escape_attr_into(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_prose_is_not_markdown() {
        assert!(!looks_like_markdown("Just a normal reply."));
        assert!(!looks_like_markdown("Multi\nline\nprose."));
        assert!(looks_like_markdown("Has **bold** in it."));
        assert!(looks_like_markdown("- a list"));
        assert!(looks_like_markdown("text\n## Heading"));
    }

    #[test]
    fn inline_markup_covers_the_common_forms() {
        assert_eq!(inline_markup("a **bold** word"), "a <b>bold</b> word");
        assert_eq!(inline_markup("a *soft* word"), "a <i>soft</i> word");
        assert_eq!(
            inline_markup("run `cargo test` now"),
            "run <tt>cargo test</tt> now"
        );
        assert_eq!(
            inline_markup("see [the docs](https://example.com/a?b=1&c=2)"),
            "see <a href=\"https://example.com/a?b=1&amp;c=2\">the docs</a>"
        );
        assert_eq!(
            inline_markup("**bold `code` mix**"),
            "<b>bold <tt>code</tt> mix</b>"
        );
        assert_eq!(inline_markup("~~gone~~"), "<s>gone</s>");
    }

    #[test]
    fn inline_markup_escapes_and_leaves_non_markup_literal() {
        assert_eq!(inline_markup("1 < 2 & 3 > 2"), "1 &lt; 2 &amp; 3 &gt; 2");
        assert_eq!(
            inline_markup("snake_case_name stays"),
            "snake_case_name stays"
        );
        assert_eq!(inline_markup("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(
            inline_markup("an **unclosed marker"),
            "an **unclosed marker"
        );
        assert_eq!(
            inline_markup("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert_eq!(inline_markup("café ☕ unicode"), "café ☕ unicode");
    }

    #[test]
    fn blocks_split_the_way_replies_are_written() {
        let text = "Intro paragraph with **weight**.\n\n## Findings\n\n- first\n- second with `code`\n\n1. one\n2. two\n\n```sh\ncargo test\n```\n\n> quoted line";
        let blocks = markdown_blocks(text);
        assert_eq!(blocks.len(), 6);
        assert_eq!(
            blocks[0],
            Block::Paragraph("Intro paragraph with <b>weight</b>.".into())
        );
        assert_eq!(blocks[1], Block::Heading(2, "Findings".into()));
        assert_eq!(
            blocks[2],
            Block::Bullets(vec!["first".into(), "second with <tt>code</tt>".into()])
        );
        assert_eq!(blocks[3], Block::Numbers(vec!["one".into(), "two".into()]));
        assert_eq!(blocks[4], Block::Code("cargo test".into()));
        assert_eq!(blocks[5], Block::Quote("quoted line".into()));
    }

    #[test]
    fn tables_become_code_and_soft_lines_join() {
        let blocks = markdown_blocks("line one\nline two\n\n| a | b |\n| - | - |\n| 1 | 2 |");
        assert_eq!(blocks[0], Block::Paragraph("line one line two".into()));
        assert!(matches!(blocks[1], Block::Code(_)));
    }
}
