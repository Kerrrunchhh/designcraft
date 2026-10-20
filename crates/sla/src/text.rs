//! Scribus text: character and paragraph attributes, styles and `StoryText` → [`Story`].

use std::collections::HashMap;

use designcraft_doc::{
    Align, BASIC_PARAGRAPH, COLUMN_BREAK, Capitalization, CharAttrs, CharFormat, CharRun, FORCED_LINE_BREAK, FRAME_BREAK, Leading, NO_CHAR_STYLE,
    PAGE_NUMBER, ParaAttrs, ParaFormat, Position, Story, StoryId, TabAlign, TabStop,
};

use crate::import::Palette;
use crate::xml::El;

/// Most characters one story takes: a hostile file can't make the import allocate without bound.
pub const MAX_STORY_BYTES: usize = 64 * 1024 * 1024;

/// Words that end a Scribus font name ("Liberation Serif Bold Italic") and make up its style.
const STYLE_WORDS: &[&str] = &[
    "regular",
    "roman",
    "book",
    "normal",
    "plain",
    "bold",
    "italic",
    "oblique",
    "slanted",
    "light",
    "thin",
    "hairline",
    "medium",
    "semibold",
    "demibold",
    "extrabold",
    "ultrabold",
    "heavy",
    "black",
    "ultralight",
    "extralight",
    "semilight",
    "condensed",
    "cond",
    "narrow",
    "compressed",
    "expanded",
    "extended",
    "wide",
    "semi",
    "demi",
    "extra",
    "ultra",
    "semicondensed",
    "semiexpanded",
    "extracondensed",
    "ultracondensed",
    "italics",
    "it",
];

/// Words a family name can end with that are also style words ("Times New Roman", "Arial
/// Black"): before a lone "Regular" they belong to the family.
const FAMILY_WORDS: &[&str] = &["roman", "book", "black", "light", "medium", "normal", "plain", "wide", "narrow", "thin", "heavy"];

/// A Scribus font name ("Family Style", one string) → (family, style). The longest leading words
/// that name a family `known` knows are the family; without one, the trailing style words are
/// the style.
pub fn split_font(name: &str, known: &dyn Fn(&str) -> bool) -> (String, String) {
    let words: Vec<&str> = name.split_whitespace().take(32).collect();
    let join = |range: std::ops::Range<usize>| words.get(range).unwrap_or_default().join(" ");
    let style_or_regular = |s: String| if s.is_empty() { "Regular".to_string() } else { s };
    if let Some(cut) = (1..=words.len()).rev().find(|cut| known(&join(0..*cut))) {
        return (join(0..cut), style_or_regular(join(cut..words.len())));
    }
    let is_style = |w: &str| STYLE_WORDS.contains(&w.to_ascii_lowercase().as_str());
    let mut cut = words.len();
    // Keep at least one word for the family.
    while cut > 1 && words.get(cut - 1).is_some_and(|w| is_style(w)) {
        cut -= 1;
    }
    if words.len().checked_sub(cut) == Some(2)
        && words.get(cut).is_some_and(|w| FAMILY_WORDS.contains(&w.to_ascii_lowercase().as_str()))
        && words.last().is_some_and(|w| w.eq_ignore_ascii_case("regular"))
    {
        cut += 1;
    }
    (join(0..cut), style_or_regular(join(cut..words.len())))
}

/// Paragraph and character style names: Scribus name → DesignCraft name. The document's default
/// styles become `[Basic Paragraph]` and no character style.
#[derive(Default)]
pub struct StyleNames {
    pub para: HashMap<String, String>,
    pub chars: HashMap<String, String>,
}

impl StyleNames {
    pub fn para(&self, name: Option<&str>) -> String {
        name.and_then(|n| self.para.get(n)).cloned().unwrap_or_else(|| BASIC_PARAGRAPH.into())
    }
    pub fn chars(&self, name: Option<&str>) -> String {
        name.and_then(|n| self.chars.get(n)).cloned().unwrap_or_else(|| NO_CHAR_STYLE.into())
    }
}

/// The character attributes `e` sets (a style, a run, a story default).
pub fn char_attrs(e: &El, palette: &mut Palette<'_>) -> CharAttrs {
    let mut a = CharAttrs::default();
    if let Some(font) = e.any(&["Font", "FONT"]).filter(|f| !f.trim().is_empty()) {
        let (family, style) = palette.font(font);
        a.font_family = Some(family);
        a.font_style = Some(style);
    }
    let size = e.num(&["FontSize", "FONTSIZE"]).filter(|s| *s > 0.0);
    a.size = size.map(|s| s.min(1296.0));
    if let Some(c) = e.any(&["FontColor", "FCOLOR"]) {
        a.fill = Some(palette.swatch(c));
    }
    if let Some(s) = e.num(&["FillShade", "FSHADE"]) {
        a.fill_tint = Some(shade(s));
    }
    if let Some(h) = e.num(&["ScaleHorizontal", "SCALEH"]).filter(|v| *v > 0.0) {
        a.h_scale = Some((h / 100.0).clamp(0.01, 10.0));
    }
    if let Some(v) = e.num(&["ScaleVertical", "SCALEV"]).filter(|v| *v > 0.0) {
        a.v_scale = Some((v / 100.0).clamp(0.01, 10.0));
    }
    // Tracking in percent of the em; DesignCraft's is in thousandths.
    if let Some(k) = e.num(&["Kerning", "KERN"]) {
        a.tracking = Some((k * 10.0).clamp(-1000.0, 10000.0));
    }
    // Baseline offset in percent of the font size.
    if let Some(b) = e.num(&["BaselineOffset", "BASEO"]).filter(|b| *b != 0.0) {
        a.baseline_shift = Some(b / 100.0 * size.unwrap_or(12.0));
    }
    if let Some(features) = e.any(&["Features", "FEATURES"]) {
        for f in features.split_whitespace() {
            match f {
                "underline" | "underlinewords" => a.underline = Some(true),
                "strike" => a.strikethrough = Some(true),
                "superscript" => a.position = Some(Position::Superscript),
                "subscript" => a.position = Some(Position::Subscript),
                "allcaps" => a.capitalization = Some(Capitalization::AllCaps),
                "smallcaps" => a.capitalization = Some(Capitalization::SmallCaps),
                "outline" => {
                    let color = e.any(&["StrokeColor", "SCOLOR"]).unwrap_or("Black");
                    a.stroke = Some(palette.swatch(color));
                    // Outline width in tenths of a percent of the font size.
                    let width = e.num(&["TextOutlineWidth", "TXTOUT"]).unwrap_or(1.0);
                    a.stroke_weight = Some((width / 1000.0 * size.unwrap_or(12.0)).clamp(0.1, 100.0));
                }
                _ => {}
            }
        }
    }
    a
}

/// The paragraph attributes `e` sets, and the leading (a character attribute in DesignCraft).
pub fn para_attrs(e: &El) -> (ParaAttrs, Option<Leading>) {
    let mut p = ParaAttrs {
        align: e.num(&["Alignment", "ALIGN"]).map(|a| match a as i64 {
            1 => Align::Center,
            2 => Align::Right,
            3 => Align::LeftJustified,
            4 => Align::FullyJustified,
            _ => Align::Left,
        }),
        ..Default::default()
    };
    let len = |keys: &[&str]| e.num(keys).map(|v| v.clamp(-10_000.0, 10_000.0));
    p.left_indent = len(&["LeftMargin", "INDENT"]);
    p.right_indent = len(&["RightMargin", "RMARGIN"]);
    p.first_line_indent = len(&["FirstIndent", "FIRST"]);
    p.space_before = len(&["GapBefore", "VOR"]).map(|v| v.max(0.0));
    p.space_after = len(&["GapAfter", "NACH"]).map(|v| v.max(0.0));
    if let Some(drop) = e.flag(&["HasDropCap", "DROP"]) {
        let lines = e.num(&["DropCapLines", "DROPLIN"]).unwrap_or(2.0).clamp(1.0, 25.0) as u32;
        p.drop_cap_lines = Some(if drop { lines } else { 0 });
        p.drop_cap_chars = Some(u32::from(drop));
    }
    let tabs: Vec<TabStop> = e
        .named(&["Tabs"])
        .take(256)
        .filter_map(|t| {
            let position = t.num(&["Pos"])?.clamp(0.0, 10_000.0);
            let (align, align_on) = match t.num(&["Type"]).unwrap_or(0.0) as i64 {
                1 => (TabAlign::Right, String::new()),
                2 => (TabAlign::Char, ".".to_string()),
                3 => (TabAlign::Char, ",".to_string()),
                4 => (TabAlign::Center, String::new()),
                _ => (TabAlign::Left, String::new()),
            };
            let leader = t.get("Fill").unwrap_or("").chars().take(8).collect();
            Some(TabStop { position, align, leader, align_on })
        })
        .collect();
    if !tabs.is_empty() {
        p.tabs = Some(tabs);
    }
    // 0: fixed (LINESP), 1: automatic, 2: aligned to the baseline grid (fixed here).
    let leading = match e.num(&["LineSpacingMode", "LINESPMode"]).map(|m| m as i64) {
        Some(1) => Some(Leading::Auto),
        _ => e.num(&["LineSpacing", "LINESP"]).filter(|l| *l > 0.0).map(|l| Leading::Points(l.min(5000.0))),
    };
    (p, leading)
}

/// A shade in percent → a tint (0–1).
pub fn shade(percent: f64) -> f32 {
    (percent / 100.0).clamp(0.0, 1.0) as f32
}

/// One paragraph being built: its format, once a `para`/`trail` element supplies it, and the
/// leading that element sets (applied to the paragraph's runs).
struct Para {
    format: Option<ParaFormat>,
    leading: Option<Leading>,
}

/// A run with the paragraph it belongs to.
struct Run {
    text: String,
    format: CharFormat,
    para: usize,
}

/// Builds a story from `StoryText` elements: runs, special characters and paragraph marks.
pub struct StoryBuilder<'a> {
    names: &'a StyleNames,
    runs: Vec<Run>,
    paras: Vec<Para>,
    bytes: usize,
    base_para: ParaFormat,
    base_leading: Option<Leading>,
    base_chars: CharAttrs,
    pub truncated: bool,
}

impl<'a> StoryBuilder<'a> {
    pub fn new(names: &'a StyleNames) -> Self {
        StoryBuilder {
            names,
            runs: Vec::new(),
            paras: vec![Para { format: None, leading: None }],
            bytes: 0,
            base_para: ParaFormat::default(),
            base_leading: None,
            base_chars: CharAttrs::default(),
            truncated: false,
        }
    }

    /// Read one frame's `StoryText` (or, in old files, the frame's own `ITEXT` children).
    pub fn read(&mut self, story_text: &El, palette: &mut Palette<'_>) {
        for e in story_text.elements() {
            match e.name.as_str() {
                "DefaultStyle" => {
                    let (para, leading) = para_attrs(e);
                    self.base_para = ParaFormat { style: self.names.para(e.any(&["Parent", "PARENT"])), para, ..Default::default() };
                    self.base_leading = leading;
                    self.base_chars = char_attrs(e, palette);
                }
                "ITEXT" | "Content" => {
                    let chars = e.any(&["Chars", "CH"]).unwrap_or("");
                    let format = self.run_format(e, palette);
                    // Old files can hold paragraph separators inside the run.
                    let mut first = true;
                    for piece in chars.split(['\r', '\n', '\u{2029}']) {
                        if !first {
                            self.end_para(None, None, &CharFormat { style: NO_CHAR_STYLE.into(), over: CharAttrs::default() });
                        }
                        first = false;
                        let clean: String = piece.chars().filter(|c| *c == '\t' || !c.is_control()).collect();
                        self.push(&clean, &format);
                    }
                }
                "tab" => self.push_special(e, "\t", palette),
                "breakline" => self.push_special(e, &FORCED_LINE_BREAK.to_string(), palette),
                "nbspace" => self.push_special(e, "\u{a0}", palette),
                "nbhyphen" => self.push_special(e, "\u{2011}", palette),
                "zwnbspace" => self.push_special(e, "\u{2060}", palette),
                "zwspace" => self.push_special(e, "\u{200b}", palette),
                "var" => match e.get("name") {
                    Some("pgno") => self.push_special(e, &PAGE_NUMBER.to_string(), palette),
                    _ => self.push_special(e, "#", palette),
                },
                "breakcol" | "breakframe" => {
                    let c = if e.name == "breakcol" { COLUMN_BREAK } else { FRAME_BREAK };
                    self.push_special(e, &c.to_string(), palette);
                    // A break ends its paragraph in DesignCraft; the next `para` styles both halves.
                    let format = self.run_format(e, palette);
                    self.end_para(None, None, &format);
                }
                "para" => {
                    let (para, leading) = para_attrs(e);
                    let mut format = self.base_para.clone();
                    if let Some(parent) = e.any(&["Parent", "PARENT"]) {
                        format.style = self.names.para(Some(parent));
                    }
                    format.para.merge(&para);
                    let run = self.run_format(e, palette);
                    self.end_para(Some(format), leading.or(self.base_leading), &run);
                }
                "trail" => {
                    let (para, leading) = para_attrs(e);
                    let mut format = self.base_para.clone();
                    if let Some(parent) = e.any(&["Parent", "PARENT"]) {
                        format.style = self.names.para(Some(parent));
                    }
                    format.para.merge(&para);
                    self.style_open(format, leading.or(self.base_leading));
                }
                _ => {}
            }
        }
    }

    fn run_format(&self, e: &El, palette: &mut Palette<'_>) -> CharFormat {
        let mut over = self.base_chars.clone();
        over.merge(&char_attrs(e, palette));
        CharFormat { style: self.names.chars(e.any(&["CParent", "CPARENT"])), over }
    }

    fn push_special(&mut self, e: &El, s: &str, palette: &mut Palette<'_>) {
        let format = self.run_format(e, palette);
        self.push(s, &format);
    }

    fn push(&mut self, s: &str, format: &CharFormat) {
        if s.is_empty() || self.truncated {
            return;
        }
        if self.bytes.saturating_add(s.len()) > MAX_STORY_BYTES {
            self.truncated = true;
            return;
        }
        self.bytes += s.len();
        let para = self.paras.len().saturating_sub(1);
        match self.runs.last_mut() {
            Some(r) if r.para == para && r.format == *format => r.text.push_str(s),
            _ => self.runs.push(Run { text: s.to_string(), format: format.clone(), para }),
        }
    }

    /// The paragraphs still without a format (the open one, and those a break ended) take
    /// `format`.
    fn style_open(&mut self, format: ParaFormat, leading: Option<Leading>) {
        for p in self.paras.iter_mut().rev() {
            if p.format.is_some() {
                break;
            }
            p.format = Some(format.clone());
            p.leading = leading;
        }
    }

    /// End the open paragraph (styled with `format`, if given) and start the next one.
    fn end_para(&mut self, format: Option<ParaFormat>, leading: Option<Leading>, mark: &CharFormat) {
        if self.truncated || self.bytes.saturating_add(1) > MAX_STORY_BYTES {
            self.truncated = true;
            return;
        }
        self.push("\n", mark);
        if let Some(f) = format {
            self.style_open(f, leading);
        }
        self.paras.push(Para { format: None, leading: None });
    }

    /// Whether anything has been read (text or paragraph marks).
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    pub fn finish(mut self, id: StoryId) -> Story {
        let base = self.base_para.clone();
        let base_leading = self.base_leading;
        self.style_open(base, base_leading);
        let mut story = Story::new(id);
        let mut text = String::with_capacity(self.bytes);
        let mut chars: Vec<CharRun> = Vec::new();
        for r in &self.runs {
            let mut format = r.format.clone();
            if format.over.leading.is_none() {
                format.over.leading = self.paras.get(r.para).and_then(|p| p.leading);
            }
            text.push_str(&r.text);
            match chars.last_mut() {
                Some(last) if last.format == format => last.len += r.text.len(),
                _ => chars.push(CharRun { len: r.text.len(), format }),
            }
        }
        if chars.is_empty() {
            chars.push(CharRun { len: 0, format: CharFormat { style: NO_CHAR_STYLE.into(), over: self.base_chars.clone() } });
        }
        let paras: Vec<ParaFormat> = self.paras.into_iter().map(|p| p.format.unwrap_or_default()).collect();
        story.text = text;
        story.chars = chars;
        story.paras = paras;
        story
    }
}
