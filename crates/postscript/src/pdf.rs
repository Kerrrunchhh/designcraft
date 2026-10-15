//! What the program paints, written as a one-page PDF.
//!
//! Paths arrive in device space (the EPS's default user space). Clipping is applied lazily:
//! before each painting operation the clips in force are matched against those the content
//! stream has applied, popping (`Q`) and pushing (`q … W n`) the difference, so the PostScript
//! program's own gsave/grestore pattern doesn't matter.

use std::rc::Rc;

use pdf_writer::types::{LineCapStyle, LineJoinStyle};
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref};

use crate::gstate::{Clip, DevColor, M, Seg, clip_chain};

/// The most path segments and images written.
const MAX_SEGMENTS: usize = 20_000_000;
const MAX_IMAGES: usize = 100_000;

#[derive(Clone, Debug, PartialEq)]
pub enum ImgSpace {
    Gray,
    Rgb,
    Cmyk,
    Indexed { base: Box<ImgSpace>, hival: u8, lookup: Vec<u8> },
}

#[derive(Debug)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub bpc: u8,
    /// None for a stencil mask (`imagemask`).
    pub space: Option<ImgSpace>,
    pub decode: Option<Vec<f32>>,
    pub data: Vec<u8>,
    /// `data` is a JPEG (DCTDecode passed through).
    pub dct: bool,
    pub interpolate: bool,
    /// An 8-bit soft mask (w, h, alpha).
    pub smask: Option<(u32, u32, Vec<u8>)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct PaintState {
    fill: Option<DevColor>,
    stroke: Option<DevColor>,
}

pub struct StrokeParams<'a> {
    pub width: f64,
    pub cap: i64,
    pub join: i64,
    pub miter: f64,
    pub dash: &'a (Vec<f64>, f64),
}

pub struct Out {
    c: Content,
    images: Vec<Image>,
    applied: Vec<Rc<Clip>>,
    state: PaintState,
    saved: Vec<PaintState>,
    segments: usize,
    /// Anything painted at all.
    pub painted: bool,
}

fn f(v: f64) -> f32 {
    if v.is_finite() { v.clamp(-3.0e7, 3.0e7) as f32 } else { 0.0 }
}

impl Out {
    pub fn new(origin: (f64, f64)) -> Self {
        let mut c = Content::new();
        c.transform([1.0, 0.0, 0.0, 1.0, f(-origin.0), f(-origin.1)]);
        Out { c, images: Vec::new(), applied: Vec::new(), state: PaintState::default(), saved: Vec::new(), segments: 0, painted: false }
    }

    fn sync_clip(&mut self, clip: &Option<Rc<Clip>>) -> Result<(), String> {
        let target = clip_chain(clip);
        let common = self.applied.iter().zip(&target).take_while(|(a, b)| Rc::ptr_eq(a, b)).count();
        while self.applied.len() > common {
            self.applied.pop();
            self.c.restore_state();
            self.state = self.saved.pop().unwrap_or_default();
        }
        for node in target.into_iter().skip(common) {
            self.c.save_state();
            self.saved.push(self.state.clone());
            self.path(&node.path)?;
            if node.eo {
                self.c.clip_even_odd();
            } else {
                self.c.clip_nonzero();
            }
            self.c.end_path();
            self.applied.push(node);
        }
        Ok(())
    }

    fn path(&mut self, path: &[Seg]) -> Result<(), String> {
        self.segments = self.segments.saturating_add(path.len());
        if self.segments > MAX_SEGMENTS {
            return Err("too many path segments".into());
        }
        for s in path {
            match *s {
                Seg::Move(x, y) => {
                    self.c.move_to(f(x), f(y));
                }
                Seg::Line(x, y) => {
                    self.c.line_to(f(x), f(y));
                }
                Seg::Curve(a, b, c, d, e, g) => {
                    self.c.cubic_to(f(a), f(b), f(c), f(d), f(e), f(g));
                }
                Seg::Close => {
                    self.c.close_path();
                }
            }
        }
        Ok(())
    }

    fn set_fill(&mut self, color: DevColor) {
        if self.state.fill == Some(color) {
            return;
        }
        match color {
            DevColor::Gray(g) => self.c.set_fill_gray(f(g)),
            DevColor::Rgb(r, g, b) => self.c.set_fill_rgb(f(r), f(g), f(b)),
            DevColor::Cmyk(c, m, y, k) => self.c.set_fill_cmyk(f(c), f(m), f(y), f(k)),
        };
        self.state.fill = Some(color);
    }

    fn set_stroke(&mut self, color: DevColor) {
        if self.state.stroke == Some(color) {
            return;
        }
        match color {
            DevColor::Gray(g) => self.c.set_stroke_gray(f(g)),
            DevColor::Rgb(r, g, b) => self.c.set_stroke_rgb(f(r), f(g), f(b)),
            DevColor::Cmyk(c, m, y, k) => self.c.set_stroke_cmyk(f(c), f(m), f(y), f(k)),
        };
        self.state.stroke = Some(color);
    }

    pub fn fill(&mut self, clip: &Option<Rc<Clip>>, path: &[Seg], eo: bool, color: DevColor) -> Result<(), String> {
        if !path.iter().any(|s| !matches!(s, Seg::Move(..))) {
            return Ok(());
        }
        self.sync_clip(clip)?;
        self.set_fill(color);
        self.path(path)?;
        if eo {
            self.c.fill_even_odd();
        } else {
            self.c.fill_nonzero();
        }
        self.painted = true;
        Ok(())
    }

    /// Stroke a device-space path drawn under `ctm` (the pen is shaped in user space).
    pub fn stroke(&mut self, clip: &Option<Rc<Clip>>, path: &[Seg], ctm: &M, p: &StrokeParams, color: DevColor) -> Result<(), String> {
        if !path.iter().any(|s| !matches!(s, Seg::Move(..))) {
            return Ok(());
        }
        let Some(inv) = ctm.inv() else { return Ok(()) };
        self.sync_clip(clip)?;
        self.set_stroke(color);
        self.c.save_state();
        self.c.transform(ctm.0.map(f));
        let user: Vec<Seg> = path.iter().map(|s| s.map(&inv)).collect();
        self.c.set_line_width(f(p.width.max(0.0)));
        self.c.set_line_cap(match p.cap {
            1 => LineCapStyle::RoundCap,
            2 => LineCapStyle::ProjectingSquareCap,
            _ => LineCapStyle::ButtCap,
        });
        self.c.set_line_join(match p.join {
            1 => LineJoinStyle::RoundJoin,
            2 => LineJoinStyle::BevelJoin,
            _ => LineJoinStyle::MiterJoin,
        });
        self.c.set_miter_limit(f(p.miter.max(1.0)));
        if !p.dash.0.is_empty() && p.dash.0.iter().any(|d| *d > 0.0) && p.dash.0.iter().all(|d| *d >= 0.0 && d.is_finite()) {
            self.c.set_dash_pattern(p.dash.0.iter().map(|d| f(*d)), f(p.dash.1));
        }
        self.path(&user)?;
        self.c.stroke();
        self.c.restore_state();
        self.painted = true;
        Ok(())
    }

    /// Paint an image: `cm` maps the unit square onto the page; stencil masks use `fill`.
    pub fn image(&mut self, clip: &Option<Rc<Clip>>, cm: &M, img: Image, fill: Option<DevColor>) -> Result<(), String> {
        if self.images.len() >= MAX_IMAGES {
            return Err("too many images".into());
        }
        if !cm.finite() || img.w == 0 || img.h == 0 {
            return Ok(());
        }
        self.sync_clip(clip)?;
        if let Some(color) = fill {
            self.set_fill(color);
        }
        let n = self.images.len();
        self.images.push(img);
        self.c.save_state();
        self.c.transform(cm.0.map(f));
        let name = format!("Im{n}");
        self.c.x_object(Name(name.as_bytes()));
        self.c.restore_state();
        self.painted = true;
        Ok(())
    }

    /// The PDF: one page of `size`, its content and images.
    pub fn finish(self, size: (f64, f64)) -> Vec<u8> {
        let mut pdf = Pdf::new();
        let (catalog, pages, page, content) = (Ref::new(1), Ref::new(2), Ref::new(3), Ref::new(4));
        let mut next = 5;
        let mut alloc = || {
            let r = Ref::new(next);
            next += 1;
            r
        };
        let image_refs: Vec<(Ref, Option<Ref>)> = self.images.iter().map(|i| (alloc(), i.smask.as_ref().map(|_| alloc()))).collect();
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        {
            let mut p = pdf.page(page);
            p.parent(pages);
            p.media_box(Rect::new(0.0, 0.0, f(size.0.max(1.0)), f(size.1.max(1.0))));
            p.contents(content);
            let mut res = p.resources();
            let mut xo = res.x_objects();
            for (i, (r, _)) in image_refs.iter().enumerate() {
                let name = format!("Im{i}");
                xo.pair(Name(name.as_bytes()), *r);
            }
            xo.finish();
            res.finish();
            p.finish();
        }
        let mut c = self.c;
        for _ in 0..self.applied.len() {
            c.restore_state();
        }
        let raw = c.finish().into_vec();
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        pdf.stream(content, &z).filter(Filter::FlateDecode);
        for (img, (r, smask)) in self.images.iter().zip(&image_refs) {
            let packed;
            let (data, filter) = if img.dct {
                (&img.data[..], Filter::DctDecode)
            } else {
                packed = miniz_oxide::deflate::compress_to_vec_zlib(&img.data, 6);
                (&packed[..], Filter::FlateDecode)
            };
            let mut x = pdf.image_xobject(*r, data);
            x.filter(filter);
            x.width(i32::try_from(img.w).unwrap_or(i32::MAX));
            x.height(i32::try_from(img.h).unwrap_or(i32::MAX));
            match &img.space {
                None => {
                    x.image_mask(true);
                }
                Some(space) => {
                    x.bits_per_component(i32::from(img.bpc));
                    match space {
                        ImgSpace::Gray => x.color_space().device_gray(),
                        ImgSpace::Rgb => x.color_space().device_rgb(),
                        ImgSpace::Cmyk => x.color_space().device_cmyk(),
                        ImgSpace::Indexed { base, hival, lookup } => {
                            let base = match **base {
                                ImgSpace::Rgb => Name(b"DeviceRGB"),
                                ImgSpace::Cmyk => Name(b"DeviceCMYK"),
                                _ => Name(b"DeviceGray"),
                            };
                            x.color_space().indexed(base, i32::from(*hival), lookup);
                        }
                    }
                }
            }
            if img.space.is_none() {
                x.bits_per_component(1);
            }
            if let Some(d) = &img.decode {
                x.decode(d.iter().copied());
            }
            if img.interpolate {
                x.interpolate(true);
            }
            if let Some(m) = smask {
                x.s_mask(*m);
            }
            x.finish();
            if let (Some(m), Some((w, h, alpha))) = (smask, &img.smask) {
                let z = miniz_oxide::deflate::compress_to_vec_zlib(alpha, 6);
                let mut s = pdf.image_xobject(*m, &z);
                s.filter(Filter::FlateDecode);
                s.width(i32::try_from(*w).unwrap_or(i32::MAX));
                s.height(i32::try_from(*h).unwrap_or(i32::MAX));
                s.color_space().device_gray();
                s.bits_per_component(8);
                s.finish();
            }
        }
        pdf.finish()
    }
}
