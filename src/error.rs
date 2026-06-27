//! Error types for dot-graffle conversions.
use miette::Diagnostic;
use thiserror::Error;

use crate::layout::GraphvizError;

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum DotGraffleError {
    /// Reading or writing a file (or stdio) failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A `.dot` input was not valid UTF-8 text.
    #[error("input is not valid UTF-8; a .dot file must be text")]
    NotUtf8(#[from] std::string::FromUtf8Error),

    /// The DOT parser rejected the input.
    #[error("could not parse DOT input:\n{0}")]
    DotParse(String),

    /// Serializing or deserializing an OmniGraffle plist failed.
    #[error(transparent)]
    Plist(#[from] plist::Error),

    /// Reading the OmniGraffle ZIP archive (a `.graffle` is a zipped bundle)
    /// failed.
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),

    /// The archive's `data.plist` is larger than we'll decompress — a guard
    /// against a crafted `.graffle` that inflates to gigabytes.
    #[error("the .graffle's data.plist exceeds the {0}-byte limit")]
    OversizedArchive(u64),

    /// A batch input file had an extension we don't know how to convert.
    #[error("don't know how to convert {0}: expected a .dot or .graffle extension")]
    UnknownExtension(String),

    /// The derived output path already exists and `--force` was not given.
    #[error("{0} already exists (use --force to overwrite)")]
    OutputExists(String),

    /// Graphviz was present but failed to lay out the DOT source.
    #[error(transparent)]
    Graphviz(#[from] GraphvizError),
}
