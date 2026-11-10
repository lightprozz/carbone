//! Compile-time aliases, following parser.findVariables/preprocessMarkers.

use std::collections::HashMap;

use crate::{Error, Result};

const MAX_SIZE: usize = 32 * 1024 * 1024;
const MAX_DEPTH: usize = 64;

#[derive(Debug)]
struct Alias {
    name: String,
    code: String,
    empty_parameters: bool,
}

pub(crate) fn expand(template: &str) -> Result<String> {
    check_size(template.len())?;
    let mut aliases = Vec::new();
    let mut without_declarations = String::new();
    let mut previous = 0;
    for (start, end) in marker_ranges(template) {
        let (plain, xml) = separate_xml(&template[start + 1..end - 1]);
        if let Some(alias) = declaration(&plain)? {
            append(&mut without_declarations, &template[previous..start])?;
            append(&mut without_declarations, &xml)?;
            aliases.push(alias);
            previous = end;
        }
    }
    append(&mut without_declarations, &template[previous..])?;
    if aliases.is_empty() {
        return Ok(without_declarations);
    }
    validate_dependencies(&aliases)?;

    let mut output = String::new();
    previous = 0;
    for (start, end) in marker_ranges(&without_declarations) {
        let raw = &without_declarations[start + 1..end - 1];
        if !raw.contains('$') {
            continue;
        }
        let (plain, xml) = separate_xml(raw);
        if !is_marker(&plain) {
            continue;
        }
        let original = format!("_root.{}", clean(&plain));
        let mut expanded = original.clone();
        // Node scans the original marker for each alias, not newly injected code.
        for alias in &aliases {
            for (usage, params) in usages(&original, &alias.name) {
                let mut code = alias.code.clone();
                let params = if alias.empty_parameters {
                    Some("")
                } else {
                    params
                };
                if let Some(params) = params {
                    // Intentionally comma-based, as in the Node reference.
                    for (index, value) in params.split(',').enumerate() {
                        code = replace_all(&code, &format!("_${index}_"), value.trim())?;
                    }
                }
                expanded = replace_all(&expanded, usage, &code)?;
            }
        }
        append(&mut output, &without_declarations[previous..start])?;
        let expanded = expanded.strip_prefix("_root.").unwrap();
        if !expanded.is_empty() {
            if !is_marker(&expanded) {
                return Err(Error::Unsupported(format!(
                    "alias expands to a non-marker expression {expanded:?}"
                )));
            }
            append(&mut output, "{")?;
            append(&mut output, &expanded)?;
            append(&mut output, "}")?;
        }
        append(&mut output, &xml)?;
        previous = end;
    }
    append(&mut output, &without_declarations[previous..])?;
    Ok(output)
}

fn check_size(size: usize) -> Result<()> {
    if size > MAX_SIZE {
        return Err(Error::Template("alias expansion exceeds 32 MiB".into()));
    }
    Ok(())
}

fn append(output: &mut String, text: &str) -> Result<()> {
    let size = output
        .len()
        .checked_add(text.len())
        .ok_or_else(|| Error::Template("alias expansion exceeds 32 MiB".into()))?;
    check_size(size)?;
    output.push_str(text);
    Ok(())
}

fn replace_all(text: &str, needle: &str, replacement: &str) -> Result<String> {
    let mut output = String::new();
    let mut previous = 0;
    for (start, matched) in text.match_indices(needle) {
        append(&mut output, &text[previous..start])?;
        let mut remaining = replacement;
        while let Some(index) = remaining.find('$') {
            append(&mut output, &remaining[..index])?;
            let after = &remaining[index + 1..];
            let substitution = match after.as_bytes().first() {
                Some(b'$') => "$",
                Some(b'&') => matched,
                Some(b'`') => &text[..start],
                Some(b'\'') => &text[start + matched.len()..],
                _ => {
                    append(&mut output, "$")?;
                    remaining = after;
                    continue;
                }
            };
            append(&mut output, substitution)?;
            remaining = &after[1..];
        }
        append(&mut output, remaining)?;
        previous = start + matched.len();
    }
    append(&mut output, &text[previous..])?;
    Ok(output)
}

fn marker_ranges(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut opening = None;
    text.char_indices().filter_map(move |(index, ch)| match ch {
        '{' => {
            opening = Some(index);
            None
        }
        '}' => opening
            .take()
            .filter(|start| index > start + 1)
            .map(|start| (start, index + 1)),
        _ => None,
    })
}

fn separate_xml(text: &str) -> (String, String) {
    let mut plain = String::new();
    let mut xml = String::new();
    let mut remaining = text;
    while let Some(start) = remaining.find('<') {
        let Some(end) = remaining[start + 1..].find('>') else {
            break;
        };
        let end = start + 1 + end + 1;
        plain.push_str(&remaining[..start]);
        xml.push_str(&remaining[start..end]);
        remaining = &remaining[end..];
    }
    plain.push_str(remaining);
    (plain, xml)
}

fn clean(text: &str) -> String {
    let decoded = text
        .replace(['\n', '\t'], "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&apos;", "'");
    let mut output = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in decoded.chars() {
        if let Some(delimiter) = quote {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            output.push(ch);
        } else if !ch.is_whitespace() && ch != '\u{feff}' {
            output.push(ch);
        }
    }
    output
}

fn identifier(text: &str) -> bool {
    let mut chars = text.bytes();
    matches!(chars.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == b'_')
}

fn declaration(text: &str) -> Result<Option<Alias>> {
    let has_body = text
        .split_once('=')
        .is_some_and(|(_, code)| !code.is_empty());
    let text = clean(text);
    let Some(text) = text.strip_prefix('#') else {
        return Ok(None);
    };
    let Some((signature, code)) = text.split_once('=') else {
        return Ok(None);
    };
    if signature.is_empty() || !has_body {
        return Ok(None);
    }
    let (name, params) = match signature.split_once('(') {
        Some((name, params)) => {
            let params = params
                .strip_suffix(')')
                .ok_or_else(|| Error::Template("unclosed alias parameter list".into()))?;
            (name, Some(params))
        }
        None => (signature, None),
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(Error::Unsupported(format!(
            "alias name {name:?}; regular-expression syntax is not supported"
        )));
    }
    let mut code = code.to_owned();
    if let Some(params) = params.filter(|params| !params.is_empty()) {
        let mut params: Vec<_> = params.split(',').enumerate().collect();
        for (_, param) in &params {
            if !param.strip_prefix('$').is_some_and(identifier) {
                return Err(Error::Template(format!(
                    "invalid alias parameter {param:?}"
                )));
            }
        }
        params.sort_by_key(|(_, param)| std::cmp::Reverse(param.len()));
        for (index, param) in params {
            code = replace_all(&code, param, &format!("_${index}_"))?;
        }
    }
    Ok(Some(Alias {
        name: name.to_owned(),
        code,
        empty_parameters: params == Some(""),
    }))
}

fn is_marker(text: &str) -> bool {
    let text = text.trim_start();
    if text.starts_with(['#', '$']) || text.starts_with("bindColor") {
        return true;
    }
    let mut chars = text.chars();
    matches!(chars.next(), Some('d' | 'c' | 't'))
        && matches!(
            chars.find(|ch| !ch.is_whitespace()),
            Some('.' | '[' | ':' | '(')
        )
}

fn usages<'a>(text: &'a str, name: &str) -> Vec<(&'a str, Option<&'a str>)> {
    let needle = format!("${name}");
    let mut matches = Vec::new();
    let mut previous = 0;
    while let Some(start) = text[previous..].find(&needle) {
        let start = previous + start;
        let after = start + needle.len();
        previous = after;
        if text
            .as_bytes()
            .get(after)
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == b'_')
        {
            continue;
        }
        let mut end = after;
        let mut params = None;
        if text.as_bytes().get(after) == Some(&b'(') {
            if let Some(close) = text[after + 1..].find(')') {
                if close > 0 {
                    end = after + 1 + close + 1;
                    params = Some(&text[after + 1..end - 1]);
                }
            }
        }
        matches.push((&text[start..end], params));
        previous = end;
    }
    matches
}

fn validate_dependencies(aliases: &[Alias]) -> Result<()> {
    let mut names = HashMap::new();
    for (index, alias) in aliases.iter().enumerate() {
        names.entry(alias.name.as_str()).or_insert(index);
    }
    let mut edges = vec![Vec::new(); aliases.len()];
    for (index, alias) in aliases.iter().enumerate() {
        for (name, &dependency) in &names {
            if !usages(&alias.code, name).is_empty() {
                edges[index].push(dependency);
            }
        }
    }
    fn visit(
        index: usize,
        edges: &[Vec<usize>],
        active: &mut [bool],
        heights: &mut [usize],
        depth: usize,
    ) -> Result<usize> {
        if active[index] {
            return Err(Error::Template("cyclic alias declarations".into()));
        }
        if depth > MAX_DEPTH {
            return Err(Error::Template(
                "alias dependencies exceed 64 levels".into(),
            ));
        }
        if heights[index] != 0 {
            if depth - 1 + heights[index] > MAX_DEPTH {
                return Err(Error::Template(
                    "alias dependencies exceed 64 levels".into(),
                ));
            }
            return Ok(heights[index]);
        }
        active[index] = true;
        let mut height = 1;
        for &dependency in &edges[index] {
            height = height.max(1 + visit(dependency, edges, active, heights, depth + 1)?);
        }
        active[index] = false;
        heights[index] = height;
        Ok(height)
    }
    let mut active = vec![false; aliases.len()];
    let mut heights = vec![0; aliases.len()];
    for index in 0..aliases.len() {
        visit(index, &edges, &mut active, &mut heights, 1)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_node_alias_edge_corpus() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/b2-alias-edges.json")).unwrap();
        for case in corpus["cases"].as_array().unwrap() {
            let actual = crate::render_template(
                case["template"].as_str().unwrap(),
                &case["data"],
                &crate::RenderOptions::default(),
            );
            assert_eq!(
                actual.unwrap(),
                case["expected"].as_str().unwrap(),
                "{}",
                case["id"]
            );
        }
    }

    #[test]
    fn selectors_and_top_level_aliases() {
        assert_eq!(
            expand("{#myVar=i=0}{#field=d.name}<x>{d.cars[$myVar].brand}|{$field}</x>").unwrap(),
            "<x>{d.cars[i=0].brand}|{d.name}</x>"
        );
        assert_eq!(expand("{$field}{#field=c.title}").unwrap(), "{c.title}");
    }

    #[test]
    fn parameters_and_names_with_shared_prefixes() {
        assert_eq!(expand("{#v2=i=2}{#v=i=1}{#range($id,$idEnd)=i&gt;$id,i&lt;$idEnd}{d[$range(1,3)].brand}|{d[$v2].brand}|{d[$v].brand}").unwrap(),
            "{d[i>1,i<3].brand}|{d[i=2].brand}|{d[i=1].brand}");
        assert_eq!(
            expand("{#x($a,$ab)=d.$ab.$a}{$x(one,two)}").unwrap(),
            "{d.two.one}"
        );
        assert_eq!(
            expand("{#x($a,$b)=d.$a.$b}{$x(one)}").unwrap(),
            "{d.one._$1_}"
        );
    }

    #[test]
    fn preserves_literals_and_unresolved_aliases() {
        let text = "{#x=d.name} {normal $x} $x {literal} {{json}} {$unknown}";
        assert_eq!(
            expand(text).unwrap(),
            " {normal $x} $x {literal} {{json}} {$unknown}"
        );
        for text in [
            "{}",
            "{{",
            "{#noDefinition}",
            "{literal {nested}}",
            "é 🦀 {x}",
        ] {
            assert_eq!(expand(text).unwrap(), text);
        }
    }

    #[test]
    fn split_xml_and_quoted_whitespace() {
        assert_eq!(
            expand("<x>{ <br> # <b> myFn ( $a,$ab ) = id = $a,g=$ab }</x>{d[$myFn(1, 2)].name}")
                .unwrap(),
            "<x><br><b></x>{d[id=1,g=2].name}"
        );
        assert_eq!(
            expand("{#field = d.name:ifEQ(&apos;hello, world&apos;)}{$field}").unwrap(),
            "{d.name:ifEQ('hello, world')}"
        );
        assert_eq!(expand("{#x=d.name}{<b>$x</b>}").unwrap(), "{d.name}<b></b>");
    }

    #[test]
    fn node_uses_original_markers_and_first_declaration() {
        assert_eq!(expand("{#a=$b}{#b=d.name}{$a}").unwrap(), "{$b}");
        assert_eq!(expand("{#a=d.name}{#a=d.other}{$a}").unwrap(), "{d.name}");
        assert_eq!(
            expand("{#a=$b}{#b=d.name}{$a}/{$b}").unwrap(),
            "{$b}/{d.name}"
        );
        assert_eq!(
            expand("{#a=d.name}{$a()} {d.name:ifEQ('$a')}").unwrap(),
            "{d.name()} {d.name:ifEQ('d.name')}"
        );
    }

    #[test]
    fn empty_parameter_signatures_follow_node_capture_groups() {
        assert_eq!(expand("{#a()=d.name}{$a}").unwrap(), "{d.name}");
        assert_eq!(expand("{#a()=d.name}{$a()}").unwrap(), "{d.name()}");
        assert_eq!(expand("{#a()=d.name}{$a(ignored)}").unwrap(), "{d.name}");
        assert_eq!(expand("{#a()=d._$0_}{$a(ignored)}").unwrap(), "{d.}");
    }

    #[test]
    fn javascript_replacement_tokens_use_the_original_match_context() {
        assert_eq!(
            replace_all("before-x-after-x", "x", "[$$][$&][$`][$']").unwrap(),
            "before-[$][x][before-][-after-x]-after-[$][x][before-x-after-][]"
        );
        assert_eq!(
            replace_all("éx🦀x", "x", "$`/$&/$'").unwrap(),
            "éé/x/🦀x🦀éx🦀/x/"
        );
        assert_eq!(
            replace_all("x", "x", "$0/$1/$99/$<name>/$").unwrap(),
            "$0/$1/$99/$<name>/$"
        );
        assert_eq!(
            expand("{#a=d.missing:ifEmpty('$`')}{$a}").unwrap(),
            "{d.missing:ifEmpty('_root.')}"
        );
        assert_eq!(expand("{#a($p)=d.$pfield}{$a($`)}").unwrap(), "{d.d.field}");
        assert_eq!(
            expand("{#a($p)=d.$pfield}{$a($')}").unwrap(),
            "{d.fieldfield}"
        );
    }

    #[test]
    fn regex_literal_alias_names() {
        assert_eq!(
            expand("{#1=d.name}{#a-b=d.other}{#é=c.title}{$1}|{$a-b}|{$é}").unwrap(),
            "{d.name}|{d.other}|{c.title}"
        );
        assert_eq!(expand("{#a=d.name}{$ab}|{$aé}").unwrap(), "{$ab}|{d.nameé}");
        assert!(expand("{#é=$a-b}{#a-b=$é}").is_err());
    }

    #[test]
    fn rejects_cycles_and_excessive_depth() {
        for text in ["{#a=$a}{$a}", "{#a=$b}{#b=$a}{$a}"] {
            assert!(matches!(expand(text), Err(Error::Template(_))));
        }
        for count in [64, 65] {
            let mut text = String::new();
            for index in 0..count - 1 {
                text.push_str(&format!("{{#a{index}=$a{}}}", index + 1));
            }
            text.push_str(&format!("{{#a{}=d.name}}{{$a0}}", count - 1));
            assert_eq!(expand(&text).is_ok(), count == 64);
        }
    }

    #[test]
    fn rejects_oversized_input_and_expansion() {
        assert!(expand(&"x".repeat(MAX_SIZE + 1)).is_err());
        assert_eq!(expand(&"x".repeat(MAX_SIZE)).unwrap().len(), MAX_SIZE);
        let template = format!(
            "{{#a={}}}{{d.name:ifEQ('{}')}}",
            "x".repeat(8192),
            "$a".repeat(4097)
        );
        assert!(expand(&template).is_err());
        let template = format!("{{#a($p)=$p$p$p}}{{$a({})}}", "x".repeat(MAX_SIZE / 3 + 1));
        assert!(expand(&template).is_err());
    }

    #[test]
    fn invalid_or_unsupported_definitions_are_errors_not_panics() {
        for text in [
            "{#x($p=d.name}",
            "{#x(p)=d.p}",
            "{#a.b=d.name}",
            "{#a=literal}{$a}",
        ] {
            assert!(expand(text).is_err(), "{text}");
        }
        assert_eq!(expand("{#x=}").unwrap(), "{#x=}");
        assert_eq!(expand("{#x=   }{$x}").unwrap(), "");
    }
}
