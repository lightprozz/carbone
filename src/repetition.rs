use std::collections::HashMap;

use crate::syntax::{self, Marker};
use crate::xml::{Kind, Tokens};
use crate::{Error, Result};

#[derive(Clone, Copy, PartialEq)]
enum Direction {
    Open,
    Close,
    Empty,
}

#[derive(Clone, Copy)]
struct Tag {
    start: usize,
    end: usize,
    direction: Direction,
}

struct MaskedMarker {
    position: usize,
    original_start: usize,
    original_end: usize,
}

struct Masked {
    text: String,
    markers: Vec<MaskedMarker>,
}

impl Masked {
    fn new(template: &str) -> Result<Self> {
        let mut text = String::with_capacity(template.len());
        let mut markers = Vec::new();
        let mut cursor = 0;
        for span in syntax::spans(template)? {
            if !syntax::is_marker(&template[span.start + 1..span.end - 1]) {
                continue;
            }
            text.push_str(&template[cursor..span.start]);
            markers.push(MaskedMarker {
                position: text.len(),
                original_start: span.start,
                original_end: span.end,
            });
            text.push('x');
            cursor = span.end;
        }
        text.push_str(&template[cursor..]);
        Ok(Self { text, markers })
    }

    fn after(&self, marker: &Marker) -> Result<usize> {
        self.markers
            .iter()
            .find(|masked| {
                masked.original_start == marker.start && masked.original_end == marker.end
            })
            .map(|masked| masked.position + 1)
            .ok_or_else(|| Error::Template("repetition marker is outside its fragment".into()))
    }

    fn original(&self, position: usize, before_marker: bool) -> usize {
        let count = self
            .markers
            .partition_point(|marker| marker.position < position);
        let Some(marker) = count.checked_sub(1).map(|index| &self.markers[index]) else {
            return position;
        };
        if before_marker && position == marker.position + 1 {
            marker.original_start
        } else {
            marker.original_end + position - marker.position - 1
        }
    }
}

pub(crate) fn bounds(
    template: &str,
    first: &Marker,
    second: &Marker,
) -> Result<(usize, usize, usize)> {
    if first.end > second.start {
        return Err(Error::Template(
            "repetition markers are out of order".into(),
        ));
    }
    let masked = Masked::new(template)?;
    let rough_start = masked.after(first)?;
    let rough_end = masked.after(second)?;
    let tags = Tokens::new(&masked.text)
        .filter_map(|token| match token {
            Ok(token) => {
                let direction = match token.kind {
                    Kind::Start { .. } => Direction::Open,
                    Kind::End { .. } => Direction::Close,
                    Kind::Empty { .. } => Direction::Empty,
                    _ => return None,
                };
                Some(Ok(Tag {
                    start: token.start,
                    end: token.end,
                    direction,
                }))
            }
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    let pivot = pivot(&tags, rough_start, rough_end)?;
    let start = opening_position(&tags, pivot.left_end, rough_start + 1);
    let end = match closing_position(&tags, pivot.right_start) {
        Some(end) => end,
        None if pivot.flat => pivot.right_start,
        None => {
            return Err(Error::Unsupported(
                "loop markers span incompatible XML elements".into(),
            ));
        }
    };
    // Node attaches values at the byte after each masked marker. The plan must
    // instead include the opening value and discard the sentinel value.
    let start = masked.original(start, start == rough_start);
    let repeat_end = masked.original(pivot.right_start, pivot.right_start == rough_end);
    let end = masked.original(end, false).max(second.end);
    if start > first.start
        || repeat_end < first.end
        || repeat_end > second.start
        || end > template.len()
    {
        return Err(Error::Unsupported(
            "incompatible repetition boundary".into(),
        ));
    }
    Ok((start, repeat_end, end))
}

struct Pivot {
    left_end: usize,
    right_start: usize,
    flat: bool,
}

fn pivot(tags: &[Tag], start: usize, end: usize) -> Result<Pivot> {
    let mut depth = 0;
    let mut highest = 0;
    let mut rose_above_start = false;
    let mut highest_tags: Vec<(Tag, usize)> = Vec::new();
    for tag in tags
        .iter()
        .filter(|tag| tag.start >= start && tag.start < end)
    {
        let previous = depth;
        depth += match tag.direction {
            Direction::Open => -1,
            Direction::Close => 1,
            Direction::Empty => 0,
        };
        rose_above_start |= depth > 0;
        if depth > highest {
            highest = depth;
            highest_tags.clear();
        }
        if depth == highest || previous == highest {
            if let Some((_, next)) = highest_tags.last_mut() {
                *next = tag.start;
            }
            highest_tags.push((*tag, end));
        }
    }
    let Some((first, first_end)) = highest_tags.first() else {
        return Ok(Pivot {
            left_end: end,
            right_start: end,
            flat: true,
        });
    };
    let (last, last_end) = highest_tags.last().expect("nonempty pivot tags");
    if depth != 0
        && ((highest_tags.len() == 1 && first.direction != Direction::Empty)
            || first.direction == Direction::Open)
    {
        return Err(Error::Unsupported("loop markers have no XML pivot".into()));
    }
    if depth == 0 && !rose_above_start {
        return Ok(Pivot {
            left_end: *last_end,
            right_start: *last_end,
            flat: true,
        });
    }
    Ok(Pivot {
        left_end: *first_end,
        right_start: if highest_tags.len() == 1 {
            *first_end
        } else {
            last.start
        },
        flat: last.direction == Direction::Empty,
    })
}

fn opening_position(tags: &[Tag], end: usize, stop: usize) -> usize {
    let mut depth = 0;
    let mut previous_end = 0;
    let mut openings = HashMap::new();
    let mut before_stop = HashMap::new();
    for tag in tags.iter().filter(|tag| tag.start < end) {
        if tag.direction == Direction::Close {
            depth -= 1;
        } else {
            if previous_end < stop {
                before_stop.insert(depth, tag.start.min(stop - 1));
            }
            openings.insert(depth, tag.start);
            if tag.direction == Direction::Open {
                depth += 1;
            }
        }
        previous_end = tag.end;
    }
    match openings.get(&depth) {
        Some(position) if *position < stop => *position,
        Some(_) => before_stop.get(&depth).copied().unwrap_or(0),
        None => 0,
    }
}

fn closing_position(tags: &[Tag], start: usize) -> Option<usize> {
    let mut depth = 0;
    for tag in tags.iter().filter(|tag| tag.start >= start) {
        depth += match tag.direction {
            Direction::Open => 1,
            Direction::Close => -1,
            Direction::Empty => 0,
        };
        if depth == 0 {
            return Some(tag.end);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(template: &str, first: usize, second: usize) -> (&str, &str, &str) {
        let markers = syntax::markers(template).unwrap();
        let (start, repeated, end) = bounds(template, &markers[first], &markers[second]).unwrap();
        (
            &template[..start],
            &template[start..repeated],
            &template[end..],
        )
    }

    #[test]
    fn sibling_rows_and_intervening_rows() {
        let template = "<xml><tr>{d[i].name}</tr><tr></tr><tr>{d[i+1].name}</tr></xml>";
        assert_eq!(
            parts(template, 0, 1),
            ("<xml>", "<tr>{d[i].name}</tr><tr></tr>", "</xml>")
        );
    }

    #[test]
    fn scalar_markers_between_rows_remain_in_the_repeated_body() {
        let template = "<xml> {d.id}<t_row>{d.id}{d.cars[i].brand}{d.id}</t_row>{d.id}<t_row>{d.id}{d.cars[i+1].brand}{d.id}</t_row>{d.id}</xml>";
        assert_eq!(
            parts(template, 2, 6),
            (
                "<xml> {d.id}",
                "<t_row>{d.id}{d.cars[i].brand}{d.id}</t_row>{d.id}",
                "{d.id}</xml>"
            )
        );
    }

    #[test]
    fn self_closing_delimiters_consume_the_sentinel_trailer() {
        let template = "<xml><p><p><br/></p></p>{d[i].brand}  <br/>{d[i+1].brand}  <br/></xml>";
        assert_eq!(
            parts(template, 0, 1),
            ("<xml><p><p><br/></p></p>", "{d[i].brand}  <br/>", "</xml>")
        );
        let template = "<xml> <br/> {d[i].brand} <br/> <br/><br/> <br/> {d[i+1].brand} <br/></xml>";
        assert_eq!(
            parts(template, 0, 1),
            (
                "<xml> <br/> ",
                "{d[i].brand} <br/> <br/><br/> <br/> ",
                "</xml>"
            )
        );
    }

    #[test]
    fn flat_nested_loop_consumes_the_following_balanced_fragment() {
        let template = "<xml>{d.cars[i].wheels[i]} <i></i> {d.cars[i].wheels[i].nuts[i].type} <b></b> {d.cars[i].wheels[i].nuts[i+1].type} <i></i> {d.cars[i+1].wheels[i+1]}</xml>";
        assert_eq!(
            parts(template, 1, 2),
            (
                "<xml>{d.cars[i].wheels[i]} <i></i> ",
                "{d.cars[i].wheels[i].nuts[i].type} <b></b> ",
                " {d.cars[i+1].wheels[i+1]}</xml>"
            )
        );
        assert_eq!(
            parts(template, 0, 3),
            (
                "<xml>",
                "{d.cars[i].wheels[i]} <i></i> {d.cars[i].wheels[i].nuts[i].type} <b></b> {d.cars[i].wheels[i].nuts[i+1].type} <i></i> ",
                "</xml>"
            )
        );
    }

    #[test]
    fn simultaneous_nested_loops_share_a_span() {
        let template =
            "<xml><tr>{d.cars[i].wheels[i].size}</tr><tr>{d.cars[i+1].wheels[i+1].size}</tr></xml>";
        assert_eq!(
            parts(template, 0, 1),
            ("<xml>", "<tr>{d.cars[i].wheels[i].size}</tr>", "</xml>")
        );
    }

    #[test]
    fn attribute_markers_unicode_and_quoted_angle_brackets() {
        let template = "<xml>é<tr title=\"a>b\" id=\"{d.items[i].id}\">名</tr><tr title=\"a>b\" id=\"{d.items[i+1].id}\">名</tr></xml>";
        assert_eq!(
            parts(template, 0, 1),
            (
                "<xml>é",
                "<tr title=\"a>b\" id=\"{d.items[i].id}\">名</tr>",
                "</xml>"
            )
        );
    }

    #[test]
    fn plain_text_and_flat_empty_boundaries() {
        assert_eq!(
            parts("{d[i].brand} , {d[i+1].brand}", 0, 1),
            ("", "{d[i].brand} , ", "")
        );
        let template = "<xml>{d.cars[i].keys[i]}<tr></tr>{d.cars[i].keys[i]..size}<b></b>{d.cars[i].keys[i]...name}<i></i>{d.cars[i].keys[i].type}<tr></tr>{d.cars[i+1].keys[i+1]}</xml>";
        assert_eq!(
            parts(template, 0, 4),
            (
                "<xml>",
                "{d.cars[i].keys[i]}<tr></tr>{d.cars[i].keys[i]..size}<b></b>{d.cars[i].keys[i]...name}<i></i>{d.cars[i].keys[i].type}<tr></tr>",
                "</xml>"
            )
        );
    }
}
