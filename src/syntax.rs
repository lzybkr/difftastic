//! Syntax tree definitions with change metadata.

#![allow(clippy::mutable_key_type)] // Hash for Syntax doesn't use mutable fields.

use lazy_static::lazy_static;
use regex::Regex;
use std::{
    cell::Cell,
    collections::HashMap,
    env, fmt,
    hash::{Hash, Hasher},
};
use typed_arena::Arena;

use crate::{
    lines::{LineNumber, NewlinePositions},
    positions::SingleLineSpan,
};
use ChangeKind::*;
use Syntax::*;

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ChangeKind<'a> {
    Unchanged(&'a Syntax<'a>),
    ReplacedComment(&'a Syntax<'a>, &'a Syntax<'a>),
    Novel,
}

/// A Debug implementation that ignores the corresponding node
/// mentioned for Unchanged. Otherwise we will infinitely loop on
/// unchanged nodes, which both point to the other.
impl<'a> fmt::Debug for ChangeKind<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let desc = match self {
            Unchanged(_) => "Unchanged",
            ReplacedComment(_, _) => "ReplacedComment",
            Novel => "Novel",
        };
        f.write_str(desc)
    }
}

/// Fields that are common to both `Syntax::List` and `Syntax::Atom`.
pub struct SyntaxInfo<'a> {
    next: Cell<Option<&'a Syntax<'a>>>,
    prev: Cell<Option<&'a Syntax<'a>>>,
    parent: Cell<Option<&'a Syntax<'a>>>,
    prev_is_contiguous: Cell<bool>,
    change: Cell<Option<ChangeKind<'a>>>,
    num_ancestors: Cell<u32>,
    unique_id: Cell<u32>,
    content_id: Cell<u32>,
}

impl<'a> SyntaxInfo<'a> {
    pub fn new() -> Self {
        Self {
            next: Cell::new(None),
            prev: Cell::new(None),
            parent: Cell::new(None),
            prev_is_contiguous: Cell::new(false),
            change: Cell::new(None),
            num_ancestors: Cell::new(0),
            unique_id: Cell::new(0),
            content_id: Cell::new(0),
        }
    }
}

impl<'a> Default for SyntaxInfo<'a> {
    fn default() -> Self {
        Self::new()
    }
}

pub enum Syntax<'a> {
    List {
        info: SyntaxInfo<'a>,
        open_position: Vec<SingleLineSpan>,
        open_content: String,
        children: Vec<&'a Syntax<'a>>,
        close_position: Vec<SingleLineSpan>,
        close_content: String,
        num_descendants: u32,
    },
    Atom {
        info: SyntaxInfo<'a>,
        position: Vec<SingleLineSpan>,
        content: String,
        kind: AtomKind,
    },
}

fn dbg_pos(pos: &[SingleLineSpan]) -> String {
    match pos {
        [] => "-".into(),
        [pos] => format!("{}:{}-{}", pos.line.0, pos.start_col, pos.end_col),
        [start, .., end] => format!(
            "{}:{}-{}:{}",
            start.line.0, start.start_col, end.line.0, end.end_col
        ),
    }
}

impl<'a> fmt::Debug for Syntax<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            List {
                open_content,
                open_position,
                children,
                close_content,
                close_position,
                info,
                ..
            } => {
                let mut ds = f.debug_struct(&format!(
                    "List id:{} content:{}",
                    self.id(),
                    self.content_id()
                ));

                ds.field("open_content", &open_content)
                    .field("open_position", &dbg_pos(open_position))
                    .field("children", &children)
                    .field("close_content", &close_content)
                    .field("close_position", &dbg_pos(close_position));

                if env::var("DFT_VERBOSE").is_ok() {
                    ds.field("change", &info.change.get());

                    let next_s = match info.next.get() {
                        Some(List { .. }) => "Some(List)",
                        Some(Atom { .. }) => "Some(Atom)",
                        None => "None",
                    };
                    ds.field("next", &next_s);
                }

                ds.finish()
            }
            Atom {
                content,
                position,
                info,
                kind: highlight,
                ..
            } => {
                let mut ds = f.debug_struct(&format!(
                    "Atom id:{} content:{}",
                    self.id(),
                    self.content_id()
                ));
                ds.field("content", &content);
                ds.field("position", &dbg_pos(position));

                if env::var("DFT_VERBOSE").is_ok() {
                    ds.field("highlight", highlight);
                    ds.field("change", &info.change.get());
                    let next_s = match info.next.get() {
                        Some(List { .. }) => "Some(List)",
                        Some(Atom { .. }) => "Some(Atom)",
                        None => "None",
                    };
                    ds.field("next", &next_s);
                }

                ds.finish()
            }
        }
    }
}

impl<'a> Syntax<'a> {
    pub fn new_list(
        arena: &'a Arena<Syntax<'a>>,
        open_content: &str,
        open_position: Vec<SingleLineSpan>,
        children: Vec<&'a Syntax<'a>>,
        close_content: &str,
        close_position: Vec<SingleLineSpan>,
    ) -> &'a Syntax<'a> {
        let mut num_descendants = 0;
        for child in &children {
            num_descendants += match child {
                List {
                    num_descendants, ..
                } => *num_descendants + 1,
                Atom { .. } => 1,
            };
        }

        arena.alloc(List {
            info: SyntaxInfo::default(),
            open_position,
            open_content: open_content.into(),
            close_content: close_content.into(),
            close_position,
            children,
            num_descendants,
        })
    }

    pub fn new_atom(
        arena: &'a Arena<Syntax<'a>>,
        position: Vec<SingleLineSpan>,
        content: &str,
        kind: AtomKind,
    ) -> &'a Syntax<'a> {
        arena.alloc(Atom {
            info: SyntaxInfo::default(),
            position,
            content: content.into(),
            kind,
        })
    }

    pub fn info(&self) -> &SyntaxInfo<'a> {
        match self {
            List { info, .. } | Atom { info, .. } => info,
        }
    }

    pub fn next(&self) -> Option<&'a Syntax<'a>> {
        self.info().next.get()
    }

    pub fn prev_is_contiguous(&self) -> bool {
        self.info().prev_is_contiguous.get()
    }

    /// A unique ID of this syntax node. Every node is guaranteed to
    /// have a different value.
    pub fn id(&self) -> u32 {
        self.info().unique_id.get()
    }

    /// A content ID of this syntax node. Two nodes have the same
    /// content ID if they have the same content, regardless of
    /// position.
    fn content_id(&self) -> u32 {
        self.info().content_id.get()
    }

    pub fn num_ancestors(&self) -> u32 {
        self.info().num_ancestors.get()
    }

    pub fn first_line(&self) -> Option<LineNumber> {
        let position = match self {
            List { open_position, .. } => open_position,
            Atom { position, .. } => position,
        };
        position.first().map(|lp| lp.line)
    }

    pub fn last_line(&self) -> Option<LineNumber> {
        let position = match self {
            List { close_position, .. } => close_position,
            Atom { position, .. } => position,
        };
        position.last().map(|lp| lp.line)
    }

    pub fn set_change(&self, ck: ChangeKind<'a>) {
        self.info().change.set(Some(ck));
    }

    pub fn set_change_deep(&self, ck: ChangeKind<'a>) {
        self.set_change(ck);

        if let List { children, .. } = self {
            // For unchanged lists, match up children with the
            // unchanged children on the other side.
            if let Unchanged(List {
                children: other_children,
                ..
            }) = ck
            {
                for (child, other_child) in children.iter().zip(other_children) {
                    child.set_change_deep(Unchanged(other_child));
                }
            } else {
                for child in children {
                    child.set_change_deep(ck);
                }
            };
        }
    }
}

/// Initialise all the fields in `SyntaxInfo`.
pub fn init_info<'a>(lhs_roots: &[&'a Syntax<'a>], rhs_roots: &[&'a Syntax<'a>]) {
    let mut id = 0;
    init_info_single(lhs_roots, &mut id);
    init_info_single(rhs_roots, &mut id);

    let mut existing = HashMap::new();
    set_content_id(lhs_roots, &mut existing);
    set_content_id(rhs_roots, &mut existing);
}

type ContentKey = (
    Option<String>,
    Option<String>,
    Vec<u32>,
    bool,
    Option<AtomKind>,
);

fn set_content_id<'a>(nodes: &[&'a Syntax<'a>], existing: &mut HashMap<ContentKey, u32>) {
    for node in nodes {
        let key: ContentKey = match node {
            List {
                open_content,
                close_content,
                children,
                ..
            } => {
                // Recurse first, so children all have their content_id set.
                set_content_id(children, existing);

                let children_content_ids: Vec<_> =
                    children.iter().map(|c| c.info().content_id.get()).collect();

                (
                    Some(open_content.clone()),
                    Some(close_content.clone()),
                    children_content_ids,
                    true,
                    None,
                )
            }
            Atom {
                content,
                kind: highlight,
                ..
            } => {
                let clean_content =
                    if *highlight == AtomKind::Comment && content.lines().count() > 1 {
                        content
                            .lines()
                            .map(|l| l.trim_start())
                            .collect::<Vec<_>>()
                            .join("\n")
                            .to_string()
                    } else {
                        content.clone()
                    };
                (Some(clean_content), None, vec![], false, Some(*highlight))
            }
        };

        // Ensure the ID is always greater than zero, so we can
        // distinguish an uninitialised SyntaxInfo value.
        let next_id = existing.len() as u32 + 1;
        let content_id = existing.entry(key).or_insert(next_id);
        node.info().content_id.set(*content_id);
    }
}

fn init_info_single<'a>(roots: &[&'a Syntax<'a>], next_id: &mut u32) {
    set_next(roots, None);
    set_prev(roots, None);
    set_parent(roots, None);
    set_num_ancestors(roots, 0);
    set_prev_is_contiguous(roots);
    set_unique_id(roots, next_id)
}

fn set_unique_id<'a>(nodes: &[&'a Syntax<'a>], next_id: &mut u32) {
    for node in nodes {
        node.info().unique_id.set(*next_id);
        *next_id += 1;
        if let List { children, .. } = node {
            set_unique_id(children, next_id);
        }
    }
}

/// For every syntax node in the tree, mark the next node according to
/// a preorder traversal.
fn set_next<'a>(nodes: &[&'a Syntax<'a>], parent_next: Option<&'a Syntax<'a>>) {
    for (i, node) in nodes.iter().enumerate() {
        let node_next = match nodes.get(i + 1) {
            Some(node_next) => Some(*node_next),
            None => parent_next,
        };

        node.info().next.set(node_next);
        if let List { children, .. } = node {
            set_next(children, node_next);
        }
    }
}

/// For every syntax node in the tree, mark the previous node
/// according to a preorder traversal.
fn set_prev<'a>(nodes: &[&'a Syntax<'a>], parent: Option<&'a Syntax<'a>>) {
    for (i, node) in nodes.iter().enumerate() {
        let node_prev = if i == 0 { parent } else { Some(nodes[i - 1]) };

        node.info().prev.set(node_prev);
        if let List { children, .. } = node {
            set_prev(children, Some(node));
        }
    }
}

fn set_parent<'a>(nodes: &[&'a Syntax<'a>], parent: Option<&'a Syntax<'a>>) {
    for node in nodes {
        node.info().parent.set(parent);
        if let List { children, .. } = node {
            set_parent(children, Some(node));
        }
    }
}

fn set_num_ancestors<'a>(nodes: &[&Syntax<'a>], num_ancestors: u32) {
    for node in nodes {
        node.info().num_ancestors.set(num_ancestors);

        if let List { children, .. } = node {
            set_num_ancestors(children, num_ancestors + 1);
        }
    }
}

fn set_prev_is_contiguous<'a>(roots: &[&Syntax<'a>]) {
    for node in roots {
        let is_contiguous = if let Some(prev) = node.info().prev.get() {
            match prev {
                List {
                    open_position,
                    close_position,
                    ..
                } => {
                    let prev_is_parent = prev.num_ancestors() < node.num_ancestors();
                    if prev_is_parent {
                        open_position.last().map(|p| p.line) == node.first_line()
                    } else {
                        // predecessor node at the same level.
                        close_position.last().map(|p| p.line) == node.first_line()
                    }
                }
                Atom { .. } => prev.last_line() == node.first_line(),
            }
        } else {
            false
        };
        node.info().prev_is_contiguous.set(is_contiguous);
        if let List { children, .. } = node {
            set_prev_is_contiguous(children);
        }
    }
}

impl<'a> PartialEq for Syntax<'a> {
    fn eq(&self, other: &Self) -> bool {
        debug_assert!(self.content_id() > 0);
        debug_assert!(other.content_id() > 0);
        self.content_id() == other.content_id()
    }
}
impl<'a> Eq for Syntax<'a> {}

impl<'a> Hash for Syntax<'a> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.content_id().hash(state);
    }
}

#[derive(PartialEq, Eq, Debug, Clone, Copy, Hash)]
pub enum AtomKind {
    Normal,
    Comment,
    Keyword,
}

/// Unlike atoms, tokens can be delimiters like `{`.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum TokenKind {
    Delimiter,
    Atom(AtomKind),
}

/// A matched token (an atom, a delimiter, or a comment word).
#[derive(PartialEq, Eq, Debug, Clone)]
pub enum MatchKind {
    Unchanged {
        highlight: TokenKind,
        self_pos: (Vec<SingleLineSpan>, Vec<SingleLineSpan>),
        // If it's a matched atom, only use the first vec. If it'a
        // matched delimiter pair, use both vecs.
        // TODO: better data type.
        opposite_pos: (Vec<SingleLineSpan>, Vec<SingleLineSpan>),
    },
    Novel {
        highlight: TokenKind,
    },
    UnchangedCommentPart {
        self_pos: SingleLineSpan,
        opposite_pos: Vec<SingleLineSpan>,
    },
    ChangedCommentPart {},
}

impl MatchKind {
    pub fn first_opposite_span(&self) -> Option<SingleLineSpan> {
        match self {
            MatchKind::Unchanged { opposite_pos, .. } => opposite_pos.0.first().copied(),
            MatchKind::UnchangedCommentPart { opposite_pos, .. } => opposite_pos.first().copied(),
            MatchKind::Novel { .. } => None,
            MatchKind::ChangedCommentPart {} => None,
        }
    }

    pub fn is_change(&self) -> bool {
        matches!(
            self,
            MatchKind::Novel { .. } | MatchKind::ChangedCommentPart {}
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedPos {
    pub kind: MatchKind,
    pub pos: SingleLineSpan,
}

// "foo bar" -> vec!["foo", " ", "bar"]
fn split_words(s: &str) -> Vec<String> {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"[a-zA-Z0-9]+|\n|[^a-zA-Z0-9\n]").unwrap();
    }

    RE.find_iter(s).map(|m| m.as_str().to_owned()).collect()
}

fn split_comment_words(
    content: &str,
    pos: SingleLineSpan,
    opposite_content: &str,
    opposite_pos: SingleLineSpan,
) -> Vec<MatchedPos> {
    // TODO: merge adjacent single-line comments unless there are
    // blank lines between them.
    let content_parts = split_words(content);
    let other_parts = split_words(opposite_content);

    let content_newlines = NewlinePositions::from(content);
    let opposite_content_newlines = NewlinePositions::from(opposite_content);

    let mut offset = 0;
    let mut opposite_offset = 0;

    let mut res = vec![];
    for diff_res in diff::slice(&content_parts, &other_parts) {
        match diff_res {
            diff::Result::Left(word) => {
                // This word is novel to this side.
                res.push(MatchedPos {
                    kind: MatchKind::ChangedCommentPart {},
                    pos: content_newlines.from_offsets_relative_to(
                        pos,
                        offset,
                        offset + word.len(),
                    )[0],
                });
                offset += word.len();
            }
            diff::Result::Both(word, opposite_word) => {
                // This word is present on both sides.
                let word_pos =
                    content_newlines.from_offsets_relative_to(pos, offset, offset + word.len())[0];
                let opposite_word_pos = opposite_content_newlines.from_offsets_relative_to(
                    opposite_pos,
                    opposite_offset,
                    opposite_offset + opposite_word.len(),
                );

                res.push(MatchedPos {
                    kind: MatchKind::UnchangedCommentPart {
                        self_pos: word_pos,
                        opposite_pos: opposite_word_pos,
                    },
                    pos: word_pos,
                });
                offset += word.len();
                opposite_offset += opposite_word.len();
            }
            diff::Result::Right(opposite_word) => {
                // Only exists on other side, nothing to do on this side.
                opposite_offset += opposite_word.len();
            }
        }
    }

    res
}

impl MatchedPos {
    fn new(
        ck: ChangeKind,
        highlight: TokenKind,
        pos: (&[SingleLineSpan], &[SingleLineSpan]),
    ) -> Vec<Self> {
        let kind = match ck {
            ReplacedComment(this, opposite) => {
                let this_content = match this {
                    List { .. } => unreachable!(),
                    Atom { content, .. } => content,
                };
                let (opposite_content, opposite_pos) = match opposite {
                    List { .. } => unreachable!(),
                    Atom {
                        content, position, ..
                    } => (content, position),
                };

                return split_comment_words(
                    this_content,
                    // TODO: handle the whole pos.0 here.
                    pos.0[0],
                    opposite_content,
                    opposite_pos[0],
                );
            }
            Unchanged(opposite) => {
                let opposite_pos = match opposite {
                    List {
                        open_position,
                        close_position,
                        ..
                    } => (open_position.clone(), close_position.clone()),
                    Atom { position, .. } => (position.clone(), position.clone()),
                };

                MatchKind::Unchanged {
                    highlight,
                    self_pos: (pos.0.to_vec(), pos.1.to_vec()),
                    opposite_pos,
                }
            }
            Novel => MatchKind::Novel { highlight },
        };

        // Create a MatchedPos for every line that `pos` covers.
        // TODO: what about opposite pos?
        let mut res = vec![];
        for line_pos in pos.0 {
            // Don't create a MatchedPos for empty positions. This
            // occurs when we have lists with empty open/close
            // delimiter positions, such as the top-level list of syntax items.
            if line_pos.start_col == line_pos.end_col {
                continue;
            }

            res.push(Self {
                kind: kind.clone(),
                pos: *line_pos,
            });
        }

        res
    }
}

/// Walk `nodes` and return a vec of all the changed positions.
pub fn change_positions<'a>(
    src: &str,
    opposite_src: &str,
    nodes: &[&Syntax<'a>],
) -> Vec<MatchedPos> {
    let nl_pos = NewlinePositions::from(src);
    let opposite_nl_pos = NewlinePositions::from(opposite_src);

    let mut positions = Vec::new();
    change_positions_(&nl_pos, &opposite_nl_pos, nodes, &mut positions);
    positions
}

fn change_positions_<'a>(
    nl_pos: &NewlinePositions,
    opposite_nl_pos: &NewlinePositions,
    nodes: &[&Syntax<'a>],
    positions: &mut Vec<MatchedPos>,
) {
    for node in nodes {
        match node {
            List {
                info,
                open_position,
                children,
                close_position,
                ..
            } => {
                let change = info
                    .change
                    .get()
                    .unwrap_or_else(|| panic!("Should have changes set in all nodes: {:#?}", node));

                positions.extend(MatchedPos::new(
                    change,
                    TokenKind::Delimiter,
                    (open_position, close_position),
                ));

                change_positions_(nl_pos, opposite_nl_pos, children, positions);

                positions.extend(MatchedPos::new(
                    change,
                    TokenKind::Delimiter,
                    // TODO: use open position here too (currently
                    // breaks display).
                    (close_position, close_position),
                ));
            }
            Atom {
                info,
                position,
                kind,
                ..
            } => {
                let change = info
                    .change
                    .get()
                    .unwrap_or_else(|| panic!("Should have changes set in all nodes: {:#?}", node));
                positions.extend(MatchedPos::new(
                    change,
                    TokenKind::Atom(*kind),
                    (position, &[]),
                ));
            }
        }
    }
}

pub fn zip_pad_shorter<Tx: Clone, Ty: Clone>(
    lhs: &[Tx],
    rhs: &[Ty],
) -> Vec<(Option<Tx>, Option<Ty>)> {
    let mut res = vec![];

    let mut i = 0;
    loop {
        match (lhs.get(i), rhs.get(i)) {
            (None, None) => break,
            (x, y) => res.push((x.cloned(), y.cloned())),
        }

        i += 1;
    }

    res
}

/// Zip `lhs` with `rhs`, but repeat the last item from the shorter
/// slice.
pub fn zip_repeat_shorter<Tx: Clone, Ty: Clone>(lhs: &[Tx], rhs: &[Ty]) -> Vec<(Tx, Ty)> {
    let lhs_last: Tx = match lhs.last() {
        Some(last) => last.clone(),
        None => return vec![],
    };
    let rhs_last: Ty = match rhs.last() {
        Some(last) => last.clone(),
        None => return vec![],
    };

    let mut res = vec![];

    let mut i = 0;
    loop {
        match (lhs.get(i), rhs.get(i)) {
            (None, None) => break,
            (x, y) => res.push((
                x.cloned().unwrap_or_else(|| lhs_last.clone()),
                y.cloned().unwrap_or_else(|| rhs_last.clone()),
            )),
        }

        i += 1;
    }

    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    impl<'a> Syntax<'a> {
        pub fn change(&'a self) -> Option<ChangeKind<'a>> {
            self.info().change.get()
        }
    }

    #[test]
    fn test_comment_and_atom_differ() {
        let pos = vec![SingleLineSpan {
            line: 0.into(),
            start_col: 2,
            end_col: 3,
        }];

        let arena = Arena::new();

        let comment = Syntax::new_atom(&arena, pos.clone(), "foo", AtomKind::Comment);
        let atom = Syntax::new_atom(&arena, pos, "foo", AtomKind::Normal);
        init_info(&[comment], &[atom]);

        assert_ne!(comment, atom);
    }

    #[test]
    fn test_multiline_comment_ignores_leading_whitespace() {
        let pos = vec![SingleLineSpan {
            line: 0.into(),
            start_col: 2,
            end_col: 3,
        }];

        let arena = Arena::new();

        let x = Syntax::new_atom(&arena, pos.clone(), "foo\nbar", AtomKind::Comment);
        let y = Syntax::new_atom(&arena, pos, "foo\n    bar", AtomKind::Comment);
        init_info(&[x], &[y]);

        assert_eq!(x, y);
    }

    #[test]
    fn test_atom_equality_ignores_change() {
        let lhs = Atom {
            info: SyntaxInfo {
                change: Cell::new(Some(Novel)),
                ..SyntaxInfo::default()
            },

            position: vec![SingleLineSpan {
                line: 1.into(),
                start_col: 2,
                end_col: 3,
            }],
            content: "foo".into(),
            kind: AtomKind::Normal,
        };
        let rhs = Atom {
            info: SyntaxInfo {
                change: Cell::new(None),
                ..SyntaxInfo::default()
            },
            position: vec![SingleLineSpan {
                line: 1.into(),
                start_col: 2,
                end_col: 3,
            }],
            content: "foo".into(),
            kind: AtomKind::Normal,
        };
        init_info(&[&lhs], &[&rhs]);

        assert_eq!(lhs, rhs);
    }

    #[test]
    fn test_split_comment_words_basic() {
        let content = "abc";
        let pos = SingleLineSpan {
            line: 0.into(),
            start_col: 0,
            end_col: 3,
        };

        let opposite_content = "def";
        let opposite_pos = SingleLineSpan {
            line: 0.into(),
            start_col: 0,
            end_col: 3,
        };

        let res = split_comment_words(content, pos, opposite_content, opposite_pos);
        assert_eq!(
            res,
            vec![MatchedPos {
                kind: MatchKind::ChangedCommentPart {},
                pos: SingleLineSpan {
                    line: 0.into(),
                    start_col: 0,
                    end_col: 3
                },
            },]
        );
    }

    #[test]
    fn test_split_words() {
        let s = "example.com";
        let res = split_words(s);
        assert_eq!(res, vec!["example", ".", "com"])
    }

    #[test]
    fn test_split_words_punctuations() {
        let s = "example..";
        let res = split_words(s);
        assert_eq!(res, vec!["example", ".", "."])
    }

    #[test]
    fn test_split_words_treats_newline_separately() {
        let s = "example.\ncom";
        let res = split_words(s);
        assert_eq!(res, vec!["example", ".", "\n", "com"])
    }
}
