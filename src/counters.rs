//! Compile-time compatibility with parser.findMarkers/assignLoopId.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::syntax::{self, Expression, Formatter, Marker};
use crate::{Error, Result};

static COUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[^\n\r\u{2028}\u{2029}]*?:count(\(([^\n\r\u{2028}\u{2029}]*?)\))?").unwrap()
});

#[derive(Debug)]
pub(crate) struct Counter {
    id: String,
    parameters: Formatter,
}

pub(crate) struct Plan {
    ordinary: Vec<(String, Option<Counter>)>,
}

fn unsupported(message: &str) -> Error {
    Error::Unsupported(format!("count: {message}"))
}

fn structural(marker: &Marker) -> bool {
    matches!(&marker.expression, Expression::Data(_, pipeline)
        if pipeline.iter().any(|formatter| crate::blocks::is_structural(&formatter.name)))
}

fn raw(template: &str, marker: &Marker) -> String {
    crate::aliases::clean(&template[marker.start + 1..marker.end - 1])
}

pub(crate) fn prepare(source: &str, normalized: &str) -> Result<Plan> {
    let markers = syntax::markers(normalized)?;
    let has_count = markers.iter().any(|marker| {
        matches!(&marker.expression, Expression::Data(_, pipeline)
            if pipeline.iter().any(|formatter| formatter.name == "count"))
    });
    if !has_count {
        return Ok(Plan {
            ordinary: Vec::new(),
        });
    }
    if markers
        .iter()
        .any(|marker| matches!(marker.expression, Expression::Translation(_)))
    {
        return Err(unsupported(
            "translations together with counters are not supported",
        ));
    }
    let source_markers: Vec<_> = syntax::spans(source)?
        .into_iter()
        .filter(|span| syntax::is_marker(&source[span.start + 1..span.end - 1]))
        .collect();
    if source_markers.len() != markers.len() {
        return Err(unsupported("alias expansion changes marker count or order"));
    }

    let mut ordinary = Vec::new();
    let mut cursor = 0;
    let mut static_position = 0;
    let mut presence = 0;
    let mut previous_position: f64 = -1.0;
    let mut last_index = 0;
    for (ordinal, (source_marker, marker)) in source_markers.iter().zip(&markers).enumerate() {
        let source_raw =
            crate::aliases::clean(&source[source_marker.start + 1..source_marker.end - 1]);
        let text = raw(normalized, marker);
        let is_structural = structural(marker);
        if source_raw.contains('$') {
            if is_structural {
                return Err(unsupported("aliases expanding to structural block markers"));
            }
        } else if source_raw != text {
            return Err(unsupported("alias expansion changes marker count or order"));
        }
        static_position += source[cursor..source_marker.start].encode_utf16().count();
        cursor = source_marker.end;
        if !is_structural {
            presence += 1;
        }
        let mut position = (static_position + presence) as f64;
        if position == previous_position.trunc() {
            position = previous_position + 1.0 / 64.0;
        }
        previous_position = position;

        let name = format!("_root.{text}");
        let Expression::Data(path, pipeline) = &marker.expression else {
            unreachable!()
        };
        let count_formatters: Vec<_> = pipeline
            .iter()
            .filter(|formatter| formatter.name == "count")
            .collect();
        if !count_formatters.is_empty() {
            if is_structural {
                return Err(unsupported(
                    "counters inside structural begin/end pipelines",
                ));
            }
            if !path.steps.iter().any(|step| step.iteration().is_some()) {
                return Err(unsupported("requires an array or object loop"));
            }
        }
        // JS replaces the first bare `count`, even when its regex matched later.
        // Restrict occurrences to actual formatter names so fields/arguments cannot be rewritten.
        let parts = syntax::split_top(&name, ':')?;
        let mut formatter_positions = Vec::new();
        let mut part_start = 0;
        for (index, part) in parts.iter().enumerate() {
            if index > 0 && (*part == "count" || part.starts_with("count(")) {
                formatter_positions.push(part_start);
            }
            part_start += part.len() + 1;
        }
        if name.contains(":count")
            && name
                .match_indices("count")
                .map(|(start, _)| start)
                .collect::<Vec<_>>()
                != formatter_positions
        {
            return Err(unsupported(
                "ambiguous count text in a field, formatter name, or argument",
            ));
        }

        let mut planned = None;
        // lastIndex is UTF-16, not bytes, and persists across every full marker name.
        let start = utf16_offset(&name, last_index);
        if let Some(captures) = start.and_then(|start| COUNT.captures_at(&name, start)) {
            let matched = captures.get(0).unwrap();
            last_index = name[..matched.end()].encode_utf16().count();
            let count_start = captures.get(1).map_or(matched.end(), |group| group.start()) - 5;
            let index = formatter_positions
                .iter()
                .position(|&start| start == count_start)
                .ok_or_else(|| unsupported("ambiguous counter regex match"))?;
            planned = Some(Counter {
                id: format!("{ordinal}{position}"),
                parameters: (*count_formatters[index]).clone(),
            });
        } else {
            last_index = 0;
        }
        if !is_structural {
            ordinary.push((text, planned));
        }
    }
    Ok(Plan { ordinary })
}

// A non-Unicode JS regex can start between surrogate halves. Advancing to the
// next scalar boundary is equivalent here because the sought suffix is ASCII.
fn utf16_offset(text: &str, index: usize) -> Option<usize> {
    let mut units = 0;
    for (offset, ch) in text.char_indices() {
        if units >= index {
            return Some(offset);
        }
        units += ch.len_utf16();
    }
    (units >= index).then_some(text.len())
}

impl Plan {
    pub(crate) fn expanded(self, expanded: &str) -> Result<HashMap<usize, Counter>> {
        if self.ordinary.is_empty() {
            return Ok(HashMap::new());
        }
        let ordinary: Vec<_> = syntax::markers(expanded)?
            .into_iter()
            .filter(|marker| !structural(marker))
            .collect();
        if ordinary.len() != self.ordinary.len() {
            return Err(unsupported("block expansion changes ordinary marker count"));
        }
        let mut counters = HashMap::new();
        for ((text, counter), marker) in self.ordinary.into_iter().zip(ordinary) {
            if text != raw(expanded, &marker) {
                return Err(unsupported("block expansion changes ordinary marker order"));
            }
            if let Some(counter) = counter {
                counters.insert(marker.start, counter);
            }
        }
        Ok(counters)
    }
}

pub(crate) fn annotate(expression: &mut Expression, counter: &Counter) {
    if let Expression::Data(_, pipeline) = expression {
        if let Some(formatter) = pipeline
            .iter_mut()
            .find(|formatter| formatter.name == "count")
        {
            formatter.counter_id = Some(counter.id.clone());
            formatter.args = counter.parameters.args.clone();
            formatter.arguments = counter.parameters.arguments.clone();
        }
    }
}
