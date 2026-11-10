use std::collections::HashSet;

use crate::registry::Registry;
use crate::syntax::{self, Argument, Expression, Marker, Path, Step, is_marker};
use crate::xml::{Kind, Tokens, tag_end};
use crate::{Error, Result};

const MAX_TEMPLATE: usize = 32 * 1024 * 1024;
const MAX_DEPTH: usize = 64;

#[derive(Debug)]
pub(crate) enum Node {
    Static(String),
    Value {
        expression: Expression,
        attribute: bool,
    },
    Block {
        expression: Expression,
        hide: bool,
        body: Vec<Node>,
    },
    Loop {
        source: Path,
        stop: usize,
        key: String,
        guards: Vec<Path>,
        body: Vec<Node>,
    },
}

pub(crate) fn compile(template: &str, registry: &Registry) -> Result<Vec<Node>> {
    if template.len() > MAX_TEMPLATE {
        return Err(Error::Template("template exceeds 32 MiB".into()));
    }
    let expanded = crate::aliases::expand(template)?;
    let normalized = normalize(&expanded)?;
    let expanded = crate::blocks::expand(&normalized)?;
    fragment(&expanded, registry, &mut HashSet::new(), 0)
}

fn markers(template: &str, registry: &Registry) -> Result<Vec<Marker>> {
    let markers = syntax::markers(template)?;
    for marker in &markers {
        if let Expression::Data(_, pipeline) = &marker.expression {
            for formatter in pipeline {
                if crate::blocks::is_structural(&formatter.name) {
                    continue;
                }
                if formatter
                    .arguments
                    .iter()
                    .all(|arg| matches!(arg, Argument::Literal))
                {
                    registry.validate(&formatter.name, &formatter.args)?;
                } else {
                    registry.validate_dynamic(&formatter.name)?;
                }
            }
        }
    }
    Ok(markers)
}

fn fragment(
    template: &str,
    registry: &Registry,
    bindings: &mut HashSet<String>,
    depth: usize,
) -> Result<Vec<Node>> {
    if depth > MAX_DEPTH {
        return Err(Error::Template("template nesting exceeds 64 levels".into()));
    }
    let regions = crate::blocks::regions(template)?;
    let marker_list = markers(template, registry)?;
    for marker in &marker_list {
        if let Expression::Data(path, _) = &marker.expression {
            for (position, step) in path.steps.iter().enumerate() {
                let Some(sentinel) = step.iteration() else {
                    continue;
                };
                let key = path.loop_key(position);
                if bindings.contains(&key) {
                    continue;
                }
                if sentinel {
                    return Err(Error::Template(format!(
                        "loop {key}[i+1] has no opening marker"
                    )));
                }
                let second = marker_list.iter().find(|other| {
                    other.start > marker.start
                        && match &other.expression {
                            Expression::Data(other_path, _) => {
                                other_path.steps.iter().enumerate().any(|(p, s)| {
                                    s.iteration() == Some(true) && other_path.loop_key(p) == key
                                })
                            }
                            Expression::Translation(_) => false,
                        }
                });
                let (start, repeat_end, end) = if let Some(second) = second {
                    repetition_bounds(template, marker, second)?
                } else if path.steps[..position].iter().enumerate().any(|(p, step)| {
                    step.iteration().is_some() && bindings.contains(&path.loop_key(p))
                }) {
                    (0, template.len(), template.len())
                } else {
                    return Err(Error::Unsupported(format!(
                        "unpaired loop {key}[i]; add an [i+1] example"
                    )));
                };
                if regions.iter().any(|region| region.start <= start && region.end >= end) {
                    return conditional_regions(template, registry, bindings, depth, regions);
                }
                let mut source = path.clone();
                if let Some(second) = second {
                    if let Expression::Data(other, _) = &second.expression {
                        if other
                            .steps
                            .iter()
                            .any(|step| matches!(step, Step::Select(selector) if selector.aligned))
                        {
                            if let Step::Select(selector) = &mut source.steps[position] {
                                selector.aligned = true;
                            }
                        }
                    }
                }
                let mut nodes = fragment(&template[..start], registry, bindings, depth + 1)?;
                let body = &template[start..repeat_end];
                let guards = markers(body, registry)?
                    .into_iter()
                    .filter_map(|marker| match marker.expression {
                        Expression::Data(path, _) => Some(path),
                        Expression::Translation(_) => None,
                    })
                    .collect();
                bindings.insert(key.clone());
                let body = fragment(body, registry, bindings, depth + 1)?;
                bindings.remove(&key);
                nodes.push(Node::Loop {
                    source,
                    stop: position,
                    key,
                    guards,
                    body,
                });
                nodes.extend(fragment(&template[end..], registry, bindings, depth + 1)?);
                return Ok(nodes);
            }
        }
    }
    if !regions.is_empty() {
        return conditional_regions(template, registry, bindings, depth, regions);
    }
    let mut nodes = Vec::new();
    let mut cursor = 0;
    for marker in marker_list {
        if cursor < marker.start {
            nodes.push(Node::Static(template[cursor..marker.start].to_owned()));
        }
        nodes.push(Node::Value {
            attribute: in_attribute(template, marker.start),
            expression: marker.expression,
        });
        cursor = marker.end;
    }
    if cursor < template.len() {
        nodes.push(Node::Static(template[cursor..].to_owned()));
    }
    Ok(nodes)
}

fn conditional_regions(
    template: &str,
    registry: &Registry,
    bindings: &mut HashSet<String>,
    depth: usize,
    regions: Vec<crate::blocks::Region>,
) -> Result<Vec<Node>> {
    let mut nodes = Vec::new();
    let mut cursor = 0;
    for region in regions {
        nodes.extend(fragment(&template[cursor..region.start], registry, bindings, depth + 1)?);
        nodes.push(Node::Block {
            expression: region.expression,
            hide: region.hide,
            body: fragment(&template[region.body], registry, bindings, depth + 1)?,
        });
        cursor = region.end;
    }
    nodes.extend(fragment(&template[cursor..], registry, bindings, depth + 1)?);
    Ok(nodes)
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
            let (ch, width) = syntax::character(&body[position..]);
            if quote.is_none()
                && ch == '<'
                && body
                    .as_bytes()
                    .get(position + 1)
                    .is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'/' | b'!' | b'?'))
            {
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
                text.push_str(&body[position..position + width]);
                position += width;
            }
        }
        if !is_marker(&text) {
            continue;
        }
        result.push_str(&template[cursor..span.start]);
        result.push('{');
        result.push_str(&text.replace('<', "&lt;"));
        result.push('}');
        result.push_str(&tags);
        cursor = span.end;
    }
    result.push_str(&template[cursor..]);
    Ok(result)
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
        if syntax::markers(&template[second.end..])?.is_empty() {
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
    let parent = |marker: &Marker| {
        elements
            .iter()
            .rposition(|element| element.start < marker.start && element.end >= marker.end)
    };
    if parent(first) == parent(second) {
        return Ok((first.start, second.start, second.end));
    }
    Err(Error::Unsupported(
        "loop markers span incompatible XML elements".into(),
    ))
}
