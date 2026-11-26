//! Characters a model reads but a person can't see.
//!
//! Unicode tag characters spell out ASCII invisibly ("ASCII smuggling"), runs of variation
//! selectors encode arbitrary bytes, and zero-width, filler and bidirectional characters hide or
//! reorder text. They can carry instructions inside a tool description or result, or hide stolen
//! data inside an argument that looks harmless.
//!
//! Joiners (U+200C, U+200D) and the directional marks (U+200E, U+200F) are left alone: emoji
//! sequences and right-to-left scripts need them. So is a single emoji or text presentation
//! selector (U+FE0E, U+FE0F) after a visible character, as in ❤️.

use serde_json::Value;

fn is_tag(c: char) -> bool {
    matches!(c, '\u{E0000}'..='\u{E007F}')
}

/// Selectors with no use outside rare CJK and mathematical variants, which smuggling relies on.
fn is_rare_selector(c: char) -> bool {
    matches!(c, '\u{FE00}'..='\u{FE0D}' | '\u{E0100}'..='\u{E01EF}')
}

fn is_presentation_selector(c: char) -> bool {
    matches!(c, '\u{FE0E}' | '\u{FE0F}')
}

fn is_selector(c: char) -> bool {
    is_rare_selector(c) || is_presentation_selector(c)
}

fn is_invisible(c: char) -> bool {
    is_tag(c)
        || is_rare_selector(c)
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{180E}'
                | '\u{200B}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{3164}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{E0100}'..='\u{E01EF}'
        )
}

/// Each character of `text`, with whether it is hidden. A presentation selector is hidden only
/// when it follows another selector, since one alone just picks how an emoji is drawn.
fn scan(text: &str) -> impl Iterator<Item = (char, bool)> + '_ {
    let mut previous: Option<char> = None;
    text.chars().map(move |c| {
        let hidden =
            is_invisible(c) || (is_presentation_selector(c) && previous.is_none_or(is_selector));
        previous = Some(c);
        (c, hidden)
    })
}

fn has_hidden(text: &str) -> bool {
    scan(text).any(|(_, hidden)| hidden)
}

pub(crate) fn contains_hidden(value: &Value) -> bool {
    match value {
        Value::String(text) => has_hidden(text),
        Value::Array(items) => items.iter().any(contains_hidden),
        Value::Object(map) => map
            .iter()
            .any(|(key, item)| has_hidden(key) || contains_hidden(item)),
        _ => false,
    }
}

/// Removes every hidden character, for text only the model reads.
pub(crate) fn strip(text: &str) -> String {
    scan(text)
        .filter_map(|(c, hidden)| (!hidden).then_some(c))
        .collect()
}

pub(crate) fn strip_value(value: &mut Value) {
    map_strings(value, &strip);
}

/// Removes only what can smuggle whole messages and has no visible use outside flag emoji and
/// rare CJK variants: tag characters and selector runs. For results and errors that otherwise
/// reach the model unchanged.
pub(crate) fn strip_smuggled(text: &str) -> String {
    scan(text)
        .filter_map(|(c, hidden)| (!hidden || !(is_tag(c) || is_selector(c))).then_some(c))
        .collect()
}

pub(crate) fn strip_smuggled_value(value: &mut Value) {
    map_strings(value, &strip_smuggled);
}

/// Writes each hidden character as a visible `[U+XXXX]`, for text a person is asked to approve.
pub(crate) fn reveal(value: &Value) -> Value {
    let mut revealed = value.clone();
    map_strings(&mut revealed, &|text| {
        let mut out = String::with_capacity(text.len());
        for (c, hidden) in scan(text) {
            if hidden {
                out.push_str(&format!("[U+{:04X}]", c as u32));
            } else {
                out.push(c);
            }
        }
        out
    });
    revealed
}

fn map_strings(value: &mut Value, f: &dyn Fn(&str) -> String) {
    match value {
        Value::String(text) if has_hidden(text) => *text = f(text),
        Value::Array(items) => items.iter_mut().for_each(|item| map_strings(item, f)),
        Value::Object(map) => {
            let entries = std::mem::take(map);
            for (key, mut item) in entries {
                map_strings(&mut item, f);
                let key = if has_hidden(&key) { f(&key) } else { key };
                map.insert(key, item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// "hi" written in tag characters.
    const SMUGGLED: &str = "\u{E0068}\u{E0069}";

    #[test]
    fn smuggled_and_zero_width_text_is_found_stripped_and_revealed() {
        let input = json!({ "sidenote": format!("ok{SMUGGLED}\u{200B}") });
        assert!(contains_hidden(&input));
        assert_eq!(
            reveal(&input),
            json!({ "sidenote": "ok[U+E0068][U+E0069][U+200B]" })
        );
        let mut stripped = input.clone();
        strip_value(&mut stripped);
        assert_eq!(stripped, json!({ "sidenote": "ok" }));
    }

    #[test]
    fn emoji_joiners_presentation_selectors_and_right_to_left_marks_are_kept() {
        let ordinary = json!("👨\u{200D}👩\u{200D}👧 ❤\u{FE0F} 1\u{FE0F}\u{20E3} \u{200F}שלום");
        assert!(!contains_hidden(&ordinary));
        assert_eq!(reveal(&ordinary), ordinary);
    }

    #[test]
    fn variation_selector_runs_and_fillers_count_as_hidden() {
        // Bytes 0x48 0x69 ("Hi") in the encoding that maps 16..=255 to U+E0100 onwards.
        let bytes = json!("😀\u{E0138}\u{E0159}");
        assert!(contains_hidden(&bytes));
        let nibbles = json!("A\u{FE04}\u{FE08}");
        assert!(contains_hidden(&nibbles));
        let stacked = json!("x\u{FE0F}\u{FE0F}");
        assert_eq!(reveal(&stacked), json!("x\u{FE0F}[U+FE0F]"));
        for filler in ['\u{3164}', '\u{115F}', '\u{FFA0}', '\u{00AD}', '\u{034F}'] {
            assert!(contains_hidden(&json!(format!("a{filler}b"))), "{filler:?}");
        }
    }

    #[test]
    fn strip_smuggled_leaves_other_controls_for_reveal_to_show() {
        let mut result = json!([format!("a{SMUGGLED}\u{202E}b😀\u{E0138}❤\u{FE0F}")]);
        strip_smuggled_value(&mut result);
        assert_eq!(result, json!(["a\u{202E}b😀❤\u{FE0F}"]));
    }
}
