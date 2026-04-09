use super::*;

/// Parse markdown and return ANSI-formatted lines.
pub(super) fn render_markdown(source: &str) -> Vec<String> {
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let parser = Parser::new_ext(source, opts);

    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();
    let mut style_stack: Vec<Style> = Vec::new();
    let mut in_code_block = false;
    let mut code_block_lang = String::new();
    let mut code_block_buf = String::new();
    let mut in_heading = false;
    let mut list_depth: usize = 0;
    let mut ordered_indices: Vec<u64> = Vec::new();
    let mut table_cells: Vec<String> = Vec::new();
    let mut table_cell_buf = String::new();
    let mut in_table = false;

    for event in parser {
        match event {
            Event::Start(tag) => match &tag {
                Tag::Heading { level, .. } => {
                    in_heading = true;
                    if !current_line.is_empty() || !lines.is_empty() {
                        push_line(&mut lines, &mut current_line);
                    }
                    // Heading prefix
                    let marker = "#".repeat(*level as usize);
                    current_line.push_str(&format!("{BOLD}{YELLOW}{marker} "));
                }
                Tag::Emphasis => {
                    current_line.push_str(ITALIC);
                    style_stack.push(Style::Emphasis);
                }
                Tag::Strong => {
                    current_line.push_str(BOLD);
                    style_stack.push(Style::Strong);
                }
                Tag::Strikethrough => {
                    current_line.push_str("\x1b[9m");
                    style_stack.push(Style::Strikethrough);
                }
                Tag::CodeBlock(kind) => {
                    in_code_block = true;
                    code_block_buf.clear();
                    code_block_lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(lang) => lang.to_string(),
                        _ => String::new(),
                    };
                }
                Tag::Link { .. } => {
                    current_line.push_str(&format!("{CYAN}{UNDERLINE}"));
                    style_stack.push(Style::Link);
                }
                Tag::BlockQuote(_) => {
                    style_stack.push(Style::BlockQuote);
                }
                Tag::List(start) => {
                    list_depth += 1;
                    if let Some(n) = start {
                        ordered_indices.push(*n);
                    } else {
                        ordered_indices.push(0); // 0 = unordered
                    }
                }
                Tag::Item => {
                    let indent = "  ".repeat(list_depth.saturating_sub(1));
                    let marker = match ordered_indices.last().copied() {
                        Some(0) => {
                            format!("{DIM}-{RESET} ")
                        }
                        Some(n) => {
                            if let Some(last) = ordered_indices.last_mut() {
                                *last = n + 1;
                            }
                            format!("{DIM}{n}.{RESET} ")
                        }
                        None => format!("{DIM}-{RESET} "),
                    };
                    current_line.push_str(&format!("{indent}{marker}"));
                }
                Tag::Table(_) => {
                    in_table = true;
                    if !lines.is_empty() {
                        push_line(&mut lines, &mut current_line);
                    }
                }
                Tag::TableHead | Tag::TableRow => {
                    table_cells.clear();
                }
                Tag::TableCell => {
                    table_cell_buf.clear();
                }
                Tag::Paragraph => {
                    // Add blank line before paragraph if there's prior content
                    // (but not for the first paragraph, and not inside list items)
                    if !lines.is_empty() && list_depth == 0 {
                        push_line(&mut lines, &mut current_line);
                    }
                    // Apply blockquote styling if inside one
                    for s in &style_stack {
                        if *s == Style::BlockQuote {
                            current_line.push_str(&format!("{GREEN}> "));
                        }
                    }
                }
                _ => {}
            },
            Event::End(tag_end) => match &tag_end {
                TagEnd::Heading(_) => {
                    current_line.push_str(RESET);
                    push_line(&mut lines, &mut current_line);
                    in_heading = false;
                    style_stack.pop();
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                    current_line.push_str(RESET);
                    style_stack.pop();
                    reapply_inline_styles(&mut current_line, &style_stack);
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    push_line(&mut lines, &mut current_line);
                    let lang_label = if code_block_lang.is_empty() {
                        "code".to_string()
                    } else {
                        code_block_lang.clone()
                    };
                    current_line.push_str(&format!("{DIM}  ╭─{RESET} {CYAN}{lang_label}{RESET}"));
                    push_line(&mut lines, &mut current_line);
                    for code_line in code_block_buf.lines() {
                        current_line.push_str(&format!("{DIM}  │{RESET} {code_line}"));
                        push_line(&mut lines, &mut current_line);
                    }
                    current_line.push_str(&format!("{DIM}  ╰─{RESET}"));
                    push_line(&mut lines, &mut current_line);
                    code_block_buf.clear();
                }
                TagEnd::Link => {
                    current_line.push_str(RESET);
                    style_stack.pop();
                }
                TagEnd::BlockQuote(_) => {
                    style_stack.pop();
                }
                TagEnd::List(_) => {
                    list_depth = list_depth.saturating_sub(1);
                    ordered_indices.pop();
                }
                TagEnd::Table => {
                    in_table = false;
                }
                TagEnd::TableHead => {
                    let row = format!(
                        "{DIM}|{RESET} {BOLD}{}{RESET} {DIM}|{RESET}",
                        table_cells.join(&format!(" {DIM}|{RESET} {BOLD}"))
                    );
                    current_line.push_str(&row);
                    push_line(&mut lines, &mut current_line);
                    let sep = table_cells
                        .iter()
                        .map(|c| "-".repeat(c.chars().count().max(3)))
                        .collect::<Vec<_>>()
                        .join(&format!("-{DIM}|{RESET}-"));
                    current_line.push_str(&format!("{DIM}|{RESET}-{sep}-{DIM}|{RESET}"));
                    push_line(&mut lines, &mut current_line);
                    table_cells.clear();
                }
                TagEnd::TableRow => {
                    let row = format!(
                        "{DIM}|{RESET} {} {DIM}|{RESET}",
                        table_cells.join(&format!(" {DIM}|{RESET} "))
                    );
                    current_line.push_str(&row);
                    push_line(&mut lines, &mut current_line);
                    table_cells.clear();
                }
                TagEnd::TableCell => {
                    table_cells.push(std::mem::take(&mut table_cell_buf));
                }
                TagEnd::Item => {}
                TagEnd::Paragraph => {
                    push_line(&mut lines, &mut current_line);
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_code_block {
                    code_block_buf.push_str(&text);
                } else if in_table {
                    table_cell_buf.push_str(&text);
                } else {
                    current_line.push_str(&text);
                }
            }
            Event::Code(code) => {
                if in_table {
                    table_cell_buf.push_str(&format!("`{code}`"));
                } else {
                    current_line.push_str(&format!("{CYAN}`{code}`{RESET}"));
                    // Re-apply active styles after reset
                    if in_heading {
                        current_line.push_str(&format!("{BOLD}{YELLOW}"));
                    }
                    reapply_inline_styles(&mut current_line, &style_stack);
                }
            }
            Event::SoftBreak => {
                current_line.push(' ');
            }
            Event::HardBreak => {
                push_line(&mut lines, &mut current_line);
            }
            Event::Rule => {
                push_line(&mut lines, &mut current_line);
                current_line.push_str(&format!("{DIM}───{RESET}"));
                push_line(&mut lines, &mut current_line);
            }
            _ => {}
        }
    }

    if !current_line.is_empty() {
        push_line(&mut lines, &mut current_line);
    }

    lines
}

pub(super) fn push_line(lines: &mut Vec<String>, current: &mut String) {
    lines.push(std::mem::take(current));
}

pub(super) fn reapply_inline_styles(current_line: &mut String, style_stack: &[Style]) {
    for style in style_stack {
        match style {
            Style::Emphasis => current_line.push_str(ITALIC),
            Style::Strong => current_line.push_str(BOLD),
            Style::Strikethrough => current_line.push_str("\x1b[9m"),
            Style::Link => current_line.push_str(&format!("{CYAN}{UNDERLINE}")),
            Style::BlockQuote => {}
        }
    }
}
