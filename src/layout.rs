//! Layout via graphviz.
//!
//! We shell out to `dot -Tjson`, which yields node positions *and* cluster
//! bounding boxes (computed with proper label margins) in a single document.
//! graphviz is the whole point of the project. If `dot` isn't installed we
//! report that distinctly so the caller can fall back to a plain grid (and no
//! clusters). Other graphviz failures are surfaced as conversion errors.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, ExitStatus, Stdio};

use serde_json::Value as Json;
use thiserror::Error;

/// A laid-out rectangle in OmniGraffle coordinates: top-left corner, top-left
/// origin, units in points.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NodeBox {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

/// A graphviz cluster (subgraph), rendered as a background rectangle.
#[derive(Clone, Debug)]
pub(crate) struct Cluster {
    pub(crate) b: NodeBox,
    pub(crate) label: Option<String>,
    pub(crate) style: Option<String>,
    pub(crate) fill: Option<String>,
    pub(crate) pen: Option<String>,
    pub(crate) font_name: Option<String>,
    /// Label font size in points, parsed here at the graphviz boundary.
    pub(crate) font_size: Option<f64>,
    pub(crate) font_color: Option<String>,
}

/// A floating text label graphviz positioned for us — the graph caption, or a
/// node/edge `xlabel`, `headlabel`, or `taillabel`. Rendered as text with no
/// shape; the font triple mirrors [`Cluster`]'s.
#[derive(Clone, Debug)]
pub(crate) struct FloatingLabel {
    pub(crate) b: NodeBox,
    pub(crate) text: String,
    pub(crate) font_name: Option<String>,
    /// Label font size in points, parsed here at the graphviz boundary.
    pub(crate) font_size: Option<f64>,
    pub(crate) font_color: Option<String>,
}

/// One field of a `shape=record` node: a sub-rectangle of the node holding the
/// text graphviz rendered into it.
#[derive(Clone, Debug)]
pub(crate) struct RecordCell {
    pub(crate) b: NodeBox,
    pub(crate) text: String,
}

/// A node whose label graphviz rendered as structured content rather than a
/// single string. Plain nodes have no entry.
pub(crate) enum NodeLabel {
    /// `shape=record`/`Mrecord`: one box + text per field.
    Record(Vec<RecordCell>),
    /// An HTML-like label. Its text runs are emitted as floating labels (see
    /// [`Layout::labels`]); the node itself draws no inline label.
    Html,
}

/// An edge keyed by its two endpoint node names.
type EdgeKey = (String, String);
/// A polyline of waypoints in OmniGraffle coordinates.
type Polyline = Vec<(f64, f64)>;

pub(crate) struct Layout {
    pub(crate) nodes: HashMap<String, NodeBox>,
    pub(crate) clusters: Vec<Cluster>,
    pub(crate) labels: Vec<FloatingLabel>,
    pub(crate) node_labels: HashMap<String, NodeLabel>,
    /// Routed edge splines, keyed by tail node name then head node name (two
    /// levels so callers can look up by `&str` without building an owned
    /// key): on-curve waypoints in OmniGraffle coords, sampled from
    /// graphviz's bezier. Parallel edges between the same pair collapse to
    /// one entry (last wins).
    pub(crate) edges: HashMap<String, HashMap<String, Polyline>>,
}

const PT_PER_INCH: f64 = 72.0;

#[derive(Debug, Error)]
pub(crate) enum GraphvizError {
    /// `dot` is not installed; callers may choose a layout fallback.
    #[error("graphviz `dot` was not found on PATH")]
    NotFound,

    /// The `dot` process could not be started for a reason other than absence.
    #[error("failed to start graphviz `dot`: {0}")]
    Spawn(#[source] std::io::Error),

    /// DOT source could not be written to graphviz's stdin.
    #[error("failed to write DOT source to graphviz `dot`: {0}")]
    Write(#[source] std::io::Error),

    /// Waiting for graphviz failed.
    #[error("failed to wait for graphviz `dot`: {0}")]
    Wait(#[source] std::io::Error),

    /// Graphviz rejected the input or failed internally.
    #[error("graphviz `dot -Tjson` exited with {status}: {stderr}")]
    Failed { status: ExitStatus, stderr: String },

    /// Graphviz succeeded but did not produce parseable JSON.
    #[error("graphviz `dot -Tjson` produced invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Run `dot -Tjson` over the DOT source and extract node boxes and clusters.
pub(crate) fn graphviz_layout(dot_source: &str) -> Result<Layout, GraphvizError> {
    let stdout = run_dot(dot_source, "-Tjson")?;
    let json: Json = serde_json::from_slice(&stdout)?;
    Ok(parse_json(&json))
}

fn run_dot(source: &str, format: &str) -> Result<Vec<u8>, GraphvizError> {
    let mut child = match Command::new("dot")
        .arg(format)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(GraphvizError::NotFound),
        Err(err) => return Err(GraphvizError::Spawn(err)),
    };
    // Closing stdin (the handle drops at the end of this statement) signals EOF.
    let mut stdin = child.stdin.take().ok_or_else(|| {
        GraphvizError::Write(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "graphviz stdin was not available",
        ))
    })?;
    stdin.write_all(source.as_bytes()).map_err(GraphvizError::Write)?;
    drop(stdin);

    let out = child.wait_with_output().map_err(GraphvizError::Wait)?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stderr = if stderr.is_empty() {
            "no stderr output".to_string()
        } else {
            stderr
        };
        Err(GraphvizError::Failed {
            status: out.status,
            stderr,
        })
    }
}

/// graphviz coordinates are points with a bottom-left origin; we flip Y against
/// the drawing height (root `bb` upper-right Y) to OmniGraffle's top-left one.
fn parse_json(d: &Json) -> Layout {
    let height = d
        .get("bb")
        .and_then(Json::as_str)
        .and_then(parse_floats::<4>)
        .map_or(0.0, |[_, _, _, ury]| ury);

    let mut nodes = HashMap::new();
    let mut clusters = Vec::new();
    let mut labels = Vec::new();
    let mut node_labels = HashMap::new();

    // The graph-level caption, if any (`extend` pushes 0 or 1).
    labels.extend(parse_graph_label(d, height));

    let objects = d.get("objects").and_then(Json::as_array);
    for o in objects.into_iter().flatten() {
        // A node (or cluster) may also carry an external `xlabel`.
        labels.extend(external_label(o, "xlabel", "xlp", height));
        if let Some(name) = o.get("name").and_then(Json::as_str)
            && let Some([cx, cy]) = o.get("pos").and_then(Json::as_str).and_then(parse_floats::<2>)
        {
            let w = str_f64(o.get("width")).unwrap_or(1.0) * PT_PER_INCH;
            let h = str_f64(o.get("height")).unwrap_or(0.5) * PT_PER_INCH;
            nodes.insert(
                name.to_string(),
                NodeBox {
                    x: cx - w / 2.0,
                    y: (height - cy) - h / 2.0,
                    w,
                    h,
                },
            );

            // A record decomposes into cells; an HTML-like label decomposes
            // into floating text runs (which the node then renders no inline
            // label for).
            if let Some(cells) = record_cells(o, height) {
                node_labels.insert(name.to_string(), NodeLabel::Record(cells));
            } else if is_html_label(o) {
                labels.extend(html_text_labels(o, height));
                node_labels.insert(name.to_string(), NodeLabel::Html);
            }
        } else if let Some(name) = o.get("name").and_then(Json::as_str)
            && name.starts_with("cluster")
            && let Some([llx, lly, urx, ury]) = o.get("bb").and_then(Json::as_str).and_then(parse_floats::<4>)
        {
            clusters.push(Cluster {
                b: NodeBox {
                    x: llx,
                    y: height - ury,
                    w: urx - llx,
                    h: ury - lly,
                },
                label: string_attr(o, "label"),
                style: string_attr(o, "style"),
                fill: string_attr(o, "fillcolor"),
                pen: string_attr(o, "color"),
                font_name: string_attr(o, "fontname"),
                font_size: str_f64(o.get("fontsize")),
                font_color: string_attr(o, "fontcolor"),
            });
        }
    }

    // Edges carry their routed spline plus any end / external labels. Edge
    // `tail`/`head` are indices into `objects` (gvid order), so map them through
    // the object names.
    let names_by_index: Vec<&str> = d
        .get("objects")
        .and_then(Json::as_array)
        .map(|objs| {
            objs.iter()
                .map(|o| o.get("name").and_then(Json::as_str).unwrap_or(""))
                .collect()
        })
        .unwrap_or_default();
    let mut edges: HashMap<String, HashMap<String, Polyline>> = HashMap::new();
    for e in d.get("edges").and_then(Json::as_array).into_iter().flatten() {
        for (text_key, pos_key) in [("xlabel", "xlp"), ("headlabel", "head_lp"), ("taillabel", "tail_lp")] {
            labels.extend(external_label(e, text_key, pos_key, height));
        }
        if let Some(((tail, head), points)) = edge_spline(e, &names_by_index, height) {
            edges.entry(tail).or_default().insert(head, points);
        }
    }

    Layout {
        nodes,
        clusters,
        labels,
        node_labels,
        edges,
    }
}

/// On-curve waypoints to sample per cubic bezier segment. Dense enough that the
/// polyline tracks graphviz's curve whether OmniGraffle draws its waypoints
/// straight or smoothed.
const SAMPLES_PER_SEGMENT: usize = 8;

/// An edge's routed path: graphviz draws the spline as a `b` (bezier) op in
/// `_draw_`, a control polygon of `1 + 3k` points. We sample it to on-curve
/// waypoints and flip Y like everything else. Keyed by the endpoint node names
/// resolved through `names` (the `objects` array, gvid order). `None` when the
/// edge has no bezier op or an endpoint index is out of range.
fn edge_spline(e: &Json, names: &[&str], height: f64) -> Option<(EdgeKey, Polyline)> {
    let endpoint = |key| {
        e.get(key)
            .and_then(Json::as_u64)
            .and_then(|i| names.get(i as usize))
            .copied()
    };
    let (tail, head) = (endpoint("tail")?, endpoint("head")?);
    if tail.is_empty() || head.is_empty() {
        return None;
    }
    let control: Vec<(f64, f64)> = e
        .get("_draw_")
        .and_then(Json::as_array)?
        .iter()
        .find(|op| op.get("op").and_then(Json::as_str) == Some("b"))
        .and_then(|op| op.get("points").and_then(Json::as_array))?
        .iter()
        .filter_map(|p| {
            let p = p.as_array()?;
            Some((p.first()?.as_f64()?, p.get(1)?.as_f64()?))
        })
        .collect();
    if control.len() < 2 {
        return None;
    }
    let points = sample_spline(&control, SAMPLES_PER_SEGMENT)
        .into_iter()
        .map(|(x, y)| (x, height - y))
        .collect();
    Some(((tail.to_string(), head.to_string()), points))
}

/// A point on the cubic bezier `seg` (start, two controls, end) at `t` in [0,
/// 1].
fn cubic(seg: &[(f64, f64); 4], t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    let at = |sel: fn(&(f64, f64)) -> f64| {
        w[0] * sel(&seg[0]) + w[1] * sel(&seg[1]) + w[2] * sel(&seg[2]) + w[3] * sel(&seg[3])
    };
    (at(|p| p.0), at(|p| p.1))
}

/// Sample a graphviz bezier control polygon (`1 + 3k` points: start, then three
/// per cubic segment) into on-curve waypoints, `per_seg` samples per segment.
/// Returns the points unchanged when there is no full cubic to sample.
fn sample_spline(control: &[(f64, f64)], per_seg: usize) -> Vec<(f64, f64)> {
    if control.len() < 4 || per_seg == 0 {
        return control.to_vec();
    }
    let mut out = vec![control[0]];
    let mut i = 0;
    while i + 3 < control.len() {
        let seg = [control[i], control[i + 1], control[i + 2], control[i + 3]];
        for s in 1..=per_seg {
            out.push(cubic(&seg, s as f64 / per_seg as f64));
        }
        i += 3;
    }
    out
}

/// The graph-level label (diagram title/caption), positioned by graphviz via
/// its `lp` (label center) and `lwidth`/`lheight` (inches). `None` when the
/// graph has no label. Y is flipped against `height` to OmniGraffle's top-left
/// origin — the same transform nodes get.
fn parse_graph_label(d: &Json, height: f64) -> Option<FloatingLabel> {
    let text = string_attr(d, "label").filter(|s| !s.is_empty())?;
    let [cx, cy] = d.get("lp").and_then(Json::as_str).and_then(parse_floats::<2>)?;
    let w = str_f64(d.get("lwidth")).unwrap_or(0.0) * PT_PER_INCH;
    let h = str_f64(d.get("lheight")).unwrap_or(0.0) * PT_PER_INCH;
    Some(FloatingLabel {
        b: NodeBox {
            x: cx - w / 2.0,
            y: (height - cy) - h / 2.0,
            w,
            h,
        },
        text,
        font_name: string_attr(d, "fontname"),
        font_size: str_f64(d.get("fontsize")),
        font_color: string_attr(d, "fontcolor"),
    })
}

/// Nominal point size for estimating a label box graphviz didn't dimension.
const NOMINAL_LABEL_PT: f64 = 14.0;

/// A floating label graphviz positioned by a single center point (`xlabel`,
/// `headlabel`, `taillabel`) but gave no size for. We estimate the box from the
/// text at a nominal font size and center it on the point, flipping Y like
/// nodes. `None` when the text or position is missing/empty.
fn external_label(o: &Json, text_key: &str, pos_key: &str, height: f64) -> Option<FloatingLabel> {
    let text = string_attr(o, text_key).filter(|s| !s.is_empty())?;
    let [cx, cy] = o.get(pos_key).and_then(Json::as_str).and_then(parse_floats::<2>)?;
    // graphviz carries line breaks as a literal `\n`; size for the widest line.
    let cols = text.split("\\n").map(|l| l.chars().count()).max().unwrap_or(0) as f64;
    let rows = text.split("\\n").count().max(1) as f64;
    let w = (cols * NOMINAL_LABEL_PT * 0.6).max(NOMINAL_LABEL_PT);
    let h = rows * NOMINAL_LABEL_PT * 1.3;
    Some(FloatingLabel {
        b: NodeBox {
            x: cx - w / 2.0,
            y: (height - cy) - h / 2.0,
            w,
            h,
        },
        text,
        font_name: string_attr(o, "fontname"),
        font_size: str_f64(o.get("fontsize")),
        font_color: string_attr(o, "fontcolor"),
    })
}

/// A text run from a graphviz label-draw op list (`_ldraw_`): a `T` op plus the
/// font/color state set by the preceding `F`/`c` ops. Positions are global
/// graphviz coordinates (bottom-left origin); `cy` is the text baseline.
struct TextRun {
    text: String,
    cx: f64,
    cy: f64,
    width: f64,
    size: f64,
    face: Option<String>,
    color: Option<String>,
}

/// Walk a node's `_ldraw_` ops, pairing each text run with the active font and
/// color. graphviz uses these for record fields and HTML-like label content.
fn ldraw_runs(o: &Json) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut size = NOMINAL_LABEL_PT;
    let mut face: Option<String> = None;
    let mut color: Option<String> = None;

    for op in o.get("_ldraw_").and_then(Json::as_array).into_iter().flatten() {
        match op.get("op").and_then(Json::as_str) {
            Some("F") => {
                size = op.get("size").and_then(Json::as_f64).unwrap_or(size);
                face = op.get("face").and_then(Json::as_str).map(str::to_string);
            }
            // `c` sets the pen color; graphviz's default black reads as "auto".
            Some("c") => {
                color = op
                    .get("color")
                    .and_then(Json::as_str)
                    .filter(|c| c.starts_with('#') && *c != "#000000")
                    .map(str::to_string);
            }
            Some("T") => {
                let pt = op.get("pt").and_then(Json::as_array);
                let cx = pt.and_then(|p| p.first()).and_then(Json::as_f64);
                let cy = pt.and_then(|p| p.get(1)).and_then(Json::as_f64);
                if let Some(text) = op.get("text").and_then(Json::as_str)
                    && let (Some(cx), Some(cy)) = (cx, cy)
                {
                    runs.push(TextRun {
                        text: text.to_string(),
                        cx,
                        cy,
                        width: op.get("width").and_then(Json::as_f64).unwrap_or(0.0),
                        size,
                        face: face.clone(),
                        color: color.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    runs
}

/// Decompose a `shape=record` node into its field cells: one rectangle per
/// `rects` entry, paired with the `_ldraw_` text run that falls inside it.
/// `None` when the node has no `rects` (i.e. isn't a record).
fn record_cells(o: &Json, height: f64) -> Option<Vec<RecordCell>> {
    let rects = o.get("rects").and_then(Json::as_str)?;
    let runs = ldraw_runs(o);
    let cells = rects
        .split_whitespace()
        .filter_map(|r| {
            let [llx, lly, urx, ury] = parse_floats::<4>(r)?;
            let text = runs
                .iter()
                .find(|t| t.cx >= llx && t.cx <= urx && t.cy >= lly && t.cy <= ury)
                .map(|t| t.text.clone())
                .unwrap_or_default();
            Some(RecordCell {
                b: NodeBox {
                    x: llx,
                    y: height - ury,
                    w: urx - llx,
                    h: ury - lly,
                },
                text,
            })
        })
        .collect();
    Some(cells)
}

/// Whether graphviz rendered this node from an HTML-like label — the only kind
/// whose JSON `label` is angle-bracket markup. We recover its text from
/// `_ldraw_` runs rather than the (markup) label string.
fn is_html_label(o: &Json) -> bool {
    o.get("label")
        .and_then(Json::as_str)
        .is_some_and(|l| l.trim_start().starts_with('<'))
}

/// Each `_ldraw_` text run of an HTML-like label as a floating text label. We
/// recover the text, position, and per-run font/color but not the table's
/// borders — content survives, the grid does not.
fn html_text_labels(o: &Json, height: f64) -> Vec<FloatingLabel> {
    ldraw_runs(o)
        .into_iter()
        .map(|r| {
            let w = r.width.max(NOMINAL_LABEL_PT);
            let h = r.size * 1.3;
            // `cy` is the baseline; nudge up to the run's visual center.
            let center_y = r.cy + r.size * 0.35;
            FloatingLabel {
                b: NodeBox {
                    x: r.cx - w / 2.0,
                    y: (height - center_y) - h / 2.0,
                    w,
                    h,
                },
                text: r.text,
                font_name: r.face,
                font_size: Some(r.size),
                font_color: r.color,
            }
        })
        .collect()
}

fn str_f64(v: Option<&Json>) -> Option<f64> {
    v?.as_str()?.parse().ok()
}

fn string_attr(o: &Json, key: &str) -> Option<String> {
    o.get(key).and_then(Json::as_str).map(str::to_string)
}

/// Parse exactly `N` comma-separated floats (e.g. a `pos` pair or a `bb` quad).
fn parse_floats<const N: usize>(s: &str) -> Option<[f64; N]> {
    let mut out = [0.0; N];
    let mut field = s.split(',');
    for slot in &mut out {
        *slot = field.next()?.trim().parse().ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_label_flips_y_and_scales_inches_to_points() {
        // Shaped like graphviz's `-Tjson` root: lp is the label center in
        // bottom-left space; lwidth/lheight are inches; the title keeps its
        // literal `\n`.
        let d = serde_json::json!({
            "label": "Title\\nsubtitle",
            "lp": "894.75,703",
            "lwidth": "6.27",
            "lheight": "0.58",
            "fontname": "Helvetica",
            "fontsize": "14",
        });
        let gl = parse_graph_label(&d, 728.0).expect("label present");

        assert_eq!(gl.text, "Title\\nsubtitle");
        assert_eq!(gl.font_name.as_deref(), Some("Helvetica"));
        // Size in points: inches * 72.
        assert!((gl.b.w - 6.27 * PT_PER_INCH).abs() < 1e-9);
        assert!((gl.b.h - 0.58 * PT_PER_INCH).abs() < 1e-9);
        // Y flips against height: center 728 - 703 = 25, then back up half the
        // box; x is the center minus half the width.
        assert!((gl.b.x - (894.75 - 6.27 * PT_PER_INCH / 2.0)).abs() < 1e-9);
        assert!((gl.b.y - (25.0 - 0.58 * PT_PER_INCH / 2.0)).abs() < 1e-9);
    }

    #[test]
    fn no_graph_label_when_unlabeled() {
        let d = serde_json::json!({ "name": "G", "bb": "0,0,100,100" });
        assert!(parse_graph_label(&d, 100.0).is_none());
        // An explicit empty label is also "no caption".
        let empty = serde_json::json!({ "label": "", "lp": "1,1" });
        assert!(parse_graph_label(&empty, 100.0).is_none());
    }

    #[test]
    fn graphviz_failure_is_not_a_fallback() {
        let err = match graphviz_layout("digraph { a [label=<unterminated> }") {
            Ok(_) => panic!("invalid graphviz input should not produce a layout"),
            Err(GraphvizError::NotFound) => return,
            Err(err) => err,
        };

        match err {
            GraphvizError::Failed { stderr, .. } => assert!(!stderr.is_empty()),
            other => panic!("expected graphviz process failure, got {other}"),
        }
    }

    #[test]
    fn collects_caption_external_head_and_tail_labels() {
        // A `-Tjson`-shaped doc: a graph caption, a node `xlabel`, and an edge
        // carrying xlabel + headlabel + taillabel (plus a main `label` that
        // rides the edge attrs, NOT this path).
        let d = serde_json::json!({
            "bb": "0,0,100,200",
            "label": "Caption",
            "lp": "50,190",
            "lwidth": "1.0",
            "lheight": "0.5",
            "objects": [
                { "name": "a", "pos": "50,150", "width": "1", "height": "0.5",
                  "xlabel": "node-x", "xlp": "20,150" },
                { "name": "b", "pos": "50,50", "width": "1", "height": "0.5" },
            ],
            "edges": [
                { "tail": 0, "head": 1, "label": "mid",
                  "xlabel": "edge-x", "xlp": "40,100",
                  "headlabel": "H", "head_lp": "55,60",
                  "taillabel": "T", "tail_lp": "55,140" },
            ],
        });
        let layout = parse_json(&d);
        let texts: Vec<&str> = layout.labels.iter().map(|l| l.text.as_str()).collect();

        // Caption + node xlabel + edge xlabel + head + tail = 5.
        assert_eq!(layout.labels.len(), 5, "{texts:?}");
        for want in ["Caption", "node-x", "edge-x", "H", "T"] {
            assert!(texts.contains(&want), "missing {want}: {texts:?}");
        }
        // The main edge label is not a floating label.
        assert!(!texts.contains(&"mid"));
        assert!(layout.nodes.contains_key("a") && layout.nodes.contains_key("b"));
    }

    #[test]
    fn record_cells_pair_rects_with_their_text() {
        // Two side-by-side cells; each `_ldraw_` text run sits inside its rect.
        let o = serde_json::json!({
            "rects": "0,0,40,20 40,0,90,20",
            "_ldraw_": [
                { "op": "F", "size": 14.0, "face": "Times-Roman" },
                { "op": "T", "pt": [20.0, 8.0], "text": "left", "width": 18.0 },
                { "op": "T", "pt": [65.0, 8.0], "text": "right", "width": 22.0 },
            ],
        });
        let cells = record_cells(&o, 20.0).expect("has rects");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].text, "left");
        assert_eq!(cells[1].text, "right");
        assert!((cells[0].b.x - 0.0).abs() < 1e-9 && (cells[0].b.w - 40.0).abs() < 1e-9);
        // No `rects` -> not a record.
        assert!(record_cells(&serde_json::json!({ "pos": "0,0" }), 20.0).is_none());
    }

    #[test]
    fn sample_spline_evaluates_on_curve_points() {
        // A symmetric arch: control polygon (0,0)-(0,3)-(3,3)-(3,0). The cubic at
        // t=0.5 is the analytic midpoint (1.5, 2.25) — an interior control point,
        // by contrast, would be off the curve.
        let pts = sample_spline(&[(0.0, 0.0), (0.0, 3.0), (3.0, 3.0), (3.0, 0.0)], 2);
        assert_eq!(pts.len(), 3); // start + 2 samples for one segment
        assert_eq!(pts[0], (0.0, 0.0));
        assert!((pts[1].0 - 1.5).abs() < 1e-9 && (pts[1].1 - 2.25).abs() < 1e-9);
        assert!((pts[2].0 - 3.0).abs() < 1e-9 && pts[2].1.abs() < 1e-9);
        // Too few control points to form a cubic: returned unchanged.
        assert_eq!(
            sample_spline(&[(0.0, 0.0), (1.0, 1.0)], 8),
            vec![(0.0, 0.0), (1.0, 1.0)]
        );
    }

    #[test]
    fn edge_spline_samples_and_flips_y() {
        // tail/head index into `objects`; the `_draw_` `b` op is the bezier.
        let d = serde_json::json!({
            "bb": "0,0,100,100",
            "objects": [
                { "name": "a", "pos": "10,80", "width": "1", "height": "0.5" },
                { "name": "b", "pos": "10,20", "width": "1", "height": "0.5" },
            ],
            "edges": [
                { "tail": 0, "head": 1,
                  "_draw_": [ { "op": "b", "points": [[10,80],[10,60],[10,40],[10,20]] } ] },
            ],
        });
        let layout = parse_json(&d);
        let spline = layout
            .edges
            .get("a")
            .and_then(|by_head| by_head.get("b"))
            .expect("a->b spline");
        // One cubic segment -> 1 + 8 sampled waypoints, more than a straight pair.
        assert!(spline.len() > 2, "got {} points", spline.len());
        // Ends are the bezier endpoints, Y flipped against height 100.
        assert_eq!(spline.first().copied(), Some((10.0, 20.0)));
        assert_eq!(spline.last().copied(), Some((10.0, 80.0)));
        // The line is vertical, so every waypoint keeps x = 10.
        assert!(spline.iter().all(|p| (p.0 - 10.0).abs() < 1e-9));
    }

    #[test]
    fn html_label_text_runs_become_floating_labels() {
        let o = serde_json::json!({
            "label": "<table><tr><td>A</td></tr></table>",
            "_ldraw_": [
                { "op": "F", "size": 14.0, "face": "Times-Roman" },
                { "op": "T", "pt": [10.0, 5.0], "text": "A", "width": 9.0 },
            ],
        });
        assert!(is_html_label(&o));
        let labels = html_text_labels(&o, 20.0);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].text, "A");
        assert_eq!(labels[0].font_name.as_deref(), Some("Times-Roman"));
    }
}
