use std::fmt;

/// 1-based line and column coordinates within an SFGN source buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineCol {
    pub line: usize,
    pub col: usize,
}

impl LineCol {
    #[inline]
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }

    #[inline]
    pub const fn start() -> Self {
        Self { line: 1, col: 1 }
    }
}

impl fmt::Display for LineCol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// Mandatory fail-fast diagnostic codes defined in SFGN v1.0 Section 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// Invalid UTF-8 sequence, leading UTF-8 BOM, standalone `\r`, or illegal control byte (`0x00`–`0x1F`, `0x7F`).
    E001,
    /// File lacks a leading `>:` block, or declares `>:` more than once.
    E002,
    /// Byte at column 0 is not `>`, `"`, `#`, horizontal whitespace before EOL, or EOF.
    E003,
    /// First Depth 1 line uses 1 space, mixes tabs and spaces, or places whitespace after `\t`.
    E004,
    /// Subsequent Depth 1 line does not match the locked `Tab` or `Spaces(N)` prefix.
    E005,
    /// Line begins with extra whitespace beyond the locked Depth 1 prefix outside a `"""` block.
    E006,
    /// Reached EOL or EOF before closing `"` on a node segment, key, scalar, or external path.
    E007,
    /// Unrecognized escape character or malformed `\u{...}` scalar inside a quoted string.
    E008,
    /// Missing `:` after `NodePath`, trailing `>` without segment, or trailing tokens before EOL.
    E009,
    /// `#` in an identifier chain not followed by one or more ASCII digits.
    E010,
    /// Encountered `[` or `{` inside an unquoted `[...]` collection.
    E011,
    /// Reached EOL or EOF before closing `]` on a `TargetArray` or `FlatCollection`.
    E012,
    /// Reached EOF before encountering a closing `"""` delimiter line.
    E013,
    /// Non-whitespace tokens other than a valid `FormatTag` appear after opening `"""`.
    E014,
    /// Unclosed `<` in `ExternalRef`, missing quoted file path, or invalid trailing path syntax.
    E015,
    /// Two node blocks resolve to the exact same canonical `NodePath` within the same file.
    E016,
    /// A declared node is not reachable as a child (`>`) of `>:` or any other connected node.
    E017,
}

impl ErrorCode {
    /// Returns the canonical error code string (e.g., `"E001"`).
    pub const fn code(self) -> &'static str {
        match self {
            Self::E001 => "E001",
            Self::E002 => "E002",
            Self::E003 => "E003",
            Self::E004 => "E004",
            Self::E005 => "E005",
            Self::E006 => "E006",
            Self::E007 => "E007",
            Self::E008 => "E008",
            Self::E009 => "E009",
            Self::E010 => "E010",
            Self::E011 => "E011",
            Self::E012 => "E012",
            Self::E013 => "E013",
            Self::E014 => "E014",
            Self::E015 => "E015",
            Self::E016 => "E016",
            Self::E017 => "E017",
        }
    }

    /// Returns the specification diagnostic identifier name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::E001 => "InvalidEncodingOrControlByte",
            Self::E002 => "MissingOrDuplicateRootNode",
            Self::E003 => "UnexpectedColumnZeroToken",
            Self::E004 => "InvalidInitialIndent",
            Self::E005 => "MismatchedIndentation",
            Self::E006 => "DepthTwoIndentation",
            Self::E007 => "UnterminatedQuotedString",
            Self::E008 => "InvalidEscapeSequence",
            Self::E009 => "MalformedNodeHeader",
            Self::E010 => "MalformedIndexChain",
            Self::E011 => "NestedCollectionError",
            Self::E012 => "UnterminatedCollection",
            Self::E013 => "UnterminatedMultilineBlock",
            Self::E014 => "InvalidMultilineHeader",
            Self::E015 => "MalformedExternalRef",
            Self::E016 => "DuplicateNodeIdentifier",
            Self::E017 => "OrphanNodeError",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.code(), self.name())
    }
}

/// Deterministic fail-fast parser diagnostic with byte offset and `line:col` location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: ErrorCode,
    pub loc: LineCol,
    pub offset: usize,
    pub message: String,
}

impl Diagnostic {
    pub fn new(code: ErrorCode, loc: LineCol, offset: usize, message: impl Into<String>) -> Self {
        Self {
            code,
            loc,
            offset,
            message: message.into(),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} {}: {}",
            self.code.code(),
            self.loc,
            self.code.name(),
            self.message
        )
    }
}

impl std::error::Error for Diagnostic {}

pub type Result<T> = std::result::Result<T, Diagnostic>;
