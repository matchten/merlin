//! The Markdown subset Merlin's replies use: paragraphs, `#` headings, `-` / `1.` lists, fenced
//! code blocks, `---` rules, and inline **bold**, *italic* and `code`. Parsing is forgiving: a
//! marker without its closing half (a reply still streaming in) stays plain text.

/// A run of text with one style.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Span>),
    /// Level 1–3.
    Heading(u8, Vec<Span>),
    /// A list item: its marker (`•` or `3.`) and nesting depth.
    Item {
        marker: String,
        depth: usize,
        spans: Vec<Span>,
    },
    Code {
        lang: String,
        text: String,
    },
    Rule,
}

/// Split `src` into blocks.
pub fn parse(src: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut lines = src.lines();
    let flush = |para: &mut Vec<&str>, out: &mut Vec<Block>| {
        if !para.is_empty() {
            out.push(Block::Paragraph(inline(&para.join("\n"))));
            para.clear();
        }
    };
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if let Some(lang) = trimmed.strip_prefix("```") {
            flush(&mut para, &mut out);
            let mut body = Vec::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    break;
                }
                body.push(l);
            }
            out.push(Block::Code { lang: lang.trim().to_string(), text: body.join("\n") });
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut para, &mut out);
            continue;
        }
        if matches!(trimmed, "---" | "***" | "___") {
            flush(&mut para, &mut out);
            out.push(Block::Rule);
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        let after_hashes = trimmed.get(hashes..).unwrap_or_default();
        if (1..=6).contains(&hashes) && after_hashes.starts_with(' ') {
            flush(&mut para, &mut out);
            out.push(Block::Heading(hashes.min(3) as u8, inline(after_hashes.trim())));
            continue;
        }
        let depth = line.len().saturating_sub(trimmed.len()) / 2;
        if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")).or_else(|| trimmed.strip_prefix("• ")) {
            flush(&mut para, &mut out);
            out.push(Block::Item { marker: "•".into(), depth, spans: inline(rest) });
            continue;
        }
        let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
        if (1..=3).contains(&digits)
            && let Some(rest) = trimmed.get(digits..).and_then(|r| r.strip_prefix(". "))
        {
            flush(&mut para, &mut out);
            let marker = trimmed.get(..digits + 1).unwrap_or_default().to_string();
            out.push(Block::Item { marker, depth, spans: inline(rest) });
            continue;
        }
        para.push(line);
    }
    flush(&mut para, &mut out);
    out
}

/// Inline styles of one block's text.
pub fn inline(s: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut cur = Span::default();
    let mut rest = s;
    let push = |cur: &mut Span, out: &mut Vec<Span>| {
        if !cur.text.is_empty() {
            let next = Span { text: String::new(), ..cur.clone() };
            out.push(std::mem::replace(cur, next));
        }
    };
    while let Some(c) = rest.chars().next() {
        let after = rest.get(c.len_utf8()..).unwrap_or_default();
        if c == '`' && (cur.code || after.contains('`')) {
            push(&mut cur, &mut out);
            cur.code = !cur.code;
            rest = after;
            continue;
        }
        if !cur.code
            && let Some(tail) = rest.strip_prefix("**")
            && (cur.bold || tail.contains("**"))
        {
            push(&mut cur, &mut out);
            cur.bold = !cur.bold;
            rest = tail;
            continue;
        }
        // An unpaired `**` is literal (not two italic markers).
        if !cur.code && rest.starts_with("**") {
            cur.text.push_str("**");
            rest = rest.get(2..).unwrap_or_default();
            continue;
        }
        // A single `*` opens italics only before a non-space and when it closes later; `_` is left
        // alone (identifiers like snake_case are common in replies).
        if !cur.code && c == '*' && (cur.italic || (after.chars().next().is_some_and(|n| !n.is_whitespace()) && after.contains('*'))) {
            push(&mut cur, &mut out);
            cur.italic = !cur.italic;
            rest = after;
            continue;
        }
        cur.text.push(c);
        rest = after;
    }
    push(&mut cur, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn inline_styles() {
        let s = inline("Make **this** *pop* with `wiggle()` now");
        let styled: Vec<(&str, bool, bool, bool)> = s.iter().map(|s| (s.text.as_str(), s.bold, s.italic, s.code)).collect();
        assert_eq!(
            styled,
            vec![
                ("Make ", false, false, false),
                ("this", true, false, false),
                (" ", false, false, false),
                ("pop", false, true, false),
                (" with ", false, false, false),
                ("wiggle()", false, false, true),
                (" now", false, false, false)
            ]
        );
    }

    #[test]
    fn unclosed_markers_and_code_contents_stay_literal() {
        assert_eq!(plain(&inline("a **half")), "a **half");
        assert_eq!(plain(&inline("2 * 3 = 6")), "2 * 3 = 6");
        let s = inline("`a**b**` snake_case_name");
        assert_eq!(s[0].text, "a**b**");
        assert!(s[0].code && !s[0].bold);
        assert_eq!(s[1].text, " snake_case_name");
        assert!(inline("").is_empty());
        assert_eq!(plain(&inline("é**ü**ñ")), "éüñ");
    }

    #[test]
    fn blocks() {
        let b = parse("# Title\nIntro line\nsecond line\n\n- one\n  - nested\n2. two\n\n```js\nvar a = 1;\n```\n---\nend");
        assert!(matches!(&b[0], Block::Heading(1, s) if plain(s) == "Title"));
        assert!(matches!(&b[1], Block::Paragraph(s) if plain(s) == "Intro line\nsecond line"));
        assert!(matches!(&b[2], Block::Item { marker, depth: 0, spans } if marker == "•" && plain(spans) == "one"));
        assert!(matches!(&b[3], Block::Item { depth: 1, .. }));
        assert!(matches!(&b[4], Block::Item { marker, .. } if marker == "2."));
        assert!(matches!(&b[5], Block::Code { lang, text } if lang == "js" && text == "var a = 1;"));
        assert_eq!(b[6], Block::Rule);
        assert!(matches!(&b[7], Block::Paragraph(s) if plain(s) == "end"));
    }

    #[test]
    fn an_unfinished_code_fence_takes_the_rest() {
        let b = parse("Here:\n```\nline 1\nline 2");
        assert!(matches!(&b[1], Block::Code { text, .. } if text == "line 1\nline 2"));
        assert!(parse("").is_empty());
        assert!(matches!(&parse("#hashtag")[0], Block::Paragraph(_)));
    }
}
