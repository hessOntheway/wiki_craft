use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn markdown_heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|char| *char == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = trimmed.get(level..)?;
    if !rest.starts_with(' ') {
        return None;
    }
    let heading = rest.trim();
    if heading.is_empty() {
        None
    } else {
        Some(heading.to_string())
    }
}

pub(crate) fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

pub(crate) fn sort_dedup_nonempty(values: &mut Vec<String>) {
    values.retain(|value| !value.trim().is_empty());
    values.sort();
    values.dedup();
}

pub(crate) fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    truncated.push_str("...");
    truncated
}
