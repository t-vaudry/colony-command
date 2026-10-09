//! A span-keeping JSON scanner. The settings editor never re-serializes the
//! user's file: it finds where things are and splices around them, so every
//! byte it doesn't mean to touch stays exactly as it was.
//!
//! Input is validated with serde_json first, so this only has to be correct on
//! valid JSON; it still fails cleanly rather than panicking on anything odd.

#[derive(Debug, Clone)]
pub struct Node {
    pub start: usize,
    /// One past the last byte.
    pub end: usize,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub enum Kind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    Str(String),
    Other,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub key: String,
    /// Where the key's opening quote is. The member spans `start..value.end`.
    pub start: usize,
    pub value: Node,
}

impl Node {
    pub fn members(&self) -> Option<&[Member]> {
        match &self.kind {
            Kind::Object(m) => Some(m),
            _ => None,
        }
    }
    pub fn items(&self) -> Option<&[Node]> {
        match &self.kind {
            Kind::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn member(&self, key: &str) -> Option<&Member> {
        self.members()?.iter().find(|m| m.key == key)
    }
    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            Kind::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Parses the document (after any BOM). The caller has already checked it is valid JSON.
pub fn scan(text: &str, from: usize) -> Result<Node, String> {
    let mut p = P { b: text.as_bytes(), t: text, i: from };
    p.ws();
    let n = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err("unexpected content after the JSON value".into());
    }
    Ok(n)
}

struct P<'a> {
    b: &'a [u8],
    t: &'a str,
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }

    fn string(&mut self) -> Result<(usize, String), String> {
        let start = self.i;
        if self.b.get(self.i) != Some(&b'"') {
            return Err(format!("expected a string at byte {start}"));
        }
        self.i += 1;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    let decoded: String = serde_json::from_str(&self.t[start..self.i]).map_err(|e| e.to_string())?;
                    return Ok((start, decoded));
                }
                _ => self.i += 1,
            }
        }
        Err("unterminated string".into())
    }

    fn value(&mut self, depth: u32) -> Result<Node, String> {
        if depth > 64 {
            return Err("nested too deeply".into());
        }
        let start = self.i;
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut members = Vec::new();
                loop {
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b'}') => {
                            self.i += 1;
                            break;
                        }
                        Some(b',') => self.i += 1,
                        Some(b'"') => {
                            let (kstart, key) = self.string()?;
                            self.ws();
                            if self.b.get(self.i) != Some(&b':') {
                                return Err("expected ':'".into());
                            }
                            self.i += 1;
                            self.ws();
                            let value = self.value(depth + 1)?;
                            members.push(Member { key, start: kstart, value });
                        }
                        _ => return Err(format!("unexpected byte in object at {}", self.i)),
                    }
                }
                Ok(Node { start, end: self.i, kind: Kind::Object(members) })
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        Some(b',') => self.i += 1,
                        Some(_) => items.push(self.value(depth + 1)?),
                        None => return Err("unterminated array".into()),
                    }
                }
                Ok(Node { start, end: self.i, kind: Kind::Array(items) })
            }
            Some(b'"') => {
                let (_, s) = self.string()?;
                Ok(Node { start, end: self.i, kind: Kind::Str(s) })
            }
            Some(_) => {
                while self.i < self.b.len() && !matches!(self.b[self.i], b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n') {
                    self.i += 1;
                }
                if self.i == start {
                    return Err(format!("unexpected byte at {start}"));
                }
                Ok(Node { start, end: self.i, kind: Kind::Other })
            }
            None => Err("unexpected end of input".into()),
        }
    }
}
