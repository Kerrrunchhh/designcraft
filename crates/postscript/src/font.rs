//! Fonts: dictionaries, scaling, and showing text. Type 3 fonts draw through their BuildGlyph
//! or BuildChar procedures; other font types advance by their widths (or an estimate) without
//! drawing until their glyph programs are read.

use crate::gstate::M;
use crate::interp::{Interp, PsErr, R, err, matrix_obj};
use crate::obj::{Key, Obj, PArr, PDict, name};

fn font_matrix(d: &PDict) -> M {
    match d.get_name(b"FontMatrix") {
        Some(Obj::Array(a, _)) => crate::interp::matrix_of(&a).unwrap_or(M([0.001, 0.0, 0.0, 0.001, 0.0, 0.0])),
        _ => M([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]),
    }
}

/// A stand-in for a font the file doesn't carry: a Type 1 shell with StandardEncoding.
fn substitute(ip: &mut Interp, font_name: &[u8]) -> PDict {
    let d = PDict::new(12);
    d.put_name(b"FontName", Obj::lit(font_name));
    d.put_name(b"FontType", Obj::Int(1));
    d.put_name(b"FontMatrix", matrix_obj(&M([0.001, 0.0, 0.0, 0.001, 0.0, 0.0])));
    d.put_name(b"FontBBox", Obj::Array(PArr::new(vec![Obj::Int(0), Obj::Int(-200), Obj::Int(1000), Obj::Int(900)]), false));
    d.put_name(b"PaintType", Obj::Int(0));
    if let Some(enc) = ip.systemdict.get_name(b"StandardEncoding") {
        d.put_name(b"Encoding", enc);
    }
    d.put_name(b"CharStrings", Obj::Dict(PDict::new(1)));
    d.put_name(b"Private", Obj::Dict(PDict::new(1)));
    d.put_name(b"DesignCraftSubstitute", Obj::Bool(true));
    add_fid(ip, &d);
    d
}

fn add_fid(ip: &mut Interp, d: &PDict) {
    if !d.known(&Key::Name(name(b"FID"))) {
        d.put_name(b"FID", Obj::FontId(ip.next_fid));
        ip.next_fid += 1;
    }
}

pub fn find_font(ip: &mut Interp, key: &Obj) -> PDict {
    if let Some(Obj::Dict(d)) = ip.fontdir.get(&key.key()) {
        return d;
    }
    if let Some(Obj::Dict(fonts)) = ip.resources.get_name(b"Font")
        && let Some(Obj::Dict(d)) = fonts.get(&key.key())
    {
        return d;
    }
    let n = crate::ops::basic::text_of(key);
    let d = substitute(ip, &n);
    ip.fontdir.put(key.clone(), Obj::Dict(d.clone()));
    d
}

pub fn findfont(ip: &mut Interp) -> R {
    let k = ip.pop()?;
    let d = find_font(ip, &k);
    ip.push(Obj::Dict(d))
}

pub fn definefont(ip: &mut Interp) -> R {
    let d = ip.pop_dict()?;
    let k = ip.pop()?;
    add_fid(ip, &d);
    ip.fontdir.put(k, Obj::Dict(d.clone()));
    ip.push(Obj::Dict(d))
}

pub fn undefinefont(ip: &mut Interp) -> R {
    let k = ip.pop()?;
    ip.fontdir.remove(&k.key());
    Ok(())
}

/// A copy of a font dictionary with its FontMatrix replaced (and a new FID).
fn transformed(ip: &mut Interp, d: &PDict, m: &M) -> PDict {
    let copy = PDict::new(d.len() + 2);
    for (k, v) in d.entries() {
        copy.put(k, v);
    }
    copy.put_name(b"FontMatrix", matrix_obj(&font_matrix(d).then(m)));
    copy.put_name(b"FID", Obj::FontId(ip.next_fid));
    ip.next_fid += 1;
    copy
}

pub fn scalefont(ip: &mut Interp) -> R {
    let s = ip.pop_num()?;
    let d = ip.pop_dict()?;
    let f = transformed(ip, &d, &M([s, 0.0, 0.0, s, 0.0, 0.0]));
    ip.push(Obj::Dict(f))
}

pub fn makefont(ip: &mut Interp) -> R {
    let m = ip.pop_matrix()?;
    let d = ip.pop_dict()?;
    let f = transformed(ip, &d, &m);
    ip.push(Obj::Dict(f))
}

pub fn setfont(ip: &mut Interp) -> R {
    let d = ip.pop_dict()?;
    ip.gs.font = Some(d);
    Ok(())
}

pub fn currentfont(ip: &mut Interp) -> R {
    let f = match ip.gs.font.clone() {
        Some(f) => f,
        None => find_font(ip, &Obj::lit(b"Courier")),
    };
    ip.push(Obj::Dict(f))
}

pub fn selectfont(ip: &mut Interp) -> R {
    let m = match ip.pop()? {
        Obj::Array(a, _) => crate::interp::matrix_of(&a)?,
        o => {
            let s = o.num().ok_or(PsErr::Ps("typecheck"))?;
            M([s, 0.0, 0.0, s, 0.0, 0.0])
        }
    };
    let k = ip.pop()?;
    let d = match k {
        Obj::Dict(d) => d,
        k => find_font(ip, &k),
    };
    let f = transformed(ip, &d, &m);
    ip.gs.font = Some(f);
    Ok(())
}

/// The glyph name for a character code through the font's Encoding.
fn glyph_name(font: &PDict, code: u8) -> Vec<u8> {
    match font.get_name(b"Encoding") {
        Some(Obj::Array(a, _)) => match a.get(usize::from(code)) {
            Some(Obj::Name(n, _)) => n.to_vec(),
            _ => b".notdef".to_vec(),
        },
        _ => b".notdef".to_vec(),
    }
}

/// A glyph's advance from the font's Metrics or CharStrings-free estimate, in glyph space.
fn estimated_width(font: &PDict, code: u8) -> (f64, f64) {
    let g = glyph_name(font, code);
    if let Some(Obj::Dict(m)) = font.get_name(b"Metrics")
        && let Some(v) = m.get(&Key::Name(name(&g)))
    {
        match v {
            Obj::Array(a, _) if a.len >= 2 => {
                let n: Vec<f64> = a.items().iter().filter_map(Obj::num).collect();
                return (n.get(n.len() - 2).copied().unwrap_or(0.0), n.last().copied().unwrap_or(0.0));
            }
            o => return (o.num().unwrap_or(0.0), 0.0),
        }
    }
    // An average glyph: half the em, as glyph space usually has 1000 units to it.
    let em = 1.0 / font_matrix(font).0[0].abs().max(1e-6);
    (if code == b' ' { 0.25 } else { 0.5 } * em, 0.0)
}

/// A Type 1 glyph of `font` (cached), None for fonts without charstrings.
fn outline(ip: &mut Interp, font: &PDict, code: u8) -> Option<std::rc::Rc<crate::type1::Glyph>> {
    if font.get_name(b"DesignCraftSubstitute").is_some() {
        return None;
    }
    let fid = match font.get_name(b"FID") {
        Some(Obj::FontId(f)) => f,
        _ => 0,
    };
    let g = glyph_name(font, code);
    if let Some(hit) = ip.glyphs.get(&(fid, g.clone())) {
        return Some(hit.clone());
    }
    let std = crate::encodings::standard();
    let glyph = std::rc::Rc::new(crate::type1::glyph(font, &g, &std)?);
    if ip.glyphs.len() < 100_000 {
        ip.glyphs.insert((fid, g), glyph.clone());
    }
    Some(glyph)
}

/// Where a glyph at the current point lands: glyph space → device space.
fn glyph_to_device(ip: &Interp, font: &PDict) -> R<M> {
    let (x, y) = ip.gs.cur.ok_or(PsErr::Ps("nocurrentpoint"))?;
    let (ux, uy) = ip.gs.ctm.inv().ok_or(PsErr::Ps("undefinedresult"))?.apply(x, y);
    Ok(font_matrix(font).then(&M([1.0, 0.0, 0.0, 1.0, ux, uy])).then(&ip.gs.ctm))
}

/// Run one Type 3 glyph at the current point: its advance in glyph space.
fn type3_glyph(ip: &mut Interp, font: &PDict, code: u8) -> R<(f64, f64)> {
    let (x, y) = ip.gs.cur.ok_or(PsErr::Ps("nocurrentpoint"))?;
    let fm = font_matrix(font);
    ip.gsave()?;
    let r = (|| -> R<(f64, f64)> {
        let (ux, uy) = ip.gs.ctm.inv().ok_or(PsErr::Ps("undefinedresult"))?.apply(x, y);
        ip.gs.ctm = fm.then(&M([1.0, 0.0, 0.0, 1.0, ux, uy]).then(&ip.gs.ctm));
        ip.gs.path.clear();
        ip.gs.cur = None;
        ip.gs.char_width = None;
        if let Some(bg) = font.get_name(b"BuildGlyph") {
            ip.push(Obj::Dict(font.clone()))?;
            ip.push(Obj::lit(&glyph_name(font, code)))?;
            ip.call(bg)?;
        } else if let Some(bc) = font.get_name(b"BuildChar") {
            ip.push(Obj::Dict(font.clone()))?;
            ip.push(Obj::Int(i64::from(code)))?;
            ip.call(bc)?;
        }
        Ok(ip.gs.char_width.unwrap_or((0.0, 0.0)))
    })();
    ip.grestore();
    r
}

/// Show (or, with `draw` false, measure) a string: the total advance in user space.
fn show_string(ip: &mut Interp, s: &[u8], draw: bool, per_char: &mut dyn FnMut(&mut Interp, u8, usize) -> R<(f64, f64)>) -> R<(f64, f64)> {
    let font = match ip.gs.font.clone() {
        Some(f) => f,
        None => return err("invalidfont"),
    };
    let fm = font_matrix(&font);
    let type3 = font.get_name(b"FontType").and_then(|o| o.num()) == Some(3.0);
    let mut total = (0.0, 0.0);
    for (i, &code) in s.iter().enumerate() {
        let w = if type3 {
            if draw && ip.gs.cur.is_some() {
                type3_glyph(ip, &font, code)?
            } else {
                let saved = ip.gs.nulldev;
                ip.gs.nulldev = true;
                if ip.gs.cur.is_none() {
                    ip.gs.cur = Some((0.0, 0.0));
                }
                let w = type3_glyph(ip, &font, code);
                ip.gs.nulldev = saved;
                w?
            }
        } else if let Some(g) = outline(ip, &font, code) {
            if draw && !g.path.is_empty() && ip.gs.cur.is_some() {
                let m = glyph_to_device(ip, &font)?;
                let path: Vec<crate::gstate::Seg> = g.path.iter().map(|s| s.map(&m)).collect();
                if ip.charpath {
                    ip.gs.path.extend(path);
                } else {
                    crate::ops::gfx::fill_path(ip, &path, false)?;
                }
            }
            match font.get_name(b"Metrics") {
                Some(_) => estimated_width(&font, code),
                None => g.width,
            }
        } else {
            estimated_width(&font, code)
        };
        let (wx, wy) = fm.delta(w.0, w.1);
        let (ex, ey) = per_char(ip, code, i)?;
        let (dx, dy) = (wx + ex, wy + ey);
        total = (total.0 + dx, total.1 + dy);
        if draw && let Some((cx, cy)) = ip.gs.cur {
            let (ddx, ddy) = ip.gs.ctm.delta(dx, dy);
            ip.gs.cur = Some((cx + ddx, cy + ddy));
        }
    }
    Ok(total)
}

pub fn show(ip: &mut Interp) -> R {
    let s = ip.pop_str()?.bytes();
    if ip.gs.cur.is_none() {
        return err("nocurrentpoint");
    }
    show_string(ip, &s, true, &mut |_, _, _| Ok((0.0, 0.0))).map(|_| ())
}

pub fn ashow(ip: &mut Interp) -> R {
    let s = ip.pop_str()?.bytes();
    let ay = ip.pop_num()?;
    let ax = ip.pop_num()?;
    show_string(ip, &s, true, &mut |_, _, _| Ok((ax, ay))).map(|_| ())
}

pub fn widthshow(ip: &mut Interp) -> R {
    let s = ip.pop_str()?.bytes();
    let ch = ip.pop_int()?;
    let cy = ip.pop_num()?;
    let cx = ip.pop_num()?;
    show_string(ip, &s, true, &mut |_, c, _| Ok(if i64::from(c) == ch { (cx, cy) } else { (0.0, 0.0) })).map(|_| ())
}

pub fn awidthshow(ip: &mut Interp) -> R {
    let s = ip.pop_str()?.bytes();
    let ay = ip.pop_num()?;
    let ax = ip.pop_num()?;
    let ch = ip.pop_int()?;
    let cy = ip.pop_num()?;
    let cx = ip.pop_num()?;
    show_string(ip, &s, true, &mut |_, c, _| Ok(if i64::from(c) == ch { (cx + ax, cy + ay) } else { (ax, ay) })).map(|_| ())
}

/// `xshow`, `yshow`, `xyshow`: explicit displacements replace the widths.
fn explicit_show(ip: &mut Interp, xs: bool, ys: bool) -> R {
    let disp: Vec<f64> = match ip.pop()? {
        Obj::Array(a, _) => a.items().iter().filter_map(Obj::num).collect(),
        Obj::Str(_, _) => return err("typecheck"),
        _ => return err("typecheck"),
    };
    let s = ip.pop_str()?.bytes();
    let step = if xs && ys { 2 } else { 1 };
    let saved_cur = ip.gs.cur;
    let mut cur = saved_cur.ok_or(PsErr::Ps("nocurrentpoint"))?;
    for (i, c) in s.iter().enumerate() {
        ip.gs.cur = Some(cur);
        show_string(ip, &[*c], true, &mut |_, _, _| Ok((0.0, 0.0)))?;
        let d = |k: usize| disp.get(i * step + k).copied().unwrap_or(0.0);
        let (dx, dy) = match (xs, ys) {
            (true, true) => (d(0), d(1)),
            (true, false) => (d(0), 0.0),
            _ => (0.0, d(0)),
        };
        let (ddx, ddy) = ip.gs.ctm.delta(dx, dy);
        cur = (cur.0 + ddx, cur.1 + ddy);
    }
    ip.gs.cur = Some(cur);
    Ok(())
}

pub fn xshow(ip: &mut Interp) -> R {
    explicit_show(ip, true, false)
}

pub fn yshow(ip: &mut Interp) -> R {
    explicit_show(ip, false, true)
}

pub fn xyshow(ip: &mut Interp) -> R {
    explicit_show(ip, true, true)
}

pub fn kshow(ip: &mut Interp) -> R {
    let s = ip.pop_str()?.bytes();
    let proc = ip.pop()?;
    for (i, c) in s.iter().enumerate() {
        show_string(ip, &[*c], true, &mut |_, _, _| Ok((0.0, 0.0)))?;
        if let Some(next) = s.get(i + 1) {
            ip.push(Obj::Int(i64::from(*c)))?;
            ip.push(Obj::Int(i64::from(*next)))?;
            ip.call(proc.clone())?;
        }
    }
    Ok(())
}

pub fn cshow(ip: &mut Interp) -> R {
    let s = ip.pop_str()?.bytes();
    let proc = ip.pop()?;
    for c in s {
        let (wx, wy) = show_string(ip, &[c], false, &mut |_, _, _| Ok((0.0, 0.0)))?;
        ip.push(Obj::Int(i64::from(c)))?;
        ip.push(Obj::Real(wx))?;
        ip.push(Obj::Real(wy))?;
        ip.call(proc.clone())?;
    }
    Ok(())
}

pub fn glyphshow(ip: &mut Interp) -> R {
    let _ = ip.pop()?;
    // Advance by an average glyph.
    let font = ip.gs.font.clone().ok_or(PsErr::Ps("invalidfont"))?;
    let (wx, wy) = font_matrix(&font).delta(500.0, 0.0);
    if let Some((cx, cy)) = ip.gs.cur {
        let (dx, dy) = ip.gs.ctm.delta(wx, wy);
        ip.gs.cur = Some((cx + dx, cy + dy));
    }
    Ok(())
}

pub fn stringwidth(ip: &mut Interp) -> R {
    let s = ip.pop_str()?.bytes();
    let (wx, wy) = show_string(ip, &s, false, &mut |_, _, _| Ok((0.0, 0.0)))?;
    ip.push(Obj::Real(wx))?;
    ip.push(Obj::Real(wy))
}

pub fn charpath(ip: &mut Interp) -> R {
    let _stroke = ip.pop_bool()?;
    let s = ip.pop_str()?.bytes();
    let saved = ip.charpath;
    ip.charpath = true;
    let cur = ip.gs.cur;
    let r = show_string(ip, &s, true, &mut |_, _, _| Ok((0.0, 0.0)));
    ip.charpath = saved;
    // The path ends at the advanced point.
    if let (Some(p), Ok(_)) = (ip.gs.cur, &r) {
        ip.gs.path.push(crate::gstate::Seg::Move(p.0, p.1));
        ip.gs.start = Some(p);
    }
    let _ = cur;
    r.map(|_| ())
}

pub fn setcharwidth(ip: &mut Interp) -> R {
    let wy = ip.pop_num()?;
    let wx = ip.pop_num()?;
    ip.gs.char_width = Some((wx, wy));
    Ok(())
}

pub fn setcachedevice(ip: &mut Interp) -> R {
    for _ in 0..4 {
        ip.pop_num()?;
    }
    setcharwidth(ip)
}

/// `w0x w0y llx lly urx ury w1x w1y vx vy setcachedevice2`: the mode-0 width counts.
pub fn setcachedevice2(ip: &mut Interp) -> R {
    for _ in 0..8 {
        ip.pop_num()?;
    }
    setcharwidth(ip)
}
