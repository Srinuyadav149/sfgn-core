use std::fmt::Write;

use sfgn_llm::escape_json_str;

/// Progressive reveal depth for an SFGN node projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealLevel {
    /// Level 0: Root / directory index (`>` child addresses only; `payload` omitted).
    Level0Index,
    /// Level 1: File / doc / tool-branch outline (`>` typed signatures & `>>` relatives; `payload` omitted).
    Level1Outline,
    /// Level 2: Leaf payload (`payload: """..."""` verbatim source/text or tool schema + instructions).
    Level2Leaf,
}

impl RevealLevel {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Level0Index => "LEVEL 0 - INDEX",
            Self::Level1Outline => "LEVEL 1 - OUTLINE",
            Self::Level2Leaf => "LEVEL 2 - LEAF",
        }
    }
}

/// Typed property value rendered inside a projected SFGN node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropValue {
    Str(String),
    Number(String),
    Bool(bool),
    Null,
    Array(Vec<String>),
    /// Raw JSON snippet (used for tool `"schema"` and `"example"` lines).
    RawJson(String),
}

/// Single key-value property on a [`GraphNode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeProp {
    pub key: String,
    pub value: PropValue,
}

/// Projected SFGN graph node presented to the LLM inside `ACTIVE SFGN NODES`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNode {
    /// Canonical node address (`"workspace"`, `"src/"`, `"src/flat_path.rs"`,
    /// `"src/flat_path.rs::fn encode_flat_rel_path"`, `"tools/memory/record_memory"`).
    pub address: String,
    /// Progressive reveal level of this projection.
    pub level: RevealLevel,
    /// Scalar / array properties (`"kind"`, `"signature"`, `"lines"`, `"summary"`, etc.).
    pub props: Vec<NodeProp>,
    /// Containment edges (`> [...]`).
    pub children: Vec<String>,
    /// Associative / cross-graph edges (`>> [...]`).
    pub relatives: Vec<String>,
    /// Optional multiline format tag (`"rust"`, `"md"`, `"csv"`, `"text"`).
    pub payload_tag: Option<String>,
    /// Optional verbatim multiline `payload: """..."""` (present at Level 2).
    pub payload: Option<String>,
}

impl GraphNode {
    pub fn new(address: impl Into<String>, level: RevealLevel, kind: &str) -> Self {
        Self {
            address: address.into(),
            level,
            props: vec![NodeProp {
                key: "kind".to_owned(),
                value: PropValue::Str(kind.to_owned()),
            }],
            children: Vec::new(),
            relatives: Vec::new(),
            payload_tag: None,
            payload: None,
        }
    }

    pub fn add_str_prop(&mut self, key: impl Into<String>, val: impl Into<String>) {
        self.props.push(NodeProp {
            key: key.into(),
            value: PropValue::Str(val.into()),
        });
    }

    pub fn add_num_prop(&mut self, key: impl Into<String>, raw: impl Into<String>) {
        self.props.push(NodeProp {
            key: key.into(),
            value: PropValue::Number(raw.into()),
        });
    }

    pub fn add_array_prop(&mut self, key: impl Into<String>, items: Vec<String>) {
        self.props.push(NodeProp {
            key: key.into(),
            value: PropValue::Array(items),
        });
    }

    pub fn add_raw_json_prop(&mut self, key: impl Into<String>, raw_json: impl Into<String>) {
        self.props.push(NodeProp {
            key: key.into(),
            value: PropValue::RawJson(raw_json.into()),
        });
    }

    pub fn set_payload(&mut self, tag: Option<impl Into<String>>, text: impl Into<String>) {
        self.payload_tag = tag.map(Into::into);
        self.payload = Some(text.into());
    }

    /// Renders this node in clean, token-minimal SFGN notation.
    pub fn render_sfgn(&self) -> String {
        let mut out = String::with_capacity(256);
        let _ = writeln!(out, "{}>:", self.address);

        for prop in &self.props {
            let _ = write!(out, "  {}: ", escape_json_str(&prop.key));
            match &prop.value {
                PropValue::Str(s) => {
                    let _ = writeln!(out, "{}", escape_json_str(s));
                }
                PropValue::Number(n) => {
                    let _ = writeln!(out, "{}", n);
                }
                PropValue::Bool(b) => {
                    let _ = writeln!(out, "{}", if *b { "true" } else { "false" });
                }
                PropValue::Null => {
                    out.push_str("null\n");
                }
                PropValue::Array(items) => {
                    out.push('[');
                    for (idx, item) in items.iter().enumerate() {
                        if idx > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(&escape_json_str(item));
                    }
                    out.push_str("]\n");
                }
                PropValue::RawJson(raw) => {
                    let _ = writeln!(out, "{}", raw);
                }
            }
        }

        if !self.children.is_empty() || self.level != RevealLevel::Level2Leaf {
            out.push_str("  > [");
            if self.children.len() <= 4 {
                for (idx, child) in self.children.iter().enumerate() {
                    if idx > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&escape_json_str(child));
                }
                out.push_str("]\n");
            } else {
                out.push('\n');
                for (idx, child) in self.children.iter().enumerate() {
                    let comma = if idx + 1 < self.children.len() {
                        ","
                    } else {
                        ""
                    };
                    let _ = writeln!(out, "    {}{}", escape_json_str(child), comma);
                }
                out.push_str("  ]\n");
            }
        }

        if !self.relatives.is_empty() {
            out.push_str("  >> [");
            for (idx, rel) in self.relatives.iter().enumerate() {
                if idx > 0 {
                    out.push_str(", ");
                }
                out.push_str(&escape_json_str(rel));
            }
            out.push_str("]\n");
        }

        if let Some(body) = &self.payload {
            let tag = self.payload_tag.as_deref().unwrap_or("");
            let _ = writeln!(out, "  \"payload\": \"\"\"{}", tag);
            out.push_str(body);
            if !body.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("  \"\"\"\n");
        }

        out
    }
}
