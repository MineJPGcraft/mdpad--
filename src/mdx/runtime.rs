//! Block-level evaluation of MDX / Markdoc documents.
//!
//! [`expand`] takes the parsed block tree plus a [`Scope`] and returns the blocks
//! that should actually be drawn: Markdoc conditionals are decided, loops are
//! repeated, comments disappear, and known MDX components are rewritten into the
//! editor's native block kinds.
//!
//! ```text
//! parse_document ──▶ [Block] ──expand(scope)──▶ [Block]  (what the view shows)
//! ```
//!
//! # What a resolved block can and cannot say
//!
//! A [`Block`] carries byte *spans* into the document, not text, so expansion may
//! reorder, drop and repeat blocks but must never invent text. That is why
//! `{% if %}` bodies are kept whole and `{% $var %}` cannot be substituted here:
//! the value has no span to point at. Text interpolation is handled by the view
//! layer, which already overlays inline replacements (see the module notes in
//! `edit`), and is deliberately *not* faked by pointing a block at the tag's own
//! source.
//!
//! Expansion never mutates the document: the editor keeps editing the original
//! text, so caret mapping and undo are unaffected.

use std::collections::BTreeMap;

use crate::doc::model::{
    Block, BlockKind, Callout, CalloutSyntax, CalloutVariant, MarkdocTagKind,
};

use super::expr::{Scope, Value, eval};
use super::scope::scope_from_document;

/// Maximum nesting of expanded containers; guards against pathological input.
const MAX_DEPTH: u32 = 32;

/// Result of expanding a document for display.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub blocks: Vec<Block>,
    pub warnings: Vec<String>,
}

/// Expand `blocks` against `scope`, recursively.
pub fn expand(blocks: &[Block], scope: &Scope) -> Resolved {
    let mut out = Resolved::default();
    expand_into(blocks, scope, &mut out, 0);
    out
}

/// Convenience: parse-free entry point for one document text.
///
/// The caller supplies the already parsed blocks (the editor caches them) and the
/// document text, from which the data scope is derived.
pub fn expand_document(blocks: &[Block], text: &str) -> Resolved {
    let (scope, mut warnings) = scope_from_document(text);
    let mut resolved = expand(blocks, &scope);
    warnings.append(&mut resolved.warnings);
    resolved.warnings = warnings;
    resolved
}

fn expand_into(blocks: &[Block], scope: &Scope, out: &mut Resolved, depth: u32) {
    if depth > MAX_DEPTH {
        warn(out, "MDX/Markdoc 嵌套过深，已截断".to_owned());
        return;
    }
    for block in blocks {
        expand_block(block, scope, out, depth);
    }
}

fn expand_block(block: &Block, scope: &Scope, out: &mut Resolved, depth: u32) {
    match &block.kind {
        BlockKind::MarkdocTag {
            kind,
            name,
            attrs,
            body,
            ..
        } => match kind {
            MarkdocTagKind::Comment => {}
            MarkdocTagKind::Conditional => match name.as_str() {
                "if" | "unless" => {
                    let taken = take_branch(attrs, body, name == "unless", scope, out);
                    expand_into(&taken, scope, out, depth + 1);
                }
                // `{% else %}` / `{% elsif %}` outside an `if` are stray markers.
                _ => {}
            },
            MarkdocTagKind::Pair if name == "for" => loop_block(attrs, body, scope, out, depth),
            MarkdocTagKind::Reference => {
                // `$x` is a value (it needs the view layer to place text) and
                // `#partial` is an include of a file that cannot be loaded.
                if name.starts_with('$') {
                    warn(out, format!("{{% {name} %}} 的值需要视图层插值"));
                } else {
                    warn(out, format!("Markdoc 引用 {name} 无法在离线状态解析"));
                }
            }
            // A leaf tag is a custom component: keep its body so the structure
            // still reads, which is what the parser already grouped for pairs.
            MarkdocTagKind::Leaf | MarkdocTagKind::Pair => {
                expand_into(body, scope, out, depth + 1);
                out.blocks.push(block.clone());
            }
        },
        BlockKind::MdxJsx {
            name,
            props,
            self_closing,
            children,
            children_span,
            ..
        } => expand_jsx(
            block,
            name,
            props,
            *self_closing,
            children,
            children_span,
            scope,
            out,
            depth,
        ),
        // A callout is the only other container that holds blocks.
        BlockKind::Callout(callout) => {
            expand_into(&callout.body, scope, out, depth + 1);
            out.blocks.push(block.clone());
        }
        // Front matter is metadata: an MDX/Markdoc renderer does not draw it.
        BlockKind::FrontMatter { .. } => {}
        // Everything else is leaf Markdown content whose text lives in `text`.
        _ => out.blocks.push(block.clone()),
    }
}

/// Pick the branch of an `if` / `unless` chain.
///
/// Branch markers (`{% else /%}`, `{% elsif … /%}`) arrive as their own blocks
/// inside the body, so the chain is split on them.
fn take_branch(
    condition: &str,
    body: &[Block],
    negate: bool,
    scope: &Scope,
    out: &mut Resolved,
) -> Vec<Block> {
    let mut segments: Vec<(Option<&str>, Vec<Block>)> = vec![(Some(condition), Vec::new())];
    for block in body {
        match branch_marker(block) {
            Some(marker) => segments.push((marker, Vec::new())),
            None => {
                if let Some((_, current)) = segments.last_mut() {
                    current.push(block.clone());
                }
            }
        }
    }

    for (cond, blocks) in segments {
        match cond {
            Some(text) => {
                let (value, error) = super::expr::eval_or_null(text, scope);
                if let Some(e) = error {
                    warn(out, format!("条件 {{{text}}} 求值失败: {e}"));
                }
                let truth = value.truthy() ^ negate;
                if truth {
                    return blocks;
                }
            }
            // A bare `else` always matches.
            None => return blocks,
        }
    }
    Vec::new()
}

/// If `block` is `{% else /%}` or `{% elsif … /%}`, return its condition text.
fn branch_marker(block: &Block) -> Option<Option<&str>> {
    let BlockKind::MarkdocTag {
        kind: MarkdocTagKind::Conditional,
        name,
        attrs,
        ..
    } = &block.kind
    else {
        return None;
    };
    match name.as_str() {
        "else" => Some(None),
        "elsif" | "elif" => Some(Some(attrs.as_str())),
        _ => None,
    }
}

/// `{% for item in $items %}` — repeat the body once per element.
fn loop_block(attrs: &str, body: &[Block], scope: &Scope, out: &mut Resolved, depth: u32) {
    let Some((binding, source)) = attrs.split_once(" in ") else {
        warn(out, format!("for 语法无法识别: {attrs}"));
        return;
    };
    let binding = binding.trim();
    let (value, error) = super::expr::eval_or_null(source.trim(), scope);
    if let Some(e) = error {
        warn(out, format!("for 的数据源求值失败: {e}"));
        return;
    }
    let items = match value {
        Value::List(items) => items,
        Value::Object(map) => map.into_values().collect(),
        Value::Null => Vec::new(),
        other => vec![other],
    };
    for item in items {
        let mut inner = scope.clone();
        inner.set(binding, item);
        expand_into(body, &inner, out, depth + 1);
    }
}

/// Rewrite a known MDX component into a native block, or keep its children.
#[allow(clippy::too_many_arguments)]
fn expand_jsx(
    block: &Block,
    name: &str,
    raw_props: &str,
    self_closing: bool,
    children: &[Block],
    children_span: &std::ops::Range<usize>,
    scope: &Scope,
    out: &mut Resolved,
    depth: u32,
) {
    let props = component_props(raw_props, scope, out);

    if let Some(variant) = callout_variant(name, &props) {
        let title = props
            .get("title")
            .or_else(|| props.get("header"))
            .map(|v| v.to_display())
            .filter(|t| !t.trim().is_empty());
        let mut body = Resolved::default();
        expand_into(children, scope, &mut body, depth + 1);
        out.warnings.append(&mut body.warnings);
        out.blocks.push(Block {
            kind: BlockKind::Callout(Callout {
                // The block was written as a component, not as callout syntax;
                // GitHub's shape is the neutral choice when it is rendered.
                syntax: CalloutSyntax::GitHub,
                variant,
                title,
                foldable: None,
                body: body.blocks,
                body_span: children_span.clone(),
            }),
            span: block.span.clone(),
            inline_span: children_span.clone(),
        });
        return;
    }

    // Layout-only components (`<Steps>`, `<Tabs>`, …) contribute their children
    // and no chrome of their own.
    expand_into(children, scope, out, depth + 1);
    if self_closing && name.chars().next().is_some_and(|c| c.is_lowercase()) {
        // Lower-case tags are HTML, not components: keep them as they are so the
        // HTML renderer still sees the element.
        out.blocks.push(block.clone());
    }
}

/// Evaluate a component's attributes: `type="tip"`, `level={page.level}`.
fn component_props(props: &str, scope: &Scope, out: &mut Resolved) -> BTreeMap<String, Value> {
    let mut map = BTreeMap::new();
    for (name, raw) in split_props(props) {
        let value = if let Some(inner) = raw.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
            let (value, error) = super::expr::eval_or_null(inner, scope);
            if let Some(e) = error {
                warn(out, format!("属性 {name} 求值失败: {e}"));
            }
            value
        } else {
            Value::Str(raw.trim_matches('"').trim_matches('\'').to_owned())
        };
        map.insert(name, value);
    }
    map
}

/// Split `a="x" b={y} c` into `(name, raw value)` pairs.
fn split_props(props: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let chars: Vec<char> = props.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ',') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect();
        if i >= chars.len() || chars[i] != '=' {
            if !name.is_empty() {
                out.push((name, "true".to_owned()));
            }
            continue;
        }
        i += 1; // '='
        if i < chars.len() && (chars[i] == '"' || chars[i] == '\'') {
            let quote = chars[i];
            i += 1;
            let value_start = i;
            while i < chars.len() && chars[i] != quote {
                i += 1;
            }
            let value: String = chars[value_start..i].iter().collect();
            i += 1.min(chars.len() - i);
            out.push((name, value));
        } else if i < chars.len() && chars[i] == '{' {
            let value_start = i;
            let mut depth = 0i32;
            while i < chars.len() {
                match chars[i] {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let value: String = chars[value_start..i.min(chars.len())].iter().collect();
            out.push((name, value));
        } else {
            let value_start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            let value: String = chars[value_start..i].iter().collect();
            out.push((name, value));
        }
    }
    out
}

/// Map a component name (or its `type` prop) onto a callout variant.
fn callout_variant(name: &str, props: &BTreeMap<String, Value>) -> Option<CalloutVariant> {
    let hint = props
        .get("type")
        .or_else(|| props.get("variant"))
        .map(|v| v.to_display())
        .unwrap_or_else(|| name.to_owned());
    let lower = hint.to_ascii_lowercase();
    let variant = match lower.as_str() {
        "callout" | "note" => CalloutVariant::Note,
        "tip" | "hint" => CalloutVariant::Tip,
        "info" => CalloutVariant::Info,
        "important" => CalloutVariant::Important,
        "warning" | "warn" => CalloutVariant::Warning,
        "caution" => CalloutVariant::Caution,
        "danger" | "error" => CalloutVariant::Danger,
        "success" | "check" => CalloutVariant::Success,
        "question" | "faq" => CalloutVariant::Question,
        "failure" | "fail" => CalloutVariant::Failure,
        "bug" => CalloutVariant::Bug,
        "example" => CalloutVariant::Example,
        "abstract" | "summary" => CalloutVariant::Abstract,
        _ => return None,
    };
    // Only names that read as callouts get rewritten; `<Note>` does, `<Card>`
    // does not.
    let known_component = matches!(
        lower.as_str(),
        "callout"
            | "note"
            | "tip"
            | "hint"
            | "info"
            | "important"
            | "warning"
            | "warn"
            | "caution"
            | "danger"
            | "error"
            | "success"
            | "check"
            | "question"
            | "failure"
            | "bug"
            | "example"
            | "abstract"
    );
    if known_component { Some(variant) } else { None }
}

fn warn(out: &mut Resolved, message: String) {
    if !out.warnings.iter().any(|w| *w == message) {
        out.warnings.push(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::block::parse_document;
    use crate::doc::model::Flavor;

    fn resolve(text: &str, flavor: Flavor) -> Resolved {
        let blocks = parse_document(text, flavor);
        expand_document(&blocks, text)
    }

    fn kinds(resolved: &Resolved) -> Vec<String> {
        resolved
            .blocks
            .iter()
            .map(|b| crate::doc::model::kind_label(&b.kind).to_owned())
            .collect()
    }

    const MARKDOC: &str = r#"---
draft: false
count: 3
---

{% if $page.draft %}
草稿内容
{% else /%}
正式内容
{% /if %}
"#;

    #[test]
    fn if_else_picks_the_matching_branch() {
        let resolved = resolve(MARKDOC, Flavor::Markdoc);
        // Only the `else` branch survives, so one paragraph remains.
        assert_eq!(kinds(&resolved), vec!["paragraph"]);
        assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    }

    #[test]
    fn if_with_a_true_condition_keeps_the_then_branch() {
        let text = "---\ndraft: true\n---\n\n{% if $page.draft %}\n草稿\n{% else /%}\n正式\n{% /if %}\n";
        let resolved = resolve(text, Flavor::Markdoc);
        assert_eq!(kinds(&resolved), vec!["paragraph"]);
    }

    #[test]
    fn unless_negates_its_condition() {
        let text = "---\ndraft: true\n---\n\n{% unless $page.draft %}\n隐藏\n{% /unless %}\n";
        let resolved = resolve(text, Flavor::Markdoc);
        assert!(resolved.blocks.is_empty(), "{:?}", kinds(&resolved));
    }

    #[test]
    fn for_repeats_the_body_once_per_item() {
        let text = "\n{% for x in [1, 2, 3] %}\n项目\n{% /for %}\n";
        let resolved = resolve(text, Flavor::Markdoc);
        let paragraphs = kinds(&resolved)
            .into_iter()
            .filter(|k| k == "paragraph")
            .count();
        assert_eq!(paragraphs, 3, "{:?}", kinds(&resolved));
    }

    #[test]
    fn for_over_an_empty_list_draws_nothing() {
        let text = "\n{% for x in [] %}\n项目\n{% /for %}\n";
        let resolved = resolve(text, Flavor::Markdoc);
        assert!(resolved.blocks.is_empty(), "{:?}", kinds(&resolved));
    }

    #[test]
    fn comments_disappear() {
        let text = "\n{% comment %}\n不会显示\n{% /comment %}\n\n可见\n";
        let resolved = resolve(text, Flavor::Markdoc);
        assert_eq!(kinds(&resolved), vec!["paragraph"]);
    }

    #[test]
    fn variable_references_are_reported_not_drawn() {
        let text = "\n{% $page.count %}\n";
        let resolved = resolve(text, Flavor::Markdoc);
        assert!(resolved.blocks.is_empty());
        assert!(
            resolved.warnings.iter().any(|w| w.contains("视图层插值")),
            "{:?}",
            resolved.warnings
        );
    }

    #[test]
    fn mdx_callout_components_become_callout_blocks() {
        let text = "<Callout type=\"tip\" title={page.title}>\n提示内容\n</Callout>\n";
        let doc = "---\ntitle: 标题\n---\n\n".to_owned() + text;
        let blocks = parse_document(&doc, Flavor::Mdx);
        let resolved = expand_document(&blocks, &doc);
        let callouts: Vec<&Callout> = resolved
            .blocks
            .iter()
            .filter_map(|b| match &b.kind {
                BlockKind::Callout(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(callouts.len(), 1, "{:?}", kinds(&resolved));
        assert_eq!(callouts[0].variant, CalloutVariant::Tip);
        // The title came from the evaluated `{page.title}` expression.
        assert_eq!(callouts[0].title.as_deref(), Some("标题"));
        assert_eq!(callouts[0].body.len(), 1, "子块应成为 callout 的正文");
    }

    #[test]
    fn unknown_components_keep_their_children() {
        let text = "<Tabs>\n第一页\n</Tabs>\n";
        let blocks = parse_document(text, Flavor::Mdx);
        let resolved = expand_document(&blocks, text);
        assert!(
            kinds(&resolved).iter().any(|k| k == "paragraph"),
            "{:?}",
            kinds(&resolved)
        );
    }

    #[test]
    fn expansion_never_touches_the_source_spans() {
        let text = "\n{% if true %}\n保留\n{% /if %}\n";
        let blocks = parse_document(text, Flavor::Markdoc);
        let before: Vec<_> = blocks.iter().map(|b| b.span.clone()).collect();
        let _ = expand_document(&blocks, text);
        let after: Vec<_> = blocks.iter().map(|b| b.span.clone()).collect();
        assert_eq!(before, after, "展开不得修改输入块");
    }
}
