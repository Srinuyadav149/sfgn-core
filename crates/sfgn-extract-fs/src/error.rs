use std::fmt;
use std::io;
use std::path::PathBuf;

/// Error type returned by the `sfgn-extract-fs` compiler.
#[derive(Debug)]
pub enum FsExtractError {
    Io { path: PathBuf, source: io::Error },
    Walk(ignore::Error),
    Compile(sfgn_core::CompileError),
}

impl fmt::Display for FsExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "I/O error at '{}': {}", path.display(), source)
            }
            Self::Walk(err) => write!(f, "directory walk error: {}", err),
            Self::Compile(diag) => write!(f, "SFGN compilation error: {}", diag),
        }
    }
}

impl std::error::Error for FsExtractError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Walk(err) => Some(err),
            Self::Compile(diag) => Some(diag),
        }
    }
}

impl From<ignore::Error> for FsExtractError {
    fn from(err: ignore::Error) -> Self {
        Self::Walk(err)
    }
}

impl From<sfgn_core::CompileError> for FsExtractError {
    fn from(err: sfgn_core::CompileError) -> Self {
        Self::Compile(err)
    }
}

pub type Result<T> = std::result::Result<T, FsExtractError>;
