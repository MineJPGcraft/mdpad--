//! Where an MDX / Markdoc document's data comes from.
//!
//! A document carries its own data in three places, and the runtime has to see
//! all of them:
//!
//! | source | written as | example |
//! |---|---|---|
//! | front matter | YAML between `---` fences | `draft: false` |
//! | MDX module scope | `export const x = …` | `export const year = 2026` |
//! | import binding | `import { meta } from './x'` | resolved as an empty object |
//!
//! Values are parsed with the same subset as expressions, so
//! `export const tags = ['a', 'b']` and `count: 3 + 4` both work. Imports are
//! *not* followed: the editor is offline and a relative path has nothing to load,
//! so an imported name becomes an empty object and the document keeps rendering.

use std::collections::BTreeMap;

use super::expr::{Scope, Value, eval};

/// Build the runtime scope from a document body.
///
/// `text` is the whole document source; only the front-matter fence and the
/// top-level `export` / `import` statements are read, everything else is ignored.
pub fn scope_from_document(text: &str) -> (Scope, Vec<String>) {
    let mut scope = Scope::new();
    let mut warnings = Vec::new();

    // Front matter: `---` on its own line, then YAML-ish key/value lines.
    if let Some((yaml, _)) = front_matter(text) {
        let mut page = BTreeMap::new();
        for line in yaml.lines() {
            let Some((key, value)) = split_yaml_pair(line) else {
                continue;
            };
            let v = yaml_value(&value, &scope);
            page.insert(key, v);
        }
        scope.set("page", Value::Object(page.clone()));
        scope.set("frontmatter", Value::Object(page));
    }

    // `export const x = …` / `export let` / `export default`.
    for value in exports(text) {
        match eval(&value.rhs, &scope) {
            Ok(v) => scope.set(value.name.clone(), v),
            Err(e) => {
                warnings.push(format!("无法求值 export {}: {e}", value.name));
                scope.set(value.name, Value::Null);
            }
        }
    }

    // Imported names exist but hold nothing: nothing can be loaded offline.
    for name in imports(text) {
        if scope.get(&name).is_none() {
            scope.set(name.clone(), Value::Object(BTreeMap::new()));
            warnings.push(format!("import {name} 未解析（离线下不加载模块），按空对象处理"));
        }
    }

    (scope, warnings)
}

/// The YAML block between the leading `---` fences, plus its byte range.
pub fn front_matter(text: &str) -> Option<(String, std::ops::Range<usize>)> {
    let trimmed_start = text.len() - text.trim_start().len();
    let body = &text[trimmed_start..];
    if !body.starts_with("---") {
        return None;
    }
    let after_first = &body[3..];
    let close = after_first.find("\n---")?;
    let yaml = after_first[..close].trim_start_matches(['\r', '\n']).to_owned();
    // Point the range at the closing fence line itself.
    let start = (trimmed_start + 3 + close + 1).min(text.len());
    let end = (start + 3).min(text.len());
    Some((yaml, start..end))
}

/// Interpret a front-matter value.
///
/// YAML is not JavaScript: `title: 示例` is the literal text `示例`, not an
/// identifier. Only values that look like literals (numbers, quoted strings,
/// arrays, objects, booleans, `$vars`) go through the expression evaluator.
fn yaml_value(raw: &str, scope: &Scope) -> Value {
    let t = raw.trim();
    if t.is_empty() {
        return Value::Str(String::new());
    }
    let quoted = t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')));
    if quoted {
        return Value::Str(unquote(t));
    }
    let first = t.chars().next().unwrap_or(' ');
    let looks_like_literal = matches!(first, '[' | '{' | '$')
        || t == "true"
        || t == "false"
        || t == "null"
        || first.is_ascii_digit()
        || (first == '-' && t[1..].starts_with(|c: char| c.is_ascii_digit()));
    if looks_like_literal
        && let Ok(v) = eval(t, scope)
    {
        return v;
    }
    Value::Str(t.to_owned())
}

/// Split `key: value` (also `key : value`), ignoring list items and comments.
fn split_yaml_pair(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
        return None;
    }
    let (key, value) = line.split_once(':')?;
    let key = key.trim().to_owned();
    if key.is_empty() {
        return None;
    }
    let value = value.trim();
    let value = match value.find(" #") {
        Some(i) => value[..i].trim(),
        None => value,
    };
    Some((key, value.to_owned()))
}

/// Strip a pair of matching quotes.
fn unquote(text: &str) -> String {
    let t = text.trim();
    for q in ['"', '\''] {
        if t.len() >= 2 && t.starts_with(q) && t.ends_with(q) {
            return t[1..t.len() - 1].to_owned();
        }
    }
    t.to_owned()
}

/// One `export const name = rhs` binding.
struct Export {
    name: String,
    rhs: String,
}

/// Collect top-level `export` statements, honouring braces and strings.
fn exports(text: &str) -> Vec<Export> {
    let mut out = Vec::new();
    for stmt in statements(text, "export") {
        let rest = stmt.trim_start_matches("export").trim();
        // `export default function …` / `export { a, b }` have no simple value.
        if rest.starts_with('{') || rest.starts_with('*') {
            continue;
        }
        let rest = rest
            .strip_prefix("const")
            .or_else(|| rest.strip_prefix("let"))
            .or_else(|| rest.strip_prefix("var"))
            .unwrap_or(rest)
            .trim();
        let Some((name, rhs)) = rest.split_once('=') else {
            continue;
        };
        let name = name.trim().trim_end_matches(':').trim().to_owned();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') {
            continue;
        }
        // A trailing `;` is not part of the value.
        let rhs = rhs.trim().trim_end_matches(';').trim().to_owned();
        if !rhs.is_empty() {
            out.push(Export { name, rhs });
        }
    }
    out
}

/// Names brought in by `import … from '…'`.
fn imports(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for stmt in statements(text, "import") {
        let rest = stmt.trim_start_matches("import").trim();
        if rest.starts_with('(') || rest.starts_with("type ") {
            continue;
        }
        let binding = match rest.find(" from ") {
            Some(i) => &rest[..i],
            None => continue,
        };
        let binding = binding.trim();
        if let Some(inner) = binding.strip_prefix('{') {
            for part in inner.trim_end_matches('}').split(',') {
                let name = part.trim().split_once(" as ").map(|(_, b)| b).unwrap_or(part);
                let name = name.trim();
                if !name.is_empty() {
                    out.push(name.to_owned());
                }
            }
        } else if let Some((default_name, rest)) = binding.split_once(',') {
            let default_name = default_name.trim();
            if !default_name.is_empty() {
                out.push(default_name.to_owned());
            }
            if let Some(inner) = rest.trim().trim_start().strip_prefix('{') {
                for part in inner.trim_end_matches('}').split(',') {
                    let name = part.trim().split_once(" as ").map(|(_, b)| b).unwrap_or(part);
                    let name = name.trim();
                    if !name.is_empty() {
                        out.push(name.to_owned());
                    }
                }
            }
        } else if !binding.is_empty() {
            out.push(binding.trim_matches('*').trim().to_owned());
        }
    }
    out
}

/// Whole-line statements starting with `keyword`, with continuation lines joined.
fn statements<'a>(text: &'a str, keyword: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        let starts = trimmed.starts_with(keyword)
            && trimmed[keyword.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace());
        if starts {
            if let Some(done) = current.take() {
                out.push(done);
            }
            current = Some(trimmed.to_owned());
            if balanced(trimmed) {
                if let Some(done) = current.take() {
                    out.push(done);
                }
            }
            continue;
        }
        if let Some(buf) = current.as_mut() {
            buf.push(' ');
            buf.push_str(trimmed);
            if balanced(buf) {
                if let Some(done) = current.take() {
                    out.push(done);
                }
            }
        }
    }
    if let Some(done) = current {
        out.push(done);
    }
    out
}

/// `true` when brackets and quotes are balanced (a statement can be closed).
fn balanced(text: &str) -> bool {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            },
        }
    }
    depth <= 0 && quote.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"---
title: 示例
draft: false
views: 42
tags: ['a', 'b']
---

import { meta } from './meta'

export const year = 2026
export const total = 2 + 3

正文内容。
"#;

    #[test]
    fn front_matter_feeds_the_page_scope() {
        let (scope, _) = scope_from_document(DOC);
        let page = scope.get("page").cloned().expect("page");
        assert_eq!(page.member("title"), Value::str("示例"));
        assert_eq!(page.member("draft"), Value::Bool(false));
        assert_eq!(page.member("views"), Value::Number(42.0));
        assert_eq!(
            page.member("tags"),
            Value::List(vec![Value::str("a"), Value::str("b")])
        );
        // `frontmatter` is an alias for the same object.
        assert_eq!(
            scope.get("frontmatter").cloned().expect("fm").member("views"),
            Value::Number(42.0)
        );
    }

    #[test]
    fn export_const_bindings_are_evaluated() {
        let (scope, warnings) = scope_from_document(DOC);
        assert_eq!(scope.get("year").cloned(), Some(Value::Number(2026.0)));
        assert_eq!(scope.get("total").cloned(), Some(Value::Number(5.0)));
        // The unresolved import is reported but still present as an object.
        assert_eq!(
            scope.get("meta").cloned(),
            Some(Value::Object(BTreeMap::new()))
        );
        assert!(warnings.iter().any(|w| w.contains("import meta")));
    }

    #[test]
    fn documents_without_metadata_still_work() {
        let (scope, warnings) = scope_from_document("# 纯正文\n\n没有元数据。");
        assert!(scope.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn multi_line_exports_are_joined() {
        let doc = "export const big = {\n  a: 1,\n  b: 'x'\n}\n";
        let (scope, _) = scope_from_document(doc);
        let big = scope.get("big").cloned().expect("big");
        assert_eq!(big.member("a"), Value::Number(1.0));
        assert_eq!(big.member("b"), Value::str("x"));
    }

    #[test]
    fn front_matter_range_points_at_the_closing_fence() {
        let (_, range) = front_matter(DOC).expect("front matter");
        let closing = &DOC[range.start..range.end];
        assert!(closing.starts_with("---"), "{closing:?}");
    }
}
