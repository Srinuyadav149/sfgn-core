use std::collections::BTreeSet;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Extracted fenced code block inside a Markdown section.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedCodeBlock {
    pub language: Option<String>,
    pub code: String,
}

/// Extracted Markdown table serialized into CSV rows.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedTable {
    pub csv: String,
    pub rows: usize,
    pub cols: usize,
}

/// Extracted heading section from a Markdown document.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedSection {
    pub title: String,
    pub slug: String,
    pub level: u8,
    pub content: String,
    pub code_blocks: Vec<ExtractedCodeBlock>,
    pub tables: Vec<ExtractedTable>,
    pub links: Vec<String>,
}

/// Extracted Markdown file representation.
#[derive(Debug, Clone)]
pub(crate) struct ExtractedMarkdown {
    pub preamble: String,
    pub preamble_links: Vec<String>,
    pub sections: Vec<ExtractedSection>,
}

/// Parses CommonMark + GFM tables into an ordered list of heading sections, prose,
/// fenced code blocks, CSV-encoded tables, and outgoing links.
pub(crate) fn parse_markdown(src: &str) -> ExtractedMarkdown {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(src, opts);

    let mut preamble_text = String::new();
    let mut preamble_links = BTreeSet::new();
    let mut sections: Vec<ExtractedSection> = Vec::new();

    let mut in_heading: Option<u8> = None;
    let mut heading_buf = String::new();

    let mut in_code_block = false;
    let mut code_lang: Option<String> = None;
    let mut code_buf = String::new();

    let mut in_table = false;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();
    let mut in_cell = false;

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                in_heading = Some(level as u8);
                heading_buf.clear();
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(lvl) = in_heading.take() {
                    let title = heading_buf.split_whitespace().collect::<Vec<_>>().join(" ");
                    let slug = slugify_heading(&title);
                    sections.push(ExtractedSection {
                        title,
                        slug,
                        level: lvl,
                        content: String::new(),
                        code_blocks: Vec::new(),
                        tables: Vec::new(),
                        links: Vec::new(),
                    });
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code_block = true;
                code_buf.clear();
                code_lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        let lang = info.split_whitespace().next().unwrap_or("").trim();
                        if lang.is_empty() {
                            None
                        } else {
                            Some(sanitize_format_tag(lang))
                        }
                    }
                    CodeBlockKind::Indented => None,
                };
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                let block = ExtractedCodeBlock {
                    language: code_lang.take(),
                    code: code_buf.trim_end_matches('\n').to_owned(),
                };
                if let Some(sec) = sections.last_mut() {
                    sec.code_blocks.push(block);
                }
            }
            Event::Start(Tag::Table(_)) => {
                in_table = true;
                table_rows.clear();
            }
            Event::End(TagEnd::TableHead) => {
                if !current_row.is_empty() {
                    table_rows.push(std::mem::take(&mut current_row));
                }
            }
            Event::Start(Tag::TableRow) => {
                current_row.clear();
            }
            Event::End(TagEnd::TableRow) => {
                if !current_row.is_empty() {
                    table_rows.push(std::mem::take(&mut current_row));
                }
            }
            Event::Start(Tag::TableCell) => {
                in_cell = true;
                current_cell.clear();
            }
            Event::End(TagEnd::TableCell) => {
                in_cell = false;
                let cell_text = current_cell
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                current_row.push(cell_text);
            }
            Event::End(TagEnd::Table) => {
                in_table = false;
                if !table_rows.is_empty() {
                    let row_count = table_rows.len();
                    let col_count = table_rows.iter().map(Vec::len).max().unwrap_or(0);
                    let csv = render_table_csv(&table_rows);
                    if let Some(sec) = sections.last_mut() {
                        sec.tables.push(ExtractedTable {
                            csv,
                            rows: row_count,
                            cols: col_count,
                        });
                    }
                }
            }
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                let target = dest_url.trim();
                if !target.is_empty()
                    && !target.starts_with("http://")
                    && !target.starts_with("https://")
                    && !target.starts_with("mailto:")
                {
                    if let Some(sec) = sections.last_mut() {
                        if !sec.links.iter().any(|l| l == target) {
                            sec.links.push(target.to_owned());
                        }
                    } else {
                        preamble_links.insert(target.to_owned());
                    }
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if in_heading.is_some() {
                    heading_buf.push_str(&text);
                } else if in_code_block {
                    code_buf.push_str(&text);
                } else if in_table && in_cell {
                    current_cell.push_str(&text);
                } else if let Some(sec) = sections.last_mut() {
                    sec.content.push_str(&text);
                } else {
                    preamble_text.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_heading.is_some() {
                    heading_buf.push(' ');
                } else if in_code_block {
                    code_buf.push('\n');
                } else if in_table && in_cell {
                    current_cell.push(' ');
                } else if let Some(sec) = sections.last_mut() {
                    sec.content.push('\n');
                } else {
                    preamble_text.push('\n');
                }
            }
            Event::End(TagEnd::Paragraph | TagEnd::Item | TagEnd::BlockQuote(_)) => {
                if in_heading.is_none() && !in_code_block && !in_table {
                    if let Some(sec) = sections.last_mut() {
                        if !sec.content.ends_with("\n\n") {
                            sec.content.push('\n');
                        }
                    } else if !preamble_text.ends_with("\n\n") {
                        preamble_text.push('\n');
                    }
                }
            }
            _ => {}
        }
    }

    for sec in &mut sections {
        sec.content = sec.content.trim().to_owned();
    }

    ExtractedMarkdown {
        preamble: preamble_text.trim().to_owned(),
        preamble_links: preamble_links.into_iter().collect(),
        sections,
    }
}

/// Converts a Markdown heading into a GitHub-compatible lowercase `#anchor` slug.
pub fn slugify_heading(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == ' ' || ch == '-' || ch == '_' {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

fn render_table_csv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for (r_idx, row) in rows.iter().enumerate() {
        if r_idx > 0 {
            out.push('\n');
        }
        for (c_idx, cell) in row.iter().enumerate() {
            if c_idx > 0 {
                out.push(',');
            }
            if cell.contains(',') || cell.contains('"') || cell.contains('\n') {
                out.push('"');
                for ch in cell.chars() {
                    if ch == '"' {
                        out.push_str("\"\"");
                    } else {
                        out.push(ch);
                    }
                }
                out.push('"');
            } else {
                out.push_str(cell);
            }
        }
    }
    out
}

fn sanitize_format_tag(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for (idx, ch) in raw.chars().enumerate() {
        if idx == 0 {
            if ch.is_ascii_alphabetic() || ch == '_' {
                out.push(ch);
            } else {
                out.push('_');
            }
        } else if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+' | '.') {
            out.push(ch);
        }
    }
    if out.is_empty() {
        "text".to_owned()
    } else {
        out
    }
}
