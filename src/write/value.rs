//! How one value becomes bytes on a line (SPEC §7.3, plan §1.12b).
//!
//! A value is written bare only when it reads back as the same string.
//! Otherwise it is double-quoted with the smallest escape set that still
//! round-trips. The parse-back decides; the character class below is only a
//! cheap way to reject the values that obviously need quoting.
//!
//! Why the parse-back and not a class: `a: b` and `- x` both pass a permissive
//! class and both change meaning when written bare, while `007` and `null` change
//! from a string to a number and to a null. YAML decides, so YAML is asked.

use oneyaml::Value;

/// Renders a patch value as the bytes that go after `key: `.
///
/// `None` writes `null`, which is a value (M7-6). Deleting a key is not part of
/// this milestone.
#[must_use]
pub fn render_value(value: Option<&str>) -> String {
    match value {
        None => "null".to_owned(),
        Some(text) if plain_is_safe(text) && reads_back_bare(text) => text.to_owned(),
        Some(text) => quoted(text),
    }
}

/// The fast path: the bytes a bare scalar may hold, minus the shapes YAML reads
/// as structure.
///
/// This is a filter for a refusal, never an acceptance: a value that passes it
/// is still parsed back before it is written bare.
fn plain_is_safe(text: &str) -> bool {
    let first = text.chars().next();
    let last = text.chars().last();
    let Some(first) = first else {
        // The empty string is a value, and `key:` with nothing after it is a
        // null. Only `""` says "empty string".
        return false;
    };

    // A leading `-`, `?` or `:` opens a structure; `@` and `%` are reserved at
    // the start of a scalar. A leading or trailing space is trimmed by the
    // parser. A trailing `:` or an inner `: ` turns the value into a mapping.
    if matches!(first, '-' | '?' | ':' | '@' | '%') || first == ' ' {
        return false;
    }
    if last == Some(' ') || last == Some(':') || text.contains(": ") {
        return false;
    }

    text.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '/' | ':' | '@' | '+' | '-')
    })
}

/// Whether a bare write of `text` parses back as the same string.
///
/// The string comparison uses the same coercion as the typed reader, so a value
/// that would read back as a number or a boolean is quoted. `k` is a scratch key
/// because only the value is under test.
fn reads_back_bare(expected: &str) -> bool {
    let line = format!("k: {expected}\n");
    let parsed: Value = match oneyaml::from_str(&line) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let Some(mapping) = parsed.as_mapping() else {
        return false;
    };
    let Some(value) = mapping.get_str("k") else {
        return false;
    };
    as_string(value).is_some_and(|text| text == expected)
}

/// The string the typed reader would report for a scalar.
///
/// Mirrors `vault::frontmatter`: strings, numbers and booleans coerce, and every
/// other shape has no string form.
fn as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// A double-quoted scalar with the smallest escape set that round-trips.
///
/// `\\`, `\"`, `\n`, `\r`, `\t` are written as the short forms. Every other
/// character below `0x20`, and `0x7f`, is written as `\uXXXX`. Bytes above
/// `0x7f` are UTF-8 and are written as they are.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
