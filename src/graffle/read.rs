//! graffle -> dot: walk the OmniGraffle plist into a flat [`GModel`].
//!
//! Missing or malformed structure yields an empty graph rather than an error —
//! an empty diagram is a valid (if dull) thing to convert. Values come out in
//! DOT vocabulary (shape keywords, `#rrggbb` colors) so the DOT writer needs no
//! OmniGraffle knowledge.

use std::collections::HashSet;

use plist::{Dictionary, Value};

use super::model::{Arrow, GCluster, GEdge, GModel, GNode, Shape};
use super::{num, to_u8};

/// Build the recovered model from the root plist document.
pub(crate) fn build_model(doc: &Value) -> GModel {
    let sheets = doc
        .as_dictionary()
        .and_then(|d| d.get("Sheets"))
        .and_then(Value::as_array);
    if let Some(s) = sheets
        && s.len() > 1
    {
        // DOT is one graph per file, so the extra canvases are dropped — say so.
        eprintln!(
            "graffle-dot: document has {} sheets; converting only the first",
            s.len()
        );
    }
    let sheet = sheets.and_then(|s| s.first()).and_then(Value::as_dictionary);
    let name = sheet
        .and_then(|d| d.get("SheetTitle"))
        .and_then(Value::as_string)
        .map(str::to_owned);

    let mut model = GModel {
        name,
        nodes: Vec::new(),
        edges: Vec::new(),
        clusters: Vec::new(),
    };
    let mut edge_candidates: Vec<GEdge> = Vec::new();

    if let Some(list) = sheet.and_then(|d| d.get("GraphicsList")).and_then(Value::as_array) {
        let (_top_ids, clusters) = collect(list, &mut model.nodes, &mut edge_candidates);
        model.clusters = clusters;
    }

    // Keep only edges whose endpoints are real nodes — this drops dangling
    // lines and lines whose end touches a group rather than a shape.
    let ids: HashSet<i64> = model.nodes.iter().map(|n| n.id).collect();
    model.edges = edge_candidates
        .into_iter()
        .filter(|e| ids.contains(&e.tail) && ids.contains(&e.head))
        .collect();

    model
}

/// Recursively walk a `GraphicsList`, pushing every node into `nodes` and every
/// candidate edge into `edges`. Returns the ids declared directly at this level
/// (so a parent group can claim them) and the clusters found at this level.
fn collect(items: &[Value], nodes: &mut Vec<GNode>, edges: &mut Vec<GEdge>) -> (Vec<i64>, Vec<GCluster>) {
    let mut direct_ids = Vec::new();
    let mut clusters = Vec::new();

    for item in items {
        let Some(d) = item.as_dictionary() else {
            continue;
        };
        match d.get("Class").and_then(Value::as_string) {
            Some("ShapedGraphic") => {
                if let Some(node) = node_from(d) {
                    direct_ids.push(node.id);
                    nodes.push(node);
                }
            }
            Some("LineGraphic") => {
                if let Some(edge) = edge_from(d) {
                    edges.push(edge);
                }
            }
            Some("Group" | "TableGroup") => {
                let Some(sub) = d.get("Graphics").and_then(Value::as_array) else {
                    continue;
                };
                let id = d.get("ID").and_then(Value::as_signed_integer).unwrap_or_default();
                let label = group_label(d);
                let (font_name, font_size, font_color) = group_font(d);
                let (node_ids, children) = collect(sub, nodes, edges);
                clusters.push(GCluster {
                    id,
                    label,
                    node_ids,
                    children,
                    font_name,
                    font_size,
                    font_color,
                });
            }
            _ => {}
        }
    }

    (direct_ids, clusters)
}

/// Build a node from a `ShapedGraphic` dict. `None` only if it has no `ID`.
fn node_from(d: &Dictionary) -> Option<GNode> {
    let id = d.get("ID").and_then(Value::as_signed_integer)?;

    let style = d.get("Style").and_then(Value::as_dictionary);
    let fill_d = style.and_then(|s| s.get("fill")).and_then(Value::as_dictionary);
    let stroke_d = style.and_then(|s| s.get("stroke")).and_then(Value::as_dictionary);
    let draws_no = |dd: Option<&Dictionary>| dd.and_then(|x| x.get("Draws")).and_then(Value::as_string) == Some("NO");
    let no_fill = draws_no(fill_d);
    let no_stroke = draws_no(stroke_d);

    // No fill and no stroke is graphviz `plaintext` (the inverse of how the
    // writer renders a text-only node). Otherwise recover the named shape, and a
    // `VFlip` turns a base shape back into its inverted DOT variant.
    let shape = if no_fill && no_stroke {
        Some("plaintext")
    } else {
        let flip = d.get("VFlip").and_then(Value::as_string) == Some("YES");
        d.get("Shape")
            .and_then(Value::as_string)
            .and_then(Shape::from_omni)
            .map(|s| s.to_dot(flip))
    };

    let fill = if no_fill {
        None
    } else {
        fill_d.and_then(|f| f.get("Color")).and_then(color_hex)
    };
    let pen = if no_stroke {
        None
    } else {
        stroke_d.and_then(|s| s.get("Color")).and_then(color_hex)
    };
    let pen_width = stroke_d.and_then(|s| s.get("Width")).and_then(num);
    let rounded = stroke_d.is_some_and(|s| s.get("CornerRadius").is_some());
    let dash = stroke_d
        .and_then(|s| s.get("Pattern"))
        .and_then(num)
        .and_then(pattern_to_style);

    let stripped = label_rtf(d);
    let label = stripped.as_ref().and_then(|s| non_empty(&s.text));

    // Prefer the explicit `FontInfo` dict; fall back to what the RTF carries
    // (nodes often omit `FontInfo`).
    let font_info = d.get("FontInfo").and_then(Value::as_dictionary);
    let font_name = font_info
        .and_then(|f| f.get("Font"))
        .and_then(Value::as_string)
        .map(str::to_owned)
        .or_else(|| stripped.as_ref().and_then(|s| s.font.clone()));
    let font_size = font_info
        .and_then(|f| f.get("Size"))
        .and_then(num)
        .or_else(|| stripped.as_ref().and_then(|s| s.size));
    let font_color = font_info
        .and_then(|f| f.get("Color"))
        .and_then(color_hex)
        .or_else(|| stripped.as_ref().and_then(|s| s.color.clone()));

    Some(GNode {
        id,
        label,
        shape,
        fill,
        pen,
        pen_width,
        rounded,
        dash,
        font_name,
        font_size,
        font_color,
    })
}

/// Build an edge from a `LineGraphic` dict. `None` for a dangling line (missing
/// `Tail`/`Head` id) — those are skipped.
fn edge_from(d: &Dictionary) -> Option<GEdge> {
    let endpoint = |key| {
        d.get(key)
            .and_then(Value::as_dictionary)
            .and_then(|e| e.get("ID"))
            .and_then(Value::as_signed_integer)
    };
    let tail = endpoint("Tail")?;
    let head = endpoint("Head")?;

    let stroke = d
        .get("Style")
        .and_then(Value::as_dictionary)
        .and_then(|s| s.get("stroke"))
        .and_then(Value::as_dictionary);
    let color = stroke.and_then(|s| s.get("Color")).and_then(color_hex);
    let pen_width = stroke.and_then(|s| s.get("Width")).and_then(num);
    let style = stroke
        .and_then(|s| s.get("Pattern"))
        .and_then(num)
        .and_then(pattern_to_style);

    // An arrow is present when its key holds a real identifier; OmniGraffle's
    // `"0"` and a missing key both mean "no arrow".
    let arrow = |key| {
        stroke
            .and_then(|s| s.get(key))
            .and_then(Value::as_string)
            .filter(|v| *v != "0")
    };
    let head_end = arrow("HeadArrow");
    let tail_end = arrow("TailArrow");
    let dir = match (head_end.is_some(), tail_end.is_some()) {
        (true, false) => None, // forward — graphviz's default, so omit it
        (false, true) => Some("back"),
        (true, true) => Some("both"),
        (false, false) => Some("none"),
    };
    let arrowhead = head_end.and_then(Arrow::from_omni);
    let arrowtail = tail_end.and_then(Arrow::from_omni);

    let label = label_rtf(d).as_ref().and_then(|s| non_empty(&s.text));

    Some(GEdge {
        tail,
        head,
        label,
        color,
        pen_width,
        style,
        dir,
        arrowhead,
        arrowtail,
    })
}

/// A group's label: its `Name` (TableGroups carry one), else any text it holds.
fn group_label(d: &Dictionary) -> Option<String> {
    if let Some(name) = d.get("Name").and_then(Value::as_string).and_then(non_empty) {
        return Some(name);
    }
    label_rtf(d).as_ref().and_then(|s| non_empty(&s.text))
}

/// A group's label font from its `FontInfo` dict — name, size, color — the same
/// way [`node_from`] reads a node's font. Returned as a triple to fill the
/// matching [`GCluster`] fields.
fn group_font(d: &Dictionary) -> (Option<String>, Option<f64>, Option<String>) {
    let font_info = d.get("FontInfo").and_then(Value::as_dictionary);
    let name = font_info
        .and_then(|f| f.get("Font"))
        .and_then(Value::as_string)
        .map(str::to_owned);
    let size = font_info.and_then(|f| f.get("Size")).and_then(num);
    let color = font_info.and_then(|f| f.get("Color")).and_then(color_hex);
    (name, size, color)
}

/// Strip a graphic's `Text.Text` RTF blob, if any.
fn label_rtf(d: &Dictionary) -> Option<crate::rtf::RtfText> {
    d.get("Text")
        .and_then(Value::as_dictionary)
        .and_then(|t| t.get("Text"))
        .and_then(Value::as_string)
        .map(crate::rtf::strip)
}

fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() { None } else { Some(s.to_owned()) }
}

/// Recover a graphviz line `style` from an OmniGraffle stroke `Pattern`: 2 ->
/// `dotted`, any other non-zero -> `dashed`, 0/absent -> solid (`None`).
fn pattern_to_style(pattern: f64) -> Option<&'static str> {
    match pattern as i64 {
        0 => None,
        2 => Some("dotted"),
        _ => Some("dashed"),
    }
}

/// An OmniGraffle color dict (`{r, g, b}`, components 0..1 as reals or strings;
/// or grayscale `w`) back to `#rrggbb`. The color `space` is ignored — sRGB,
/// grayscale, and hashed ICC profiles all carry usable components.
fn color_hex(v: &Value) -> Option<String> {
    let d = v.as_dictionary()?;
    let comp = |k| d.get(k).and_then(num);
    let (r, g, b) = match (comp("r"), comp("g"), comp("b")) {
        (Some(r), Some(g), Some(b)) => (r, g, b),
        _ => {
            let w = comp("w").or_else(|| comp("white"))?;
            (w, w, w)
        }
    };
    Some(format!("#{:02x}{:02x}{:02x}", to_u8(r), to_u8(g), to_u8(b)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dotviz::DotData;
    use crate::graffle::test_support::{parses_as_dot, read_fixture};

    fn color(r: f64, g: f64, b: f64) -> Value {
        let mut d = Dictionary::new();
        d.insert("r".into(), Value::Real(r));
        d.insert("g".into(), Value::Real(g));
        d.insert("b".into(), Value::Real(b));
        d.insert("space".into(), Value::String("srgb".into()));
        Value::Dictionary(d)
    }

    #[test]
    fn color_dict_back_to_hex() {
        assert_eq!(color_hex(&color(0.0, 0.0, 0.0)).as_deref(), Some("#000000"));
        assert_eq!(color_hex(&color(1.0, 1.0, 1.0)).as_deref(), Some("#ffffff"));
        assert_eq!(color_hex(&color(1.0, 0.0, 0.0)).as_deref(), Some("#ff0000"));

        // Grayscale dicts (only `w`) resolve too.
        let mut gray = Dictionary::new();
        gray.insert("w".into(), Value::Real(0.5));
        assert_eq!(color_hex(&Value::Dictionary(gray)).as_deref(), Some("#808080"));
    }

    #[test]
    fn missing_structure_yields_an_empty_graph() {
        // The module contract: a plist with no recognizable structure converts
        // to an empty graph rather than erroring. Neither an empty dict nor a
        // document whose `Sheets` is absent should panic or fail.
        for doc in [Value::Dictionary(Dictionary::new()), Value::Array(Vec::new())] {
            let model = build_model(&doc);
            assert!(model.nodes.is_empty());
            assert!(model.edges.is_empty());
            assert!(model.clusters.is_empty());
        }
    }

    #[test]
    fn only_the_first_sheet_is_converted() {
        // A multi-sheet document converts just the first canvas (the rest are
        // dropped, with a note on stderr) — DOT is one graph per file.
        let shaped = |id: i64| {
            let mut g = Dictionary::new();
            g.insert("Class".into(), Value::String("ShapedGraphic".into()));
            g.insert("ID".into(), Value::Integer(id.into()));
            Value::Dictionary(g)
        };
        let sheet = |id: i64| {
            let mut s = Dictionary::new();
            s.insert("GraphicsList".into(), Value::Array(vec![shaped(id)]));
            Value::Dictionary(s)
        };
        let mut doc = Dictionary::new();
        doc.insert("Sheets".into(), Value::Array(vec![sheet(1), sheet(2)]));

        let model = build_model(&Value::Dictionary(doc));
        assert_eq!(model.nodes.len(), 1);
        assert_eq!(model.nodes[0].id, 1);
    }

    #[test]
    fn node_from_recovers_font_info() {
        // The explicit `FontInfo` dict (preferred over RTF-carried font) comes
        // back as the node's font name, size, and color.
        let mut font = Dictionary::new();
        font.insert("Font".into(), Value::String("Courier".into()));
        font.insert("Size".into(), Value::Real(11.0));
        font.insert("Color".into(), color(0.0, 0.0, 0.0));
        let mut d = Dictionary::new();
        d.insert("ID".into(), Value::Integer(7.into()));
        d.insert("Shape".into(), Value::String("Rectangle".into()));
        d.insert("FontInfo".into(), Value::Dictionary(font));

        let n = node_from(&d).expect("valid node dict");
        assert_eq!(n.font_name.as_deref(), Some("Courier"));
        assert_eq!(n.font_size, Some(11.0));
        assert_eq!(n.font_color.as_deref(), Some("#000000"));
    }

    #[test]
    fn group_font_recovers_label_font() {
        // A group carries only `FontInfo` (no fill/stroke); recover the size and
        // color even when it names no `Font`.
        let mut font = Dictionary::new();
        font.insert("Size".into(), Value::Real(20.0));
        font.insert("Color".into(), color(1.0, 0.0, 0.0));
        let mut d = Dictionary::new();
        d.insert("FontInfo".into(), Value::Dictionary(font));

        let (name, size, col) = group_font(&d);
        assert_eq!(name, None);
        assert_eq!(size, Some(20.0));
        assert_eq!(col.as_deref(), Some("#ff0000"));
    }

    #[test]
    fn vflip_recovers_inverted_shape() {
        // graffle -> dot: a `VFlip`ped base shape comes back as its inverse.
        let mut d = Dictionary::new();
        d.insert("ID".into(), Value::Integer(1.into()));
        d.insert("Shape".into(), Value::String("VerticalTriangle".into()));
        d.insert("VFlip".into(), Value::String("YES".into()));
        assert_eq!(node_from(&d).expect("valid node dict").shape, Some("invtriangle"));
        // Without the flip it stays the upright base shape.
        d.remove("VFlip");
        assert_eq!(node_from(&d).expect("valid node dict").shape, Some("triangle"));
    }

    fn line_dict(head: &str, tail: &str, pattern: Option<i64>) -> Dictionary {
        let mut stroke = Dictionary::new();
        stroke.insert("HeadArrow".into(), Value::String(head.into()));
        stroke.insert("TailArrow".into(), Value::String(tail.into()));
        if let Some(p) = pattern {
            stroke.insert("Pattern".into(), Value::Integer(p.into()));
        }
        let mut style = Dictionary::new();
        style.insert("stroke".into(), Value::Dictionary(stroke));
        let ep = |id: i64| {
            let mut e = Dictionary::new();
            e.insert("ID".into(), Value::Integer(id.into()));
            Value::Dictionary(e)
        };
        let mut d = Dictionary::new();
        d.insert("Tail".into(), ep(1));
        d.insert("Head".into(), ep(2));
        d.insert("Style".into(), Value::Dictionary(style));
        d
    }

    #[test]
    fn edge_from_recovers_direction_shapes_and_pattern() {
        // Head only is graphviz's forward default, so `dir` stays unset.
        let e = edge_from(&line_dict("FilledArrow", "0", None)).expect("valid edge dict");
        assert_eq!((e.dir, e.arrowhead, e.style), (None, None, None));

        // Tail only -> back; neither -> none.
        assert_eq!(
            edge_from(&line_dict("0", "FilledArrow", None))
                .expect("valid edge dict")
                .dir,
            Some("back")
        );
        assert_eq!(
            edge_from(&line_dict("0", "0", None)).expect("valid edge dict").dir,
            Some("none")
        );

        // Both ends, custom shapes, dotted pattern all come back.
        let e = edge_from(&line_dict("Diamond", "Arrow", Some(2))).expect("valid edge dict");
        assert_eq!(e.dir, Some("both"));
        assert_eq!(e.arrowhead, Some("diamond"));
        assert_eq!(e.arrowtail, Some("vee"));
        assert_eq!(e.style, Some("dotted"));
    }

    #[test]
    fn node_from_recovers_border_dash() {
        let mut stroke = Dictionary::new();
        stroke.insert("Pattern".into(), Value::Integer(2.into()));
        let mut style = Dictionary::new();
        style.insert("stroke".into(), Value::Dictionary(stroke));
        let mut d = Dictionary::new();
        d.insert("ID".into(), Value::Integer(1.into()));
        d.insert("Shape".into(), Value::String("Rectangle".into()));
        d.insert("Style".into(), Value::Dictionary(style));
        assert_eq!(node_from(&d).expect("valid node dict").dash, Some("dotted"));
    }

    #[test]
    fn reads_shutdown_topology() {
        let model = read_fixture("shutdown.graffle").to_model();
        assert_eq!(model.nodes.len(), 74);
        assert_eq!(model.edges.len(), 24);
        assert_eq!(model.clusters.len(), 2);
        // A known connected edge: LineGraphic 176 wires Tail 128 -> Head 175.
        assert!(model.edges.iter().any(|e| e.tail == 128 && e.head == 175));

        let dot = DotData::try_from(read_fixture("shutdown.graffle"))
            .expect("shutdown converts to dot")
            .to_string();
        assert!(dot.contains("subgraph cluster_"), "{dot}");
        // Nodes are now named by their labels, so the 128 -> 175 wiring checked
        // above renders as a label-to-label edge; assert every edge made it into
        // the text rather than matching the old numeric ids.
        assert_eq!(dot.matches(" -> ").count(), model.edges.len(), "{dot}");
        assert!(parses_as_dot(&dot), "emitted DOT must re-parse");
    }

    #[test]
    fn reads_complicated_topology() {
        let model = read_fixture("complicated.graffle").to_model();
        assert_eq!(model.nodes.len(), 243);
        assert_eq!(model.edges.len(), 152);
        // 11 top-level groups (some hold nested groups of their own).
        assert_eq!(model.clusters.len(), 11);

        let dot = DotData::try_from(read_fixture("complicated.graffle"))
            .expect("complicated converts to dot")
            .to_string();
        assert!(dot.contains("subgraph cluster_"));
        assert!(parses_as_dot(&dot), "emitted DOT must re-parse");
    }

    #[test]
    fn disconnected_fixtures_have_nodes_but_no_edges() {
        // These carry decorative lines with no Head/Tail wiring — we recover the
        // boxes but no topology, and must not panic.
        for name in ["boxes-arrows.graffle", "vmu-emulator.graffle"] {
            let model = read_fixture(name).to_model();
            assert!(!model.nodes.is_empty(), "{name} should yield nodes");
            assert_eq!(model.edges.len(), 0, "{name} has no connected edges");

            let dot = DotData::try_from(read_fixture(name))
                .expect("fixture converts to dot")
                .to_string();
            assert!(!dot.contains(" -> "), "{name} should emit no edges");
            assert!(parses_as_dot(&dot));
        }
    }
}
