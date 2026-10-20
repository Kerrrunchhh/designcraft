//! A read-only XML DOM on top of `quick-xml`: elements, attributes and text, enough for Scribus
//! documents (one XML file; no namespaces, CDATA or processing instructions are needed).

use quick_xml::events::Event;

/// Deeper nesting is refused (groups in groups, …) so a hostile file can't exhaust the stack.
pub const MAX_DEPTH: usize = 256;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<El>,
}

impl El {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
    /// The first of `keys` that is set: Scribus 1.7 renamed most attributes (`XPosition` was
    /// `XPOS`), so callers pass the new name, then the old one.
    pub fn any(&self, keys: &[&str]) -> Option<&str> {
        keys.iter().find_map(|k| self.get(k))
    }
    /// A finite number from the first of `keys` that holds one.
    pub fn num(&self, keys: &[&str]) -> Option<f64> {
        self.any(keys).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite())
    }
    /// A flag written as `1`/`0` (or `true`/`false`).
    pub fn flag(&self, keys: &[&str]) -> Option<bool> {
        self.any(keys).map(|v| matches!(v.trim(), "1" | "true"))
    }
    pub fn elements(&self) -> impl Iterator<Item = &El> {
        self.children.iter()
    }
    /// Children named any of `names`.
    pub fn named<'a>(&'a self, names: &'a [&'a str]) -> impl Iterator<Item = &'a El> + 'a {
        self.children.iter().filter(move |c| names.contains(&c.name.as_str()))
    }
    pub fn find(&self, names: &[&str]) -> Option<&El> {
        self.children.iter().find(|c| names.contains(&c.name.as_str()))
    }
}

/// Parse `bytes` into the document's root element.
pub fn parse(bytes: &[u8]) -> Result<El, String> {
    let mut r = quick_xml::Reader::from_reader(bytes);
    r.config_mut().trim_text(true);
    let mut stack: Vec<El> = vec![El::default()];
    let mut buf = Vec::new();
    loop {
        let ev = r.read_event_into(&mut buf).map_err(|e| format!("XML error at byte {}: {e}", r.buffer_position()))?;
        match ev {
            Event::Start(e) => {
                if stack.len() > MAX_DEPTH {
                    return Err(format!("XML elements nested deeper than {MAX_DEPTH} levels"));
                }
                stack.push(start_el(&e)?);
            }
            Event::Empty(e) => {
                let el = start_el(&e)?;
                stack.last_mut().ok_or("unbalanced XML")?.children.push(el);
            }
            Event::End(_) => {
                let el = stack.pop().ok_or("unbalanced end tag")?;
                stack.last_mut().ok_or("unbalanced end tag")?.children.push(el);
            }
            Event::Eof => break,
            // Scribus keeps everything in attributes; text between elements is whitespace.
            _ => {}
        }
        buf.clear();
    }
    if stack.len() != 1 {
        return Err("unexpected end of document".into());
    }
    stack.pop().and_then(|d| d.children.into_iter().next()).ok_or_else(|| "empty document".into())
}

fn start_el(e: &quick_xml::events::BytesStart) -> Result<El, String> {
    let mut el = El { name: String::from_utf8_lossy(e.name().as_ref()).into_owned(), ..Default::default() };
    for a in e.attributes().with_checks(false) {
        let a = a.map_err(|e| e.to_string())?;
        let k = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let v = a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map(|v| v.into_owned())
            .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned());
        el.attrs.push((k, v));
    }
    Ok(el)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_entities_and_nesting() {
        let root = parse(br#"<?xml version="1.0"?><A x="1 &amp; 2"><B XPOS="3"/><C><D/></C></A>"#).unwrap();
        assert_eq!(root.get("x"), Some("1 & 2"));
        assert_eq!(root.find(&["B"]).and_then(|b| b.num(&["XPosition", "XPOS"])), Some(3.0));
        assert_eq!(root.find(&["C"]).map(|c| c.children.len()), Some(1));
    }

    #[test]
    fn nesting_past_the_budget_is_refused() {
        let deep = format!("{}{}", "<a>".repeat(MAX_DEPTH + 2), "</a>".repeat(MAX_DEPTH + 2));
        assert!(parse(deep.as_bytes()).is_err());
    }

    #[test]
    fn broken_documents_are_errors() {
        assert!(parse(b"<a><b></a>").is_err());
        assert!(parse(b"").is_err());
        assert!(parse(b"<a>").is_err());
    }
}
