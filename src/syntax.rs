use crate::{Error, Result};

const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Key(String),
    Index(isize),
    Parent,
    Select(Selector),
    Loop(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Filter {
    pub(crate) field: String,
    pub(crate) operator: String,
    pub(crate) value: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Selector {
    pub(crate) iterators: Vec<String>,
    pub(crate) sentinel: bool,
    pub(crate) aligned: bool,
    pub(crate) filters: Vec<Filter>,
}

impl Step {
    pub(crate) fn iteration(&self) -> Option<bool> {
        match self {
            Self::Loop(sentinel) => Some(*sentinel),
            Self::Select(selector) if !selector.iterators.is_empty() => Some(selector.sentinel),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Argument {
    Literal,
    Relative { levels: usize, path: String },
    Root(Path),
    Math(String),
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
    pub(crate) arguments: Vec<Argument>,
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

pub(crate) fn split_top(text: &str, separator: char) -> Result<Vec<&str>> {
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

pub(crate) fn parse_path(text: &str) -> Result<Path> {
    let mut compact = String::new();
    let mut quote = None;
    for ch in text.chars() {
        if let Some(q) = quote {
            compact.push(ch);
            if ch == q {
                quote = None;
            }
        } else if ch == '\'' || ch == '"' {
            compact.push(ch);
            quote = Some(ch);
        } else if !ch.is_whitespace() {
            compact.push(ch);
        }
    }
    let text = compact;
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
        if let Some(after) = remaining.strip_prefix("..") {
            steps.push(Step::Parent);
            remaining = after;
            if !remaining.is_empty() && !remaining.starts_with('.') {
                let length = remaining.find(['.', '[']).unwrap_or(remaining.len());
                steps.push(Step::Key(remaining[..length].into()));
                remaining = &remaining[length..];
            }
        } else if let Some(after) = remaining.strip_prefix('.') {
            let length = after.find(['.', '[']).unwrap_or(after.len());
            let key = &after[..length];
            if key.is_empty() || key.contains(['(', ')', ']', '=', '+', '$', '#']) {
                return Err(Error::Unsupported(format!("data path {text}")));
            }
            steps.push(Step::Key(key.into()));
            remaining = &after[length..];
        } else if let Some(after) = remaining.strip_prefix('[') {
            let mut quote = None;
            let end = after
                .char_indices()
                .find_map(|(index, ch)| {
                    if let Some(q) = quote {
                        if ch == q {
                            quote = None;
                        }
                    } else if ch == '\'' || ch == '"' {
                        quote = Some(ch);
                    } else if ch == ']' {
                        return Some(index);
                    }
                    None
                })
                .ok_or_else(|| Error::Template("unclosed selector".into()))?;
            let selector = &after[..end];
            let number = selector.strip_prefix("i=").unwrap_or(selector);
            let step = if let Ok(index) = number.parse::<isize>() {
                Step::Index(index)
            } else if selector == "i" {
                Step::Loop(false)
            } else if selector == "i+1" {
                Step::Loop(true)
            } else {
                let mut iterators = Vec::new();
                let mut filters = Vec::new();
                let mut sentinel = None;
                let mut aligned = false;
                for item in split_top(selector, ',')? {
                    let comparison = ["!=", ">=", "<=", "=", ">", "<"]
                        .iter()
                        .filter_map(|operator| item.find(operator).map(|index| (index, *operator)))
                        .min_by_key(|(index, _)| *index);
                    if let Some((index, operator)) = comparison {
                        let field = &item[..index];
                        let value = &item[index + operator.len()..];
                        if field.is_empty() || value.is_empty() {
                            return Err(Error::Template("empty filter operand".into()));
                        }
                        filters.push(Filter {
                            field: field.into(),
                            operator: operator.into(),
                            value: value.into(),
                        });
                    } else {
                        let item = if let Some(name) = item.strip_suffix("++") {
                            aligned = true;
                            format!("{name}+1")
                        } else {
                            item.to_owned()
                        };
                        let (name, next) = item
                            .strip_suffix("+1")
                            .map_or((item.as_str(), false), |name| (name, true));
                        if name.is_empty() || name.contains(['+', '-', '$', '(', ')']) {
                            return Err(Error::Unsupported(format!("iterator {item}")));
                        }
                        if sentinel.is_some_and(|old| old != next) {
                            return Err(Error::Unsupported("mixed iterator offsets".into()));
                        }
                        sentinel = Some(next);
                        iterators.push(name.into());
                    }
                }
                Step::Select(Selector {
                    iterators,
                    filters,
                    sentinel: sentinel.unwrap_or(false),
                    aligned,
                })
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
        let mut arguments = Vec::new();
        let (name, args) = if let Some(position) = part.find('(') {
            let body = part[position + 1..]
                .strip_suffix(')')
                .ok_or_else(|| Error::Template("malformed formatter".into()))?;
            let mut args = Vec::new();
            let arithmetic = matches!(part[..position].trim(), "add" | "sub" | "mul" | "div");
            if !body.is_empty() {
                for argument in split_top(body, ',')? {
                    let quoted = argument.starts_with(['\'', '"']);
                    let unquoted = if quoted && argument.len() >= 2 {
                        &argument[1..argument.len() - 1]
                    } else {
                        argument
                    };
                    let kind = if arithmetic {
                        crate::extras::arithmetic(unquoted, &serde_json::Value::Null)?;
                        Argument::Math(unquoted.into())
                    } else if !quoted && argument.starts_with('.') {
                        let levels = argument.chars().take_while(|ch| *ch == '.').count();
                        Argument::Relative {
                            levels,
                            path: argument[levels..].into(),
                        }
                    } else if !quoted
                        && (argument.starts_with("d.")
                            || argument.starts_with("d[")
                            || argument.starts_with("c.")
                            || argument.starts_with("c["))
                    {
                        Argument::Root(parse_path(argument)?)
                    } else if argument.starts_with('$') {
                        return Err(Error::Unsupported("unexpanded alias argument".into()));
                    } else {
                        Argument::Literal
                    };
                    arguments.push(kind);
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
            arguments,
        });
    }
    Ok(Expression::Data(path, formatters))
}

// Entity-encoded quotes delimit arguments after XML decoding as well.
pub(crate) fn character(text: &str) -> (char, usize) {
    if text.starts_with("&quot;") {
        ('"', 6)
    } else if text.starts_with("&apos;") {
        ('\'', 6)
    } else {
        let ch = text.chars().next().unwrap();
        (ch, ch.len_utf8())
    }
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
        let (ch, width) = character(&template[position..]);
        if let Some(open) = start {
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
            } else {
                match ch {
                    '\'' | '"'
                        if is_marker(&template[open + 1..position])
                            && !template[open + 1..].trim_start().starts_with("t(") =>
                    {
                        quote = Some(ch)
                    }
                    '<' if is_marker(&template[open + 1..position])
                        && template.as_bytes().get(position + 1).is_some_and(|b| {
                            b.is_ascii_alphabetic() || matches!(b, b'/' | b'!' | b'?')
                        }) =>
                    {
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
        position += width;
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
        for text in ["$alias", "#alias=d.a"] {
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
                Step::Parent => key.push_str(".."),
                Step::Select(selector) => {
                    key.push('[');
                    key.push_str(&selector.iterators.join(","));
                    key.push(']');
                }
            }
        }
        key
    }
}
