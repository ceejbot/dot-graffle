//! The recovered graph model (graffle -> dot).
//!
//! A flat, DOT-shaped intermediate: the reader ([`super::read`]) fills it from
//! the OmniGraffle plist, and [`crate::dotviz`] renders it back to DOT text.
//! Values are already in DOT vocabulary (DOT shape keywords, `#rrggbb` colors)
//! so the DOT writer needs no OmniGraffle knowledge.

/// A node recovered from a `ShapedGraphic`.
pub(crate) struct GNode {
    pub(crate) id: i64,
    pub(crate) label: Option<String>,
    pub(crate) shape: Option<&'static str>,
    pub(crate) fill: Option<String>,
    pub(crate) pen: Option<String>,
    pub(crate) pen_width: Option<f64>,
    pub(crate) rounded: bool,
    /// Border line texture from the stroke `Pattern`: `"dashed"`, `"dotted"`,
    /// or `None` for a solid border.
    pub(crate) dash: Option<&'static str>,
    pub(crate) font_name: Option<String>,
    pub(crate) font_size: Option<f64>,
    pub(crate) font_color: Option<String>,
}

/// An edge recovered from a connected `LineGraphic` (`Tail` -> `Head`).
pub(crate) struct GEdge {
    pub(crate) tail: i64,
    pub(crate) head: i64,
    pub(crate) label: Option<String>,
    pub(crate) color: Option<String>,
    pub(crate) pen_width: Option<f64>,
    /// Line texture recovered from the stroke `Pattern`: `"dashed"`,
    /// `"dotted"`, or `None` for a solid line.
    pub(crate) style: Option<&'static str>,
    /// Arrow direction relative to graphviz's `forward` default: `"none"`,
    /// `"back"`, `"both"`, or `None` for a plain forward edge.
    pub(crate) dir: Option<&'static str>,
    /// Head/tail arrowhead shapes when they differ from graphviz's `normal`.
    pub(crate) arrowhead: Option<&'static str>,
    pub(crate) arrowtail: Option<&'static str>,
}

/// A `Group`/`TableGroup`, emitted as a DOT `subgraph cluster_<id>`. `node_ids`
/// are the nodes declared directly in this cluster; `children` are nested ones.
/// A group carries no fill or stroke of its own (it is a pure container), but
/// it does hold the label `FontInfo` — the only styling there is to recover.
pub(crate) struct GCluster {
    pub(crate) id: i64,
    pub(crate) label: Option<String>,
    pub(crate) node_ids: Vec<i64>,
    pub(crate) children: Vec<GCluster>,
    pub(crate) font_name: Option<String>,
    pub(crate) font_size: Option<f64>,
    pub(crate) font_color: Option<String>,
}

/// The whole recovered graph. `nodes` is the flat global set (every node, in
/// document order); clusters reference nodes by id.
pub(crate) struct GModel {
    pub(crate) name: Option<String>,
    pub(crate) nodes: Vec<GNode>,
    pub(crate) edges: Vec<GEdge>,
    pub(crate) clusters: Vec<GCluster>,
}

// ---- shape & arrow vocabulary ----------------------------------------------
//
// The DOT <-> OmniGraffle shape and arrow tables, as enums rather than two
// hand-synced `match`es. `to_omni` is exhaustive, so adding a variant forces
// both directions to account for it. The OmniGraffle identifiers are its own
// (harvested from `ShapeNames.strings` in the app bundle and real documents) —
// note the misspelled `Trapazoid` and that the upward triangle is keyed
// `VerticalTriangle`.

/// Whether a DOT shape renders as text with no outline or fill (`plaintext`).
/// Kept apart from [`Shape`]: it is the *absence* of a drawn shape, the inverse
/// of how the reader recognizes a node with neither fill nor stroke.
pub(crate) fn is_text_only(dot: &str) -> bool {
    matches!(dot, "plaintext" | "plain" | "none")
}

/// An OmniGraffle shape with a DOT equivalent we translate. The plain rectangle
/// (DOT `box`/default) is deliberately *not* a variant — it is the absence of a
/// named shape, so it falls out of both `from_dot` and `from_omni` as `None`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Shape {
    Cylinder,
    Note,
    Document,
    Diamond,
    Triangle,
    Circle,
    Pentagon,
    Hexagon,
    Octagon,
    Parallelogram,
    Trapezoid,
    House,
    Cube,
    Star,
}

impl Shape {
    /// A DOT shape keyword -> the named shape plus a vertical-flip flag (set
    /// for graphviz's inverted variants). `None` for `box`, the decorated
    /// `Msquare`, `plaintext`, or anything we don't translate — all of
    /// which the writer renders as the default rectangle (or, for
    /// plaintext, as text only). Decorated / multi-periphery variants
    /// (`Mdiamond`, `doublecircle`, …) fold onto their base shape, since
    /// OmniGraffle has no extra rings or marks.
    pub(crate) fn from_dot(dot: &str) -> Option<(Shape, bool)> {
        let upright = |s| Some((s, false));
        let flipped = |s| Some((s, true));
        match dot {
            "cylinder" => upright(Shape::Cylinder),
            "note" => upright(Shape::Note),
            "folder" | "tab" | "component" => upright(Shape::Document),
            "diamond" | "Mdiamond" => upright(Shape::Diamond),
            "triangle" => upright(Shape::Triangle),
            "invtriangle" => flipped(Shape::Triangle),
            "ellipse" | "oval" | "circle" | "Mcircle" | "doublecircle" => upright(Shape::Circle),
            "pentagon" => upright(Shape::Pentagon),
            "hexagon" => upright(Shape::Hexagon),
            "octagon" | "doubleoctagon" | "tripleoctagon" => upright(Shape::Octagon),
            "parallelogram" => upright(Shape::Parallelogram),
            "trapezium" => upright(Shape::Trapezoid),
            "invtrapezium" => flipped(Shape::Trapezoid),
            "house" => upright(Shape::House),
            "invhouse" => flipped(Shape::House),
            "box3d" => upright(Shape::Cube),
            "star" => upright(Shape::Star),
            _ => None,
        }
    }

    /// The OmniGraffle `Shape` identifier.
    pub(crate) fn to_omni(self) -> &'static str {
        match self {
            Shape::Cylinder => "Cylinder",
            Shape::Note => "NoteShape",
            Shape::Document => "DocumentShape",
            Shape::Diamond => "Diamond",
            Shape::Triangle => "VerticalTriangle",
            Shape::Circle => "Circle",
            Shape::Pentagon => "Pentagon",
            Shape::Hexagon => "Hexagon",
            Shape::Octagon => "Octagon",
            Shape::Parallelogram => "Parallelogram",
            Shape::Trapezoid => "Trapazoid",
            Shape::House => "House",
            Shape::Cube => "Cube",
            Shape::Star => "Star",
        }
    }

    /// An OmniGraffle `Shape` identifier -> the named shape. `None` for a plain
    /// `Rectangle`, a custom stencil (a UUID), or an absent key — all the DOT
    /// default box.
    pub(crate) fn from_omni(id: &str) -> Option<Shape> {
        Some(match id {
            "Cylinder" => Shape::Cylinder,
            "NoteShape" => Shape::Note,
            "DocumentShape" => Shape::Document,
            "Diamond" => Shape::Diamond,
            "VerticalTriangle" => Shape::Triangle,
            "Circle" => Shape::Circle,
            "Pentagon" => Shape::Pentagon,
            "Hexagon" => Shape::Hexagon,
            "Octagon" => Shape::Octagon,
            "Parallelogram" => Shape::Parallelogram,
            "Trapazoid" => Shape::Trapezoid,
            "House" => Shape::House,
            "Cube" => Shape::Cube,
            "Star" => Shape::Star,
            _ => return None,
        })
    }

    /// The canonical (upright) DOT keyword. `flip` selects the inverted variant
    /// for the three shapes graphviz names one for (`triangle`/`house`/
    /// `trapezium`); every other shape ignores it.
    pub(crate) fn to_dot(self, flip: bool) -> &'static str {
        match (self, flip) {
            (Shape::Cylinder, _) => "cylinder",
            (Shape::Note, _) => "note",
            (Shape::Document, _) => "folder",
            (Shape::Diamond, _) => "diamond",
            (Shape::Triangle, false) => "triangle",
            (Shape::Triangle, true) => "invtriangle",
            (Shape::Circle, _) => "ellipse",
            (Shape::Pentagon, _) => "pentagon",
            (Shape::Hexagon, _) => "hexagon",
            (Shape::Octagon, _) => "octagon",
            (Shape::Parallelogram, _) => "parallelogram",
            (Shape::Trapezoid, false) => "trapezium",
            (Shape::Trapezoid, true) => "invtrapezium",
            (Shape::House, false) => "house",
            (Shape::House, true) => "invhouse",
            (Shape::Cube, _) => "box3d",
            (Shape::Star, _) => "star",
        }
    }
}

/// An edge arrowhead. `Filled` is graphviz's `normal` default (OmniGraffle's
/// `FilledArrow`); the rest are the shapes we round-trip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Arrow {
    Filled,
    Vee,
    Inv,
    Dot,
    Odot,
    Box,
    Diamond,
    Crow,
    Tee,
}

impl Arrow {
    /// A graphviz arrow name -> arrowhead. Unrecognized names fall back to the
    /// filled default (`normal`). Open/empty variants share their filled
    /// cousin, since OmniGraffle has no hollow equivalent.
    pub(crate) fn from_dot(name: &str) -> Arrow {
        match name {
            "vee" | "open" | "empty" | "onormal" => Arrow::Vee,
            "inv" => Arrow::Inv,
            "dot" => Arrow::Dot,
            "odot" => Arrow::Odot,
            "box" | "obox" => Arrow::Box,
            "diamond" | "odiamond" | "ediamond" => Arrow::Diamond,
            "crow" => Arrow::Crow,
            "tee" => Arrow::Tee,
            _ => Arrow::Filled,
        }
    }

    /// The OmniGraffle line-ending identifier.
    pub(crate) fn to_omni(self) -> &'static str {
        match self {
            Arrow::Filled => "FilledArrow",
            Arrow::Vee => "Arrow",
            Arrow::Inv => "SharpBackArrow",
            Arrow::Dot => "FilledBall",
            Arrow::Odot => "EmptyCenterBall",
            Arrow::Box => "FilledBox",
            Arrow::Diamond => "Diamond",
            Arrow::Crow => "CrowBall",
            Arrow::Tee => "DoubleBar",
        }
    }

    /// An OmniGraffle line-ending identifier -> the graphviz arrow name. `None`
    /// is the default filled arrow (`normal`) or anything unrecognized, so the
    /// reader emits no explicit `arrowhead`.
    pub(crate) fn from_omni(id: &str) -> Option<&'static str> {
        Some(match id {
            "Arrow" => "vee",
            "SharpBackArrow" => "inv",
            "FilledBall" => "dot",
            "EmptyCenterBall" => "odot",
            "FilledBox" => "box",
            "Diamond" => "diamond",
            "CrowBall" => "crow",
            "DoubleBar" => "tee",
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_round_trips_dot_omni_dot() {
        // Every named shape: dot -> omni id -> back to the same dot keyword. The
        // collapsing aliases (`oval`/`circle` -> ellipse, `tab` -> folder) are
        // not listed; only the canonical keyword round-trips exactly.
        for dot in [
            "cylinder", "note", "folder", "diamond", "triangle", "ellipse", "pentagon", "hexagon", "octagon",
            "parallelogram", "trapezium", "house", "box3d", "star",
        ] {
            let (shape, flip) = Shape::from_dot(dot).expect("known shape");
            assert_eq!(Shape::from_omni(shape.to_omni()), Some(shape), "{dot}");
            assert_eq!(shape.to_dot(flip), dot, "{dot}");
        }
        // Inverted variants carry the flip and recover their inv* keyword.
        for (inv, base) in [
            ("invtriangle", "triangle"),
            ("invhouse", "house"),
            ("invtrapezium", "trapezium"),
        ] {
            let (shape, flip) = Shape::from_dot(inv).expect("inverted shape");
            assert!(flip, "{inv} should flip");
            assert_eq!(shape.to_dot(false), base);
            assert_eq!(shape.to_dot(true), inv);
        }
        // The plain rectangle and text-only shapes have no named variant.
        assert_eq!(Shape::from_dot("box"), None);
        assert_eq!(Shape::from_omni("Rectangle"), None);
        assert!(is_text_only("plaintext") && !is_text_only("box"));
    }

    #[test]
    fn arrow_round_trips_dot_omni_dot() {
        for dot in ["vee", "inv", "dot", "odot", "box", "diamond", "crow", "tee"] {
            assert_eq!(Arrow::from_omni(Arrow::from_dot(dot).to_omni()), Some(dot), "{dot}");
        }
        // `normal`/unknown is the filled default, which reads back as "no arrow".
        assert_eq!(Arrow::from_dot("normal"), Arrow::Filled);
        assert_eq!(Arrow::from_omni("FilledArrow"), None);
    }
}
