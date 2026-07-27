//! Convert graphviz `.dot` files to OmniGraffle `.graffle` diagrams and back.
//!
//! One binary, two directions: the direction is taken from each file's
//! extension, or — when streaming stdin to stdout — sniffed from the input
//! content (a `.graffle` bundle vs `.dot` text), falling back to the name the
//! tool was invoked as (`dot-graffle` vs `graffle-dot`). graphviz does the
//! layout; we translate its result into OmniGraffle's plist, and recover a
//! graph from the plist on the way back.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use clap::builder::Styles;
use clap::builder::styling::AnsiColor;
use miette::Result as MietteResult;

mod dotviz;
mod error;
mod graffle;
mod layout;
mod rtf;

use crate::dotviz::DotData;
use crate::error::DotGraffleError;
use crate::graffle::GraffleData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    /// dot -> graffle (invoked as `dot-graffle`)
    DotToGraffle,
    /// graffle -> dot (invoked as `graffle-dot`)
    GraffleToDot,
}

impl Direction {
    /// Pick a direction from `argv[0]` — the name we were invoked as. Used for
    /// the streaming (stdin -> stdout) mode, where there's no file extension to
    /// consult.
    #[must_use]
    pub fn from_invocation() -> Self {
        let invoked_as = std::env::args_os().next();
        let name = invoked_as
            .as_deref()
            .map(Path::new)
            .and_then(Path::file_stem)
            .map(OsStr::to_string_lossy);

        match name.as_deref() {
            Some("graffle-dot") => Self::GraffleToDot,
            // `dot-graffle`, `cargo run` (argv[0] == crate name), and any
            // unexpected name all fall through to the default direction.
            _ => Self::DotToGraffle,
        }
    }

    /// Pick a direction by sniffing the leading bytes of streamed input. A
    /// `.graffle` is a zip bundle (`PK\x03\x04`) or a bare plist — binary
    /// (`bplist00`) or XML (`<?xml`/`<plist`); a `.dot` is text whose first
    /// token past any comments is a `graph`/`digraph`/`strict` keyword. Those
    /// signatures are disjoint, so the right direction is recovered no matter
    /// which name the tool was invoked as. `None` when the content is neither
    /// (empty or unrecognized) — the caller then falls back to the invocation
    /// name.
    #[must_use]
    pub fn from_content(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"bplist00") {
            return Some(Self::GraffleToDot);
        }
        // The rest must be valid UTF-8 to be either an XML plist or DOT text;
        // anything else is binary junk we can't place.
        let text = std::str::from_utf8(bytes).ok()?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let trimmed = text.trim_start();
        if trimmed.starts_with("<?xml") || trimmed.starts_with("<!DOCTYPE plist") || trimmed.starts_with("<plist") {
            return Some(Self::GraffleToDot);
        }
        if starts_with_dot_keyword(trimmed) {
            return Some(Self::DotToGraffle);
        }
        None
    }

    /// Pick a direction from an input file's extension. `None` if the extension
    /// is neither `.dot` nor `.graffle`.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension().and_then(OsStr::to_str) {
            Some("dot") => Some(Self::DotToGraffle),
            Some("graffle") => Some(Self::GraffleToDot),
            _ => None,
        }
    }

    /// The extension the converted output should carry.
    #[must_use]
    pub fn output_extension(self) -> &'static str {
        match self {
            Self::DotToGraffle => "graffle",
            Self::GraffleToDot => "dot",
        }
    }
}

/// Whether `s`, past any leading whitespace and DOT comments (`// line`,
/// `# line`, `/* block */`), begins with a graph keyword (`graph`, `digraph`,
/// or `strict`) as a whole token. Used to recognize DOT source when sniffing
/// stdin; a false negative just means we fall back to the invocation name.
fn starts_with_dot_keyword(s: &str) -> bool {
    let mut rest = s.trim_start();
    loop {
        let after_comment = if let Some(after) = rest.strip_prefix("//").or_else(|| rest.strip_prefix('#')) {
            after.find('\n').map_or("", |i| &after[i + 1..])
        } else if let Some(after) = rest.strip_prefix("/*") {
            after.find("*/").map_or("", |i| &after[i + 2..])
        } else {
            break;
        };
        rest = after_comment.trim_start();
    }
    ["strict", "graph", "digraph"].iter().any(|kw| {
        rest.strip_prefix(kw)
            // Whole-token only: the keyword can't run into an identifier.
            .is_some_and(|after| after.chars().next().is_none_or(|c| !c.is_alphanumeric() && c != '_'))
    })
}

#[derive(Debug, Clone, Parser, Default)]
#[clap(version, styles = v3_styles(), max_term_width = 100)]
#[command(next_line_help = true)]
/// Convert graphviz .dot files to OmniGraffle diagrams and back.
pub(crate) struct Args {
    /// Input files to convert. Each `.dot` is written out as a sibling
    /// `.graffle`, and each `.graffle` as a sibling `.dot`. With no files,
    /// reads stdin and writes stdout — the direction is inferred from the input
    /// content, falling back to the name the tool was invoked as.
    files: Vec<String>,

    /// Overwrite existing output files instead of refusing to clobber them.
    #[arg(short, long)]
    force: bool,
}

/// I like my clap help styled the old way.
fn v3_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Yellow.on_default())
        .usage(AnsiColor::Green.on_default())
        .literal(AnsiColor::Green.on_default())
        .placeholder(AnsiColor::Green.on_default())
}

/// Read the whole input — a file if given, otherwise stdin — as raw bytes.
/// Bytes, not a `String`: a `.graffle` is a binary zip, so we can't assume
/// UTF-8.
fn read_input(maybe_file: Option<&Path>) -> Result<Vec<u8>, DotGraffleError> {
    let mut data = Vec::new();
    match maybe_file {
        Some(path) => {
            File::open(path)?.read_to_end(&mut data)?;
        }
        None => {
            std::io::stdin().read_to_end(&mut data)?;
        }
    }
    Ok(data)
}

/// Write all bytes to a file if given, otherwise to stdout.
fn write_output(data: &[u8], maybe_file: Option<&Path>) -> Result<(), DotGraffleError> {
    match maybe_file {
        Some(path) => File::create(path)?.write_all(data)?,
        None => std::io::stdout().write_all(data)?,
    }
    Ok(())
}

/// Convert raw input bytes in the given direction, returning the output bytes.
fn convert(direction: Direction, input: Vec<u8>) -> Result<Vec<u8>, DotGraffleError> {
    Ok(match direction {
        Direction::DotToGraffle => {
            let dot = DotData::try_from(input)?;
            let graffle = GraffleData::try_from(dot)?;
            graffle.to_plist_xml()?
        }
        Direction::GraffleToDot => {
            let graffle = GraffleData::try_from(input)?;
            let dot = DotData::try_from(graffle)?;
            dot.to_string().into_bytes()
        }
    })
}

/// Derive the output path for an input path: the same path with its extension
/// toggled to the other format. `with_extension` keeps the directory and
/// replaces only the final extension, so `/a/b/x.dot` -> `/a/b/x.graffle`.
fn output_path(input: &Path, direction: Direction) -> PathBuf {
    input.with_extension(direction.output_extension())
}

/// Convert a single named file to its sibling, returning the path written.
/// Refuses to clobber an existing output unless `force` is set.
fn convert_file(path: &str, force: bool) -> Result<PathBuf, DotGraffleError> {
    let in_path = Path::new(path);
    let direction = Direction::from_path(in_path).ok_or_else(|| DotGraffleError::UnknownExtension(path.to_string()))?;
    let out_path = output_path(in_path, direction);

    if out_path.exists() && !force {
        return Err(DotGraffleError::OutputExists(out_path.display().to_string()));
    }

    let input = read_input(Some(in_path))?;
    let output = convert(direction, input)?;
    write_output(&output, Some(out_path.as_path()))?;
    Ok(out_path)
}

/// Convert every file, reporting progress and failures to stderr. Keeps going
/// past failures and returns `true` only if every file converted.
fn run_batch(files: &[String], force: bool) -> bool {
    let mut converted = 0usize;
    let mut failed = 0usize;
    for file in files {
        match convert_file(file, force) {
            Ok(out) => {
                converted += 1;
                eprintln!("wrote {}", out.display());
            }
            Err(err) => {
                failed += 1;
                eprintln!("error: {file}: {err}");
            }
        }
    }
    if files.len() > 1 || failed > 0 {
        eprintln!("{converted} converted, {failed} failed");
    }
    failed == 0
}

/// Parse our options and do the thing.
fn main() -> MietteResult<()> {
    let args = Args::parse();

    if args.files.is_empty() {
        // Streaming mode: stdin -> stdout. Infer the direction from the content
        // itself; only when it's neither recognizably a `.graffle` nor `.dot`
        // do we fall back to the name we were invoked as.
        let input = read_input(None)?;
        let direction = Direction::from_content(&input).unwrap_or_else(Direction::from_invocation);
        let output = convert(direction, input)?;
        write_output(&output, None)?;
    } else if !run_batch(&args.files, args.force) {
        // Batch mode failed on at least one file. Per-file errors were already
        // printed; exit nonzero without a redundant miette report.
        std::process::exit(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn from_path_maps_known_extensions() {
        assert_eq!(
            Direction::from_path(Path::new("foo.dot")),
            Some(Direction::DotToGraffle)
        );
        assert_eq!(
            Direction::from_path(Path::new("bar.graffle")),
            Some(Direction::GraffleToDot)
        );
        assert_eq!(Direction::from_path(Path::new("notes.txt")), None);
        assert_eq!(Direction::from_path(Path::new("no_extension")), None);
    }

    #[test]
    fn output_extension_toggles_format() {
        assert_eq!(Direction::DotToGraffle.output_extension(), "graffle");
        assert_eq!(Direction::GraffleToDot.output_extension(), "dot");
    }

    #[test]
    fn output_path_swaps_only_the_final_extension() {
        assert_eq!(
            output_path(Path::new("foo.dot"), Direction::DotToGraffle),
            PathBuf::from("foo.graffle")
        );
        assert_eq!(
            output_path(Path::new("/a/b/x.graffle"), Direction::GraffleToDot),
            PathBuf::from("/a/b/x.dot")
        );
        // A dotted parent directory must not be mistaken for the extension.
        assert_eq!(
            output_path(Path::new("/has.dots/x.dot"), Direction::DotToGraffle),
            PathBuf::from("/has.dots/x.graffle")
        );
    }

    /// A fresh, unique scratch directory under the system temp dir.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dot-graffle-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn convert_file_writes_sibling_and_guards_overwrite() {
        let dir = scratch("convert_file");
        let dot = dir.join("g.dot");
        fs::write(&dot, b"digraph g { a -> b; }").expect("write test dot");
        let dot_str = dot.to_str().expect("dot path is valid utf-8");

        // First conversion writes the sibling .graffle, and it's a valid plist.
        let out = convert_file(dot_str, false).expect("convert");
        assert_eq!(out, dir.join("g.graffle"));
        let bytes = fs::read(&out).expect("read converted graffle");
        let value = plist::Value::from_reader(std::io::Cursor::new(bytes)).expect("valid plist");
        assert!(value.as_dictionary().is_some());

        // A second run refuses to clobber the now-existing output.
        match convert_file(dot_str, false) {
            Err(DotGraffleError::OutputExists(_)) => {}
            other => panic!("expected OutputExists, got {other:?}"),
        }

        // ...unless --force is given.
        convert_file(dot_str, true).expect("forced overwrite");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn convert_file_rejects_unknown_extension() {
        // Resolves the extension before touching the filesystem, so the path
        // need not exist.
        match convert_file("notes.txt", false) {
            Err(DotGraffleError::UnknownExtension(p)) => assert_eq!(p, "notes.txt"),
            other => panic!("expected UnknownExtension, got {other:?}"),
        }
    }

    #[test]
    fn from_content_recognizes_graffle_inputs() {
        use Direction::GraffleToDot;
        // Zip bundle, bare binary plist, and bare XML plist all read as graffle.
        assert_eq!(Direction::from_content(b"PK\x03\x04..."), Some(GraffleToDot));
        assert_eq!(Direction::from_content(b"bplist00\x00\x01"), Some(GraffleToDot));
        assert_eq!(
            Direction::from_content(br#"<?xml version="1.0"?><plist></plist>"#),
            Some(GraffleToDot)
        );
        assert_eq!(Direction::from_content(b"<plist></plist>"), Some(GraffleToDot));
    }

    #[test]
    fn from_content_recognizes_dot_inputs() {
        use Direction::DotToGraffle;
        assert_eq!(Direction::from_content(b"digraph g { a -> b; }"), Some(DotToGraffle));
        // Leading comments (line and block) and a `strict` prefix are skipped.
        assert_eq!(
            Direction::from_content(b"  // note\n  strict graph {}"),
            Some(DotToGraffle)
        );
        assert_eq!(Direction::from_content(b"/* hi */ digraph {}"), Some(DotToGraffle));
        assert_eq!(Direction::from_content(b"# shebang-ish\ngraph {}"), Some(DotToGraffle));
        // A UTF-8 BOM in front of the keyword is tolerated.
        assert_eq!(Direction::from_content(b"\xef\xbb\xbfgraph {}"), Some(DotToGraffle));
    }

    #[test]
    fn from_content_is_undecided_on_ambiguous_input() {
        // Empty, prose, a keyword that's only a prefix of an identifier, and
        // binary junk all defer to the invocation name (`None`).
        assert_eq!(Direction::from_content(b""), None);
        assert_eq!(Direction::from_content(b"hello, world"), None);
        assert_eq!(Direction::from_content(b"graphics_card = 1"), None);
        assert_eq!(Direction::from_content(&[0xff, 0xfe, 0x00, 0x01]), None);
    }
}
