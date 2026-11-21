use std::fmt;
use std::io;
use std::path::PathBuf;

/// Error type returned by `sfgn-extract-code` extraction and bidirectional source patching.
#[derive(Debug)]
pub enum CodeExtractError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Walk(ignore::Error),
    Parse {
        path: PathBuf,
        language: &'static str,
        reason: String,
    },
    Patch {
        path: PathBuf,
        reason: String,
    },
    Compile(sfgn_core::CompileError),
}

impl fmt::Display for CodeExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "I/O error at '{}': {}", path.display(), source)
            }
            Self::Walk(err) => write!(f, "directory walk error: {}", err),
            Self::Parse {
                path,
                language,
                reason,
            } => write!(
                f,
                "tree-sitter {} parse error in '{}': {}",
                language,
                path.display(),
                reason
            ),
            Self::Patch { path, reason } => {
                write!(f, "source splice error in '{}': {}", path.display(), reason)
            }
            Self::Compile(diag) => write!(f, "SFGN compilation error: {}", diag),
        }
    }
}

impl std::error::Error for CodeExtractError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Walk(err) => Some(err),
            Self::Compile(diag) => Some(diag),
            Self::Parse { .. } | Self::Patch { .. } => None,
        }
    }
}

impl From<ignore::Error> for CodeExtractError {
    fn from(err: ignore::Error) -> Self {
        Self::Walk(err)
    }
}

impl From<sfgn_core::CompileError> for CodeExtractError {
    fn from(err: sfgn_core::CompileError) -> Self {
        Self::Compile(err)
    }
}

pub type Result<T> = std::result::Result<T, CodeExtractError>;
