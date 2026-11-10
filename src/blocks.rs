//! Structural conditional blocks. Expand once on normalized XML before compiling
//! loops; then use `regions` on each recursive fragment of the expanded template.

use std::ops::Range;

use crate::syntax::{self, Expression, Marker};
use crate::xml::{Kind, Tokens};
use crate::{Error, Result};

const MAX_DEPTH: usize = 64;

#[derive(Debug)]
pub(crate) struct Region {
    /// Complete span, including the opening and closing markers.
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) body: Range<usize>,
    /// Opening pipeline without Begin. If it stops early, emit its value and
    /// leave the body visible: the structural formatter was never reached.
    pub(crate) expression: Expression,
    pub(crate) hide: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Boundary {
    Begin(bool),
    End(bool),
}

pub(crate) fn is_structural(name: &str) -> bool {
    matches!(name, "showBegin" | "showEnd" | "hideBegin" | "hideEnd")
}

fn boundary(marker: &Marker) -> Result<Option<Boundary>> {
    let Expression::Data(_, pipeline) = &marker.expression else {
        return Ok(None);
    };
    let mut result = None;
    for (index, formatter) in pipeline.iter().enumerate() {
        if !is_structural(&formatter.name) {
            continue;
        }
        if result.is_some() || index + 1 != pipeline.len() || !formatter.args.is_empty() {
            return Err(invalid(
                "block formatter must be last and have no arguments",
            ));
        }
        result = Some(match formatter.name.as_str() {
            "showBegin" => Boundary::Begin(false),
            "hideBegin" => Boundary::Begin(true),
            "showEnd" => Boundary::End(false),
            "hideEnd" => Boundary::End(true),
            _ => unreachable!(),
        });
    }
    Ok(result)
}

fn invalid(message: &str) -> Error {
    Error::Template(format!("conditional block: {message}"))
}

/// Read outermost blocks from the result of `expand`. Offsets address `template`.
/// Compile the prefix/suffix normally and recurse into each `body` under a guard.
/// Closing expressions do not participate in the condition (nor need to match
/// the opening data path).
pub(crate) fn regions(template: &str) -> Result<Vec<Region>> {
    let mut stack: Vec<(Marker, bool)> = Vec::new();
    let mut regions = Vec::new();
    for marker in syntax::markers(template)? {
        match boundary(&marker)? {
            Some(Boundary::Begin(hide)) => {
                if stack.len() == MAX_DEPTH {
                    return Err(invalid("nesting exceeds 64 levels"));
                }
                stack.push((marker, hide));
            }
            Some(Boundary::End(hide)) => {
                let Some((mut opening, expected)) = stack.pop() else {
                    return Err(invalid("closing marker has no opening marker"));
                };
                if hide != expected {
                    return Err(invalid(
                        "show/hide opening and closing markers do not match",
                    ));
                }
                if stack.is_empty() {
                    let Expression::Data(_, pipeline) = &mut opening.expression else {
                        unreachable!()
                    };
                    pipeline.pop();
                    regions.push(Region {
                        start: opening.start,
                        end: marker.end,
                        body: opening.end..marker.start,
                        expression: opening.expression,
                        hide,
                    });
                }
            }
            None => {}
        }
    }
    if !stack.is_empty() {
        return Err(invalid("opening marker has no closing marker"));
    }
    Ok(regions)
}

#[derive(Debug)]
struct Block {
    begin: usize,
    end: usize,
    opening: String,
    closing: String,
    parent: Option<usize>,
}

#[derive(Debug)]
struct Part {
    start: usize,
    end: usize,
    matching: Option<usize>,
}

fn flatten(template: &str) -> Result<Vec<Part>> {
    let mut parts: Vec<Part> = Vec::new();
    let mut stack = Vec::new();
    for token in Tokens::new(template) {
        let token = token?;
        let index = parts.len();
        let mut part = Part {
            start: token.start,
            end: token.end,
            matching: None,
        };
        match token.kind {
            Kind::Start { name } => stack.push((index, name)),
            Kind::End { name } => {
                let Some((opening, expected)) = stack.pop() else {
                    return Err(invalid("unbalanced XML closing tag"));
                };
                if name != expected {
                    return Err(invalid("mismatched XML element names"));
                }
                parts[opening].matching = Some(index);
                part.matching = Some(opening);
            }
            _ => {}
        }
        parts.push(part);
    }
    if !stack.is_empty() {
        return Err(invalid("unbalanced XML opening tag"));
    }
    Ok(parts)
}

// Port of findSafeConditionalBlockPosition/moveConditionalBlockBeginEnd, using
// tokenized XML so quoted '>', comments, CDATA and declarations remain atomic.
fn safe_ranges(parts: &[Part], mut begin: usize, mut end: usize) -> Vec<Range<usize>> {
    let mut first = parts.partition_point(|part| part.end <= begin);
    let original_begin = begin;
    let original_end = end;
    let mut index = first;
    while let Some(part) = parts.get(index) {
        if let Some(matching) = part.matching {
            let other = &parts[matching];
            if other.end < original_end && index < matching {
                index = matching;
            }
            if other.end == begin {
                begin = other.start;
                first = matching;
            }
            if part.start >= original_end
                && other.start < part.start
                && other.start >= original_begin
            {
                end = part.end;
            }
            if part.end > end || (other.end < begin && part.start > end) {
                break;
            }
        } else if part.end > end {
            break;
        }
        index += 1;
    }

    let mut ranges: Vec<Range<usize>> = Vec::new();
    index = first;
    while let Some(part) = parts.get(index) {
        if part.start >= end {
            break;
        }
        let candidate = match part.matching {
            Some(matching) if index < matching && parts[matching].end <= end => {
                index = matching;
                Some(part.start..parts[matching].end)
            }
            None => Some(part.start..part.end),
            _ => None,
        };
        if let Some(candidate) = candidate {
            let candidate = candidate.start.max(begin)..candidate.end.min(end);
            if candidate.start < candidate.end {
                if let Some(last) = ranges.last_mut().filter(|last| last.end == candidate.start) {
                    last.end = candidate.end;
                } else {
                    ranges.push(candidate);
                }
            }
        }
        index += 1;
    }
    if ranges.is_empty() {
        ranges.push(end..end);
    }
    ranges
}

#[derive(Debug)]
struct Interval {
    range: Range<usize>,
    block: usize,
    children: Vec<usize>,
}

fn ancestor(blocks: &[Block], outer: usize, mut inner: usize) -> bool {
    while let Some(parent) = blocks[inner].parent {
        if parent == outer {
            return true;
        }
        inner = parent;
    }
    false
}

/// Move block markers to the largest safe XML spans. One block can become
/// several guards, leaving unmatched surrounding XML tags unconditional.
/// Ordinary markers remain present while finding boundaries: a value marker
/// next to an opening XML tag must prevent that tag from being pulled inside.
/// Malformed/crossing block pairs and unsafe XML overlaps fail explicitly.
pub(crate) fn expand(template: &str) -> Result<String> {
    let markers = syntax::markers(template)?;
    let mut structural = Vec::new();
    for marker in markers {
        if let Some(kind) = boundary(&marker)? {
            structural.push((marker, kind));
        }
    }
    if structural.is_empty() {
        return Ok(template.into());
    }

    // A structural marker must occupy XML text, never an attribute or comment.
    let tokens = Tokens::new(template).collect::<Result<Vec<_>>>()?;
    let mut token_index = 0;
    let mut clean = String::with_capacity(template.len());
    let mut cursor = 0;
    let mut blocks: Vec<Block> = Vec::new();
    let mut stack: Vec<(usize, bool)> = Vec::new();
    for (marker, kind) in structural {
        while tokens[token_index].end <= marker.start {
            token_index += 1;
        }
        let token = &tokens[token_index];
        if token.kind != Kind::Text || token.end < marker.end {
            return Err(invalid("markers must be normalized and occur in XML text"));
        }
        clean.push_str(&template[cursor..marker.start]);
        let position = clean.len();
        match kind {
            Boundary::Begin(hide) => {
                if stack.len() == MAX_DEPTH {
                    return Err(invalid("nesting exceeds 64 levels"));
                }
                let index = blocks.len();
                blocks.push(Block {
                    begin: position,
                    end: position,
                    opening: template[marker.start..marker.end].into(),
                    closing: String::new(),
                    parent: stack.last().map(|(index, _)| *index),
                });
                stack.push((index, hide));
            }
            Boundary::End(hide) => {
                let Some((index, expected)) = stack.pop() else {
                    return Err(invalid("closing marker has no opening marker"));
                };
                if expected != hide {
                    return Err(invalid(
                        "show/hide opening and closing markers do not match",
                    ));
                }
                blocks[index].end = position;
                blocks[index].closing = template[marker.start..marker.end].into();
            }
        }
        cursor = marker.end;
    }
    if !stack.is_empty() {
        return Err(invalid("opening marker has no closing marker"));
    }
    clean.push_str(&template[cursor..]);
    let parts = flatten(&clean)?;
    let mut intervals: Vec<Interval> = blocks
        .iter()
        .enumerate()
        .flat_map(|(block, pair)| {
            safe_ranges(&parts, pair.begin, pair.end)
                .into_iter()
                .map(move |range| Interval {
                    range,
                    block,
                    children: Vec::new(),
                })
        })
        .collect();
    intervals.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then_with(|| b.range.end.cmp(&a.range.end))
            .then_with(|| a.block.cmp(&b.block))
    });
    let mut roots = Vec::new();
    let mut parents: Vec<usize> = Vec::new();
    for index in 0..intervals.len() {
        while let Some(&parent) = parents.last() {
            let outer = &intervals[parent];
            let inner = &intervals[index];
            let nested = ancestor(&blocks, outer.block, inner.block);
            if outer.range.end < inner.range.start
                || (outer.range.end == inner.range.start && !(nested && inner.range.is_empty()))
            {
                parents.pop();
                continue;
            }
            if outer.range.end < inner.range.end || !nested {
                return Err(invalid("XML boundary expansion creates overlapping blocks"));
            }
            break;
        }
        if let Some(&parent) = parents.last() {
            intervals[parent].children.push(index);
        } else {
            roots.push(index);
        }
        parents.push(index);
    }
    let mut output = String::with_capacity(template.len());
    emit(
        &clean,
        0..clean.len(),
        &roots,
        &intervals,
        &blocks,
        &mut output,
    );
    Ok(output)
}

fn emit(
    clean: &str,
    range: Range<usize>,
    children: &[usize],
    intervals: &[Interval],
    blocks: &[Block],
    output: &mut String,
) {
    let mut cursor = range.start;
    for &index in children {
        let interval = &intervals[index];
        let block = &blocks[interval.block];
        output.push_str(&clean[cursor..interval.range.start]);
        output.push_str(&block.opening);
        emit(
            clean,
            interval.range.clone(),
            &interval.children,
            intervals,
            blocks,
            output,
        );
        output.push_str(&block.closing);
        cursor = interval.range.end;
    }
    output.push_str(&clean[cursor..range.end]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(template: &str, visible: bool) -> String {
        fn fragment(template: &str, visible: bool) -> String {
            let mut result = String::new();
            let mut cursor = 0;
            for region in regions(template).unwrap() {
                result.push_str(&template[cursor..region.start]);
                if visible != region.hide {
                    result.push_str(&fragment(&template[region.body], visible));
                }
                cursor = region.end;
            }
            result.push_str(&template[cursor..]);
            result
        }
        fragment(&expand(template).unwrap(), visible)
    }

    #[test]
    fn plain_text_and_empty_blocks() {
        let template = "é{d.ok:showBegin}hello🙂{d.anything:showEnd}fin";
        assert_eq!(output(template, false), "éfin");
        assert_eq!(output(template, true), "éhello🙂fin");
        assert_eq!(output("{d.ok:hideBegin}{d.end:hideEnd}", true), "");
        let expanded = expand("{d.ok:ifEQ(1):showBegin}{d.end:showEnd}").unwrap();
        assert_eq!(regions(&expanded).unwrap().len(), 1);
    }

    #[test]
    fn expands_adjacent_surrounding_elements_but_not_text() {
        let template = "<xml><p>{d.ok:showBegin}hello</p>{d.end:showEnd}<q/></xml>";
        assert_eq!(output(template, false), "<xml><q/></xml>");
        assert_eq!(output(template, true), "<xml><p>hello</p><q/></xml>");
        let template = "<xml><p> {d.ok:showBegin}hello</p>{d.end:showEnd}</xml>";
        assert_eq!(output(template, false), "<xml><p> </p></xml>");
    }

    #[test]
    fn preserves_unmatched_xml_skeleton_using_disjoint_ranges() {
        let template =
            "<xml><tr> a {d.hidden:hideBegin} b </tr> b <tr> d </tr> e {d.end:hideEnd}f</xml>";
        assert_eq!(output(template, true), "<xml><tr> a </tr>f</xml>");
        assert_eq!(
            output(template, false),
            "<xml><tr> a  b </tr> b <tr> d </tr> e f</xml>"
        );
        let template = "<a><b>x{d.ok:showBegin}</b></a><i>Z</i><j><k>{d.end:showEnd}m</k></j>";
        assert_eq!(output(template, false), "<a><b>x</b></a><j><k>m</k></j>");
    }

    #[test]
    fn value_markers_prevent_outward_expansion() {
        let template = "<xml><p>{d.id}{d.ok:showBegin}a</p>{d.end:showEnd}</xml>";
        assert_eq!(output(template, false), "<xml><p>{d.id}</p></xml>");
        let template = "<xml><p>{d.ok:showBegin}a{d.end:showEnd}{d.id}</p></xml>";
        assert_eq!(output(template, false), "<xml><p>{d.id}</p></xml>");
    }

    #[test]
    fn nested_and_consecutive_blocks() {
        let template =
            "<xml>{d.a:showBegin}<p>A{d.b:showBegin}<b>B</b>{d.b:showEnd}C</p>{d.a:showEnd}</xml>";
        assert_eq!(output(template, true), "<xml><p>A<b>B</b>C</p></xml>");
        assert_eq!(output(template, false), "<xml></xml>");
        let template =
            "<xml>{d.a:showBegin}<p>A</p>{d.a:showEnd}{d.b:showBegin}<q>B</q>{d.b:showEnd}</xml>";
        assert_eq!(output(template, true), "<xml><p>A</p><q>B</q></xml>");
        assert_eq!(output(template, false), "<xml></xml>");
    }

    #[test]
    fn preserves_condition_chains_and_ignores_closing_expression() {
        let template = "<r>{d.test.a:ifEQ(1):and(.text):ifNEM:showBegin()}yes{d.test.other:ifEM:showEnd()}</r>";
        let expanded = expand(template).unwrap();
        let blocks = regions(&expanded).unwrap();
        let Expression::Data(path, pipeline) = &blocks[0].expression else {
            panic!()
        };
        assert_eq!(path.root, 'd');
        assert_eq!(
            pipeline.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            ["ifEQ", "and", "ifNEM"]
        );
        assert_eq!(pipeline[1].args, [".text"]);
        assert!(!blocks[0].hide);
        assert_eq!(output(template, true), "<r>yes</r>");
    }

    #[test]
    fn xml_token_boundaries_are_atomic() {
        let template = "<r>{d.a:showBegin}<p title='>'><!-- <x> --><![CDATA[<unbalanced>]]><?p?><br/></p>{d.end:showEnd}tail</r>";
        assert_eq!(output(template, false), "<r>tail</r>");
        assert_eq!(
            output(template, true),
            "<r><p title='>'><!-- <x> --><![CDATA[<unbalanced>]]><?p?><br/></p>tail</r>"
        );
    }

    #[test]
    fn rejects_malformed_or_unsafe_blocks() {
        for template in [
            "{d.a:showBegin}",
            "{d.a:showEnd}",
            "{d.a:showBegin}x{d.a:hideEnd}",
            "{d.a:showBegin(1)}x{d.a:showEnd}",
            "{d.a:showBegin:upperCase}x{d.a:showEnd}",
            "{d.a:showBegin:hideBegin}x{d.a:hideEnd}",
            "<r>{d.a:showBegin}<a></b>{d.a:showEnd}</r>",
            "<r>{d.a:showBegin}x{d.a:showEnd}",
            "<r a='{d.a:showBegin}'>x{d.a:showEnd}</r>",
            "<r><!-- {d.a:showBegin} -->x{d.a:showEnd}</r>",
            "<r><![CDATA[{d.a:showBegin}]]>x{d.a:showEnd}</r>",
        ] {
            assert!(expand(template).is_err(), "{template}");
        }
    }

    #[test]
    fn limits_nesting_and_leaves_nonblocks_unchanged() {
        let template = format!(
            "{}x{}",
            "{d.a:showBegin}".repeat(65),
            "{d.a:showEnd}".repeat(65)
        );
        assert!(expand(&template).is_err());
        assert!(regions(&template).is_err());
        let template = "<r a='{d.name}'>{d.name:upperCase}</r>";
        assert_eq!(expand(template).unwrap(), template);
        assert!(regions(template).unwrap().is_empty());
    }
    #[test]
    fn matches_original_parser_safe_boundary_cases() {
        assert_eq!(
            safe_ranges(&flatten("<xml><p><h1></h1>a</p></xml>").unwrap(), 5, 17),
            vec![8..17]
        );
        assert_eq!(
            safe_ranges(&flatten("<xml><p><h1></h1></p></xml>").unwrap(), 8, 17),
            vec![8..17]
        );
        assert_eq!(
            safe_ranges(&flatten("<xm><p>a<h1></h1></p> </xm>").unwrap(), 8, 21),
            vec![8..17]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>a</a></a></a></a></xml>").unwrap(),
                5,
                17
            ),
            vec![17..17]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>a</a></a></a></a></xml>").unwrap(),
                18,
                34
            ),
            vec![34..34]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p><h1><br/><br/><br/></h1>a</p></xml>").unwrap(),
                5,
                32
            ),
            vec![8..32]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p><h1><br/><br/><br/></h1><br/>a</p></xml>").unwrap(),
                5,
                37
            ),
            vec![8..37]
        );
        assert_eq!(
            safe_ranges(&flatten("<xm><b>a</b><br/>ab<b>a</b></xm>").unwrap(), 8, 22),
            vec![12..19]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a> <br/> b</a></a></a></a></xml>").unwrap(),
                5,
                24
            ),
            vec![17..24]
        );
        assert_eq!(
            safe_ranges(&flatten("<xml>ab</xml>").unwrap(), 0, 13),
            vec![0..13]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p><h1><br/><br/><br/></h1><br/>aa</p></xml>").unwrap(),
                5,
                38
            ),
            vec![8..38]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p><h1><br/><br/><br/></h1><br/>a </p></xml>").unwrap(),
                5,
                38
            ),
            vec![8..38]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xm><p>aa<h1><br/><br/><br/></h1><br/>aa</p></xm>").unwrap(),
                4,
                39
            ),
            vec![7..39]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p> a<h1><br/><br/><br/></h1><br/>a</p></xml>").unwrap(),
                5,
                39
            ),
            vec![8..39]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p>a <h1><br/><br/><br/></h1><br/>aa</p></xml>").unwrap(),
                5,
                40
            ),
            vec![8..40]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p>a<h1><br/><br/><br/></h1><br/>a</p></xml>").unwrap(),
                5,
                38
            ),
            vec![8..38]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  a</a></a></a></a></xml>").unwrap(),
                5,
                19
            ),
            vec![17..19]
        );
        assert_eq!(
            safe_ranges(&flatten("<xml>ab</xml>").unwrap(), 5, 7),
            vec![5..7]
        );
        assert_eq!(
            safe_ranges(&flatten("<xm>xab</xm>").unwrap(), 5, 12),
            vec![5..7]
        );
        assert_eq!(safe_ranges(&flatten("ab").unwrap(), 0, 2), vec![0..2]);
        assert_eq!(safe_ranges(&flatten("abcd").unwrap(), 1, 3), vec![1..3]);
        assert_eq!(
            safe_ranges(&flatten("<xm><b>x</b>ab<b><br/>a</b></xm>").unwrap(), 8, 22),
            vec![12..14, 17..22]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xm><p><p><p><p>x <a>b</a> </p></p></p></p> <br/><br/><br/> </xm>")
                    .unwrap(),
                17,
                60
            ),
            vec![17..27, 43..60]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xm><p><p><p><p>x <a>b</a> <br/></p></p></p></p> <br/><br/><br/> </xm>")
                    .unwrap(),
                17,
                65
            ),
            vec![17..32, 48..65]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><b><br/><br/></b><br/>ab<br/><b><br/><br/></b></xml>").unwrap(),
                13,
                42
            ),
            vec![13..18, 22..34, 37..42]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml> <p><br/><br/><br/>x</p></xml>").unwrap(),
                5,
                24
            ),
            vec![5..6, 9..24]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml> <p><br/><br/><br/> w</p></xml>").unwrap(),
                5,
                25
            ),
            vec![5..6, 9..25]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xm><p><p><p><p>x </p></p></p></p> <br/><br/><br/> </xm>").unwrap(),
                17,
                51
            ),
            vec![17..18, 34..51]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<div><if>aa</if><if>bb</if></div>").unwrap(),
                10,
                21
            ),
            vec![10..11, 20..21]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<div><if url=\"https://\">a<br></br>abb</if><if>sssss</if></div>")
                    .unwrap(),
                5,
                49
            ),
            vec![5..42, 46..49]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<a>xtext</a><b></b><c></c><d>texta</d><br/>").unwrap(),
                4,
                33
            ),
            vec![4..8, 12..26, 29..33]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<a>x</a><b></b><c></c><d>texta</d><br/>").unwrap(),
                4,
                29
            ),
            vec![8..22, 25..29]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<a>xtext</a><b></b><c></c><d>a</d><br/>").unwrap(),
                4,
                29
            ),
            vec![4..8, 12..26]
        );
        assert_eq!(
            safe_ranges(
                &flatten(
                    "<xm>x<table><tr><td><p></p></td></tr><tr><td><p></p></td></tr></table></xm>"
                )
                .unwrap(),
                5,
                79
            ),
            vec![5..70]
        );
        assert_eq!(safe_ranges(&flatten("<xml><table> <tr> <td><p></p></td> </tr>   <tr> <td><p></p></td> </tr> </table></xml>").unwrap(), 5, 79), vec![5..79]);
        assert_eq!(
            safe_ranges(
                &flatten("<xml> <table> <tr> <li> </li><li> </li> </tr> <tr></tr></table> </xml>")
                    .unwrap(),
                24,
                24
            ),
            vec![24..24]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<body><a></a><p></p><p></p><n></n></body> ").unwrap(),
                9,
                30
            ),
            vec![6..34]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a></a></a></a></a></xml>").unwrap(),
                5,
                17
            ),
            vec![5..33]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a></a></a></a></a></xml>").unwrap(),
                17,
                33
            ),
            vec![5..33]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a> <br/> </a></a></a></a></xml>").unwrap(),
                5,
                24
            ),
            vec![5..40]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  </a></a></a></a></xml>").unwrap(),
                5,
                19
            ),
            vec![5..35]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  </a></a></a></a></xml>").unwrap(),
                8,
                19
            ),
            vec![8..31]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  </a></a></a></a></xml>").unwrap(),
                11,
                19
            ),
            vec![11..27]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  </a></a></a></a></xml>").unwrap(),
                14,
                19
            ),
            vec![14..23]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  </a></a></a></a></xml>").unwrap(),
                17,
                23
            ),
            vec![14..23]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  </a></a></a></a></xml>").unwrap(),
                17,
                27
            ),
            vec![11..27]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  </a></a></a></a></xml>").unwrap(),
                17,
                31
            ),
            vec![8..31]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  </a></a></a></a></xml>").unwrap(),
                17,
                35
            ),
            vec![5..35]
        );
        assert_eq!(
            safe_ranges(
                &flatten(
                    "<xml><a><a><a><a>  </a></a></a></a>a<a><a><a><a>  </a></a></a></a></xml>"
                )
                .unwrap(),
                35,
                48
            ),
            vec![35..36]
        );
        assert_eq!(
            safe_ranges(
                &flatten(
                    "<xml><a><a><a><a>  </a></a></a></a>a<a><a><a><a>  </a></a></a></a></xml>"
                )
                .unwrap(),
                35,
                58
            ),
            vec![35..66]
        );
        assert_eq!(
            safe_ranges(
                &flatten(
                    "<xml><a><a><a><a>  </a></a></a></a>a<a><a><a><a>  </a></a>b</a></a></xml>"
                )
                .unwrap(),
                35,
                58
            ),
            vec![35..36, 42..58]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<body><p>a</p><g></g><g></g><p></p></body>").unwrap(),
                10,
                31
            ),
            vec![14..35]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  </a><p></p></a></a></a></xml>").unwrap(),
                5,
                19
            ),
            vec![14..23]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  </a><p></p></a></a></a></xml>").unwrap(),
                17,
                42
            ),
            vec![5..42]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><p><p></p></p><a><p></p><a>  </a></a></a></a></xml>").unwrap(),
                5,
                40
            ),
            vec![5..56]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a>za<a>zd<p><p>dza</p></p><a><p></p><a>  </a></a></a></a></xml>")
                    .unwrap(),
                5,
                47
            ),
            vec![5..63]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><p></p><a>  </a></a></a></a></xml>").unwrap(),
                5,
                26
            ),
            vec![5..42]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><p></p><a>  </a></a></a></a></xml>").unwrap(),
                24,
                42
            ),
            vec![21..30]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a>x<a>  </a></a></a></a></xml>").unwrap(),
                18,
                36
            ),
            vec![15..24]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a>x<a><a>  </a></a></a></a></xml>").unwrap(),
                18,
                36
            ),
            vec![12..28]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a>  </a><p>z</p></a><p>z</p></a></a></xml>").unwrap(),
                17,
                51
            ),
            vec![5..51]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a><p>qs</p></a></a></a></a></xml>").unwrap(),
                17,
                26
            ),
            vec![17..26]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><a><a><a><a> ed<br/><p>qs</p>qs </a></a></a></a></xml>").unwrap(),
                17,
                37
            ),
            vec![17..37]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml> <tr></tr>a<tr></tr> </xml>").unwrap(),
                10,
                20
            ),
            vec![6..25]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p><b></b>ab<b></b></p></xml>").unwrap(),
                11,
                20
            ),
            vec![8..24]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p>ab<b></b>ab<b></b></p></xml>").unwrap(),
                13,
                26
            ),
            vec![10..26]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p><b></b>ab<b></b>ab</p></xml>").unwrap(),
                11,
                20
            ),
            vec![8..24]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p><h1><br/><br/><br/></h1><br/>a</p></xml>").unwrap(),
                5,
                38
            ),
            vec![5..42]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p>a<h1><br/><br/><br/></h1><br/>a</p></xml>").unwrap(),
                5,
                39
            ),
            vec![5..43]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p>a <h1><br/><br/><br/></h1><br/>a</p></xml>").unwrap(),
                5,
                40
            ),
            vec![5..44]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p>a<h1><br/><br/><br/></h1><br/></p></xml>").unwrap(),
                5,
                38
            ),
            vec![5..42]
        );
        assert_eq!(
            safe_ranges(&flatten("<xml>ab</xml>").unwrap(), 5, 13),
            vec![0..13]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><b></b>ab<b><br/>a</b></xml>").unwrap(),
                8,
                22
            ),
            vec![5..14, 17..22]
        );
        assert_eq!(
            safe_ranges(&flatten("<xml><b></b>ab<b><br/></b></xml>").unwrap(), 8, 22),
            vec![5..26]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p><p><p><p> <a>b</a> </p></p></p></p> <br/><br/><br/> </xml>")
                    .unwrap(),
                17,
                60
            ),
            vec![5..60]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p><p><p><p> <a>b</a> <br/></p></p></p></p> <br/><br/><br/> </xml>")
                    .unwrap(),
                17,
                65
            ),
            vec![5..65]
        );
        assert_eq!(
            safe_ranges(
                &flatten(
                    "<a><xml><p><p><p><p> <a>b</a> <br/></p></p></p></p> <br/><br/><br/> </xml></a>"
                )
                .unwrap(),
                20,
                68
            ),
            vec![8..68]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml> <p><br/><br/><br/></p> </xml>").unwrap(),
                5,
                29
            ),
            vec![5..29]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<xml><p><p><p><p> </p></p></p></p> <br/><br/><br/> </xml>").unwrap(),
                17,
                51
            ),
            vec![5..51]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<a>text</a><b></b><c></c><d>text</d><br/>").unwrap(),
                3,
                32
            ),
            vec![0..36]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<a></a><b></b><c></c><d>text</d><br/>").unwrap(),
                3,
                28
            ),
            vec![0..32]
        );
        assert_eq!(
            safe_ranges(
                &flatten("<a>text</a><b></b><c></c><d></d><br/>").unwrap(),
                3,
                28
            ),
            vec![0..32]
        );
        assert_eq!(
            safe_ranges(
                &flatten(
                    "<xml><table><tr><td><p></p></td></tr><tr><td><p></p></td></tr></table></xml>"
                )
                .unwrap(),
                5,
                79
            ),
            vec![0..76]
        );
    }
}
