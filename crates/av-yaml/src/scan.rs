//! The event-level policy: what is refused before the typed parse sees the text.
//!
//! One walk over the parser's own event stream (the same `granit-parser` events the typed
//! parse is built on). It refuses, with the line and column:
//!
//! - a second document (`serde_yaml` refused a multi-document stream too);
//! - a `%YAML` directive (the version changes how a YAML 1.1 reader resolves scalars);
//! - any explicit tag (`!Foo`, `!!str`, `!!binary`, ...): `serde_yaml` mostly ignored the tag
//!   on a typed read and kept it in an untyped one, while the new parser obeys it (`!!str 5`
//!   into an integer field, `!!binary` decoded as base64, `!!null x` a null), so no tag is
//!   read the same way by both;
//! - a plain `<<` mapping key: `serde_yaml` kept it as an ordinary key (a typed struct skipped
//!   it as unknown), the new parser merges the mappings it names, which is the author's
//!   evident intent and a different value;
//! - a plain scalar the two parsers read differently (`crate::dialect`);
//! - a block scalar (`|` or `>`) that ends the input when the input does not end in a line
//!   break: `serde_yaml` kept the text as written, the new parser appends the missing `\n`.
//!
//! Duplicate keys, alias bombs and recursive aliases are the typed parse's own refusals
//! (`crate::options`), mapped to typed errors in `crate::convert`.

use serde_saphyr::granit_parser::{Event, Parser, ScalarStyle};

use crate::dialect::{At, Checker};
use crate::Error;

/// U+0085 (NEL), U+2028 (LS) and U+2029 (PS) are line breaks to `serde_yaml`'s YAML 1.1 reader
/// (a scalar containing one is folded to a space) and ordinary characters to the new parser
/// (YAML 1.2), so the same text is two different strings. Written as an escape (`"\N"`,
/// `"\L"`, `"\P"`) they are unambiguous; raw, they are refused wherever they stand.
fn refuse_unicode_line_breaks(text: &str) -> Result<(), Error> {
    if let Some((index, ch)) = text.char_indices().find(|&(_, c)| matches!(c, '\u{85}' | '\u{2028}' | '\u{2029}')) {
        let before = &text[..index];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        return Err(Error::UnicodeLineBreak { character: ch, line, column });
    }
    Ok(())
}

enum Frame {
    Seq,
    /// Whether the next node in this mapping is a key.
    Map { next_is_key: bool },
}

pub(crate) fn scan(text: &str) -> Result<(), Error> {
    refuse_unicode_line_breaks(text)?;
    let mut stack: Vec<Frame> = Vec::new();
    let mut documents = 0usize;
    let mut checker = Checker::default();
    // The most recent scalar was a block scalar and no node has started since.
    let mut block_scalar_is_last = false;

    for item in Parser::new_from_str(text) {
        let (event, span) = item.map_err(|e| Error::Parse(e.to_string()))?;
        let at = At { line: span.start.line(), column: span.start.col() + 1 };

        // Is this node a mapping key? Then flip the parent's expectation (every node, scalar or
        // collection, is one entry of its parent).
        let is_key = match (&event, stack.last_mut()) {
            (Event::Scalar(..) | Event::Alias(_) | Event::SequenceStart(..) | Event::MappingStart(..), Some(Frame::Map { next_is_key })) => {
                let key = *next_is_key;
                *next_is_key = !key;
                key
            }
            _ => false,
        };

        match &event {
            Event::DocumentStart(_, version) => {
                documents += 1;
                if documents > 1 {
                    return Err(Error::MultipleDocuments);
                }
                if version.is_some() {
                    return Err(Error::Directive { line: at.line, column: at.column });
                }
            }
            Event::Scalar(value, style, _anchor, tag) => {
                if let Some(tag) = tag {
                    return Err(Error::ExplicitTag { tag: tag.to_string(), line: at.line, column: at.column });
                }
                block_scalar_is_last = matches!(style, ScalarStyle::Literal | ScalarStyle::Folded);
                if *style == ScalarStyle::Plain {
                    if is_key && value == "<<" {
                        return Err(Error::MergeKey { line: at.line, column: at.column });
                    }
                    checker.check(value, at)?;
                }
            }
            Event::SequenceStart(_, _, tag) | Event::MappingStart(_, _, tag) => {
                if let Some(tag) = tag {
                    return Err(Error::ExplicitTag { tag: tag.to_string(), line: at.line, column: at.column });
                }
                block_scalar_is_last = false;
                stack.push(if matches!(event, Event::SequenceStart(..)) { Frame::Seq } else { Frame::Map { next_is_key: true } });
            }
            Event::SequenceEnd | Event::MappingEnd => {
                stack.pop();
            }
            Event::Alias(_) => block_scalar_is_last = false,
            _ => {}
        }
    }

    if block_scalar_is_last && !text.ends_with(['\n', '\r']) {
        return Err(Error::BlockScalarAtEnd);
    }
    Ok(())
}
