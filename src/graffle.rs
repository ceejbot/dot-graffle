//! The OmniGraffle side of the conversion.
//!
//! [`GraffleData`] owns the root plist document. The two directions live in
//! submodules — [`write`](mod@write) (dot -> graffle) and [`read`] (graffle ->
//! [`model`]) — sharing the [`model`] graph types and its `Shape`/`Arrow`
//! vocabulary.

use std::io::{Cursor, Read};

use plist::Value;

use crate::error::DotGraffleError;

mod model;
mod read;
mod write;

// The recovered-graph types are the public face of this module: `crate::dotviz`
// renders them back to DOT text.
pub(crate) use model::{GCluster, GEdge, GModel, GNode};

pub(crate) struct GraffleData {
    /// The root plist dictionary, ready to serialize.
    doc: Value,
}

impl GraffleData {
    /// Serialize to a flat XML property list (UTF-8 bytes).
    pub(crate) fn to_plist_xml(&self) -> Result<Vec<u8>, DotGraffleError> {
        let mut buf = Vec::new();
        self.doc.to_writer_xml(&mut buf)?;
        Ok(buf)
    }

    /// Walk the plist into a flat, DOT-shaped [`GModel`]. Missing or malformed
    /// structure yields an empty graph rather than an error.
    #[must_use]
    pub(crate) fn to_model(&self) -> GModel {
        read::build_model(&self.doc)
    }
}

/// graffle -> dot, step 1: read the bytes into the OmniGraffle plist document.
///
/// A `.graffle` is normally a zip bundle holding `data.plist`; we also accept a
/// bare plist (what our own dot -> graffle emitter writes), so the field name
/// `doc` means the same thing in both directions.
impl TryFrom<Vec<u8>> for GraffleData {
    type Error = DotGraffleError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        // `from_reader` auto-detects binary vs XML plists.
        let doc = Value::from_reader(Cursor::new(plist_bytes(bytes, MAX_PLIST_BYTES)?))?;
        Ok(GraffleData { doc })
    }
}

/// The most we'll decompress out of a `.graffle`'s `data.plist`. Real documents
/// are a few MB at most; the cap exists only to bound a hostile archive.
const MAX_PLIST_BYTES: u64 = 128 << 20; // 128 MiB

/// Extract the `data.plist` from a `.graffle` zip, or pass a bare plist through
/// unchanged (what our own emitter writes). Refuses a `data.plist` that
/// inflates past `max`: a crafted archive could declare a tiny compressed entry
/// that decompresses to gigabytes, so we reject on the declared size and again
/// on the capped read in case the header lies.
fn plist_bytes(bytes: Vec<u8>, max: u64) -> Result<Vec<u8>, DotGraffleError> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return Ok(bytes);
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    let entry = zip.by_name("data.plist")?;
    if entry.size() > max {
        return Err(DotGraffleError::OversizedArchive(max));
    }
    let mut buf = Vec::new();
    entry.take(max).read_to_end(&mut buf)?;
    if buf.len() as u64 >= max {
        return Err(DotGraffleError::OversizedArchive(max));
    }
    Ok(buf)
}

// ---- shared low-level plist helpers ----------------------------------------

/// Coerce a plist number (real, integer, or numeric string) to `f64`.
fn num(v: &Value) -> Option<f64> {
    v.as_real()
        .or_else(|| v.as_signed_integer().map(|i| i as f64))
        .or_else(|| v.as_string().and_then(|s| s.parse().ok()))
}

/// A 0..1 color component to an 8-bit channel.
fn to_u8(v: f64) -> u8 {
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Test-only helpers shared across the submodules' inline test suites.
#[cfg(test)]
pub(crate) mod test_support {
    use plist::{Dictionary, Value};

    use super::GraffleData;
    use crate::dotviz::DotData;

    /// The `Sheets[0].GraphicsList` of a serialized `.graffle` plist.
    pub(crate) fn graphics_list(xml: Vec<u8>) -> Vec<Value> {
        Value::from_reader(std::io::Cursor::new(xml))
            .expect("valid plist xml")
            .as_dictionary()
            .and_then(|d| d.get("Sheets"))
            .and_then(Value::as_array)
            .and_then(|s| s.first())
            .and_then(Value::as_dictionary)
            .and_then(|d| d.get("GraphicsList"))
            .and_then(Value::as_array)
            .expect("a GraphicsList")
            .clone()
    }

    /// dot bytes -> the emitted `GraphicsList`.
    pub(crate) fn graphics_of(dot: &[u8]) -> Vec<Value> {
        let data = DotData::try_from(dot.to_vec()).expect("dot parses");
        let graffle = GraffleData::try_from(data).expect("dot converts to graffle");
        graphics_list(graffle.to_plist_xml().expect("graffle serializes"))
    }

    /// The `stroke` dict of the first graphic of the given `Class`.
    fn first_stroke(dot: &[u8], class: &str) -> Dictionary {
        graphics_of(dot)
            .iter()
            .filter_map(Value::as_dictionary)
            .find(|g| g.get("Class").and_then(Value::as_string) == Some(class))
            .and_then(|g| g.get("Style"))
            .and_then(Value::as_dictionary)
            .and_then(|s| s.get("stroke"))
            .and_then(Value::as_dictionary)
            .cloned()
            .unwrap_or_else(|| panic!("a {class} with a stroke"))
    }

    pub(crate) fn first_line_stroke(dot: &[u8]) -> Dictionary {
        first_stroke(dot, "LineGraphic")
    }

    pub(crate) fn first_node_stroke(dot: &[u8]) -> Dictionary {
        first_stroke(dot, "ShapedGraphic")
    }

    pub(crate) fn arrow(stroke: &Dictionary, end: &str) -> Option<String> {
        stroke.get(end).and_then(Value::as_string).map(str::to_owned)
    }

    pub(crate) fn read_fixture(name: &str) -> GraffleData {
        let path = format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        GraffleData::try_from(bytes).expect("fixture is a valid .graffle")
    }

    pub(crate) fn parses_as_dot(text: &str) -> bool {
        dot_parser::ast::Graph::try_from(text).is_ok()
    }

    /// Whether graphviz `dot` is on PATH — edge-spline routing needs it.
    pub(crate) fn dot_available() -> bool {
        std::process::Command::new("dot")
            .arg("-V")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

#[cfg(test)]
mod tests {
    use plist::Value;

    use super::GraffleData;
    use super::test_support::{
        arrow, dot_available, first_line_stroke, first_node_stroke, graphics_list, parses_as_dot,
    };
    use crate::dotviz::DotData;

    #[test]
    fn inverted_shape_emits_base_shape_with_vflip() {
        // `invtriangle` is `VerticalTriangle` plus a `VFlip` to point it down.
        let dot = br#"digraph g { n [shape=invtriangle]; }"#.to_vec();
        let graffle =
            GraffleData::try_from(DotData::try_from(dot).expect("dot parses")).expect("dot converts to graffle");
        let node = graphics_list(graffle.to_plist_xml().expect("graffle serializes"))
            .iter()
            .find_map(Value::as_dictionary)
            .expect("a ShapedGraphic")
            .clone();
        assert_eq!(node.get("Shape").and_then(Value::as_string), Some("VerticalTriangle"));
        assert_eq!(node.get("VFlip").and_then(Value::as_string), Some("YES"));
    }

    // ---- edge arrows & line patterns ----

    #[test]
    fn default_digraph_edge_draws_a_head_arrow_only() {
        let s = first_line_stroke(b"digraph g { a -> b; }");
        assert_eq!(arrow(&s, "HeadArrow").as_deref(), Some("FilledArrow"));
        assert_eq!(arrow(&s, "TailArrow").as_deref(), Some("0"));
        assert!(s.get("Pattern").is_none(), "a plain edge is solid");
    }

    #[test]
    fn dir_none_suppresses_both_arrows() {
        let s = first_line_stroke(b"digraph g { a -> b [dir=none]; }");
        assert_eq!(arrow(&s, "HeadArrow").as_deref(), Some("0"));
        assert_eq!(arrow(&s, "TailArrow").as_deref(), Some("0"));
    }

    #[test]
    fn arrowhead_none_drops_just_the_head() {
        let s = first_line_stroke(b"digraph g { a -> b [arrowhead=none]; }");
        assert_eq!(arrow(&s, "HeadArrow").as_deref(), Some("0"));
    }

    #[test]
    fn dir_back_puts_the_arrow_on_the_tail() {
        let s = first_line_stroke(b"digraph g { a -> b [dir=back]; }");
        assert_eq!(arrow(&s, "HeadArrow").as_deref(), Some("0"));
        assert_eq!(arrow(&s, "TailArrow").as_deref(), Some("FilledArrow"));
    }

    #[test]
    fn undirected_graph_edges_have_no_arrows() {
        let s = first_line_stroke(b"graph g { a -- b; }");
        assert_eq!(arrow(&s, "HeadArrow").as_deref(), Some("0"));
        assert_eq!(arrow(&s, "TailArrow").as_deref(), Some("0"));
    }

    #[test]
    fn arrowhead_shapes_map_to_omnigraffle_identifiers() {
        for (dot_arrow, omni) in [
            ("vee", "Arrow"),
            ("diamond", "Diamond"),
            ("dot", "FilledBall"),
            ("inv", "SharpBackArrow"),
        ] {
            let dot = format!("digraph g {{ a -> b [arrowhead={dot_arrow}]; }}");
            let s = first_line_stroke(dot.as_bytes());
            assert_eq!(arrow(&s, "HeadArrow").as_deref(), Some(omni), "{dot_arrow}");
        }
    }

    #[test]
    fn dashed_and_dotted_pick_distinct_patterns() {
        let dashed = first_line_stroke(b"digraph g { a -> b [style=dashed]; }");
        assert_eq!(dashed.get("Pattern").and_then(Value::as_signed_integer), Some(1));
        let dotted = first_line_stroke(b"digraph g { a -> b [style=dotted]; }");
        assert_eq!(dotted.get("Pattern").and_then(Value::as_signed_integer), Some(2));
    }

    #[test]
    fn bold_edge_without_penwidth_thickens_the_stroke() {
        let s = first_line_stroke(b"digraph g { a -> b [style=bold]; }");
        assert_eq!(s.get("Width").and_then(super::num), Some(2.0));
    }

    #[test]
    fn edge_styling_round_trips_through_graffle_and_back() {
        let dot = br#"digraph g { a -> b [dir=both, arrowhead=diamond, arrowtail=vee, style=dotted]; }"#.to_vec();
        let graffle =
            GraffleData::try_from(DotData::try_from(dot).expect("dot parses")).expect("dot converts to graffle");
        let back = DotData::try_from(graffle).expect("graffle converts to dot").to_string();
        assert!(parses_as_dot(&back), "emitted DOT must parse:\n{back}");
        for needle in [
            "dir=\"both\"", "arrowhead=\"diamond\"", "arrowtail=\"vee\"", "style=\"dotted\"",
        ] {
            assert!(back.contains(needle), "missing {needle} in:\n{back}");
        }
    }

    #[test]
    fn edges_follow_graphviz_routed_spline() {
        if !dot_available() {
            return; // without graphviz layout, edges fall back to straight lines
        }
        // The skip edge a->d routes around b and c, so it samples to many
        // waypoints — a straight line would be just its two endpoints.
        let dot = b"digraph { rankdir=LR; a -> b -> c -> d; a -> d; }".to_vec();
        let graffle =
            GraffleData::try_from(DotData::try_from(dot).expect("dot parses")).expect("dot converts to graffle");
        let max_points = graphics_list(graffle.to_plist_xml().expect("graffle serializes"))
            .iter()
            .filter_map(Value::as_dictionary)
            .filter(|g| g.get("Class").and_then(Value::as_string) == Some("LineGraphic"))
            .map(|g| g.get("Points").and_then(Value::as_array).map_or(0, Vec::len))
            .max()
            .unwrap_or(0);
        assert!(
            max_points > 2,
            "expected a routed edge with >2 waypoints, got {max_points}"
        );
    }

    // ---- node border line styles ----

    #[test]
    fn node_border_dash_maps_to_pattern() {
        let dashed = first_node_stroke(b"digraph g { n [style=dashed]; }");
        assert_eq!(dashed.get("Pattern").and_then(Value::as_signed_integer), Some(1));
        let dotted = first_node_stroke(b"digraph g { n [style=dotted]; }");
        assert_eq!(dotted.get("Pattern").and_then(Value::as_signed_integer), Some(2));
    }

    #[test]
    fn bold_node_border_thickens_the_stroke() {
        let s = first_node_stroke(b"digraph g { n [style=bold]; }");
        assert_eq!(s.get("Width").and_then(super::num), Some(2.0));
    }

    #[test]
    fn node_border_style_round_trips_through_graffle_and_back() {
        let dot = br#"digraph g { n [shape=box, style="dashed,filled", fillcolor=red]; }"#.to_vec();
        let graffle =
            GraffleData::try_from(DotData::try_from(dot).expect("dot parses")).expect("dot converts to graffle");
        let back = DotData::try_from(graffle).expect("graffle converts to dot").to_string();
        assert!(parses_as_dot(&back), "emitted DOT must parse:\n{back}");
        // Both style keywords survive the trip (order may differ).
        assert!(back.contains("dashed"), "missing dashed in:\n{back}");
        assert!(back.contains("filled"), "missing filled in:\n{back}");
    }

    #[test]
    fn oversized_archive_is_rejected() {
        use crate::error::DotGraffleError;
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/shutdown.graffle"))
            .expect("read shutdown fixture");
        // A tiny cap rejects the real (multi-KB) data.plist before inflating it...
        assert!(matches!(
            super::plist_bytes(bytes.clone(), 16),
            Err(DotGraffleError::OversizedArchive(16))
        ));
        // ...and the real cap reads it into a valid plist.
        let ok = super::plist_bytes(bytes, super::MAX_PLIST_BYTES).expect("under the cap");
        assert!(Value::from_reader(std::io::Cursor::new(ok)).is_ok());
    }
}
