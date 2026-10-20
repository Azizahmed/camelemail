//! Lossless iCalendar (RFC 5545) document model.
//!
//! Parsing unfolds content lines and keeps every component, property and
//! parameter, so an object fetched from a server can be edited and written
//! back without dropping data this module does not understand.

use anyhow::{bail, Result};

/// Deepest component nesting kept by [`Component::parse`]; anything deeper is
/// skipped, so hostile input can't overflow the stack when the tree is
/// cloned, written or dropped (real data nests three levels).
pub const MAX_DEPTH: usize = 16;

/// One content line: `NAME;PARAM=VALUE:value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// Upper-cased property name.
    pub name: String,
    /// Parameters in wire order; names upper-cased, single quoted values unquoted.
    pub params: Vec<(String, String)>,
    /// The value exactly as on the wire (TEXT values are still escaped).
    pub value: String,
}

impl Property {
    /// A property with a raw (already escaped) value and no parameters.
    pub fn new(name: &str, value: &str) -> Property {
        Property {
            name: name.to_ascii_uppercase(),
            params: Vec::new(),
            value: value.to_string(),
        }
    }

    /// A TEXT property; `text` is escaped for the wire.
    pub fn new_text(name: &str, text: &str) -> Property {
        Property::new(name, &escape_text(text))
    }

    /// Builder-style parameter append.
    pub fn with_param(mut self, name: &str, value: &str) -> Property {
        self.params
            .push((name.to_ascii_uppercase(), value.to_string()));
        self
    }

    /// The value with TEXT escapes (`\n`, `\,`, `\;`, `\\`) resolved.
    pub fn text(&self) -> String {
        unescape_text(&self.value)
    }

    /// First value of a parameter (case-insensitive name).
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn write(&self, out: &mut String) {
        let mut line = self.name.clone();
        for (name, value) in &self.params {
            line.push(';');
            line.push_str(name);
            line.push('=');
            if !value.starts_with('"') && value.contains([':', ';', ',']) {
                line.push('"');
                line.push_str(&value.replace('"', "'"));
                line.push('"');
            } else {
                line.push_str(value);
            }
        }
        line.push(':');
        line.push_str(&self.value);
        fold_into(&line, out);
    }
}

/// A `BEGIN:NAME` … `END:NAME` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// Upper-cased component name (`VCALENDAR`, `VEVENT`, …).
    pub name: String,
    pub props: Vec<Property>,
    pub children: Vec<Component>,
}

impl Component {
    /// An empty component.
    pub fn new(name: &str) -> Component {
        Component {
            name: name.to_ascii_uppercase(),
            props: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Parse an iCalendar stream and return its `VCALENDAR`.
    ///
    /// Tolerates LF-only line endings, a BOM, lower-case names, junk lines and
    /// trailing data. Bare components without a calendar wrapper are wrapped.
    /// Components nested deeper than [`MAX_DEPTH`] are dropped.
    pub fn parse(ics: &str) -> Result<Component> {
        let ics = ics.trim_start_matches('\u{feff}');
        let mut stack: Vec<Component> = Vec::new();
        let mut roots: Vec<Component> = Vec::new();
        // Open BEGINs below MAX_DEPTH being skipped.
        let mut skipping = 0usize;
        for line in unfold(ics) {
            let Some(prop) = parse_line(&line) else {
                continue;
            };
            if skipping > 0 {
                match prop.name.as_str() {
                    "BEGIN" => skipping += 1,
                    "END" => skipping -= 1,
                    _ => {}
                }
                continue;
            }
            match prop.name.as_str() {
                "BEGIN" if stack.len() >= MAX_DEPTH => skipping = 1,
                "BEGIN" => stack.push(Component::new(prop.value.trim())),
                "END" => {
                    let name = prop.value.trim().to_ascii_uppercase();
                    if !stack.iter().any(|c| c.name == name) {
                        continue;
                    }
                    while let Some(done) = stack.pop() {
                        let matched = done.name == name;
                        match stack.last_mut() {
                            Some(parent) => parent.children.push(done),
                            None => roots.push(done),
                        }
                        if matched {
                            break;
                        }
                    }
                    if stack.is_empty() && roots.iter().any(|r| r.name == "VCALENDAR") {
                        break;
                    }
                }
                _ => {
                    if let Some(cur) = stack.last_mut() {
                        cur.props.push(prop);
                    }
                }
            }
        }
        while let Some(done) = stack.pop() {
            match stack.last_mut() {
                Some(parent) => parent.children.push(done),
                None => roots.push(done),
            }
        }
        if let Some(pos) = roots.iter().position(|r| r.name == "VCALENDAR") {
            return Ok(roots.swap_remove(pos));
        }
        if roots.is_empty() {
            bail!("no iCalendar data found");
        }
        let mut cal = Component::new("VCALENDAR");
        cal.props.push(Property::new("VERSION", "2.0"));
        cal.children = roots;
        Ok(cal)
    }

    /// Serialize with CRLF line endings, folding lines at 75 octets.
    pub fn to_ics(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        fold_into(&format!("BEGIN:{}", self.name), out);
        for p in &self.props {
            p.write(out);
        }
        for c in &self.children {
            c.write(out);
        }
        fold_into(&format!("END:{}", self.name), out);
    }

    /// First property with this name.
    pub fn prop(&self, name: &str) -> Option<&Property> {
        self.props
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// All properties with this name.
    pub fn props<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Property> {
        self.props
            .iter()
            .filter(move |p| p.name.eq_ignore_ascii_case(name))
    }

    /// Replace every property named `p.name` with `p` (kept at the first one's position).
    pub fn set_prop(&mut self, p: Property) {
        match self
            .props
            .iter()
            .position(|x| x.name.eq_ignore_ascii_case(&p.name))
        {
            Some(pos) => {
                let name = p.name.clone();
                self.props[pos] = p;
                let mut i = 0;
                self.props.retain(|x| {
                    i += 1;
                    i - 1 == pos || !x.name.eq_ignore_ascii_case(&name)
                });
            }
            None => self.props.push(p),
        }
    }

    /// Drop every property with this name.
    pub fn remove_props(&mut self, name: &str) {
        self.props.retain(|p| !p.name.eq_ignore_ascii_case(name));
    }

    /// Child components with this name.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Component> {
        self.children
            .iter()
            .filter(move |c| c.name.eq_ignore_ascii_case(name))
    }
}

/// Escape TEXT for the wire.
pub fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// Resolve TEXT escapes.
pub fn unescape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some(c) => out.push(c),
            None => out.push('\\'),
        }
    }
    out
}

/// Split into logical lines, joining folded continuations.
fn unfold(ics: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match raw.strip_prefix([' ', '\t']) {
            Some(rest) if !lines.is_empty() => {
                if let Some(last) = lines.last_mut() {
                    last.push_str(rest);
                }
            }
            _ => {
                if !raw.trim().is_empty() {
                    lines.push(raw.to_string());
                }
            }
        }
    }
    lines
}

/// Parse one unfolded content line; `None` for junk.
fn parse_line(line: &str) -> Option<Property> {
    let bytes = line.as_bytes();
    let name_end = line.find([';', ':'])?;
    let name = line[..name_end].trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let mut params = Vec::new();
    let mut i = name_end;
    while i < bytes.len() && bytes[i] == b';' {
        i += 1;
        let pstart = i;
        while i < bytes.len() && !matches!(bytes[i], b'=' | b';' | b':') {
            i += 1;
        }
        let pname = line[pstart..i].trim().to_ascii_uppercase();
        let mut pvalue = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            let vstart = i;
            let mut in_quotes = false;
            while i < bytes.len() {
                match bytes[i] {
                    b'"' => in_quotes = !in_quotes,
                    b';' | b':' if !in_quotes => break,
                    _ => {}
                }
                i += 1;
            }
            pvalue = line[vstart..i].to_string();
            if pvalue.len() >= 2
                && pvalue.starts_with('"')
                && pvalue.ends_with('"')
                && !pvalue[1..pvalue.len() - 1].contains('"')
            {
                pvalue = pvalue[1..pvalue.len() - 1].to_string();
            }
        }
        if !pname.is_empty() {
            params.push((pname, pvalue));
        }
    }
    if i >= bytes.len() || bytes[i] != b':' {
        return None;
    }
    Some(Property {
        name: name.to_ascii_uppercase(),
        params,
        value: line[i + 1..].to_string(),
    })
}

/// Append `line` folded at 75 octets (never inside a UTF-8 sequence) plus CRLF.
fn fold_into(line: &str, out: &mut String) {
    let mut limit = 75;
    let mut rest = line;
    while rest.len() > limit {
        let mut cut = limit;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push_str(&rest[..cut]);
        out.push_str("\r\n ");
        rest = &rest[cut..];
        limit = 74;
    }
    out.push_str(rest);
    out.push_str("\r\n");
}
