use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::{Error, RenderOptions, Result, formatters};

const MAX_TEMPLATE: usize = 32 * 1024 * 1024;
const MAX_OUTPUT: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
static MARKERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\{([^{}]*)\}").unwrap());

#[derive(Clone, Debug, PartialEq)]
enum Step {
    Key(String),
    Index(usize),
    Loop(bool),
}

#[derive(Clone, Debug)]
struct Path {
    root: char,
    steps: Vec<Step>,
}

impl Path {
    fn loop_key(&self, stop: usize) -> String {
        let mut key = self.root.to_string();
        for step in &self.steps[..stop] {
            match step {
                Step::Key(name) => { key.push('.'); key.push_str(name); }
                Step::Index(index) => key.push_str(&format!("[{index}]")),
                Step::Loop(_) => key.push_str("[i]"),
            }
        }
        key
    }

    fn value<'a>(&self, data: &'a Value, options: &'a RenderOptions, bindings: &HashMap<String, usize>, stop: usize) -> &'a Value {
        let mut value = if self.root == 'd' { data } else { &options.complement };
        for (position, step) in self.steps[..stop].iter().enumerate() {
            value = match step {
                Step::Key(key) => value.get(key),
                Step::Index(index) => value.get(*index),
                Step::Loop(_) => bindings.get(&self.loop_key(position)).and_then(|index| value.get(*index)),
            }.unwrap_or(&Value::Null);
        }
        value
    }
}

#[derive(Debug)]
struct Formatter {
    name: String,
    args: Vec<String>,
}

#[derive(Debug)]
enum Expression {
    Data(Path, Vec<Formatter>),
    Translation(String),
}

#[derive(Debug)]
struct Marker {
    start: usize,
    end: usize,
    expression: Expression,
}

/// Render XML or text without executing template code.
///
/// Standard paired `[i]` / `[i+1]` loops repeat sibling XML elements (or a
/// simple text segment). Unsupported template constructs return an error.
pub fn render_template(template: &str, data: &Value, options: &RenderOptions) -> Result<String> {
    if template.len() > MAX_TEMPLATE {
        return Err(Error::Template("template exceeds 32 MiB".into()));
    }
    let normalized = normalize(template)?;
    let mut output = String::new();
    render(&normalized, data, options, &mut HashMap::new(), 0, &mut output)?;
    Ok(output)
}

fn push(output: &mut String, text: &str) -> Result<()> {
    if output.len().saturating_add(text.len()) > MAX_OUTPUT {
        return Err(Error::Template("rendered output exceeds 64 MiB".into()));
    }
    output.push_str(text);
    Ok(())
}

fn is_marker(text: &str) -> bool {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    ["d.", "d[", "d:", "c.", "c[", "c:", "t(", "$", "#", "bindColor"].iter().any(|p| compact.starts_with(p))
}

fn decode(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&apos;", "'")
        .replace("&quot;", "\"").replace("&amp;", "&")
}

// Word processors can split one marker across multiple formatting runs.
fn normalize(template: &str) -> Result<String> {
    let mut result = String::new();
    let mut cursor = 0;
    for capture in MARKERS.captures_iter(template) {
        let full = capture.get(0).unwrap();
        let body = &capture[1];
        let mut text = String::new();
        let mut tags = String::new();
        let mut position = 0;
        let mut quote = None;
        while position < body.len() {
            let ch = body[position..].chars().next().unwrap();
            if quote.is_none() && ch == '<' {
                let end = tag_end(body, position)?;
                tags.push_str(&body[position..end]);
                position = end;
            } else {
                if ch == '\'' || ch == '"' {
                    if quote == Some(ch) { quote = None; }
                    else if quote.is_none() { quote = Some(ch); }
                }
                text.push(ch);
                position += ch.len_utf8();
            }
        }
        if !is_marker(&text) { continue; }
        result.push_str(&template[cursor..full.start()]);
        result.push('{');
        result.push_str(&text);
        result.push('}');
        result.push_str(&tags);
        cursor = full.end();
    }
    result.push_str(&template[cursor..]);
    // A malformed recognized marker must not leak into a seemingly successful report.
    let mut remainder = MARKERS.replace_all(&result, "").into_owned();
    while let Some(position) = remainder.find('{') {
        remainder = remainder[position + 1..].to_owned();
        if is_marker(&remainder) {
            return Err(Error::Template("unclosed or nested marker".into()));
        }
    }
    Ok(result)
}

fn split_top(text: &str, separator: char) -> Result<Vec<&str>> {
    let mut quote = None;
    let mut depth = 0usize;
    let mut brackets = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (index, ch) in text.char_indices() {
        if let Some(q) = quote {
            if ch == q { quote = None; }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1).ok_or_else(|| Error::Template("unexpected ')'".into()))?,
            '[' => brackets += 1,
            ']' => brackets = brackets.checked_sub(1).ok_or_else(|| Error::Template("unexpected ']'".into()))?,
            _ if ch == separator && depth == 0 && brackets == 0 => {
                parts.push(text[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {},
        }
    }
    if quote.is_some() || depth != 0 || brackets != 0 {
        return Err(Error::Template("unbalanced quotes, parentheses or brackets".into()));
    }
    parts.push(text[start..].trim());
    Ok(parts)
}

fn parse_path(text: &str) -> Result<Path> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let root = text.chars().next().ok_or_else(|| Error::Template("missing data path".into()))?;
    if root != 'd' && root != 'c' { return Err(Error::Unsupported(format!("data path {text}"))); }
    let mut steps = Vec::new();
    let mut remaining = &text[1..];
    while !remaining.is_empty() {
        if let Some(after) = remaining.strip_prefix('.') {
            let length = after.find(['.', '[']).unwrap_or(after.len());
            let key = &after[..length];
            if key.is_empty() || key.contains(['(', ')', ']', '=', '+', '$', '#']) {
                return Err(Error::Unsupported(format!("data path {text}")));
            }
            steps.push(Step::Key(key.into()));
            remaining = &after[length..];
        } else if let Some(after) = remaining.strip_prefix('[') {
            let end = after.find(']').ok_or_else(|| Error::Template("unclosed selector".into()))?;
            let selector = &after[..end];
            let step = match selector {
                "i" => Step::Loop(false),
                "i+1" => Step::Loop(true),
                _ => {
                    let number = selector.strip_prefix("i=").unwrap_or(selector);
                    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(Error::Unsupported(format!("array selector [{selector}]")));
                    }
                    Step::Index(number.parse().map_err(|_| Error::Template("array index too large".into()))?)
                }
            };
            steps.push(step);
            remaining = &after[end + 1..];
        } else {
            return Err(Error::Unsupported(format!("data path {text}")));
        }
    }
    Ok(Path { root, steps })
}

fn parse_expression(text: &str) -> Result<Expression> {
    let decoded = decode(text);
    let text = decoded.trim();
    if text.starts_with(['#', '$']) || text.starts_with("bindColor") {
        return Err(Error::Unsupported(format!("marker {{{text}}}")));
    }
    if let Some(key) = text.strip_prefix("t(").and_then(|s| s.strip_suffix(')')) {
        return Ok(Expression::Translation(key.into()));
    }
    let parts = split_top(text, ':')?;
    let path = parse_path(parts[0])?;
    let mut formatters = Vec::new();
    for part in &parts[1..] {
        let (name, args) = if let Some(position) = part.find('(') {
            let body = part[position + 1..].strip_suffix(')').ok_or_else(|| Error::Template("malformed formatter".into()))?;
            let mut args = Vec::new();
            if !body.is_empty() {
                for argument in split_top(body, ',')? {
                    if argument.starts_with('.') || argument.starts_with('$') {
                        return Err(Error::Unsupported("dynamic formatter arguments".into()));
                    }
                    let argument = if (argument.starts_with('\'') && argument.ends_with('\'')) || (argument.starts_with('"') && argument.ends_with('"')) {
                        &argument[1..argument.len()-1]
                    } else { argument };
                    args.push(argument.to_owned());
                }
            }
            (part[..position].trim(), args)
        } else { (*part, Vec::new()) };
        if name.is_empty() { return Err(Error::Template("empty formatter".into())); }
        formatters.push(Formatter { name: name.into(), args });
    }
    Ok(Expression::Data(path, formatters))
}

fn markers(template: &str) -> Result<Vec<Marker>> {
    MARKERS.captures_iter(template).filter(|capture| is_marker(&capture[1])).map(|capture| {
        let full = capture.get(0).unwrap();
        Ok(Marker { start: full.start(), end: full.end(), expression: parse_expression(&capture[1])? })
    }).collect()
}

fn render(template: &str, data: &Value, options: &RenderOptions, bindings: &mut HashMap<String, usize>, depth: usize, output: &mut String) -> Result<()> {
    if depth > MAX_DEPTH { return Err(Error::Template("template nesting exceeds 64 levels".into())); }
    let markers = markers(template)?;
    for marker in &markers {
        if let Expression::Data(path, _) = &marker.expression {
            for (position, step) in path.steps.iter().enumerate() {
                if let Step::Loop(sentinel) = step {
                    let key = path.loop_key(position);
                    if bindings.contains_key(&key) { continue; }
                    if *sentinel { return Err(Error::Template(format!("loop {key}[i+1] has no opening marker"))); }
                    let end = markers.iter().find(|other| {
                        if other.start <= marker.start { return false; }
                        if let Expression::Data(other_path, _) = &other.expression {
                            other_path.steps.iter().enumerate().any(|(p, s)| *s == Step::Loop(true) && other_path.loop_key(p) == key)
                        } else { false }
                    }).ok_or_else(|| Error::Unsupported(format!("unpaired loop {key}[i]; add an [i+1] example")))?;
                    let (start, repeat_end, end) = repetition_bounds(template, marker, end)?;
                    let array = path.value(data, options, bindings, position);
                    let values = match array {
                        Value::Array(values) => values.as_slice(),
                        Value::Null => &[],
                        _ => return Err(Error::Template(format!("loop {key} requires an array"))),
                    };
                    if values.len() > 100_000 { return Err(Error::Template("loop exceeds 100000 items".into())); }
                    render(&template[..start], data, options, bindings, depth + 1, output)?;
                    for index in 0..values.len() {
                        bindings.insert(key.clone(), index);
                        render(&template[start..repeat_end], data, options, bindings, depth + 1, output)?;
                    }
                    bindings.remove(&key);
                    render(&template[end..], data, options, bindings, depth + 1, output)?;
                    return Ok(());
                }
            }
        }
    }
    let mut cursor = 0;
    for marker in &markers {
        push(output, &template[cursor..marker.start])?;
        let value = match &marker.expression {
            Expression::Translation(key) => options.translations.get(&options.lang).and_then(|map| map.get(key)).and_then(Value::as_str).filter(|value| !value.is_empty()).unwrap_or(key).to_owned(),
            Expression::Data(path, pipeline) => {
                let mut value = path.value(data, options, bindings, path.steps.len()).clone();
                for formatter in pipeline {
                    value = formatters::apply(&formatter.name, value, &formatter.args)?;
                }
                formatters::display(&value)
            }
        };
        if options.escape_xml {
            push(output, &escape(&value, in_attribute(template, marker.start)))?;
        } else {
            push(output, &value)?;
        }
        cursor = marker.end;
    }
    push(output, &template[cursor..])
}

fn escape(value: &str, attribute: bool) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' if attribute => escaped.push_str("&quot;"),
            '\'' if attribute => escaped.push_str("&apos;"),
            '\u{0}'..='\u{8}' | '\u{b}'..='\u{c}' | '\u{e}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}' => {},
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn in_attribute(template: &str, position: usize) -> bool {
    let mut inside = false;
    let mut quote = None;
    for ch in template[..position].chars() {
        match ch {
            '<' if !inside => inside = true,
            '>' if quote.is_none() => inside = false,
            '\'' | '"' if inside => {
                if quote == Some(ch) { quote = None; }
                else if quote.is_none() { quote = Some(ch); }
            }
            _ => {},
        }
    }
    inside && quote.is_some()
}

#[derive(Debug)]
struct Element {
    start: usize,
    end: usize,
    parent: Option<usize>,
    name: String,
}

fn tag_end(text: &str, start: usize) -> Result<usize> {
    for (prefix, suffix) in [("<!--", "-->"), ("<![CDATA[", "]]>"), ("<?", "?>")] {
        if text[start..].starts_with(prefix) {
            return text[start + prefix.len()..].find(suffix).map(|end| start + prefix.len() + end + suffix.len()).ok_or_else(|| Error::Template("unclosed XML section".into()));
        }
    }
    let mut quote = None;
    for (offset, ch) in text[start..].char_indices() {
        if let Some(q) = quote { if ch == q { quote = None; } }
        else if ch == '\'' || ch == '"' { quote = Some(ch); }
        else if ch == '>' { return Ok(start + offset + 1); }
    }
    Err(Error::Template("unclosed XML tag".into()))
}

fn elements(template: &str) -> Result<Vec<Element>> {
    let mut elements: Vec<Element> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = template[cursor..].find('<') {
        let start = cursor + offset;
        let end = tag_end(template, start)?;
        let tag = &template[start + 1..end - 1];
        cursor = end;
        if tag.starts_with(['!', '?']) { continue; }
        if let Some(name) = tag.strip_prefix('/') {
            let index = stack.pop().ok_or_else(|| Error::Unsupported("repetition requires balanced XML elements".into()))?;
            if elements[index].name != name.trim() {
                return Err(Error::Unsupported("repetition requires balanced XML elements".into()));
            }
            elements[index].end = end;
        } else {
            let name = tag.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
            let index = elements.len();
            elements.push(Element { start, end, name: name.into(), parent: stack.last().copied() });
            if !tag.trim_end().ends_with('/') { stack.push(index); }
        }
    }
    if !stack.is_empty() { return Err(Error::Unsupported("repetition requires balanced XML elements".into())); }
    Ok(elements)
}

fn repetition_bounds(template: &str, first: &Marker, second: &Marker) -> Result<(usize, usize, usize)> {
    if !template.contains('<') {
        if markers(&template[second.end..])?.is_empty() {
            return Ok((first.start, second.start, second.end));
        }
        return Err(Error::Unsupported("multi-marker plain-text repetition".into()));
    }
    let elements = elements(template)?;
    for first_element in elements.iter().rev().filter(|e| e.start < first.start && e.end >= first.end) {
        if let Some(second_element) = elements.iter().find(|e| e.start < second.start && e.end >= second.end && e.start > first_element.start && e.parent == first_element.parent && e.name == first_element.name) {
            return Ok((first_element.start, second_element.start, second_element.end));
        }
    }
    Err(Error::Unsupported("loop markers must occur in matching sibling XML elements".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn render_default(template: &str, data: Value) -> String {
        render_template(template, &data, &RenderOptions::default()).unwrap()
    }

    #[test]
    fn scalar_paths_and_complement() {
        let options = RenderOptions { complement: json!({"date":"today"}), ..Default::default() };
        assert_eq!(render_template("{d.name} {c.date} {d.list[i=1]} {d.missing}", &json!({"name":"Ada", "list":[4,5]}), &options).unwrap(), "Ada today 5 ");
    }

    #[test]
    fn escapes_data_but_preserves_template_and_literal_braces() {
        assert_eq!(render_default("<p>{d.name}</p>{uuid}{m.x}", json!({"name":"<&>\u{0000}'\""})), "<p>&lt;&amp;&gt;'\"</p>{uuid}{m.x}");
        assert_eq!(render_default("<p title=\"{d.name}\">x</p>", json!({"name":"\" a='b'"})), "<p title=\"&quot; a=&apos;b&apos;\">x</p>");
    }

    #[test]
    fn split_runs_and_chained_formatters() {
        assert_eq!(render_default("<p><b>{d.na</b><b>me:upperCase}</b></p>", json!({"name":"Ada"})), "<p><b>ADA</b><b></b></p>");
        assert_eq!(render_default("{d.name:upperCase:prepend('Hello: ')}", json!({"name":"Ada"})), "Hello: ADA");
    }

    #[test]
    fn sibling_repetition_and_empty_arrays() {
        let template = "<xml><tr>{d[i].name}</tr> <tr>{d[i+1].name}</tr></xml>";
        assert_eq!(render_default(template, json!([{ "name":"A" }, { "name":"B" }])), "<xml><tr>A</tr> <tr>B</tr> </xml>");
        assert_eq!(render_default(template, json!([])), "<xml></xml>");
    }

    #[test]
    fn nested_repetition_and_literal_data_markers() {
        let template = "<root><group><h>{d[i].name}</h><p>{d[i].items[i]}</p><p>{d[i].items[i+1]}</p></group><group>{d[i+1].name}</group></root>";
        assert_eq!(render_default(template, json!([{ "name":"{d.secret}", "items":["A","B"] }, { "name":"two", "items":["C"] }])), "<root><group><h>{d.secret}</h><p>A</p><p>B</p></group><group><h>two</h><p>C</p></group></root>");
    }

    #[test]
    fn translates_with_fallback_and_escapes() {
        let options = RenderOptions { lang:"fr".into(), translations:json!({"fr":{"Hello":"Bonjour & salut"}}), ..Default::default() };
        assert_eq!(render_template("{t(Hello)} {t(Unknown)}", &Value::Null, &options).unwrap(), "Bonjour &amp; salut Unknown");
    }

    #[test]
    fn malformed_and_unsupported_markers_are_errors() {
        for template in ["{d.name", "{d.name:upperCase(}", "{d.x:unknown}", "{d.x[sort]}", "{$variable}", "{#variable=d.name}", "{d.x:showBegin}", "{d.x:print(.other)}", "{d.x[i]}"] {
            assert!(render_template(template, &json!({}), &RenderOptions::default()).is_err(), "{template}");
        }
    }
}
