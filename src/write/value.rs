//! How one value becomes bytes on a line (SPEC §7.3, plan §1.12b).
//!
//! A value is written bare only when it reads back as a YAML **string** that is
//! the same text. A coerced scalar is not enough: `true` reads back as the
//! string "true" through this project's typed reader, but the file would hold a
//! boolean, and every other consumer of the vault — an editor's property panel,
//! a linter, a static site generator — would see a type the caller did not ask
//! for. The same rule catches `1.0`, `007` and `null`.
//!
//! Otherwise the value is double-quoted with the smallest escape set that still
//! round-trips. The parse-back decides; the character class below is only a
//! cheap way to reject the values that obviously need quoting.
//!
//! Why the parse-back and not a class: `a: b` and `- x` both pass a permissive
//! class and both change meaning when written bare. YAML decides, so YAML is
//! asked.

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

/// Whether a bare write of `text` parses back as the same *string*.
///
/// Only `Value::String` is accepted. A number, a boolean or a null is a
/// different type in the file, even when this project's typed reader would render
/// it as the same text. `k` is a scratch key because only the value is under
/// test.
fn reads_back_bare(expected: &str) -> bool {
    if is_yaml_1_1_bool(expected) {
        return false;
    }
    let line = format!("k: {expected}\n");
    let parsed: Value = match oneyaml::from_str(&line) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let Some(mapping) = parsed.as_mapping() else {
        return false;
    };
    matches!(mapping.get_str("k"), Some(Value::String(text)) if text == expected)
}

/// Spellings that a YAML 1.1 reader treats as a boolean.
///
/// YAML 1.2 reads `yes` as a string, so the parse-back alone would write it
/// bare. A YAML 1.1 consumer would then see a boolean, so these are quoted
/// too.
fn is_yaml_1_1_bool(text: &str) -> bool {
    ["yes", "no", "true", "false", "on", "off"]
        .iter()
        .any(|word| text.eq_ignore_ascii_case(word))
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
