use crate::{Error, Result};

const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Key(String),
    Index(usize),
    Loop(bool),
}

#[derive(Clone, Debug)]
pub(crate) struct Path {
    pub(crate) root: char,
    pub(crate) steps: Vec<Step>,
}

#[derive(Clone, Debug)]
pub(crate) struct Formatter {
    pub(crate) name: String,
    pub(crate) args: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum Expression {
    Data(Path, Vec<Formatter>),
    Translation(String),
}

#[derive(Debug)]
pub(crate) struct Marker {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) expression: Expression,
}

pub(crate) fn is_marker(text: &str) -> bool {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    [
        "d.",
        "d[",
        "d:",
        "c.",
        "c[",
        "c:",
        "t(",
        "$",
        "#",
        "bindColor",
    ]
    .iter()
    .any(|p| compact.starts_with(p))
}

fn decode(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

fn split_top(text: &str, separator: char) -> Result<Vec<&str>> {
    let mut quote = None;
    let mut delimiters = Vec::new();
    let mut start = 0;
    let mut parts = Vec::new();
    for (index, ch) in text.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' | '[' => {
                if delimiters.len() == MAX_DEPTH {
                    return Err(Error::Template("expression exceeds 64 levels".into()));
                }
                delimiters.push(ch);
            }
            ')' | ']' => {
                let expected = if ch == ')' { '(' } else { '[' };
                if delimiters.pop() != Some(expected) {
                    return Err(Error::Template("mismatched expression delimiters".into()));
                }
            }
            _ if ch == separator && delimiters.is_empty() => {
                parts.push(text[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    if quote.is_some() || !delimiters.is_empty() {
        return Err(Error::Template(
            "unbalanced quotes, parentheses or brackets".into(),
        ));
    }
    parts.push(text[start..].trim());
    Ok(parts)
}

fn parse_path(text: &str) -> Result<Path> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let root = text
        .chars()
        .next()
        .ok_or_else(|| Error::Template("missing data path".into()))?;
    if root != 'd' && root != 'c' {
        return Err(Error::Unsupported(format!("data path {text}")));
    }
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
            let end = after
                .find(']')
                .ok_or_else(|| Error::Template("unclosed selector".into()))?;
            let selector = &after[..end];
            let step = match selector {
                "i" => Step::Loop(false),
                "i+1" => Step::Loop(true),
                _ => {
                    let number = selector.strip_prefix("i=").unwrap_or(selector);
                    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(Error::Unsupported(format!("array selector [{selector}]")));
                    }
                    Step::Index(
                        number
                            .parse()
                            .map_err(|_| Error::Template("array index too large".into()))?,
                    )
                }
            };
            steps.push(step);
            remaining = &after[end + 1..];
        } else {
            return Err(Error::Unsupported(format!("data path {text}")));
        }
    }
    if steps.len() > MAX_DEPTH {
        return Err(Error::Template("data path exceeds 64 levels".into()));
    }
    Ok(Path { root, steps })
}

pub(crate) fn parse_expression(text: &str) -> Result<Expression> {
    let decoded = decode(text);
    let text = decoded.trim();
    if text.starts_with(['#', '$']) || text.starts_with("bindColor") {
        return Err(Error::Unsupported(format!("marker {{{text}}}")));
    }
    if let Some(key) = text.strip_prefix("t(").and_then(|s| s.strip_suffix(')')) {
        return Ok(Expression::Translation(key.into()));
    }
    if text.contains("\\'") || text.contains("\\\"") {
        return Err(Error::Unsupported(
            "escaped quotes in formatter arguments".into(),
        ));
    }
    let parts = split_top(text, ':')?;
    let path = parse_path(parts[0])?;
    let mut formatters = Vec::new();
    for part in &parts[1..] {
        let (name, args) = if let Some(position) = part.find('(') {
            let body = part[position + 1..]
                .strip_suffix(')')
                .ok_or_else(|| Error::Template("malformed formatter".into()))?;
            let mut args = Vec::new();
            if !body.is_empty() {
                for argument in split_top(body, ',')? {
                    if argument.starts_with('.') || argument.starts_with('$') {
                        return Err(Error::Unsupported("dynamic formatter arguments".into()));
                    }
                    let argument = if (argument.starts_with('\'') && argument.ends_with('\''))
                        || (argument.starts_with('"') && argument.ends_with('"'))
                    {
                        &argument[1..argument.len() - 1]
                    } else {
                        argument
                    };
                    args.push(argument.to_owned());
                }
            }
            (part[..position].trim(), args)
        } else {
            (*part, Vec::new())
        };
        if name.is_empty() {
            return Err(Error::Template("empty formatter".into()));
        }
        formatters.push(Formatter {
            name: name.into(),
            args,
        });
    }
    Ok(Expression::Data(path, formatters))
}

#[derive(Debug)]
pub(crate) struct Span {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

// Keep byte offsets into the original UTF-8 input; unknown brace syntax is literal.
pub(crate) fn spans(template: &str) -> Result<Vec<Span>> {
    let mut spans = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut position = 0;
    while position < template.len() {
        let ch = template[position..].chars().next().unwrap();
        if let Some(open) = start {
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
            } else {
                match ch {
                    '\'' | '"' if is_marker(&template[open + 1..position]) => quote = Some(ch),
                    '<' if is_marker(&template[open + 1..position]) => {
                        position = crate::xml::tag_end(template, position)?;
                        continue;
                    }
                    '{' => {
                        if is_marker(&template[open + 1..position]) {
                            return Err(Error::Template("nested marker".into()));
                        }
                        start = Some(position);
                    }
                    '}' => {
                        spans.push(Span {
                            start: open,
                            end: position + 1,
                        });
                        start = None;
                    }
                    _ => {}
                }
            }
        } else if ch == '{' {
            start = Some(position);
        }
        position += ch.len_utf8();
    }
    if let Some(open) = start {
        if is_marker(&template[open + 1..]) {
            return Err(Error::Template("unclosed marker".into()));
        }
    }
    Ok(spans)
}

pub(crate) fn markers(template: &str) -> Result<Vec<Marker>> {
    spans(template)?
        .into_iter()
        .filter(|span| is_marker(&template[span.start + 1..span.end - 1]))
        .map(|span| {
            Ok(Marker {
                expression: parse_expression(&template[span.start + 1..span.end - 1])?,
                start: span.start,
                end: span.end,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_paths_and_selectors() {
        let Expression::Data(path, pipeline) =
            parse_expression(" c.items [i=2].name : upperCase ").unwrap()
        else {
            panic!()
        };
        assert_eq!(path.root, 'c');
        assert_eq!(
            path.steps,
            vec![
                Step::Key("items".into()),
                Step::Index(2),
                Step::Key("name".into())
            ]
        );
        assert_eq!(pipeline[0].name, "upperCase");
        for (text, sentinel) in [("d[i]", false), ("d[i+1]", true)] {
            assert_eq!(parse_path(text).unwrap().steps, vec![Step::Loop(sentinel)]);
        }
    }

    #[test]
    fn quoted_delimiters_and_xml_entities() {
        let Expression::Data(_, pipeline) =
            parse_expression("d.name:prepend('a:b,c([x])'):append(&quot;!&quot;)").unwrap()
        else {
            panic!()
        };
        assert_eq!(pipeline[0].args, vec!["a:b,c([x])"]);
        assert_eq!(pipeline[1].args, vec!["!"]);
        let template = "é {uuid} {d.name:prepend('a{b}c')} {t(Bonjour)}";
        let markers = markers(template).unwrap();
        assert_eq!(markers.len(), 2);
        assert_eq!(
            &template[markers[0].start..markers[0].end],
            "{d.name:prepend('a{b}c')}"
        );
        assert!(matches!(&markers[1].expression, Expression::Translation(key) if key == "Bonjour"));
    }

    #[test]
    fn rejects_malformed_and_unimplemented_syntax() {
        for text in [
            "{d.name",
            "{d.name{d.x}}",
            "{d.name:prepend('x)}",
            "{d.name:append(]}",
            "{d.a[}",
        ] {
            assert!(markers(text).is_err(), "{text}");
        }
        for text in [
            "d.a[sort]",
            "d.a[i, x>1]",
            "d.a:print(.other)",
            "$alias",
            "#alias=d.a",
        ] {
            assert!(
                matches!(parse_expression(text), Err(Error::Unsupported(_))),
                "{text}"
            );
        }
        assert!(parse_path(&format!("d{}", ".a".repeat(65))).is_err());
    }

    #[test]
    fn scanner_skips_xml_sections_inside_split_markers() {
        let template = "<b>{d.na</b><b title='{literal}'>me}</b>";
        let result = spans(template).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            &template[result[0].start..result[0].end],
            "{d.na</b><b title='{literal}'>me}"
        );
    }

    #[test]
    fn deterministic_fuzz_inputs_do_not_panic() {
        let alphabet = [
            '{', '}', 'd', '.', '[', ']', '(', ')', ':', ',', '\'', '"', '<', '>', '/', ' ', 'é',
            '中', '🦀', '\0',
        ];
        let mut state = 0x1234_5678u32;
        for round in 0..4096 {
            let mut input = if round % 2 == 0 {
                "{d.".to_owned()
            } else {
                String::new()
            };
            for _ in 0..round % 128 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                input.push(alphabet[state as usize % alphabet.len()]);
            }
            if let Ok(spans) = spans(&input) {
                for span in spans {
                    assert!(input.get(span.start..span.end).is_some());
                }
            }
            let _ = markers(&input);
            let _ = parse_expression(&input);
        }
    }
}

impl Path {
    pub(crate) fn loop_key(&self, stop: usize) -> String {
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
}
