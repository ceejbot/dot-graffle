//! dot -> graffle.
//!
//! Clusters become background rectangles, nodes become `ShapedGraphic`s
//! positioned by a real graphviz layout (a plain grid if graphviz is
//! unavailable) and styled from their DOT attributes, and edges become
//! `LineGraphic`s wired by `Head`/`Tail` ID. The output is a flat XML plist.

use std::collections::HashMap;

use dot_parser::ast::AList;
use dot_parser::canonical::{AttrStmt, Graph};
use plist::{Dictionary, Value};

use super::model::{Arrow, Shape, is_text_only};
use super::{GraffleData, to_u8};
use crate::dotviz::{Attr, DotData};
use crate::error::DotGraffleError;
use crate::layout::{self, Cluster, FloatingLabel, Layout, NodeBox, NodeLabel, RecordCell};

// Grid fallback geometry (points), used only when graphviz layout is absent.
const COLS: usize = 4;
const GRID_W: f64 = 170.0;
const GRID_H: f64 = 48.0;
const COL_GAP: f64 = 230.0;
const ROW_GAP: f64 = 120.0;

// Font fallbacks when the source sets none.
const DEFAULT_FONT: &str = "Helvetica";
const DEFAULT_FONT_SIZE: f64 = 14.0;

/// Resolved label font.
struct FontViz<'a> {
    name: &'a str,
    size: f64,
    color: Option<&'a str>,
}

/// Resolved node styling.
struct NodeViz<'a> {
    label: &'a str,
    style: &'a str,
    shape: &'a str,
    fill: Option<&'a str>,
    pen: Option<&'a str>,
    pen_width: Option<f64>,
    font: FontViz<'a>,
}

/// Resolved edge styling.
struct EdgeViz<'a> {
    color: Option<&'a str>,
    /// OmniGraffle arrow identifier for each end, or `"0"` for no arrow.
    head_arrow: &'static str,
    tail_arrow: &'static str,
    /// Stroke dash-pattern preset index, or `None` for a solid line.
    pattern: Option<i64>,
    pen_width: Option<f64>,
    font: FontViz<'a>,
}

/// dot -> graffle.
impl TryFrom<DotData> for GraffleData {
    type Error = DotGraffleError;

    fn try_from(dot: DotData) -> Result<Self, Self::Error> {
        let graph = &dot.graph;

        // Real layout (node positions + cluster boxes) from graphviz. If `dot`
        // is not installed, fall back to a grid and emit no clusters; if
        // graphviz is present but fails, surface that instead of silently
        // degrading the diagram.
        let layout = match layout::graphviz_layout(&dot.source) {
            Ok(layout) => Some(layout),
            Err(layout::GraphvizError::NotFound) => {
                eprintln!("dot-graffle: graphviz `dot` unavailable; using a plain grid layout");
                None
            }
            Err(err) => return Err(err.into()),
        };

        // Graphics are assembled back-to-front (clusters, nodes, edges, caption);
        // `finish` reverses to OmniGraffle's front-to-back order.
        let mut b = Builder::new(graph);
        b.emit_clusters(layout.as_ref());
        b.emit_nodes(graph, layout.as_ref());
        b.emit_edges(graph, layout.as_ref());
        b.emit_floating_labels(layout.as_ref());

        Ok(GraffleData {
            doc: b.finish(graph.name.as_deref()),
        })
    }
}

/// Accumulates the OmniGraffle graphics for one dot -> graffle conversion: the
/// running id counter, the canvas extent, and the node geometry edges attach
/// to. The graph-level `node`/`edge` defaults are resolved once up front; each
/// `emit_*` phase appends graphics, and `finish` frames the canvas.
struct Builder<'a> {
    node_defaults: HashMap<&'a str, &'a str>,
    edge_defaults: HashMap<&'a str, &'a str>,
    next_id: i64,
    graphics: Vec<Value>,
    extent: Extent,
    /// node name -> (representative graphic id, center x, center y)
    geom: HashMap<&'a str, (i64, f64, f64)>,
}

impl<'a> Builder<'a> {
    fn new(graph: &'a Graph<Attr>) -> Self {
        // Graph-level `node [...]` / `edge [...]` defaults, folded into maps so
        // we can resolve each node's/edge's effective attributes.
        let mut node_defaults: HashMap<&'a str, &'a str> = HashMap::new();
        let mut edge_defaults: HashMap<&'a str, &'a str> = HashMap::new();
        for stmt in &graph.attr {
            match stmt {
                AttrStmt::Node(a) => {
                    node_defaults.insert(a.key.as_str(), a.value.as_str());
                }
                AttrStmt::Edge(a) => {
                    edge_defaults.insert(a.key.as_str(), a.value.as_str());
                }
                AttrStmt::Graph(_) => {}
            }
        }
        Builder {
            node_defaults,
            edge_defaults,
            next_id: 1,
            graphics: Vec::new(),
            extent: Extent::new(),
            geom: HashMap::new(),
        }
    }

    /// Take the next graphic id.
    fn next(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Cluster background rectangles, emitted first so they land behind nodes.
    fn emit_clusters(&mut self, layout: Option<&Layout>) {
        for cluster in layout.iter().flat_map(|l| &l.clusters) {
            let id = self.next();
            self.extent.add(&cluster.b);
            self.graphics.push(cluster_graphic(id, cluster));
        }
    }

    /// One `ShapedGraphic` per node (a record emits one per field), recording
    /// each node's center and representative id for edges to attach to.
    fn emit_nodes(&mut self, graph: &'a Graph<Attr>, layout: Option<&Layout>) {
        // `NodeSet` is a `HashMap` (random order); sort for deterministic output.
        let mut names: Vec<&'a str> = graph.nodes.set.keys().map(String::as_str).collect();
        names.sort_unstable();

        for (i, name) in names.iter().copied().enumerate() {
            let b = layout
                .and_then(|l| l.nodes.get(name))
                .copied()
                .unwrap_or_else(|| grid_box(i));
            // A node's representative id (what edges attach to) is the first
            // graphic we emit for it; a record emits one per field.
            self.geom.insert(name, (self.next_id, b.x + b.w / 2.0, b.y + b.h / 2.0));
            self.extent.add(&b);

            let attrs = &graph.nodes.set[name].attr;
            let style = effective(attrs, &self.node_defaults, "style").unwrap_or("");
            let viz = NodeViz {
                label: effective(attrs, &self.node_defaults, "label").unwrap_or(name),
                style,
                shape: effective(attrs, &self.node_defaults, "shape").unwrap_or("box"),
                fill: effective(attrs, &self.node_defaults, "fillcolor"),
                pen: effective(attrs, &self.node_defaults, "color"),
                pen_width: effective(attrs, &self.node_defaults, "penwidth")
                    .and_then(|s| s.parse().ok())
                    .or_else(|| has_kw(style, "bold").then_some(BOLD_WIDTH)),
                font: font_viz(|k| effective(attrs, &self.node_defaults, k)),
            };

            match layout.and_then(|l| l.node_labels.get(name)) {
                // A record: one bordered rectangle per field, reproducing the
                // subdivided box. Edges attach to the first cell.
                Some(NodeLabel::Record(cells)) if !cells.is_empty() => {
                    for cell in cells {
                        let id = self.next();
                        self.graphics.push(record_cell_graphic(id, cell, &viz));
                    }
                }
                // An HTML-like label: the text rides in floating labels, so the
                // node draws its box with no inline label.
                Some(NodeLabel::Html) => {
                    let id = self.next();
                    self.graphics.push(shaped_graphic(id, b, &viz, None));
                }
                _ => {
                    let id = self.next();
                    self.graphics.push(shaped_graphic(id, b, &viz, Some(viz.label)));
                }
            }
        }
    }

    /// One `LineGraphic` per edge, following graphviz's routed spline when we
    /// have it, else a straight line between node centers.
    fn emit_edges(&mut self, graph: &'a Graph<Attr>, layout: Option<&Layout>) {
        for edge in &graph.edges.set {
            let (Some(&(tail_id, tx, ty)), Some(&(head_id, hx, hy))) =
                (self.geom.get(edge.from.as_str()), self.geom.get(edge.to.as_str()))
            else {
                // canonical adds edge endpoints as nodes, so this is unreachable.
                continue;
            };
            let id = self.next();

            let label = effective(&edge.attr, &self.edge_defaults, "label");
            let style = effective(&edge.attr, &self.edge_defaults, "style").unwrap_or("");
            // `dir` picks which ends draw arrows; `arrowhead`/`arrowtail` pick
            // each arrow's shape. They are orthogonal, so resolve them apart.
            let (head_on, tail_on) = arrow_ends(effective(&edge.attr, &self.edge_defaults, "dir"), graph.is_digraph);
            let viz = EdgeViz {
                color: effective(&edge.attr, &self.edge_defaults, "color"),
                head_arrow: arrow_id(head_on, effective(&edge.attr, &self.edge_defaults, "arrowhead")),
                tail_arrow: arrow_id(tail_on, effective(&edge.attr, &self.edge_defaults, "arrowtail")),
                pattern: line_pattern(style),
                pen_width: effective(&edge.attr, &self.edge_defaults, "penwidth")
                    .and_then(|s| s.parse().ok())
                    .or_else(|| has_kw(style, "bold").then_some(BOLD_WIDTH)),
                font: font_viz(|k| effective(&edge.attr, &self.edge_defaults, k)),
            };
            // graphviz's routed spline if we have it; otherwise a straight line
            // between node centers (no layout, or an edge dot didn't route).
            let spline = layout.and_then(|l| l.edges.get(&(edge.from.clone(), edge.to.clone())));
            let points = spline.cloned().unwrap_or_else(|| vec![(tx, ty), (hx, hy)]);
            self.graphics
                .push(line_graphic(id, tail_id, head_id, &points, label, &viz));
        }
    }

    /// Floating text labels graphviz positioned for us — the graph caption and
    /// any node/edge xlabel, headlabel, taillabel. Emitted last so they land on
    /// top.
    fn emit_floating_labels(&mut self, layout: Option<&Layout>) {
        for lbl in layout.iter().flat_map(|l| &l.labels) {
            let id = self.next();
            self.extent.add(&lbl.b);
            self.graphics.push(floating_label_graphic(id, lbl));
        }
    }

    /// Reverse into OmniGraffle's front-to-back order (index 0 is the topmost
    /// graphic, so clusters land at the back and the caption on top) and frame
    /// the canvas around everything placed.
    fn finish(mut self, name: Option<&str>) -> Value {
        self.graphics.reverse();
        document(name, self.graphics, self.extent)
    }
}

// ---- attribute resolution --------------------------------------------------

/// A node/edge attribute, falling back to the graph-level default.
fn effective<'a>(explicit: &'a AList<Attr>, defaults: &HashMap<&'a str, &'a str>, key: &str) -> Option<&'a str> {
    explicit
        .elems
        .iter()
        .find(|a| a.key == key)
        .map(|a| a.value.as_str())
        .or_else(|| defaults.get(key).copied())
}

/// Build a `FontViz` from an attribute resolver
/// (`fontname`/`fontsize`/`fontcolor`).
fn font_viz<'a>(get: impl Fn(&str) -> Option<&'a str>) -> FontViz<'a> {
    FontViz {
        name: get("fontname").unwrap_or(DEFAULT_FONT),
        size: get("fontsize")
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_FONT_SIZE),
        color: get("fontcolor"),
    }
}

fn grid_box(i: usize) -> NodeBox {
    NodeBox {
        x: (i % COLS) as f64 * COL_GAP,
        y: (i / COLS) as f64 * ROW_GAP,
        w: GRID_W,
        h: GRID_H,
    }
}

// ---- plist builders --------------------------------------------------------

/// Resolve a label font from optional name/size/color attribute strings,
/// falling back to the project defaults. Shared by the cluster and graph-label
/// builders, which carry the same font triple.
fn label_font<'a>(name: Option<&'a str>, size: Option<&str>, color: Option<&'a str>) -> FontViz<'a> {
    FontViz {
        name: name.unwrap_or(DEFAULT_FONT),
        size: size.and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_FONT_SIZE),
        color,
    }
}

/// A cluster's background rectangle. Drawn before nodes so it sits behind them.
fn cluster_graphic(id: i64, c: &Cluster) -> Value {
    let font = label_font(c.font_name.as_deref(), c.font_size.as_deref(), c.font_color.as_deref());

    let mut g = Dictionary::new();
    g.insert("Class".into(), Value::String("ShapedGraphic".into()));
    g.insert("ID".into(), Value::Integer(id.into()));
    g.insert("Bounds".into(), Value::String(bounds(c.b.x, c.b.y, c.b.w, c.b.h)));
    g.insert("Shape".into(), Value::String("Rectangle".into()));
    g.insert(
        "Style".into(),
        node_style(
            c.style.as_deref().unwrap_or("rounded,filled"),
            false,
            c.fill.as_deref(),
            c.pen.as_deref(),
            None,
        ),
    );
    if let Some(label) = &c.label {
        insert_label(&mut g, label, &font);
        // Pin the label to the top so it doesn't sit over the enclosed nodes.
        g.insert("TextPlacement".into(), Value::Integer(0.into()));
    }
    Value::Dictionary(g)
}

/// A floating label (graph caption, xlabel, headlabel, taillabel) as a
/// text-only graphic placed where graphviz positioned it — no fill or border,
/// like a `plaintext` node.
fn floating_label_graphic(id: i64, lbl: &FloatingLabel) -> Value {
    let font = label_font(
        lbl.font_name.as_deref(),
        lbl.font_size.as_deref(),
        lbl.font_color.as_deref(),
    );

    let mut g = Dictionary::new();
    g.insert("Class".into(), Value::String("ShapedGraphic".into()));
    g.insert("ID".into(), Value::Integer(id.into()));
    g.insert(
        "Bounds".into(),
        Value::String(bounds(lbl.b.x, lbl.b.y, lbl.b.w, lbl.b.h)),
    );
    g.insert("Style".into(), node_style("", true, None, None, None));
    insert_label(&mut g, &lbl.text, &font);
    Value::Dictionary(g)
}

fn shaped_graphic(id: i64, b: NodeBox, v: &NodeViz<'_>, label: Option<&str>) -> Value {
    // Three outcomes: `plaintext` draws no outline (text only); a named shape
    // uses its OmniGraffle identifier (+ a vertical flip for inverted variants);
    // anything else (box, Msquare, unknown) is the plain default Rectangle.
    let text_only = is_text_only(v.shape);
    let named = (!text_only).then(|| Shape::from_dot(v.shape)).flatten();
    let og_shape = (!text_only).then(|| named.map_or("Rectangle", |(s, _)| s.to_omni()));
    let flip = named.is_some_and(|(_, flip)| flip);

    let mut g = Dictionary::new();
    g.insert("Class".into(), Value::String("ShapedGraphic".into()));
    g.insert("ID".into(), Value::Integer(id.into()));
    g.insert("Bounds".into(), Value::String(bounds(b.x, b.y, b.w, b.h)));
    g.insert("Magnets".into(), magnets());
    if let Some(s) = og_shape {
        g.insert("Shape".into(), Value::String(s.into()));
    }
    if flip {
        g.insert("VFlip".into(), Value::String("YES".into()));
    }
    g.insert(
        "Style".into(),
        node_style(v.style, text_only, v.fill, v.pen, v.pen_width),
    );
    if let Some(l) = label {
        insert_label(&mut g, l, &v.font);
    }
    Value::Dictionary(g)
}

/// One field of a `shape=record` node: a rectangle carrying just that cell's
/// text, styled like the parent node. Adjacent cells share edges, reproducing
/// the subdivided record box.
fn record_cell_graphic(id: i64, cell: &RecordCell, v: &NodeViz<'_>) -> Value {
    let mut g = Dictionary::new();
    g.insert("Class".into(), Value::String("ShapedGraphic".into()));
    g.insert("ID".into(), Value::Integer(id.into()));
    g.insert(
        "Bounds".into(),
        Value::String(bounds(cell.b.x, cell.b.y, cell.b.w, cell.b.h)),
    );
    g.insert("Shape".into(), Value::String("Rectangle".into()));
    g.insert("Style".into(), node_style(v.style, false, v.fill, v.pen, v.pen_width));
    if !cell.text.is_empty() {
        insert_label(&mut g, &cell.text, &v.font);
    }
    Value::Dictionary(g)
}

fn line_graphic(
    id: i64,
    tail_id: i64,
    head_id: i64,
    points: &[(f64, f64)],
    label: Option<&str>,
    v: &EdgeViz<'_>,
) -> Value {
    let mut g = Dictionary::new();
    g.insert("Class".into(), Value::String("LineGraphic".into()));
    g.insert("ID".into(), Value::Integer(id.into()));
    g.insert("Tail".into(), endpoint(tail_id));
    g.insert("Head".into(), endpoint(head_id));
    g.insert(
        "Points".into(),
        Value::Array(points.iter().map(|&p| Value::String(point(p))).collect()),
    );
    g.insert("Style".into(), line_style(v));
    if let Some(l) = label {
        insert_label(&mut g, l, &v.font);
    }
    Value::Dictionary(g)
}

/// Stroke width for a `style=bold` edge that gives no explicit `penwidth`
/// (graphviz draws bold at roughly twice the default pen).
const BOLD_WIDTH: f64 = 2.0;

/// Whether a graphviz `style` value (comma-separated keywords) contains `kw`.
fn has_kw(style: &str, kw: &str) -> bool {
    style.split(',').any(|s| s.trim() == kw)
}

/// Which ends of an edge draw an arrow, from graphviz's `dir`. An unset `dir`
/// defaults to `forward` for a digraph and `none` for an undirected graph.
fn arrow_ends(dir: Option<&str>, directed: bool) -> (bool, bool) {
    match dir {
        Some("none") => (false, false),
        Some("back") => (false, true),
        Some("both") => (true, true),
        Some("forward") => (true, false),
        _ => (directed, false),
    }
}

/// The OmniGraffle arrow identifier for one end of an edge. `"0"` is
/// OmniGraffle's own encoding for "no arrow" — written both when the end draws
/// nothing and for graphviz `arrowhead=none`/`arrowtail=none`.
fn arrow_id(on: bool, name: Option<&str>) -> &'static str {
    match (on, name) {
        (false, _) | (_, Some("none")) => "0",
        (_, other) => Arrow::from_dot(other.unwrap_or("normal")).to_omni(),
    }
}

/// Map graphviz line `style` keywords to an OmniGraffle stroke `Pattern` preset
/// index: `dashed` -> 1, `dotted` -> 2 (both observed in real documents), solid
/// -> `None`.
fn line_pattern(style: &str) -> Option<i64> {
    if has_kw(style, "dotted") {
        Some(2)
    } else if has_kw(style, "dashed") {
        Some(1)
    } else {
        None
    }
}

fn node_style(style: &str, text_only: bool, fill: Option<&str>, pen: Option<&str>, width: Option<f64>) -> Value {
    let has = |kw: &str| has_kw(style, kw);

    let mut fill_d = Dictionary::new();
    if text_only || !has("filled") {
        fill_d.insert("Draws".into(), Value::String("NO".into()));
    } else if let Some(c) = fill.and_then(color_value) {
        fill_d.insert("Color".into(), c);
    }

    let mut stroke_d = Dictionary::new();
    if text_only {
        stroke_d.insert("Draws".into(), Value::String("NO".into()));
    } else {
        if let Some(c) = pen.and_then(color_value) {
            stroke_d.insert("Color".into(), c);
        }
        if let Some(w) = width {
            stroke_d.insert("Width".into(), Value::Real(w));
        }
        if has("rounded") {
            stroke_d.insert("CornerRadius".into(), Value::Real(9.0));
        }
        if let Some(p) = line_pattern(style) {
            stroke_d.insert("Pattern".into(), Value::Integer(p.into()));
        }
    }

    let mut style_d = Dictionary::new();
    style_d.insert("fill".into(), Value::Dictionary(fill_d));
    style_d.insert("stroke".into(), Value::Dictionary(stroke_d));
    Value::Dictionary(style_d)
}

fn line_style(v: &EdgeViz<'_>) -> Value {
    let mut stroke = Dictionary::new();
    if let Some(c) = v.color.and_then(color_value) {
        stroke.insert("Color".into(), c);
    }
    if let Some(w) = v.pen_width {
        stroke.insert("Width".into(), Value::Real(w));
    }
    // Always written explicitly: an absent HeadArrow/TailArrow lets OmniGraffle
    // apply its own default, so `"0"` is the faithful encoding for "no arrow".
    stroke.insert("HeadArrow".into(), Value::String(v.head_arrow.into()));
    stroke.insert("TailArrow".into(), Value::String(v.tail_arrow.into()));
    if let Some(p) = v.pattern {
        stroke.insert("Pattern".into(), Value::Integer(p.into()));
    }

    let mut style_d = Dictionary::new();
    style_d.insert("stroke".into(), Value::Dictionary(stroke));
    Value::Dictionary(style_d)
}

fn endpoint(id: i64) -> Value {
    let mut d = Dictionary::new();
    d.insert("ID".into(), Value::Integer(id.into()));
    Value::Dictionary(d)
}

/// Insert a graphic's `Text` (RTF) and matching `FontInfo`.
fn insert_label(g: &mut Dictionary, label: &str, font: &FontViz<'_>) {
    let mut text = Dictionary::new();
    text.insert("Text".into(), Value::String(rtf(label, font)));
    g.insert("Text".into(), Value::Dictionary(text));
    g.insert("FontInfo".into(), font_info(font));
}

fn font_info(font: &FontViz<'_>) -> Value {
    let mut d = Dictionary::new();
    d.insert("Font".into(), Value::String(font.name.into()));
    d.insert("Size".into(), Value::Real(font.size));
    if let Some(c) = font.color.and_then(color_value) {
        d.insert("Color".into(), c);
    }
    Value::Dictionary(d)
}

/// The four edge-midpoint connection points OmniGraffle uses by default.
fn magnets() -> Value {
    Value::Array(vec![
        Value::String("{0, 1}".into()),
        Value::String("{0, -1}".into()),
        Value::String("{1, 0}".into()),
        Value::String("{-1, 0}".into()),
    ])
}

/// Running union of placed rectangles (top-left origin, points). Lets us frame
/// the canvas tightly around the diagram the way OmniGraffle's own files do,
/// instead of clipping to a fixed default page.
#[derive(Clone, Copy)]
struct Extent {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

impl Extent {
    fn new() -> Self {
        Self {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        }
    }

    fn add(&mut self, b: &NodeBox) {
        self.min_x = self.min_x.min(b.x);
        self.min_y = self.min_y.min(b.y);
        self.max_x = self.max_x.max(b.x + b.w);
        self.max_y = self.max_y.max(b.y + b.h);
    }

    /// `(origin_x, origin_y, width, height)`, or `None` until a rectangle is
    /// added. Floors the origin and ceils the far corner so the integer frame
    /// always contains the content (OmniGraffle's own files carry the same
    /// 1-2pt of slack).
    #[must_use]
    fn frame(&self) -> Option<(f64, f64, f64, f64)> {
        self.min_x.is_finite().then(|| {
            let ox = self.min_x.floor();
            let oy = self.min_y.floor();
            (ox, oy, self.max_x.ceil() - ox, self.max_y.ceil() - oy)
        })
    }
}

fn document(name: Option<&str>, graphics: Vec<Value>, extent: Extent) -> Value {
    let mut sheet = Dictionary::new();
    sheet.insert("GraphicsList".into(), Value::Array(graphics));

    // Frame the canvas tightly around the diagram. OmniGraffle's own files set
    // `CanvasSize` to the content's bounding-box size and `CanvasDimensionsOrigin`
    // to its top-left corner, with `CanvasOrigin` left at the {0,0} reference. An
    // empty graph has no content, so fall back to a default page size.
    sheet.insert("CanvasOrigin".into(), Value::String("{0, 0}".into()));
    if let Some((ox, oy, w, h)) = extent.frame() {
        sheet.insert("CanvasSize".into(), Value::String(point((w, h))));
        sheet.insert("CanvasDimensionsOrigin".into(), Value::String(point((ox, oy))));
    } else {
        sheet.insert("CanvasSize".into(), Value::String("{1152, 768}".into()));
    }
    // Canvas sizing mode. The enum is `0` = fixed (OmniGraffle's default when the
    // key is absent), `1` = flexible, `2` = infinite. We want infinite so a
    // graphviz layout of any size is never clipped to a fixed page; every sample
    // `.graffle` fixture uses `2`, confirming the value.
    sheet.insert("CanvasSizingMode".into(), Value::Integer(2.into()));
    if let Some(n) = name {
        sheet.insert("SheetTitle".into(), Value::String(n.into()));
    }

    let mut doc = Dictionary::new();
    doc.insert("GraphDocumentVersion".into(), Value::Integer(16.into()));
    doc.insert(
        "ApplicationVersion".into(),
        Value::Array(vec![
            Value::String("com.omnigroup.OmniGraffle7".into()),
            Value::String("205.56.3".into()),
        ]),
    );
    doc.insert("ReadOnly".into(), Value::String("NO".into()));
    doc.insert("Sheets".into(), Value::Array(vec![Value::Dictionary(sheet)]));
    Value::Dictionary(doc)
}

// ---- string / color encoders -----------------------------------------------

/// OmniGraffle geometry rectangle: `{{x, y}, {w, h}}`.
fn bounds(x: f64, y: f64, w: f64, h: f64) -> String {
    format!("{{{{{x}, {y}}}, {{{w}, {h}}}}}")
}

/// OmniGraffle point: `{x, y}`.
fn point((x, y): (f64, f64)) -> String {
    format!("{{{x}, {y}}}")
}

/// An OmniGraffle sRGB color dict, from a graphviz color spec.
fn color_value(spec: &str) -> Option<Value> {
    let (r, g, b) = parse_rgb(spec)?;
    let mut c = Dictionary::new();
    c.insert("r".into(), Value::Real(r));
    c.insert("g".into(), Value::Real(g));
    c.insert("b".into(), Value::Real(b));
    c.insert("space".into(), Value::String("srgb".into()));
    Some(Value::Dictionary(c))
}

/// Parse `#rgb`, `#rrggbb`, or a small set of named graphviz colors to 0..1
/// RGB.
fn parse_rgb(spec: &str) -> Option<(f64, f64, f64)> {
    let s = spec.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex);
    }
    let (r, g, b) = match s.to_ascii_lowercase().as_str() {
        "white" => (255, 255, 255),
        "black" => (0, 0, 0),
        "gray" | "grey" => (128, 128, 128),
        "lightgray" | "lightgrey" => (211, 211, 211),
        "darkgray" | "darkgrey" => (169, 169, 169),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "orange" => (255, 165, 0),
        "cyan" => (0, 255, 255),
        "magenta" => (255, 0, 255),
        "purple" => (128, 0, 128),
        "navy" => (0, 0, 128),
        "pink" => (255, 192, 203),
        "brown" => (165, 42, 42),
        _ => return None,
    };
    Some(rgb8(r, g, b))
}

/// Parse a 3- or 6-digit hex color body (no leading `#`).
fn parse_hex(hex: &str) -> Option<(f64, f64, f64)> {
    // Slicing below indexes by byte but assumes byte == char; a multibyte char
    // (e.g. `#é0`) would split a `char` boundary and panic. A hex body is ASCII.
    if !hex.is_ascii() {
        return None;
    }
    let (r, g, b) = match hex.len() {
        // #rgb: each nibble is doubled (`f` -> `ff`).
        3 => (
            u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?,
            u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?,
            u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?,
        ),
        6 => (
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ),
        _ => return None,
    };
    Some(rgb8(r, g, b))
}

fn rgb8(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    (f64::from(r) / 255.0, f64::from(g) / 255.0, f64::from(b) / 255.0)
}

/// Wrap plain text in a minimal RTF blob (OmniGraffle stores labels as RTF).
///
/// Carries the font (name + size) and, when set, a font color via a `colortbl`
/// entry. graphviz line breaks (`\n`/`\l`/`\r`) become RTF `\line`; RTF
/// metacharacters (`\`, `{`, `}`) are escaped.
fn rtf(text: &str, font: &FontViz<'_>) -> String {
    let mut s = String::from("{\\rtf1\\ansi\\ansicpg1252\\cocoartf2870\n");
    s.push_str("{\\fonttbl\\f0\\fnil\\fcharset0 ");
    s.push_str(font.name);
    s.push_str(";}\n");

    let color_index = match font.color.and_then(parse_rgb) {
        Some((r, g, b)) => {
            s.push_str(&format!(
                "{{\\colortbl;\\red{}\\green{}\\blue{};}}\n",
                to_u8(r),
                to_u8(g),
                to_u8(b)
            ));
            1
        }
        None => 0,
    };

    let half_points = (font.size * 2.0).round() as i64;
    s.push_str(&format!("\\f0\\fs{half_points} \\cf{color_index} "));

    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                Some('n' | 'l' | 'r') => {
                    chars.next();
                    s.push_str("\\line ");
                }
                _ => s.push_str("\\\\"),
            },
            '{' | '}' => {
                s.push('\\');
                s.push(c);
            }
            _ => s.push(c),
        }
    }
    s.push('}');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graffle::model::{Shape, is_text_only};
    use crate::graffle::test_support::graphics_list;

    fn font(size: f64, color: Option<&str>) -> FontViz<'_> {
        FontViz {
            name: "Helvetica",
            size,
            color,
        }
    }

    #[test]
    fn dot_to_graffle_is_valid_plist_with_expected_graphics() {
        let dot = br#"digraph g { a [label="Alpha"]; b; a -> b [label="to"]; }"#.to_vec();
        let data = DotData::try_from(dot).expect("dot parses");
        let graffle = GraffleData::try_from(data).expect("dot converts to graffle");
        let xml = graffle.to_plist_xml().expect("graffle serializes");

        // Re-parse to prove we emitted a structurally valid plist.
        let val = Value::from_reader(std::io::Cursor::new(xml.clone())).expect("valid plist xml");
        let sheet = val
            .as_dictionary()
            .and_then(|d| d.get("Sheets"))
            .and_then(Value::as_array)
            .and_then(|s| s.first())
            .and_then(Value::as_dictionary)
            .expect("Sheets[0] should be a dictionary");

        // 2 shapes + 1 line (no clusters in this graph).
        assert_eq!(graphics_list(xml).len(), 3);

        // Canvas must be infinite (mode 2), not OmniGraffle's fixed default.
        assert_eq!(
            sheet.get("CanvasSizingMode").and_then(Value::as_signed_integer),
            Some(2)
        );

        // And it must be framed around the content, not the hardcoded page: the
        // content-framing branch emits CanvasDimensionsOrigin; the empty-graph
        // fallback does not.
        assert!(sheet.get("CanvasDimensionsOrigin").is_some());
    }

    #[test]
    fn floating_label_becomes_a_text_only_caption() {
        let lbl = FloatingLabel {
            b: NodeBox {
                x: 10.0,
                y: 20.0,
                w: 300.0,
                h: 40.0,
            },
            text: "Diagram Title\\nsubtitle".into(),
            font_name: None,
            font_size: None,
            font_color: None,
        };
        let g = floating_label_graphic(7, &lbl);
        let d = g.as_dictionary().expect("graphic is a dict");

        assert_eq!(d.get("Class").and_then(Value::as_string), Some("ShapedGraphic"));
        // A caption is pure text: no shape outline.
        assert!(d.get("Shape").is_none(), "caption must not carry a Shape");

        // The title round-trips out of the RTF blob, and its `\n` became a
        // hard line break (so the subtitle lands on its own line).
        let blob = d
            .get("Text")
            .and_then(Value::as_dictionary)
            .and_then(|t| t.get("Text"))
            .and_then(Value::as_string)
            .expect("caption has Text");
        assert_eq!(crate::rtf::strip(blob).text, "Diagram Title\nsubtitle");
    }

    #[test]
    fn record_cell_is_a_bordered_rectangle_with_its_text() {
        let viz = NodeViz {
            label: "ignored",
            style: "",
            shape: "record",
            fill: None,
            pen: None,
            pen_width: None,
            font: font(14.0, None),
        };
        let cell = RecordCell {
            b: NodeBox {
                x: 0.0,
                y: 0.0,
                w: 40.0,
                h: 20.0,
            },
            text: "left".into(),
        };
        let d = record_cell_graphic(9, &cell, &viz);
        let d = d.as_dictionary().expect("graphic dict");
        assert_eq!(d.get("Shape").and_then(Value::as_string), Some("Rectangle"));
        let blob = d
            .get("Text")
            .and_then(Value::as_dictionary)
            .and_then(|t| t.get("Text"))
            .and_then(Value::as_string)
            .expect("cell has text");
        assert_eq!(crate::rtf::strip(blob).text, "left");

        // An empty field carries a cell box but no text.
        let empty = RecordCell {
            b: cell.b,
            text: String::new(),
        };
        let g = record_cell_graphic(10, &empty, &viz);
        assert!(g.as_dictionary().expect("graphic dict").get("Text").is_none());
    }

    #[test]
    fn parses_hex_and_named_colors() {
        assert_eq!(parse_rgb("#ff0000"), Some((1.0, 0.0, 0.0)));
        assert_eq!(parse_rgb("#f00"), Some((1.0, 0.0, 0.0))); // 3-digit expands
        assert_eq!(parse_rgb("white"), Some((1.0, 1.0, 1.0)));
        assert!(parse_rgb("orange").is_some());
        assert_eq!(parse_rgb("chartreuse"), None); // not in our small table
        assert_eq!(parse_rgb("#ggg"), None);
        // A non-ASCII body must yield None, not panic on a split char boundary.
        // `é` is 2 bytes, so each of these has a byte-len that hits a slicing arm
        // (3 and 6) while a slice edge lands inside the `é`.
        assert_eq!(parse_rgb("#é0"), None);
        assert_eq!(parse_rgb("#aabéb"), None);
    }

    #[test]
    fn maps_known_shapes() {
        let omni = |dot: &str| Shape::from_dot(dot).map(|(s, _)| s.to_omni());
        assert_eq!(omni("cylinder"), Some("Cylinder"));
        assert_eq!(omni("note"), Some("NoteShape"));
        // `box`/`Msquare`/`plaintext` have no *named* shape (the writer emits the
        // default Rectangle for a drawn box, nothing for plaintext).
        assert_eq!(omni("box"), None);
        assert_eq!(omni("plaintext"), None);
        assert!(is_text_only("plaintext") && !is_text_only("box"));
        // Expanded polygon vocabulary.
        assert_eq!(omni("diamond"), Some("Diamond"));
        assert_eq!(omni("triangle"), Some("VerticalTriangle"));
        assert_eq!(omni("ellipse"), Some("Circle"));
        assert_eq!(omni("circle"), Some("Circle"));
        assert_eq!(omni("pentagon"), Some("Pentagon"));
        assert_eq!(omni("hexagon"), Some("Hexagon"));
        assert_eq!(omni("octagon"), Some("Octagon"));
        assert_eq!(omni("parallelogram"), Some("Parallelogram"));
        // OmniGraffle's identifier is misspelled; lock it against regression.
        assert_eq!(omni("trapezium"), Some("Trapazoid"));
        assert_eq!(omni("house"), Some("House"));
        assert_eq!(omni("box3d"), Some("Cube"));
        assert_eq!(omni("star"), Some("Star"));
        // Decorated / multi-periphery variants fold onto their base shape.
        assert_eq!(omni("Mdiamond"), Some("Diamond"));
        assert_eq!(omni("Mcircle"), Some("Circle"));
        assert_eq!(omni("doublecircle"), Some("Circle"));
        assert_eq!(omni("doubleoctagon"), Some("Octagon"));
        assert_eq!(omni("tripleoctagon"), Some("Octagon"));
        // `Msquare` has no OmniGraffle decoration, so it shares the box fallback.
        assert_eq!(omni("Msquare"), None);
        // Inverted variants share the base shape; the flip is tracked separately.
        assert_eq!(omni("invtriangle"), Some("VerticalTriangle"));
        assert_eq!(omni("invhouse"), Some("House"));
        assert_eq!(omni("invtrapezium"), Some("Trapazoid"));
        assert!(Shape::from_dot("invtriangle").is_some_and(|(_, flip)| flip));
        assert!(Shape::from_dot("triangle").is_some_and(|(_, flip)| !flip));
    }

    #[test]
    fn rtf_encodes_font_size_and_color() {
        // 11pt -> \fs22, no color table -> \cf0.
        let plain = rtf("hi", &font(11.0, None));
        assert!(plain.contains("\\fs22"), "{plain}");
        assert!(plain.contains("\\cf0"), "{plain}");
        assert!(!plain.contains("\\colortbl"), "{plain}");

        // With a color: a colortbl entry appears and the run uses \cf1.
        let colored = rtf("hi", &font(9.0, Some("#ff0000")));
        assert!(colored.contains("\\fs18"), "{colored}");
        assert!(colored.contains("\\red255\\green0\\blue0"), "{colored}");
        assert!(colored.contains("\\cf1"), "{colored}");
    }

    #[test]
    fn rtf_writer_and_reader_round_trip() {
        // A DOT-style `\n` (backslash-n) survives writer -> reader as a real
        // newline; the colored font's size and color come back too.
        let blob = rtf("a\\nb", &font(12.0, Some("#ff0000")));
        let back = crate::rtf::strip(&blob);
        assert_eq!(back.text, "a\nb");
        assert_eq!(back.size, Some(12.0));
        assert_eq!(back.color.as_deref(), Some("#ff0000"));
        assert_eq!(back.font.as_deref(), Some("Helvetica"));
    }
}
