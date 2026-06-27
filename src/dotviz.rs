//! The DOT (Graphviz) side of the conversion.
//!
//! [`DotData`] owns a fully-resolved [`canonical::Graph`]: `dot-parser` has
//! already flattened subgraphs and chained edges, and we launder every borrow
//! into the source text into owned `String`s so the value is self-contained.

use std::collections::{HashMap, HashSet};
use std::fmt;

use dot_parser::{ast, canonical};

use crate::error::DotGraffleError;
use crate::graffle::{GCluster, GEdge, GModel, GNode, GraffleData};

/// A single DOT attribute (`key = value`). Carrying our own type rather than a
/// bare `(String, String)` tuple lets `canonical::Graph<Attr>` implement
/// `Display` (its blanket impl requires `A: Display`), which is how we render
/// back to DOT text in the graffle -> dot direction.
#[derive(Clone, Debug)]
pub(crate) struct Attr {
    pub(crate) key: String,
    pub(crate) value: String,
}

impl fmt::Display for Attr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `{:?}` quotes and escapes the value — valid DOT attribute syntax.
        write!(f, "{}={:?}", self.key, self.value)
    }
}

pub(crate) struct DotData {
    pub(crate) graph: canonical::Graph<Attr>,
    /// The original DOT source — handed to graphviz for layout (clusters
    /// intact), which the flattened `graph` no longer carries.
    pub(crate) source: String,
}

impl TryFrom<Vec<u8>> for DotData {
    type Error = DotGraffleError;

    /// Parse DOT text into an owned canonical graph.
    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        let text = String::from_utf8(bytes)?;
        // `dot-parser` rejects Graphviz's HTML-like label syntax (`label=<...>`),
        // so we hand it a copy with those values blanked to `""`. Graphviz still
        // receives the original `text` and renders the HTML; we recover the text
        // from its layout (`_ldraw_` runs). `try_from(&str)` parses attributes as
        // `(ID<'a>, ID<'a>)` — borrows of `sanitized` — so we canonicalize and
        // map each borrowed `&str` into an owned `String` below, releasing it.
        let sanitized = sanitize_html_labels(&text);
        let parsed = ast::Graph::try_from(sanitized.as_str()).map_err(|e| DotGraffleError::DotParse(e.to_string()))?;

        // dot-parser's canonical `NodeSet` keeps only the *last* statement for a
        // given node id (its `From` collects into a `HashMap`), so a bare
        // re-reference — the `{ rank=sink; n; }` layout idiom, or any later
        // `n;` — erases the attributes declared earlier. Graphviz instead
        // accumulates attributes across statements, so we fold every node
        // statement into a per-id set up front and reapply it after
        // canonicalization to restore what would otherwise be dropped.
        let mut node_attrs: HashMap<String, Vec<Attr>> = HashMap::new();
        collect_node_attrs(&parsed.stmts, &mut node_attrs);

        let mut graph: canonical::Graph<Attr> = canonical::Graph::from(parsed).filter_map(|(k, v)| {
            Some(Attr {
                key: k.into(),
                value: v.into(),
            })
        });
        for (id, attrs) in node_attrs {
            if let Some(node) = graph.nodes.set.get_mut(&id) {
                node.attr = ast::AList { elems: attrs };
            }
        }

        Ok(DotData { graph, source: text })
    }
}

/// Replace Graphviz HTML-like label strings (`<...>`) with an empty quoted
/// string, so `dot-parser` — which can't parse `<...>` — accepts the structure.
/// Graphviz itself gets the untouched source and renders the HTML; the writer
/// recovers the text from the layout. Quote-aware: a `<` inside a normal
/// `"..."` string is literal text and left alone. Angle brackets nest (HTML
/// tags), so we drop the whole balanced run.
fn sanitize_html_labels(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_quote = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quote = !in_quote;
                out.push(c);
            }
            // Keep a backslash escape and its escaped char together so an
            // escaped quote doesn't flip `in_quote`.
            '\\' if in_quote => {
                out.push(c);
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '<' if !in_quote => {
                let mut depth = 1;
                for h in chars.by_ref() {
                    match h {
                        '<' => depth += 1,
                        '>' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                out.push_str("\"\"");
            }
            _ => out.push(c),
        }
    }
    out
}

/// Fold every node statement's explicit attributes into `out`, keyed by node
/// id, recursing through subgraphs. Within a node id, later values win per key
/// (Graphviz semantics). This counters dot-parser's canonical `NodeSet`, which
/// keeps only the last statement per id and so drops attributes whenever a node
/// is re-referenced bare. A bare statement contributes no attributes, so it can
/// never erase ones an earlier statement set.
fn collect_node_attrs<'a>(stmts: &ast::StmtList<(ast::ID<'a>, ast::ID<'a>)>, out: &mut HashMap<String, Vec<Attr>>) {
    for stmt in &stmts.stmts {
        match stmt {
            ast::Stmt::NodeStmt(node) => {
                let attrs = out.entry(node.node.id.clone()).or_default();
                let Some(list) = &node.attr else { continue };
                // `AttrList` is a list of `AList`s; flatten and upsert each pair.
                for alist in &list.elems {
                    for (key, value) in &alist.elems {
                        let (key, value): (String, String) = (key.clone().into(), value.clone().into());
                        match attrs.iter_mut().find(|a| a.key == key) {
                            Some(existing) => existing.value = value,
                            None => attrs.push(Attr { key, value }),
                        }
                    }
                }
            }
            ast::Stmt::Subgraph(sub) => collect_node_attrs(&sub.stmts, out),
            _ => {}
        }
    }
}

/// graffle -> dot: recover a graph model from the plist, render it as DOT text,
/// then parse that text back so `DotData` keeps its "owns a resolved canonical
/// graph" invariant. Re-parsing also validates the DOT we generated — if it's
/// malformed we surface a `DotParse` error instead of emitting garbage.
impl TryFrom<GraffleData> for DotData {
    type Error = DotGraffleError;
    fn try_from(graffle: GraffleData) -> Result<Self, Self::Error> {
        let dot_text = render_dot(&graffle.to_model());
        DotData::try_from(dot_text.into_bytes())
    }
}

/// Render back to DOT text. We emit `source`, not `graph`: the canonical graph
/// has flattened away subgraphs, and the graffle -> dot direction needs the
/// `subgraph cluster_*` blocks that `source` preserves. Only that direction
/// calls `to_string` (dot -> graffle never does), so this is safe.
impl fmt::Display for DotData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.source)
    }
}

// ---- DOT text emission (graffle -> dot) ------------------------------------

/// Render a recovered [`GModel`] as DOT, with groups as `subgraph cluster_<id>`
/// blocks. Declarations keep document order for deterministic output.
fn render_dot(model: &GModel) -> String {
    let by_id: HashMap<i64, &GNode> = model.nodes.iter().map(|n| (n.id, n)).collect();

    // Nodes claimed by a cluster are declared inside it, not at top level.
    fn mark(clusters: &[GCluster], claimed: &mut HashSet<i64>) {
        for c in clusters {
            claimed.extend(&c.node_ids);
            mark(&c.children, claimed);
        }
    }
    let mut claimed: HashSet<i64> = HashSet::new();
    mark(&model.clusters, &mut claimed);

    let names = node_names(&model.nodes);

    let mut out = format!("digraph {} {{\n", quote(model.name.as_deref().unwrap_or("G")));
    for node in &model.nodes {
        if !claimed.contains(&node.id) {
            out.push_str(&format!("    {}\n", node_decl(node, &names)));
        }
    }
    for cluster in &model.clusters {
        render_cluster(cluster, &by_id, &names, 1, &mut out);
    }
    for edge in &model.edges {
        out.push_str(&format!("    {}\n", edge_decl(edge, &names)));
    }
    out.push_str("}\n");
    out
}

/// A readable, unique DOT name for each node id. A node's name is its label
/// (internal whitespace collapsed to single spaces so a multi-line label still
/// yields a one-line name), or its integer id when it has no label. Names are
/// assigned in document order and uniquified — the first claimant keeps the
/// bare name, a later collision gets the (globally unique) id appended — so the
/// mapping is deterministic and one-to-one. The exact label text is still
/// emitted as the `label` attribute, so nothing visible is lost to the
/// renaming.
fn node_names(nodes: &[GNode]) -> HashMap<i64, String> {
    let mut names = HashMap::with_capacity(nodes.len());
    let mut used: HashSet<String> = HashSet::with_capacity(nodes.len());
    for n in nodes {
        let base = n
            .label
            .as_deref()
            .map(collapse_ws)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| n.id.to_string());
        let mut name = base.clone();
        let mut k = 0;
        while !used.insert(name.clone()) {
            // Disambiguate with the unique id; the trailing counter guards the
            // (vanishing) chance the suffixed form is itself a literal label.
            name = if k == 0 {
                format!("{base} {}", n.id)
            } else {
                format!("{base} {} {k}", n.id)
            };
            k += 1;
        }
        names.insert(n.id, name);
    }
    names
}

/// Collapse internal runs of whitespace (including the newlines DOT/RTF labels
/// carry) to single spaces and trim.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The quoted DOT name for an id, falling back to the bare id if it is somehow
/// absent from the map (every real node is present, so this is defensive).
fn name_of(names: &HashMap<i64, String>, id: i64) -> String {
    match names.get(&id) {
        Some(name) => quote(name),
        None => quote(&id.to_string()),
    }
}

fn render_cluster(
    c: &GCluster,
    by_id: &HashMap<i64, &GNode>,
    names: &HashMap<i64, String>,
    depth: usize,
    out: &mut String,
) {
    let pad = "    ".repeat(depth);
    let inner = "    ".repeat(depth + 1);
    out.push_str(&format!("{pad}subgraph cluster_{} {{\n", c.id));
    if let Some(label) = &c.label {
        out.push_str(&format!("{inner}{};\n", dot_attr("label", label)));
    }
    if let Some(font) = &c.font_name {
        out.push_str(&format!("{inner}{};\n", dot_attr("fontname", font)));
    }
    if let Some(size) = c.font_size {
        out.push_str(&format!("{inner}fontsize={};\n", num_lit(size)));
    }
    if let Some(color) = &c.font_color {
        out.push_str(&format!("{inner}{};\n", dot_attr("fontcolor", color)));
    }
    for id in &c.node_ids {
        if let Some(node) = by_id.get(id) {
            out.push_str(&format!("{inner}{}\n", node_decl(node, names)));
        }
    }
    for child in &c.children {
        render_cluster(child, by_id, names, depth + 1, out);
    }
    out.push_str(&format!("{pad}}}\n"));
}

fn node_decl(n: &GNode, names: &HashMap<i64, String>) -> String {
    let mut attrs: Vec<String> = Vec::new();
    if let Some(label) = &n.label {
        attrs.push(dot_attr("label", label));
    }
    if let Some(shape) = n.shape {
        attrs.push(dot_attr("shape", shape));
    }
    let mut styles: Vec<&str> = Vec::new();
    if n.rounded {
        styles.push("rounded");
    }
    if n.fill.is_some() {
        styles.push("filled");
    }
    if let Some(dash) = n.dash {
        styles.push(dash);
    }
    if !styles.is_empty() {
        attrs.push(dot_attr("style", &styles.join(",")));
    }
    if let Some(fill) = &n.fill {
        attrs.push(dot_attr("fillcolor", fill));
    }
    if let Some(pen) = &n.pen {
        attrs.push(dot_attr("color", pen));
    }
    if let Some(w) = n.pen_width {
        attrs.push(format!("penwidth={}", num_lit(w)));
    }
    if let Some(font) = &n.font_name {
        attrs.push(dot_attr("fontname", font));
    }
    if let Some(size) = n.font_size {
        attrs.push(format!("fontsize={}", num_lit(size)));
    }
    if let Some(color) = &n.font_color {
        attrs.push(dot_attr("fontcolor", color));
    }

    let name = name_of(names, n.id);
    if attrs.is_empty() {
        format!("{name};")
    } else {
        format!("{name} [{}];", attrs.join(", "))
    }
}

fn edge_decl(e: &GEdge, names: &HashMap<i64, String>) -> String {
    let mut attrs: Vec<String> = Vec::new();
    if let Some(label) = &e.label {
        attrs.push(dot_attr("label", label));
    }
    if let Some(color) = &e.color {
        attrs.push(dot_attr("color", color));
    }
    if let Some(w) = e.pen_width {
        attrs.push(format!("penwidth={}", num_lit(w)));
    }
    if let Some(style) = e.style {
        attrs.push(dot_attr("style", style));
    }
    if let Some(dir) = e.dir {
        attrs.push(dot_attr("dir", dir));
    }
    if let Some(arrow) = e.arrowhead {
        attrs.push(dot_attr("arrowhead", arrow));
    }
    if let Some(arrow) = e.arrowtail {
        attrs.push(dot_attr("arrowtail", arrow));
    }

    let edge = format!("{} -> {}", name_of(names, e.tail), name_of(names, e.head));
    if attrs.is_empty() {
        format!("{edge};")
    } else {
        format!("{edge} [{}];", attrs.join(", "))
    }
}

/// A DOT `key=value` with the value quoted/escaped. `{:?}` on a `&str` gives
/// exactly DOT's quoted-string escaping for `"`, `\`, and newlines — the same
/// trick [`Attr`] uses on the dot -> graffle side.
fn dot_attr(key: &str, value: &str) -> String {
    format!("{key}={value:?}")
}

fn quote(s: &str) -> String {
    format!("{s:?}")
}

/// A number without a trailing `.0` (so `2.0` -> `2`, `1.5` stays `1.5`).
fn num_lit(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_attr_quotes_and_escapes() {
        assert_eq!(dot_attr("label", "hi"), r#"label="hi""#);
        // Embedded quotes and backslashes get DOT-legal escaping.
        assert_eq!(dot_attr("label", "a\"b"), r#"label="a\"b""#);
        assert_eq!(dot_attr("label", "a\\b"), r#"label="a\\b""#);
        // A real newline becomes `\n`, which graphviz renders as a line break.
        assert_eq!(dot_attr("label", "a\nb"), r#"label="a\nb""#);
    }

    #[test]
    fn num_lit_drops_trailing_zero() {
        assert_eq!(num_lit(2.0), "2");
        assert_eq!(num_lit(0.75), "0.75");
        assert_eq!(num_lit(1.5), "1.5");
    }

    #[test]
    fn html_labels_are_blanked_for_the_parser() {
        // A balanced (and nested) HTML value collapses to an empty string.
        assert_eq!(sanitize_html_labels(r#"a [label=<<b>x</b>>];"#), r#"a [label=""];"#);
        // A `<` inside a normal quoted string is literal text — left untouched.
        assert_eq!(sanitize_html_labels(r#"a [label="x < y"];"#), r#"a [label="x < y"];"#);
    }

    #[test]
    fn html_labelled_dot_parses_with_structure_intact() {
        // dot-parser rejects `<...>`; sanitizing lets the structure through so
        // nodes and edges still resolve (the text is recovered from layout).
        let dot = br#"digraph g { a [shape=plaintext label=<<table><tr><td>X</td></tr></table>>]; a -> b; }"#.to_vec();
        let data = DotData::try_from(dot).expect("HTML label must not break the parse");
        assert!(data.graph.nodes.set.contains_key("a"));
        assert!(data.graph.nodes.set.contains_key("b"));
    }

    #[test]
    fn bare_re_reference_keeps_node_attrs() {
        // A node re-mentioned bare (the `{ rank=sink; n; }` layout idiom) must
        // keep the attributes from its full declaration. dot-parser's canonical
        // NodeSet otherwise keeps only the last (empty) statement, silently
        // dropping shape / fill / label.
        let dot = br#"digraph g { n [shape=note, label="Keep me"]; { rank=sink; n; } }"#.to_vec();
        let data = DotData::try_from(dot).expect("dot parses");
        let attrs = &data.graph.nodes.set["n"].attr;
        let pairs: Vec<_> = attrs.elems.iter().map(|a| (a.key.as_str(), a.value.as_str())).collect();
        assert!(pairs.contains(&("shape", "note")), "shape attr lost: {pairs:?}");
        assert!(pairs.contains(&("label", "Keep me")), "label attr lost: {pairs:?}");
    }

    #[test]
    fn later_node_statement_overrides_attr_value() {
        // Repeated statements accumulate; a later value wins for the same key.
        let dot = br#"digraph g { n [shape=box, color=red]; n [color=blue]; }"#.to_vec();
        let data = DotData::try_from(dot).expect("dot parses");
        let attrs = &data.graph.nodes.set["n"].attr;
        let pairs: Vec<_> = attrs.elems.iter().map(|a| (a.key.as_str(), a.value.as_str())).collect();
        assert!(pairs.contains(&("shape", "box")), "earlier attr lost: {pairs:?}");
        assert!(pairs.contains(&("color", "blue")), "later value should win: {pairs:?}");
        assert!(!pairs.contains(&("color", "red")), "stale value kept: {pairs:?}");
    }

    fn gnode(id: i64, label: Option<&str>) -> GNode {
        GNode {
            id,
            label: label.map(str::to_owned),
            shape: None,
            fill: None,
            pen: None,
            pen_width: None,
            rounded: false,
            dash: None,
            font_name: None,
            font_size: None,
            font_color: None,
        }
    }

    #[test]
    fn node_names_prefer_labels_uniquify_and_fall_back_to_id() {
        let nodes = vec![
            gnode(1, Some("alpha")),
            gnode(2, Some("alpha")),       // collides -> disambiguated by id
            gnode(3, None),                // no label -> the bare id
            gnode(4, Some("multi\nline")), // internal whitespace collapses
        ];
        let names = node_names(&nodes);
        assert_eq!(names[&1], "alpha");
        assert_eq!(names[&2], "alpha 2");
        assert_eq!(names[&3], "3");
        assert_eq!(names[&4], "multi line");
    }

    #[test]
    fn render_dot_names_by_label_and_recovers_cluster_font() {
        let model = GModel {
            name: Some("g".into()),
            nodes: vec![gnode(1, Some("start")), gnode(2, Some("end"))],
            edges: vec![GEdge {
                tail: 1,
                head: 2,
                label: None,
                color: None,
                pen_width: None,
                style: None,
                dir: None,
                arrowhead: None,
                arrowtail: None,
            }],
            clusters: vec![GCluster {
                id: 9,
                label: Some("box".into()),
                node_ids: vec![1],
                children: Vec::new(),
                font_name: Some("Helvetica".into()),
                font_size: Some(18.0),
                font_color: Some("#ff0000".into()),
            }],
        };
        let dot = render_dot(&model);
        // The edge is wired by the nodes' labels, not their numeric ids.
        assert!(dot.contains(r#""start" -> "end""#), "{dot}");
        // The cluster recovers its label font as subgraph attributes.
        assert!(dot.contains("subgraph cluster_9"), "{dot}");
        assert!(dot.contains(r#"fontname="Helvetica""#), "{dot}");
        assert!(dot.contains("fontsize=18;"), "{dot}");
        assert!(dot.contains(r##"fontcolor="#ff0000""##), "{dot}");
    }
}
