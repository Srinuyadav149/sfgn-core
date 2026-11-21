use std::fmt;
use std::io;
use std::path::PathBuf;

/// Error type returned by `sfgn-extract-docs` compilation and EBNF-to-GBNF transpilation.
#[derive(Debug)]
pub enum DocsExtractError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Walk(ignore::Error),
    Grammar {
        line: usize,
        reason: String,
    },
    Compile(sfgn_core::CompileError),
}

impl fmt::Display for DocsExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "I/O error at '{}': {}", path.display(), source)
            }
            Self::Walk(err) => write!(f, "directory walk error: {}", err),
            Self::Grammar { line, reason } => {
                write!(f, "EBNF grammar error at line {}: {}", line, reason)
            }
            Self::Compile(diag) => write!(f, "SFGN compilation error: {}", diag),
        }
    }
}

impl std::error::Error for DocsExtractError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Walk(err) => Some(err),
            Self::Compile(diag) => Some(diag),
            Self::Grammar { .. } => None,
        }
    }
}

impl From<ignore::Error> for DocsExtractError {
    fn from(err: ignore::Error) -> Self {
        Self::Walk(err)
    }
}

impl From<sfgn_core::CompileError> for DocsExtractError {
    fn from(err: sfgn_core::CompileError) -> Self {
        Self::Compile(err)
    }
}

pub type Result<T> = std::result::Result<T, DocsExtractError>;

