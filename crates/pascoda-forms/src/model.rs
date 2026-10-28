//! The in-memory form model: a tree of components with typed property values.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    Int(i64),
    Float(f64),
    Str(String),
    /// Enum members, `True`/`False`, event handler names, references to other components.
    Ident(String),
    Set(Vec<String>),
    List(Vec<PropValue>),
    /// `<item ... end item ... end>`: one property list per item.
    Collection(Vec<Vec<(String, PropValue)>>),
    Binary(Vec<u8>),
    /// Source text kept as written, for values we cannot represent (e.g. integers beyond i64).
    Raw(String),
}

impl PropValue {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            PropValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropValue::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_ident(&self) -> Option<&str> {
        match self {
            PropValue::Ident(s) => Some(s),
            _ => None,
        }
    }

    /// `True` / `False` are plain identifiers in LFM.
    pub fn as_bool(&self) -> Option<bool> {
        match self.as_ident()?.to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    pub fn bool(b: bool) -> Self {
        PropValue::Ident(if b { "True" } else { "False" }.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Object,
    /// A component inherited from an ancestor form.
    Inherited,
    /// A frame placed on the form.
    Inline,
}

impl Kind {
    pub fn keyword(self) -> &'static str {
        match self {
            Kind::Object => "object",
            Kind::Inherited => "inherited",
            Kind::Inline => "inline",
        }
    }
}

/// A property as read from a file, with where it sits in the source text.
#[derive(Debug, Clone)]
pub struct Property {
    /// Possibly dotted: `Font.Style`, `AnchorSideLeft.Control`.
    pub name: String,
    pub value: PropValue,
    /// Name through the end of the value.
    pub span: Range<usize>,
    pub value_span: Range<usize>,
}

#[derive(Debug, Clone)]
pub struct Component {
    pub kind: Kind,
    pub name: String,
    pub class: String,
    /// Delphi's optional `[n]` creation index.
    pub index: Option<i64>,
    pub props: Vec<Property>,
    pub children: Vec<Component>,
    /// Keyword through the closing `end`. Empty for components built in memory.
    pub span: Range<usize>,
    pub name_span: Range<usize>,
    /// End of the header line's last token (after the class or the `[n]`).
    pub header_end: usize,
    /// Start of the closing `end` token.
    pub end_start: usize,
    /// Whitespace before the keyword when it starts its line.
    pub indent: String,
}

impl Component {
    /// A component to be inserted into a form.
    pub fn new(name: &str, class: &str) -> Self {
        Self {
            kind: Kind::Object,
            name: name.into(),
            class: class.into(),
            index: None,
            props: Vec::new(),
            children: Vec::new(),
            span: 0..0,
            name_span: 0..0,
            header_end: 0,
            end_start: 0,
            indent: String::new(),
        }
    }

    pub fn with_prop(mut self, name: &str, value: PropValue) -> Self {
        self.props.push(Property {
            name: name.into(),
            value,
            span: 0..0,
            value_span: 0..0,
        });
        self
    }

    pub fn with_child(mut self, child: Component) -> Self {
        self.children.push(child);
        self
    }

    /// Pascal is case-insensitive, so property lookup is too.
    pub fn prop(&self, name: &str) -> Option<&PropValue> {
        self.props
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .map(|p| &p.value)
    }

    pub fn child(&self, name: &str) -> Option<&Component> {
        self.children
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Same content, ignoring where it came from in the source text.
    pub fn same_as(&self, other: &Component) -> bool {
        self.kind == other.kind
            && self.name == other.name
            && self.class == other.class
            && self.index == other.index
            && self.props.len() == other.props.len()
            && self
                .props
                .iter()
                .zip(&other.props)
                .all(|(a, b)| a.name == b.name && a.value == b.value)
            && self.children.len() == other.children.len()
            && self
                .children
                .iter()
                .zip(&other.children)
                .all(|(a, b)| a.same_as(b))
    }
}
