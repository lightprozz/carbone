use std::collections::HashMap;

use serde_json::Value;

use crate::syntax::{self, Expression, Marker, Path, Step, is_marker};
use crate::xml::{Kind, Tokens, tag_end};
use crate::{Error, RenderOptions, Result, conditions, formatters};

const MAX_TEMPLATE: usize = 32 * 1024 * 1024;
const MAX_OUTPUT: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 64;

impl Path {
    fn loop_key(&self, stop: usize) -> String {
        let mut key = self.root.to_string();
        for step in &self.steps[..stop] {
            match step {
                Step::Key(name) => {
                    key.push('.');
                    key.push_str(name);
                }
                Step::Index(index) => key.push_str(&format!("[{index}]")),
                Step::Loop(_) => key.push_str("[i]"),
            }
        }
        key
    }

    fn value<'a>(
        &self,
        data: &'a Value,
        options: &'a RenderOptions,
        bindings: &HashMap<String, usize>,
        stop: usize,
    ) -> &'a Value {
        let mut value = if self.root == 'd' {
            data
        } else {
            &options.complement
        };
        for (position, step) in self.steps[..stop].iter().enumerate() {
            value = match step {
                Step::Key(key) => value.get(key),
                Step::Index(index) => value.get(*index),
                Step::Loop(_) => bindings
                    .get(&self.loop_key(position))
                    .and_then(|index| value.get(*index)),
            }
            .unwrap_or(&Value::Null);
        }
        value
    }
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
    render(
        &normalized,
        data,
        options,
        &mut HashMap::new(),
        0,
        &mut output,
    )?;
    Ok(output)
}

fn push(output: &mut String, text: &str) -> Result<()> {
    if output.len().saturating_add(text.len()) > MAX_OUTPUT {
        return Err(Error::Template("rendered output exceeds 64 MiB".into()));
    }
    output.push_str(text);
    Ok(())
}

// Word processors can split one marker across multiple formatting runs.
fn normalize(template: &str) -> Result<String> {
    let mut result = String::new();
    let mut cursor = 0;
    for span in syntax::spans(template)? {
        let body = &template[span.start + 1..span.end - 1];
        let mut text = String::new();
        let mut tags = String::new();
        let mut position = 0;
        let mut quote = None;
        while position < body.len() {
            let ch = body[position..].chars().next().unwrap();
            if quote.is_none() && ch == '<' {
                let end = match tag_end(body, position) {
                    Ok(end) => end,
                    Err(error) if is_marker(&text) => return Err(error),
                    Err(_) => {
                        text.push_str(&body[position..]);
                        break;
                    }
                };
                tags.push_str(&body[position..end]);
                position = end;
            } else {
                if ch == '\'' || ch == '"' {
                    if quote == Some(ch) {
                        quote = None;
                    } else if quote.is_none() {
                        quote = Some(ch);
                    }
                }
                text.push(ch);
                position += ch.len_utf8();
            }
        }
        if !is_marker(&text) {
            continue;
        }
        result.push_str(&template[cursor..span.start]);
        result.push('{');
        result.push_str(&text);
        result.push('}');
        result.push_str(&tags);
        cursor = span.end;
    }
    result.push_str(&template[cursor..]);
    Ok(result)
}

fn markers(template: &str) -> Result<Vec<Marker>> {
    let markers = syntax::markers(template)?;
    for marker in &markers {
        if let Expression::Data(_, pipeline) = &marker.expression {
            for formatter in pipeline {
                if !conditions::supports(&formatter.name) {
                    formatters::validate(&formatter.name, &formatter.args)?;
                }
            }
        }
    }
    Ok(markers)
}

fn render(
    template: &str,
    data: &Value,
    options: &RenderOptions,
    bindings: &mut HashMap<String, usize>,
    depth: usize,
    output: &mut String,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Template("template nesting exceeds 64 levels".into()));
    }
    let marker_list = markers(template)?;
    for marker in &marker_list {
        if let Expression::Data(path, _) = &marker.expression {
            for (position, step) in path.steps.iter().enumerate() {
                if let Step::Loop(sentinel) = step {
                    let key = path.loop_key(position);
                    if bindings.contains_key(&key) {
                        continue;
                    }
                    if *sentinel {
                        return Err(Error::Template(format!(
                            "loop {key}[i+1] has no opening marker"
                        )));
                    }
                    let end = marker_list
                        .iter()
                        .find(|other| {
                            if other.start <= marker.start {
                                return false;
                            }
                            if let Expression::Data(other_path, _) = &other.expression {
                                other_path.steps.iter().enumerate().any(|(p, s)| {
                                    *s == Step::Loop(true) && other_path.loop_key(p) == key
                                })
                            } else {
                                false
                            }
                        })
                        .ok_or_else(|| {
                            Error::Unsupported(format!(
                                "unpaired loop {key}[i]; add an [i+1] example"
                            ))
                        })?;
                    let (start, repeat_end, end) = repetition_bounds(template, marker, end)?;
                    let array = path.value(data, options, bindings, position);
                    let values = match array {
                        Value::Array(values) => values.as_slice(),
                        Value::Null => &[],
                        _ => return Err(Error::Template(format!("loop {key} requires an array"))),
                    };
                    if values.len() > 100_000 {
                        return Err(Error::Template("loop exceeds 100000 items".into()));
                    }
                    render(
                        &template[..start],
                        data,
                        options,
                        bindings,
                        depth + 1,
                        output,
                    )?;
                    let body = &template[start..repeat_end];
                    let body_markers = markers(body)?;
                    for index in 0..values.len() {
                        bindings.insert(key.clone(), index);
                        // A row supported only by empty nested arrays disappears in Carbone.
                        if body_markers.iter().any(|marker| match &marker.expression {
                            Expression::Data(path, _) => {
                                has_iteration(path, data, options, bindings, 0)
                            }
                            Expression::Translation(_) => false,
                        }) {
                            render(body, data, options, bindings, depth + 1, output)?;
                        }
                    }
                    bindings.remove(&key);
                    render(&template[end..], data, options, bindings, depth + 1, output)?;
                    return Ok(());
                }
            }
        }
    }
    let mut cursor = 0;
    for marker in &marker_list {
        push(output, &template[cursor..marker.start])?;
        let value = match &marker.expression {
            Expression::Translation(key) => options
                .translations
                .get(&options.lang)
                .and_then(|map| map.get(key))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or(key)
                .to_owned(),
            Expression::Data(path, pipeline) => {
                let mut value = path
                    .value(data, options, bindings, path.steps.len())
                    .clone();
                let mut state = conditions::State::default();
                for formatter in pipeline {
                    if state.stop {
                        break;
                    }
                    value = if conditions::supports(&formatter.name) {
                        state.apply(&formatter.name, value, &formatter.args)
                    } else {
                        formatters::apply(&formatter.name, value, &formatter.args)?
                    };
                }
                formatters::display(&value)
            }
        };
        if options.escape_xml {
            let mut escaped = escape(&value, in_attribute(template, marker.start));
            if matches!(marker.expression, Expression::Translation(_)) {
                escaped = escaped.replace('\'', "&apos;").replace('¦', "&brvbar;");
            }
            push(output, &escaped)?;
        } else {
            push(output, &value)?;
        }
        cursor = marker.end;
    }
    push(output, &template[cursor..])
}

fn has_iteration(
    path: &Path,
    data: &Value,
    options: &RenderOptions,
    bindings: &mut HashMap<String, usize>,
    start: usize,
) -> bool {
    for (position, step) in path.steps.iter().enumerate().skip(start) {
        if matches!(step, Step::Loop(_)) {
            let key = path.loop_key(position);
            if bindings.contains_key(&key) {
                continue;
            }
            let length = path
                .value(data, options, bindings, position)
                .as_array()
                .map_or(0, Vec::len);
            for index in 0..length {
                bindings.insert(key.clone(), index);
                let found = has_iteration(path, data, options, bindings, position + 1);
                bindings.remove(&key);
                if found {
                    return true;
                }
            }
            return false;
        }
    }
    true
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
            '\u{0}'..='\u{8}'
            | '\u{b}'..='\u{c}'
            | '\u{e}'..='\u{1f}'
            | '\u{fffe}'
            | '\u{ffff}' => {}
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn in_attribute(template: &str, position: usize) -> bool {
    for token in Tokens::new(template) {
        let Ok(token) = token else {
            return false;
        };
        if token.start > position {
            break;
        }
        if token.end <= position {
            continue;
        }
        if !matches!(token.kind, Kind::Start { .. } | Kind::Empty { .. }) {
            return false;
        }
        let mut quote = None;
        for ch in template[token.start..position].chars() {
            if ch == '\'' || ch == '"' {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                }
            }
        }
        return quote.is_some();
    }
    false
}

#[derive(Debug)]
struct Element {
    start: usize,
    end: usize,
    parent: Option<usize>,
    name: String,
}

fn elements(template: &str) -> Result<Vec<Element>> {
    let mut elements: Vec<Element> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for token in Tokens::new(template) {
        let token = token?;
        match token.kind {
            Kind::End { name } => {
                let Some(index) = stack.pop() else {
                    // A recursive suffix may close an ancestor outside this fragment.
                    continue;
                };
                if elements[index].name != name {
                    return Err(Error::Unsupported(
                        "repetition requires balanced XML elements".into(),
                    ));
                }
                elements[index].end = token.end;
            }
            Kind::Start { name } | Kind::Empty { name } => {
                let index = elements.len();
                elements.push(Element {
                    start: token.start,
                    end: token.end,
                    name: name.into(),
                    parent: stack.last().copied(),
                });
                if matches!(token.kind, Kind::Start { .. }) {
                    stack.push(index);
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(Error::Unsupported(
            "repetition requires balanced XML elements".into(),
        ));
    }
    Ok(elements)
}

fn repetition_bounds(
    template: &str,
    first: &Marker,
    second: &Marker,
) -> Result<(usize, usize, usize)> {
    if !template.contains('<') {
        if markers(&template[second.end..])?.is_empty() {
            return Ok((first.start, second.start, second.end));
        }
        return Err(Error::Unsupported(
            "multi-marker plain-text repetition".into(),
        ));
    }
    let elements = elements(template)?;
    for first_element in elements
        .iter()
        .rev()
        .filter(|e| e.start < first.start && e.end >= first.end)
    {
        if let Some(second_element) = elements.iter().find(|e| {
            e.start < second.start
                && e.end >= second.end
                && e.start > first_element.start
                && e.parent == first_element.parent
                && e.name == first_element.name
        }) {
            return Ok((
                first_element.start,
                second_element.start,
                second_element.end,
            ));
        }
    }
    Err(Error::Unsupported(
        "loop markers must occur in matching sibling XML elements".into(),
    ))
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
        let options = RenderOptions {
            complement: json!({"date":"today"}),
            ..Default::default()
        };
        assert_eq!(
            render_template(
                "{d.name} {c.date} {d.list[i=1]} {d.missing}",
                &json!({"name":"Ada", "list":[4,5]}),
                &options
            )
            .unwrap(),
            "Ada today 5 "
        );
    }

    #[test]
    fn escapes_data_but_preserves_template_and_literal_braces() {
        assert_eq!(
            render_default(
                "<p>{d.name}</p>{uuid}{m.x}",
                json!({"name":"<&>\u{0000}'\""})
            ),
            "<p>&lt;&amp;&gt;'\"</p>{uuid}{m.x}"
        );
        assert_eq!(
            render_default("<p title=\"{d.name}\">x</p>", json!({"name":"\" a='b'"})),
            "<p title=\"&quot; a=&apos;b&apos;\">x</p>"
        );
    }

    #[test]
    fn split_runs_and_chained_formatters() {
        assert_eq!(
            render_default(
                "<p><b>{d.na</b><b>me:upperCase}</b></p>",
                json!({"name":"Ada"})
            ),
            "<p><b>ADA</b><b></b></p>"
        );
        assert_eq!(
            render_default(
                "{d.name:upperCase:prepend('Hello: ')}",
                json!({"name":"Ada"})
            ),
            "Hello: ADA"
        );
    }

    #[test]
    fn sibling_repetition_and_empty_arrays() {
        let template = "<xml><tr>{d[i].name}</tr> <tr>{d[i+1].name}</tr></xml>";
        assert_eq!(
            render_default(template, json!([{ "name":"A" }, { "name":"B" }])),
            "<xml><tr>A</tr> <tr>B</tr> </xml>"
        );
        assert_eq!(render_default(template, json!([])), "<xml></xml>");
    }

    #[test]
    fn nested_repetition_and_literal_data_markers() {
        let template = "<root><group><h>{d[i].name}</h><p>{d[i].items[i]}</p><p>{d[i].items[i+1]}</p></group><group>{d[i+1].name}</group></root>";
        assert_eq!(
            render_default(
                template,
                json!([{ "name":"{d.secret}", "items":["A","B"] }, { "name":"two", "items":["C"] }])
            ),
            "<root><group><h>{d.secret}</h><p>A</p><p>B</p></group><group><h>two</h><p>C</p></group></root>"
        );
    }

    #[test]
    fn translates_with_fallback_and_escapes() {
        let options = RenderOptions {
            lang: "fr".into(),
            translations: json!({"fr":{"Hello":"Bonjour & salut"}}),
            ..Default::default()
        };
        assert_eq!(
            render_template("{t(Hello)} {t(Unknown)}", &Value::Null, &options).unwrap(),
            "Bonjour &amp; salut Unknown"
        );
    }

    #[test]
    fn independent_repetition_sections() {
        let template = "<root><a>{d.items[i].name}</a><a>{d.items[i+1].name}</a><b>{d.items[i].name}</b><b>{d.items[i+1].name}</b></root>";
        assert_eq!(
            render_default(template, json!({"items":[{"name":"A"},{"name":"B"}]})),
            "<root><a>A</a><a>B</a><b>A</b><b>B</b></root>"
        );
        let template = "<root><a>{d.first[i]}</a><a>{d.first[i+1]}</a><group><b>{d.second[i]}</b><b>{d.second[i+1]}</b></group></root>";
        assert_eq!(
            render_default(template, json!({"first":[1,2],"second":[3,4]})),
            "<root><a>1</a><a>2</a><group><b>3</b><b>4</b></group></root>"
        );
    }

    #[test]
    fn literal_braces_and_empty_loop_validation() {
        assert_eq!(
            render_default("{a < b} {nothing}", json!({})),
            "{a < b} {nothing}"
        );
        for formatter in ["unknown", "count", "padl(10001)", "toFixed(101)"] {
            let template =
                format!("<root><p>{{d[i].name:{formatter}}}</p><p>{{d[i+1].name}}</p></root>");
            assert!(render_template(&template, &json!([]), &RenderOptions::default()).is_err());
        }
        let deep = format!("{{d{}.name}}", ".child".repeat(65));
        assert!(render_template(&deep, &json!({}), &RenderOptions::default()).is_err());
    }

    #[test]
    fn malformed_and_unsupported_markers_are_errors() {
        for template in [
            "{d.name",
            "{d.name:upperCase(}",
            "{d.x:unknown}",
            "{d.x[sort]}",
            "{$variable}",
            "{#variable=d.name}",
            "{d.x:showBegin}",
            "{d.x:print(.other)}",
            "{d.x[i]}",
        ] {
            assert!(
                render_template(template, &json!({}), &RenderOptions::default()).is_err(),
                "{template}"
            );
        }
    }
}
