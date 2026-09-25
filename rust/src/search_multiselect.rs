//! Interactive search multiselect prompt (port of prompts/search-multiselect.ts).
//!
//! Selection is tracked by item index in insertion order, mirroring the JS
//! `Set` semantics that determine the order of the returned values.

use crate::color::pc;
use crate::sys;
use crate::ui::{self, approx_string_width, count_visual_rows, Key};

#[derive(Clone)]
pub struct SearchItem<T> {
    pub value: T,
    /// `String(item.value)`, used for filtering.
    pub key: String,
    pub label: String,
    pub hint: Option<String>,
    pub group: Option<String>,
    pub detail: Option<String>,
}

impl<T> SearchItem<T> {
    pub fn new(value: T, key: &str, label: &str) -> Self {
        SearchItem {
            value,
            key: key.to_string(),
            label: label.to_string(),
            hint: None,
            group: None,
            detail: None,
        }
    }
    pub fn hint(mut self, hint: Option<String>) -> Self {
        self.hint = hint;
        self
    }
    pub fn group(mut self, group: Option<String>) -> Self {
        self.group = group;
        self
    }
    pub fn detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }
}

pub struct LockedSection<T> {
    pub title: String,
    pub items: Vec<SearchItem<T>>,
    pub hidden_count: usize,
}

pub struct Options<T> {
    pub message: String,
    pub items: Vec<SearchItem<T>>,
    pub max_visible: usize,
    pub initial_selected: Vec<usize>,
    pub required: bool,
    pub locked_section: Option<LockedSection<T>>,
    pub searchable: bool,
    pub show_detail: bool,
    pub detail_lines: usize,
    pub show_selected_summary: bool,
    pub select_groups: bool,
    pub select_all: bool,
}

impl<T> Options<T> {
    pub fn new(message: &str, items: Vec<SearchItem<T>>) -> Self {
        Options {
            message: message.to_string(),
            items,
            max_visible: 8,
            initial_selected: Vec::new(),
            required: false,
            locked_section: None,
            searchable: true,
            show_detail: false,
            detail_lines: 2,
            show_selected_summary: true,
            select_groups: false,
            select_all: false,
        }
    }
}

#[derive(Clone)]
enum Entry {
    /// index into `items`
    Item(usize),
    Group {
        group: String,
        items: Vec<usize>,
        collapsed: bool,
    },
}

/// Ordered set of item indices (JS `Set` insertion order).
#[derive(Default, Clone)]
struct Selection(Vec<usize>);

impl Selection {
    fn has(&self, i: usize) -> bool {
        self.0.contains(&i)
    }
    fn add(&mut self, i: usize) {
        if !self.has(i) {
            self.0.push(i);
        }
    }
    fn delete(&mut self, i: usize) {
        self.0.retain(|x| *x != i);
    }
    fn len(&self) -> usize {
        self.0.len()
    }
}

fn truncate_to_width(text: &str, width: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let mut candidate = out.clone();
        candidate.push(c);
        if approx_string_width(&candidate) > width {
            break;
        }
        out = candidate;
    }
    out
}

/// Wrap description text into a fixed number of width-safe lines.
pub fn format_detail_lines(detail: Option<&str>, width: usize, max_lines: usize) -> Vec<String> {
    let safe_width = width.max(1);
    let normalized = detail
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    let mut remaining = normalized;
    while !remaining.is_empty() && lines.len() < max_lines {
        if approx_string_width(&remaining) <= safe_width {
            lines.push(remaining.clone());
            remaining.clear();
            break;
        }
        let candidate = truncate_to_width(&remaining, safe_width);
        let break_at = candidate.rfind(' ');
        match break_at {
            Some(b) if b > 0 => {
                lines.push(candidate[..b].trim_end().to_string());
                remaining = remaining[b..].trim_start().to_string();
            }
            _ => {
                lines.push(candidate.clone());
                remaining = remaining[candidate.len()..].trim_start().to_string();
            }
        }
    }
    if !remaining.is_empty() && !lines.is_empty() {
        let last = lines.len() - 1;
        lines[last] = format!(
            "{}…",
            truncate_to_width(&lines[last], safe_width.saturating_sub(1)).trim_end()
        );
    }
    while lines.len() < max_lines {
        lines.push(String::new());
    }
    lines
}

fn build_entries<T>(
    items: &[SearchItem<T>],
    filtered: &[usize],
    select_groups: bool,
    collapsed: &[String],
) -> Vec<Entry> {
    if !select_groups {
        return filtered.iter().map(|i| Entry::Item(*i)).collect();
    }
    let mut entries = Vec::new();
    let mut idx = 0;
    while idx < filtered.len() {
        let item = &items[filtered[idx]];
        let group = match &item.group {
            None => {
                entries.push(Entry::Item(filtered[idx]));
                idx += 1;
                continue;
            }
            Some(g) => g.clone(),
        };
        let mut group_items = Vec::new();
        while idx < filtered.len() && items[filtered[idx]].group.as_deref() == Some(group.as_str())
        {
            group_items.push(filtered[idx]);
            idx += 1;
        }
        let is_collapsed = collapsed.contains(&group);
        entries.push(Entry::Group {
            group: group.clone(),
            items: group_items.clone(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            entries.extend(group_items.into_iter().map(Entry::Item));
        }
    }
    entries
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Active,
    Submit,
    Cancel,
}

fn s_bar() -> String {
    pc::dim("│")
}
fn s_bar_h() -> String {
    pc::dim("─")
}

/// Run the prompt. Returns `None` when cancelled (the TS `cancelSymbol`).
pub fn search_multiselect<T: Clone>(options: Options<T>) -> Option<Vec<T>> {
    let items = &options.items;
    let mut query = String::new();
    let mut cursor: usize = 0;
    let mut selected = Selection::default();
    for i in &options.initial_selected {
        selected.add(*i);
    }
    let mut collapsed: Vec<String> = Vec::new();
    let locked_values: Vec<T> = options
        .locked_section
        .as_ref()
        .map(|l| l.items.iter().map(|i| i.value.clone()).collect())
        .unwrap_or_default();
    let has_select_all = options.select_all && !items.is_empty();
    let offset = if has_select_all { 1 } else { 0 };

    let get_filtered = |q: &str| -> Vec<usize> {
        let lq = q.to_lowercase();
        (0..items.len())
            .filter(|i| {
                q.is_empty()
                    || items[*i].label.to_lowercase().contains(&lq)
                    || items[*i].key.to_lowercase().contains(&lq)
            })
            .collect()
    };

    let render = |state: State,
                  query: &str,
                  cursor: usize,
                  selected: &Selection,
                  collapsed: &[String]|
     -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let filtered = get_filtered(query);
        let entries = build_entries(items, &filtered, options.select_groups, collapsed);
        let entry_cursor = cursor as isize - offset as isize;
        let icon = match state {
            State::Active => pc::green("◆"),
            State::Cancel => pc::red("■"),
            State::Submit => pc::green("◇"),
        };
        lines.push(format!("{}  {}", icon, pc::bold(&options.message)));

        match state {
            State::Active => {
                if let Some(locked) = &options.locked_section {
                    if !locked.items.is_empty() {
                        lines.push(s_bar());
                        let locked_title = format!(
                            "{} {}",
                            pc::bold(&locked.title),
                            pc::dim("── always included")
                        );
                        lines.push(format!(
                            "{}  {}{} {} {}",
                            s_bar(),
                            s_bar_h(),
                            s_bar_h(),
                            locked_title,
                            s_bar_h().repeat(12)
                        ));
                        for item in &locked.items {
                            lines.push(format!(
                                "{}    {} {}",
                                s_bar(),
                                pc::green("•"),
                                pc::bold(&item.label)
                            ));
                        }
                        if locked.hidden_count > 0 {
                            lines.push(format!(
                                "{}    {}",
                                s_bar(),
                                pc::dim(format!("…and {} more", locked.hidden_count))
                            ));
                        }
                        lines.push(s_bar());
                        lines.push(format!(
                            "{}  {}{} {} {}",
                            s_bar(),
                            s_bar_h(),
                            s_bar_h(),
                            pc::bold("Additional agents"),
                            s_bar_h().repeat(29)
                        ));
                    }
                }
                if options.searchable {
                    lines.push(format!(
                        "{}  {} {}{}",
                        s_bar(),
                        pc::dim("Search:"),
                        query,
                        pc::inverse(" ")
                    ));
                    lines.push(format!(
                        "{}  {}",
                        s_bar(),
                        pc::dim("↑↓ move, space select, enter confirm")
                    ));
                    lines.push(s_bar());
                }
                if has_select_all {
                    let selected_count = (0..items.len()).filter(|i| selected.has(*i)).count();
                    let radio = if selected_count == items.len() {
                        pc::green("●")
                    } else if selected_count > 0 {
                        pc::yellow("◐")
                    } else {
                        pc::dim("○")
                    };
                    let is_cursor = cursor == 0;
                    let prefix = if is_cursor {
                        pc::cyan("❯")
                    } else {
                        " ".to_string()
                    };
                    let label = if is_cursor {
                        pc::underline(pc::bold("Select All"))
                    } else {
                        pc::bold("Select All")
                    };
                    lines.push(format!(
                        "{} {} {} {} {}",
                        s_bar(),
                        prefix,
                        radio,
                        label,
                        pc::dim(format!("({}/{})", selected_count, items.len()))
                    ));
                    lines.push(format!("{}   {}", s_bar(), s_bar_h().repeat(36)));
                }

                let columns = sys::terminal_columns().map(|c| c as usize).unwrap_or(80);
                let build_footer = |include_detail: bool, include_summary: bool| -> Vec<String> {
                    let mut footer = Vec::new();
                    if include_detail {
                        let detail: Option<String> = if has_select_all && cursor == 0 {
                            Some(format!("Select or clear all {} skills.", items.len()))
                        } else if entry_cursor >= 0 {
                            match entries.get(entry_cursor as usize) {
                                Some(Entry::Group {
                                    group, items: gi, ..
                                }) => Some(format!("Select all {} skills in {}.", gi.len(), group)),
                                Some(Entry::Item(i)) => items[*i].detail.clone(),
                                None => None,
                            }
                        } else {
                            None
                        };
                        let width = columns.saturating_sub(5).max(1);
                        footer.push(s_bar());
                        footer.push(format!("{}  {}", s_bar(), pc::dim("Description")));
                        for line in
                            format_detail_lines(detail.as_deref(), width, options.detail_lines)
                        {
                            footer.push(format!("{}  {}", s_bar(), pc::dim(line)));
                        }
                    }
                    if include_summary {
                        footer.push(s_bar());
                        let mut labels: Vec<String> = options
                            .locked_section
                            .as_ref()
                            .map(|l| l.items.iter().map(|i| i.label.clone()).collect())
                            .unwrap_or_default();
                        labels.extend(
                            (0..items.len())
                                .filter(|i| selected.has(*i))
                                .map(|i| items[i].label.clone()),
                        );
                        if labels.is_empty() {
                            footer.push(format!("{}  {}", s_bar(), pc::dim("Selected: (none)")));
                        } else {
                            let summary = if labels.len() <= 3 {
                                labels.join(", ")
                            } else {
                                format!("{} +{} more", labels[..3].join(", "), labels.len() - 3)
                            };
                            footer.push(format!(
                                "{}  {} {}",
                                s_bar(),
                                pc::green("Selected:"),
                                summary
                            ));
                        }
                    }
                    if !options.searchable {
                        footer.push(s_bar());
                        footer.push(format!(
                            "{}  {}",
                            s_bar(),
                            pc::dim("↑↓ move, ←→ collapse/expand, space select, enter confirm")
                        ));
                    }
                    footer.push(pc::dim("└"));
                    footer
                };

                let build_items = |visible_limit: usize| -> Vec<String> {
                    if filtered.is_empty() {
                        return vec![format!("{}  {}", s_bar(), pc::dim("No matches found"))];
                    }
                    let mut out = Vec::new();
                    let start = (entry_cursor - (visible_limit / 2) as isize)
                        .min(entries.len() as isize - visible_limit as isize)
                        .max(0) as usize;
                    let end = entries.len().min(start + visible_limit);
                    for (i, entry) in entries[start..end].iter().enumerate() {
                        let actual = (start + i) as isize;
                        let is_cursor = actual == entry_cursor;
                        match entry {
                            Entry::Group {
                                group,
                                items: gi,
                                collapsed,
                            } => {
                                let count = gi.iter().filter(|x| selected.has(**x)).count();
                                let radio = if count == gi.len() {
                                    pc::green("●")
                                } else if count > 0 {
                                    pc::yellow("◐")
                                } else {
                                    pc::dim("○")
                                };
                                let label = if is_cursor {
                                    pc::underline(pc::bold(group))
                                } else {
                                    pc::bold(group)
                                };
                                let prefix = if is_cursor {
                                    pc::cyan("❯")
                                } else {
                                    " ".to_string()
                                };
                                let disclosure = pc::dim(if *collapsed { "▸" } else { "▾" });
                                out.push(format!(
                                    "{} {} {} {} {}",
                                    s_bar(),
                                    prefix,
                                    disclosure,
                                    radio,
                                    label
                                ));
                            }
                            Entry::Item(idx) => {
                                let item = &items[*idx];
                                let radio = if selected.has(*idx) {
                                    pc::green("●")
                                } else {
                                    pc::dim("○")
                                };
                                let label = if is_cursor {
                                    pc::underline(&item.label)
                                } else {
                                    item.label.clone()
                                };
                                let hint = item
                                    .hint
                                    .as_ref()
                                    .map(|h| pc::dim(format!(" ({})", h)))
                                    .unwrap_or_default();
                                let prefix = if is_cursor {
                                    pc::cyan("❯")
                                } else {
                                    " ".to_string()
                                };
                                let group_items: Vec<usize> =
                                    if options.select_groups && item.group.is_some() {
                                        filtered
                                            .iter()
                                            .copied()
                                            .filter(|f| items[*f].group == item.group)
                                            .collect()
                                    } else {
                                        Vec::new()
                                    };
                                let tree = if !group_items.is_empty() {
                                    format!(
                                        "{} ",
                                        pc::dim(if group_items.last() == Some(idx) {
                                            "└─"
                                        } else {
                                            "├─"
                                        })
                                    )
                                } else {
                                    String::new()
                                };
                                out.push(format!(
                                    "{} {} {}{} {}{}",
                                    s_bar(),
                                    prefix,
                                    tree,
                                    radio,
                                    label,
                                    hint
                                ));
                            }
                        }
                    }
                    let hidden_before = start;
                    let hidden_after = entries.len() - end;
                    if hidden_before > 0 || hidden_after > 0 {
                        let mut parts = Vec::new();
                        if hidden_before > 0 {
                            parts.push(format!("↑ {} more", hidden_before));
                        }
                        if hidden_after > 0 {
                            parts.push(format!("↓ {} more", hidden_after));
                        }
                        out.push(format!("{}  {}", s_bar(), pc::dim(parts.join("  "))));
                    }
                    out
                };

                let max_frame_rows =
                    sys::terminal_rows().map(|r| (r as usize).saturating_sub(1).max(1));
                let fit = |include_detail: bool,
                           include_summary: bool|
                 -> (Vec<String>, Vec<String>, usize) {
                    let footer = build_footer(include_detail, include_summary);
                    let mut limit = options.max_visible.max(1);
                    let mut item_lines = build_items(limit);
                    let rows = |il: &Vec<String>| {
                        let all: Vec<String> = lines
                            .iter()
                            .chain(il.iter())
                            .chain(footer.iter())
                            .cloned()
                            .collect();
                        count_visual_rows(&all, Some(columns))
                    };
                    let mut frame_rows = rows(&item_lines);
                    while let Some(max) = max_frame_rows {
                        if frame_rows <= max || limit <= 1 {
                            break;
                        }
                        limit -= 1;
                        item_lines = build_items(limit);
                        frame_rows = rows(&item_lines);
                    }
                    (item_lines, footer, frame_rows)
                };

                let mut include_detail = options.show_detail;
                let mut include_summary = options.show_selected_summary;
                let mut fitted = fit(include_detail, include_summary);
                if let Some(max) = max_frame_rows {
                    if fitted.2 > max && include_detail {
                        include_detail = false;
                        fitted = fit(include_detail, include_summary);
                    }
                    if fitted.2 > max && include_summary {
                        include_summary = false;
                        fitted = fit(include_detail, include_summary);
                    }
                }
                lines.extend(fitted.0);
                lines.extend(fitted.1);
            }
            State::Submit => {
                let mut labels: Vec<String> = options
                    .locked_section
                    .as_ref()
                    .map(|l| l.items.iter().map(|i| i.label.clone()).collect())
                    .unwrap_or_default();
                labels.extend(
                    (0..items.len())
                        .filter(|i| selected.has(*i))
                        .map(|i| items[i].label.clone()),
                );
                lines.push(format!("{}  {}", s_bar(), pc::dim(labels.join(", "))));
            }
            State::Cancel => {
                lines.push(format!(
                    "{}  {}",
                    s_bar(),
                    pc::strikethrough(pc::dim("Cancelled"))
                ));
            }
        }
        lines
    };

    let mut frame = ui::Frame::new();
    let raw = match ui::RawMode::enable() {
        Some(r) => r,
        None => {
            // stdin is not interactive: behave like an EOF cancel.
            frame.render(&render(State::Cancel, &query, cursor, &selected, &collapsed).join("\n"));
            return None;
        }
    };

    frame.render(&render(State::Active, &query, cursor, &selected, &collapsed).join("\n"));
    let result = loop {
        let key = ui::read_key();
        let filtered = get_filtered(&query);
        let entries = build_entries(items, &filtered, options.select_groups, &collapsed);
        let entry = if cursor >= offset {
            entries.get(cursor - offset).cloned()
        } else {
            None
        };

        match key {
            Key::Enter => {
                if options.required && selected.len() == 0 && locked_values.is_empty() {
                    continue;
                }
                frame.render(
                    &render(State::Submit, &query, cursor, &selected, &collapsed).join("\n"),
                );
                let mut out = locked_values.clone();
                out.extend(selected.0.iter().map(|i| items[*i].value.clone()));
                break Some(out);
            }
            Key::Escape | Key::CtrlC => {
                frame.render(
                    &render(State::Cancel, &query, cursor, &selected, &collapsed).join("\n"),
                );
                break None;
            }
            Key::Up => cursor = cursor.saturating_sub(1),
            Key::Down => {
                let max = (entries.len() + offset).saturating_sub(1);
                cursor = (cursor + 1).min(max);
            }
            Key::Right if options.select_groups => {
                if let Some(Entry::Group {
                    group,
                    collapsed: true,
                    ..
                }) = &entry
                {
                    collapsed.retain(|g| g != group);
                } else {
                    continue;
                }
            }
            Key::Left if options.select_groups => {
                let group = match &entry {
                    Some(Entry::Group { group, .. }) => Some(group.clone()),
                    Some(Entry::Item(i)) => items[*i].group.clone(),
                    None => None,
                };
                match group {
                    Some(g) => {
                        if !collapsed.contains(&g) {
                            collapsed.push(g.clone());
                        }
                        let ce = build_entries(items, &filtered, options.select_groups, &collapsed);
                        let pos = ce
                            .iter()
                            .position(|e| matches!(e, Entry::Group { group, .. } if *group == g));
                        cursor = match pos {
                            Some(p) => p + offset,
                            None => offset.saturating_sub(1),
                        };
                    }
                    None => continue,
                }
            }
            Key::Right | Key::Left => continue,
            Key::Space => {
                if has_select_all && cursor == 0 {
                    let all = (0..items.len()).all(|i| selected.has(i));
                    for i in 0..items.len() {
                        if all {
                            selected.delete(i);
                        } else {
                            selected.add(i);
                        }
                    }
                } else {
                    match &entry {
                        Some(Entry::Group { items: gi, .. }) => {
                            let all = gi.iter().all(|i| selected.has(*i));
                            for i in gi {
                                if all {
                                    selected.delete(*i);
                                } else {
                                    selected.add(*i);
                                }
                            }
                        }
                        Some(Entry::Item(i)) => {
                            if selected.has(*i) {
                                selected.delete(*i);
                            } else {
                                selected.add(*i);
                            }
                        }
                        None => {}
                    }
                }
            }
            Key::Backspace => {
                query.pop();
                cursor = 0;
            }
            Key::Char(c) if options.searchable => {
                query.push(c);
                cursor = 0;
            }
            _ => continue,
        }
        frame.render(&render(State::Active, &query, cursor, &selected, &collapsed).join("\n"));
    };
    drop(raw);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_lines_wrap_and_pad() {
        let lines = format_detail_lines(Some("one two three four five"), 9, 2);
        assert_eq!(lines, vec!["one two", "three…"]);
        let empty = format_detail_lines(None, 10, 2);
        assert_eq!(empty, vec!["", ""]);
    }

    #[test]
    fn visual_rows_count_wrapping() {
        assert_eq!(ui::visual_rows_for_line("abcdef", 3), 2);
        assert_eq!(ui::visual_rows_for_line("", 3), 1);
        assert_eq!(ui::visual_rows_for_line("\x1b[31mab\x1b[39m", 2), 1);
    }
}
