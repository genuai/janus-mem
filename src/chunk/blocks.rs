//! Markdown event scan: a body becomes sections with byte spans (§6.5 rule 1).
//!
//! One pass over the [`pulldown_cmark`] event stream produces a flat list of
//! top-level *entries* — headings and blocks in document order — which is then
//! folded into [`Section`]s. The two-step shape matters: the event stream is a
//! stream (it can only be walked forwards, and it borrows the body), while the
//! heading tree needs the *previous* heading's level, so the fold happens after
//! the walk, over plain data.
//!
//! Three rules are load-bearing here:
//!
//! 1. Only a heading that appears at the top level opens a section. A `#` inside
//!    a fenced block is code, a `#` inside a block quote belongs to that quote,
//!    and neither may split the document. `pulldown-cmark` already refuses to
//!    treat fenced content as headings; the block-quote case is ours to filter.
//! 2. A block is atomic. Whether a block may be split is decided later, from its
//!    [`BlockKind`], never by re-parsing its text.
//! 3. Spans are byte ranges into the body, because that is what the writer
//!    splices (§7.3) and what `read_document` quotes (§9.2). They are taken from
//!    the parser's own `Start`/`End` offsets, which cover the block *including
//!    its closing fence and trailing newline* — so no span can end in the middle
//!    of a fence.

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// What kind of top-level block a span covers.
///
/// The distinction exists so the splitter can decide what is indivisible without
/// looking at the text: [`BlockKind::Table`] and the code kinds are atomic,
/// [`BlockKind::List`] splits between items, everything else splits internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// A run of inline content between blank lines.
    Paragraph,
    /// A fenced code block (` ``` ` or `~~~`), title or language included.
    CodeBlock,
    /// A code block marked by four-space indentation. It has no closing marker,
    /// so it needs no fence protection — but it is still atomic: a chunk that
    /// starts in the middle of it is a fragment with no context.
    IndentedCode,
    /// A GFM table. Atomic: a header without its body is not a table.
    Table,
    /// A list. Splittable only *between* its top-level items.
    List,
    /// A block quote. Atomic: the `>` prefixes are structure, and splitting
    /// them would change what the text means.
    BlockQuote,
    /// A raw HTML block.
    Html,
    /// A thematic break (`---`, `***`, `___`).
    Rule,
}

impl BlockKind {
    /// True when the block may be split internally.
    ///
    /// [`BlockKind::List`] is not "splittable" in this sense: it splits between
    /// items, which the splitter does from [`BlockSpan::items`], not from the
    /// block's text.
    #[must_use]
    pub const fn is_atomic(self) -> bool {
        matches!(
            self,
            Self::Table | Self::CodeBlock | Self::IndentedCode | Self::BlockQuote
        )
    }
}

/// One top-level block within a section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSpan {
    /// Byte range in the body, from the block's first byte through the newline
    /// that follows its last line.
    pub span: Range<usize>,
    /// What the block is; decides whether it may be split.
    pub kind: BlockKind,
    /// Byte ranges of the block's top-level items, in order.
    ///
    /// Non-empty only for [`BlockKind::List`]. Nested items are deliberately
    /// excluded: a sub-list is part of its parent item, so it moves with it.
    pub items: Vec<Range<usize>>,
}

/// One heading-delimited region of the body.
///
/// A section is *not* a chunk: it is the unit the splitter starts from. A
/// section with no blocks (a heading whose only content is a deeper heading)
/// produces no section at all — that is §6.5 rule 3 applied one layer early, and
/// it keeps the splitter from having to re-check emptiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Ancestor heading texts, outermost first, excluding the document title.
    ///
    /// Empty for content before the first heading. The title is added to the
    /// *embedding* breadcrumb by the indexer, never to a chunk id (CH-5).
    pub heading_path: Vec<String>,
    /// Byte range from the section's heading line (or its first block, for the
    /// preamble) through its last block.
    pub span: Range<usize>,
    /// The section's blocks, in document order. Never empty.
    pub blocks: Vec<BlockSpan>,
}

impl Section {
    /// The source text of this section.
    ///
    /// # Panics
    ///
    /// Never for spans produced by [`sections`]: they are byte ranges inside the
    /// body it was called with.
    #[must_use]
    pub fn text<'a>(&self, body: &'a str) -> &'a str {
        &body[self.span.clone()]
    }
}

/// Splits `body` into heading-delimited sections with byte spans.
///
/// Returns an empty vector for a body with no content or only whitespace. The
/// function never fails: a body the parser cannot make sense of still yields its
/// blocks, which is the fail-open contract reads follow everywhere (§6.3).
#[must_use]
pub fn sections(body: &str) -> Vec<Section> {
    let mut scan = Scan::default();
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    for (event, span) in Parser::new_ext(body, options).into_offset_iter() {
        scan.event(event, span);
    }
    scan.finish()
}

/// A block-level event we are currently inside of.
#[derive(Debug)]
enum Frame {
    /// A block that may become a [`BlockSpan`] if it is top level.
    Block {
        kind: BlockKind,
        span: Range<usize>,
        items: Vec<Range<usize>>,
        top_level: bool,
    },
    /// A heading, whose text is collected from the inline events inside it.
    Heading {
        level: u8,
        text: String,
        span: Range<usize>,
        top_level: bool,
    },
    /// Any other block-level construct (a list item, a footnote definition, or a
    /// heading nested in a quote). It exists only to keep the stack balanced.
    Nested,
}

/// A top-level entry, in document order.
#[derive(Debug)]
enum Entry {
    Heading {
        level: u8,
        text: String,
        span: Range<usize>,
    },
    Block(BlockSpan),
}

/// The event walk's state: open frames plus the entries produced so far.
#[derive(Debug, Default)]
struct Scan {
    stack: Vec<Frame>,
    entries: Vec<Entry>,
}

impl Scan {
    fn event(&mut self, event: Event<'_>, span: Range<usize>) {
        match event {
            Event::Start(tag) => self.start(tag, span),
            Event::End(tag) => self.end(tag),
            Event::Rule => {
                if self.stack.is_empty() {
                    self.entries.push(Entry::Block(BlockSpan {
                        span,
                        kind: BlockKind::Rule,
                        items: Vec::new(),
                    }));
                }
            }
            Event::Text(text) => self.push_text(&text),
            Event::Code(code) => self.push_text(&code),
            Event::SoftBreak | Event::HardBreak => self.push_text(" "),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>, span: Range<usize>) {
        let top_level = self.stack.is_empty();
        match tag {
            Tag::Paragraph => self.push_block(BlockKind::Paragraph, span, top_level),
            Tag::CodeBlock(kind) => {
                let kind = match kind {
                    CodeBlockKind::Fenced(_) => BlockKind::CodeBlock,
                    CodeBlockKind::Indented => BlockKind::IndentedCode,
                };
                self.push_block(kind, span, top_level);
            }
            Tag::HtmlBlock => self.push_block(BlockKind::Html, span, top_level),
            Tag::BlockQuote(_) => self.push_block(BlockKind::BlockQuote, span, top_level),
            Tag::Table(_) => self.push_block(BlockKind::Table, span, top_level),
            Tag::List(_) => self.push_block(BlockKind::List, span, top_level),
            Tag::Item => {
                // Record the item only when it belongs directly to a top-level
                // list: nested items move with their parent item.
                if let Some(Frame::Block {
                    kind: BlockKind::List,
                    items,
                    top_level: true,
                    ..
                }) = self.stack.last_mut()
                {
                    items.push(span);
                }
                self.stack.push(Frame::Nested);
            }
            Tag::Heading { level, .. } => {
                self.stack.push(if top_level {
                    Frame::Heading {
                        level: level as u8,
                        text: String::new(),
                        span,
                        top_level,
                    }
                } else {
                    Frame::Nested
                });
            }
            Tag::FootnoteDefinition(_) => self.stack.push(Frame::Nested),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        // Only tags pushed in `start` are popped. `pulldown-cmark` balances the
        // stream, so this is exact rather than defensive.
        let blockish = matches!(
            tag,
            TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::CodeBlock
                | TagEnd::HtmlBlock
                | TagEnd::BlockQuote(_)
                | TagEnd::Table
                | TagEnd::List(_)
                | TagEnd::Item
                | TagEnd::FootnoteDefinition
        );
        if !blockish {
            return;
        }
        match self.stack.pop() {
            Some(Frame::Block {
                kind,
                span,
                items,
                top_level: true,
            }) => self
                .entries
                .push(Entry::Block(BlockSpan { span, kind, items })),
            Some(Frame::Heading {
                level,
                text,
                span,
                top_level: true,
            }) => self.entries.push(Entry::Heading {
                level,
                text: normalize_heading(&text),
                span,
            }),
            _ => {}
        }
    }

    /// Collects heading text. Inline markup is flattened because a heading's
    /// text is a name, not rendered content.
    fn push_text(&mut self, text: &str) {
        if let Some(Frame::Heading { text: buffer, .. }) = self.stack.last_mut() {
            buffer.push_str(text);
        }
    }

    fn push_block(&mut self, kind: BlockKind, span: Range<usize>, top_level: bool) {
        self.stack.push(Frame::Block {
            kind,
            span,
            items: Vec::new(),
            top_level,
        });
    }

    /// Folds entries into sections: a heading starts one, blocks extend the most
    /// recent one, and headings of the same or shallower level close the deeper
    /// ones.
    fn finish(self) -> Vec<Section> {
        let mut sections: Vec<Section> = Vec::new();
        let mut path: Vec<(u8, String)> = Vec::new();

        for entry in self.entries {
            match entry {
                Entry::Heading { level, text, span } => {
                    while path.last().is_some_and(|(open, _)| *open >= level) {
                        path.pop();
                    }
                    path.push((level, text));
                    sections.push(Section {
                        heading_path: path.iter().map(|(_, text)| text.clone()).collect(),
                        span,
                        blocks: Vec::new(),
                    });
                }
                Entry::Block(block) => match sections.last_mut() {
                    Some(section) => {
                        section.span.end = block.span.end;
                        section.blocks.push(block);
                    }
                    None => sections.push(Section {
                        heading_path: Vec::new(),
                        span: block.span.clone(),
                        blocks: vec![block],
                    }),
                },
            }
        }

        // §6.5 rule 3, applied to whole sections: a heading with no content of
        // its own is a name for its children, not a chunk.
        sections.retain(|section| !section.blocks.is_empty());
        sections
    }
}

/// Collapses a heading's inline whitespace so it can be joined into a breadcrumb
/// without depending on how the author wrapped the line.
fn normalize_heading(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(section: &Section) -> Vec<BlockKind> {
        section.blocks.iter().map(|block| block.kind).collect()
    }

    fn paths(sections: &[Section]) -> Vec<Vec<String>> {
        sections
            .iter()
            .map(|section| section.heading_path.clone())
            .collect()
    }

    fn only_section(body: &str) -> Section {
        let mut sections = sections(body);
        assert_eq!(sections.len(), 1, "expected one section in {body:?}");
        sections.remove(0)
    }

    #[test]
    fn heading_tree_closes_by_level() {
        let body = "# A\n\na\n\n### C\n\nc\n\n## B\n\nb\n";
        let sections = sections(body);
        assert_eq!(
            paths(&sections),
            vec![
                vec!["A".to_owned()],
                vec!["A".to_owned(), "C".to_owned()],
                vec!["A".to_owned(), "B".to_owned()],
            ]
        );
        assert_eq!(sections[2].blocks.len(), 1, "B keeps its own content only");
    }

    #[test]
    fn content_before_the_first_heading_is_a_preamble_section() {
        let body = "intro\n\n# A\n\nbody\n";
        let sections = sections(body);
        assert_eq!(sections.len(), 2);
        assert!(sections[0].heading_path.is_empty());
        assert_eq!(sections[1].heading_path, vec!["A".to_owned()]);
    }

    #[test]
    fn heading_inside_a_fence_does_not_split() {
        let body = "```\n# not a heading\n```\n\ntail\n";
        let sections = sections(body);
        assert_eq!(sections.len(), 1);
        assert_eq!(
            kinds(&sections[0]),
            vec![BlockKind::CodeBlock, BlockKind::Paragraph]
        );
    }

    #[test]
    fn heading_inside_a_quote_is_not_a_section() {
        let body = "> # quoted\n> body\n\ntail\n";
        let sections = sections(body);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].heading_path.is_empty());
        assert_eq!(
            kinds(&sections[0]),
            vec![BlockKind::BlockQuote, BlockKind::Paragraph]
        );
    }

    #[test]
    fn setext_headings_are_headings() {
        let sections = sections("Title\n=====\n\nbody\n");
        assert_eq!(paths(&sections), vec![vec!["Title".to_owned()]]);

        // A paragraph followed by `---` is a setext H2, not a thematic break.
        let sections = super::sections("Two\n---\n\nbody\n");
        assert_eq!(paths(&sections), vec![vec!["Two".to_owned()]]);
        assert_eq!(kinds(&sections[0]), vec![BlockKind::Paragraph]);
    }

    #[test]
    fn a_rule_after_a_blank_line_is_a_rule() {
        let sections = sections("a\n\n---\n\nb\n");
        assert_eq!(
            kinds(&sections[0]),
            vec![BlockKind::Paragraph, BlockKind::Rule, BlockKind::Paragraph]
        );
    }

    #[test]
    fn tilde_fences_and_indented_code_are_both_code() {
        let sections = sections("~~~\ncode\n~~~\n\n    indented\n");
        assert_eq!(
            kinds(&sections[0]),
            vec![BlockKind::CodeBlock, BlockKind::IndentedCode]
        );
    }

    #[test]
    fn an_unclosed_fence_still_spans_to_eof() {
        let body = "```\nunclosed\n";
        let section = only_section(body);
        assert_eq!(section.blocks[0].span, 0..body.len());
    }

    #[test]
    fn a_table_is_one_block() {
        let body = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let section = only_section(body);
        assert_eq!(kinds(&section), vec![BlockKind::Table]);
        assert_eq!(&body[section.blocks[0].span.clone()], body);
    }

    #[test]
    fn list_items_are_recorded_for_the_outer_list_only() {
        let body = "- a\n  - n1\n  - n2\n- b\n";
        let section = only_section(body);
        assert_eq!(kinds(&section), vec![BlockKind::List]);
        let items = &section.blocks[0].items;
        assert_eq!(items.len(), 2, "nested items belong to their parent item");
        assert_eq!(&body[items[0].clone()], "- a\n  - n1\n  - n2\n");
        assert_eq!(&body[items[1].clone()], "- b\n");
    }

    #[test]
    fn spans_reproduce_their_source_text() {
        let body = "# T\n\ntext\n\n- one\n- two\n\n```rust\nfn main() {}\n```\n\n| a |\n|---|\n| 1 |\n\n> q\n";
        let sections = sections(body);
        for section in &sections {
            for block in &section.blocks {
                assert!(
                    !body[block.span.clone()].trim().is_empty(),
                    "empty span {:?}",
                    block.span
                );
            }
        }
        let blocks: Vec<BlockKind> = sections.iter().flat_map(kinds).collect();
        assert_eq!(
            blocks,
            vec![
                BlockKind::Paragraph,
                BlockKind::List,
                BlockKind::CodeBlock,
                BlockKind::Table,
                BlockKind::BlockQuote,
            ]
        );
    }

    #[test]
    fn spans_are_char_boundaries() {
        let body = "# Überlegungen 🧠\n\n中文 body 🎉\n\n```\n# ünïcode\n```\n";
        let sections = sections(body);
        for section in &sections {
            assert!(body.is_char_boundary(section.span.start));
            assert!(body.is_char_boundary(section.span.end));
            for block in &section.blocks {
                assert!(body.is_char_boundary(block.span.start));
                assert!(body.is_char_boundary(block.span.end));
                for item in &block.items {
                    assert!(body.is_char_boundary(item.start));
                    assert!(body.is_char_boundary(item.end));
                }
            }
        }
    }

    #[test]
    fn empty_bodies_have_no_sections() {
        for body in ["", "\n", "\n\n   \n\t\n"] {
            assert!(sections(body).is_empty(), "{body:?}");
        }
    }

    #[test]
    fn headings_without_their_own_content_produce_no_section() {
        let sections = sections("# A\n\n## B\n\nb\n");
        assert_eq!(paths(&sections), vec![vec!["A".to_owned(), "B".to_owned()]]);
    }

    #[test]
    fn heading_text_flattens_inline_markup() {
        let section = only_section("# `code` and *emph* heading\n\nbody\n");
        assert_eq!(
            section.heading_path,
            vec!["code and emph heading".to_owned()]
        );
    }

    #[test]
    fn block_quotes_are_atomic_and_keep_their_markers() {
        let body = "> a\n> b\n";
        let section = only_section(body);
        assert_eq!(kinds(&section), vec![BlockKind::BlockQuote]);
        assert_eq!(&body[section.blocks[0].span.clone()], "> a\n> b\n");
        assert!(section.blocks[0].kind.is_atomic());
    }

    #[test]
    fn section_text_matches_its_span() {
        let body = "preamble\n\n# A\n\nbody\n";
        let sections = sections(body);
        assert_eq!(sections[0].text(body), "preamble\n");
        assert_eq!(sections[1].text(body), "# A\n\nbody\n");
    }
}
