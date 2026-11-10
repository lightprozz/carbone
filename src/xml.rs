//! Zero-copy XML tokenization for template fragments, not a validating XML parser.
//!
//! Names retain namespace prefixes. Entities, attribute syntax, namespace bindings,
//! and element balance are left to callers; incomplete markup is an error.

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind<'a> {
    Start {
        name: &'a str,
    },
    End {
        name: &'a str,
    },
    Empty {
        name: &'a str,
    },
    Text,
    Comment,
    Cdata,
    /// Processing instructions and declarations, including a whole DOCTYPE.
    Declaration,
}

/// A half-open byte span into the original input, including markup delimiters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Token<'a> {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) kind: Kind<'a>,
}

/// Emits one error for malformed markup, then stops. Unbalanced fragments are valid.
#[derive(Clone, Debug)]
pub(crate) struct Tokens<'a> {
    text: &'a str,
    cursor: usize,
}

impl<'a> Tokens<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self { text, cursor: 0 }
    }

    fn markup(&self, start: usize) -> Result<Token<'a>> {
        let end = tag_end(self.text, start)?;
        let raw = &self.text[start..end];
        let kind = if raw.starts_with("<!--") {
            Kind::Comment
        } else if raw.starts_with("<![CDATA[") {
            Kind::Cdata
        } else if raw.starts_with("<!") || raw.starts_with("<?") {
            Kind::Declaration
        } else {
            let body = &self.text[start + 1..end - 1];
            let closing = body.starts_with('/');
            let body = if closing { &body[1..] } else { body };
            let name_end = body
                .find(|ch: char| ch.is_whitespace() || ch == '/')
                .unwrap_or(body.len());
            let name = &body[..name_end];
            if name.is_empty() || (closing && !body[name_end..].trim().is_empty()) {
                return Err(invalid(start, "invalid XML element name"));
            }
            if closing {
                Kind::End { name }
            } else if body.trim_end().ends_with('/') {
                Kind::Empty { name }
            } else {
                Kind::Start { name }
            }
        };
        Ok(Token { start, end, kind })
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Result<Token<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor == self.text.len() {
            return None;
        }
        let start = self.cursor;
        if self.text.as_bytes()[start] != b'<' {
            let end = self.text[start..]
                .find('<')
                .map_or(self.text.len(), |offset| start + offset);
            self.cursor = end;
            return Some(Ok(Token {
                start,
                end,
                kind: Kind::Text,
            }));
        }
        let token = self.markup(start);
        self.cursor = token.as_ref().map_or(self.text.len(), |token| token.end);
        Some(token)
    }
}

impl std::iter::FusedIterator for Tokens<'_> {}

fn invalid(start: usize, message: &str) -> Error {
    Error::Template(format!("{message} at byte {start}"))
}

/// Returns the exclusive end of markup starting at the byte offset of `<`.
/// Unlike tokenization, this only checks delimiters, not the element name.
pub(crate) fn tag_end(text: &str, start: usize) -> Result<usize> {
    let Some(rest) = text.get(start..) else {
        return Err(invalid(start, "invalid XML start offset"));
    };
    if !rest.starts_with('<') {
        return Err(invalid(start, "expected XML markup"));
    }
    for (prefix, suffix) in [("<!--", "-->"), ("<![CDATA[", "]]>"), ("<?", "?>")] {
        if let Some(body) = rest.strip_prefix(prefix) {
            return body
                .find(suffix)
                .map(|end| start + prefix.len() + end + suffix.len())
                .ok_or_else(|| invalid(start, "unclosed XML section"));
        }
    }

    let declaration = rest.starts_with("<!");
    let bytes = text.as_bytes();
    let mut cursor = start + 1;
    let mut quote = None;
    let mut subset_depth = 0usize;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
            }
        } else if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
        } else if declaration && byte == b'<' && bytes[cursor..].starts_with(b"<!--") {
            // Brackets and quotes inside DTD comments must not affect the subset.
            cursor = tag_end(text, cursor)?;
            continue;
        } else if declaration && byte == b'<' && bytes[cursor..].starts_with(b"<?") {
            cursor = tag_end(text, cursor)?;
            continue;
        } else if declaration && byte == b'[' {
            subset_depth += 1;
        } else if declaration && byte == b']' && subset_depth > 0 {
            subset_depth -= 1;
        } else if byte == b'>' && subset_depth == 0 {
            return Ok(cursor + 1);
        } else if byte == b'<' && !declaration {
            return Err(invalid(cursor, "unexpected '<' in XML tag"));
        }
        cursor += 1;
    }
    Err(invalid(start, "unclosed XML tag"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(text: &str) -> Vec<Token<'_>> {
        Tokens::new(text).collect::<Result<Vec<_>>>().unwrap()
    }

    #[test]
    fn tokenizes_all_kinds_with_contiguous_byte_spans() {
        let text = "before<?xml version='1.0'?><!DOCTYPE w:p><w:p a='>'>hello<!-- <x> --><![CDATA[<raw>]]><w:br /></w:p>after";
        let expected = [
            ("before", Kind::Text),
            ("<?xml version='1.0'?>", Kind::Declaration),
            ("<!DOCTYPE w:p>", Kind::Declaration),
            ("<w:p a='>'>", Kind::Start { name: "w:p" }),
            ("hello", Kind::Text),
            ("<!-- <x> -->", Kind::Comment),
            ("<![CDATA[<raw>]]>", Kind::Cdata),
            ("<w:br />", Kind::Empty { name: "w:br" }),
            ("</w:p>", Kind::End { name: "w:p" }),
            ("after", Kind::Text),
        ];
        let actual = tokens(text);
        assert_eq!(actual.len(), expected.len());
        let mut cursor = 0;
        for (token, (raw, kind)) in actual.iter().zip(expected) {
            assert_eq!(token.start, cursor);
            assert_eq!(&text[token.start..token.end], raw);
            assert_eq!(token.kind, kind);
            cursor = token.end;
        }
        assert_eq!(cursor, text.len());
    }

    #[test]
    fn unicode_names_and_text_borrow_the_original_bytes() {
        let text = "🙂<前:項目 値='é > 🦀'>你好&amp;é</前:項目>終";
        let actual = tokens(text);
        assert_eq!(actual[0].end, "🙂".len());
        assert_eq!(actual[1].kind, Kind::Start { name: "前:項目" });
        let Kind::Start { name } = actual[1].kind else {
            panic!("expected start tag");
        };
        assert_eq!(name.as_ptr(), text[actual[1].start + 1..].as_ptr());
        assert_eq!(&text[actual[2].start..actual[2].end], "你好&amp;é");
        assert_eq!(actual[3].kind, Kind::End { name: "前:項目" });
        assert_eq!(actual[4].end, text.len());
    }

    #[test]
    fn quoted_delimiters_and_empty_tags() {
        let text = r#"<p a="' > / <" b='" > <' >text<x/><y / ></p >"#;
        let actual = tokens(text);
        assert_eq!(actual[0].kind, Kind::Start { name: "p" });
        assert_eq!(actual[2].kind, Kind::Empty { name: "x" });
        assert_eq!(actual[3].kind, Kind::Empty { name: "y" });
        assert_eq!(actual[4].kind, Kind::End { name: "p" });
        assert_eq!(tag_end(text, 0).unwrap(), text.find("text").unwrap());
    }

    #[test]
    fn doctype_internal_subset_is_one_token() {
        let dtd = "<!DOCTYPE r SYSTEM 'a>b' [<!ELEMENT r (#PCDATA)><!ENTITY value ']> <x>'><!-- ] [ ' > --><?inside ] [ ' > ?><![IGNORE[ ignored > ]] >]>";
        let text = format!("{dtd}<r/>");
        let actual = tokens(&text);
        assert_eq!(actual.len(), 2);
        assert_eq!(actual[0].kind, Kind::Declaration);
        assert_eq!(actual[0].end, dtd.len());
        assert_eq!(tag_end(&text, 0).unwrap(), dtd.len());
        assert_eq!(actual[1].kind, Kind::Empty { name: "r" });
    }

    #[test]
    fn sections_use_their_own_terminators() {
        let text = "<!-- ' > < ] --> <![CDATA[' > < ]]]> <?pi ' > < ] ?> <!ENTITY x '>'>";
        let actual = tokens(text);
        assert_eq!(actual.len(), 7);
        assert_eq!(actual[0].kind, Kind::Comment);
        assert_eq!(actual[2].kind, Kind::Cdata);
        assert_eq!(actual[4].kind, Kind::Declaration);
        assert_eq!(actual[6].kind, Kind::Declaration);
    }

    #[test]
    fn permits_fragments_and_does_not_validate_attributes_or_entities() {
        let actual = tokens("</outside><open unquoted=value>&unknown;<other>");
        assert_eq!(actual.len(), 4);
        assert_eq!(actual[0].kind, Kind::End { name: "outside" });
        assert_eq!(actual[1].kind, Kind::Start { name: "open" });
        assert_eq!(actual[2].kind, Kind::Text);
        assert_eq!(actual[3].kind, Kind::Start { name: "other" });
    }

    #[test]
    fn empty_and_plain_text_inputs() {
        assert!(tokens("").is_empty());
        assert_eq!(
            tokens("é > &amp; plain"),
            vec![Token {
                start: 0,
                end: "é > &amp; plain".len(),
                kind: Kind::Text,
            }]
        );
    }

    #[test]
    fn malformed_and_truncated_markup_returns_one_error_then_stops() {
        for text in [
            "<",
            "</",
            "<p",
            "<p a='>",
            "<p a=\">",
            "<!-->",
            "<![CDATA[>",
            "<?pi>",
            "<!DOCTYPE r [<!ELEMENT r EMPTY>>",
            "<!DOCTYPE r [<!-- ]>",
            "<!DOCTYPE r [<?pi ]>",
            "<>",
            "</>",
            "< />",
            "</p extra>",
            "<p <q>",
        ] {
            let mut iter = Tokens::new(text);
            assert!(
                matches!(iter.next(), Some(Err(Error::Template(_)))),
                "{text}"
            );
            assert!(iter.next().is_none(), "{text}");
            assert!(iter.next().is_none(), "{text}");
        }
        let mut iter = Tokens::new("ok<p");
        assert_eq!(iter.next().unwrap().unwrap().kind, Kind::Text);
        assert!(iter.next().unwrap().is_err());
        assert!(iter.next().is_none());
    }

    #[test]
    fn tag_end_rejects_invalid_offsets_without_panicking() {
        let text = "é<x/>";
        for start in [0, 1, text.len(), text.len() + 1, usize::MAX] {
            assert!(tag_end(text, start).is_err(), "offset {start}");
        }
        assert_eq!(tag_end(text, "é".len()).unwrap(), text.len());
    }

    #[test]
    fn every_unicode_truncation_keeps_valid_spans_and_terminates() {
        let text = "é<?xml?><!DOCTYPE 項 [<!ENTITY x '>'><!-- ] -->]><前:項 a='🦀>'>值<![CDATA[<>]]></前:項>";
        for end in text
            .char_indices()
            .map(|(offset, _)| offset)
            .chain([text.len()])
        {
            let prefix = &text[..end];
            let mut cursor = 0;
            let mut iter = Tokens::new(prefix);
            while let Some(result) = iter.next() {
                match result {
                    Ok(token) => {
                        assert_eq!(token.start, cursor);
                        assert!(token.end > token.start);
                        assert!(prefix.get(token.start..token.end).is_some());
                        cursor = token.end;
                    }
                    Err(_) => {
                        assert!(iter.next().is_none());
                        break;
                    }
                }
            }
        }
    }
}
