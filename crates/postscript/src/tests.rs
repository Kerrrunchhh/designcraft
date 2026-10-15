//! End-to-end: EPS programs in, PDF content out.

use super::*;

/// The page content stream of a converted EPS, inflated.
fn content(pdf: &[u8]) -> String {
    // The content stream is object 4, written right after the page.
    let at = pdf.windows(7).position(|w| w == b"4 0 obj").expect("content object");
    let rest = &pdf[at..];
    let s = rest.windows(6).position(|w| w == b"stream").unwrap() + 6;
    let s = s + if rest[s] == b'\r' { 2 } else { 1 };
    let e = rest.windows(9).position(|w| w == b"endstream").unwrap();
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&rest[s..e])
        .unwrap_or_else(|_| miniz_oxide::inflate::decompress_to_vec_zlib(&rest[s..e - 1]).unwrap());
    String::from_utf8_lossy(&raw).into_owned()
}

fn eps(body: &str) -> Vec<u8> {
    format!("%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 10 20 110 70\n%%EndComments\n{body}\n%%EOF\n").into_bytes()
}

fn run(body: &str) -> (String, Converted) {
    let c = eps_to_pdf(&eps(body), Limits::default()).unwrap();
    (content(&c.pdf), c)
}

#[test]
fn paths_fill_and_stroke_in_their_colours() {
    let (c, conv) = run("0 0 1 0 setcmykcolor 10 20 50 30 rectfill  1 0 0 setrgbcolor 2 setlinewidth newpath 10 20 moveto 100 60 lineto stroke");
    assert_eq!(conv.bbox, [10.0, 20.0, 110.0, 70.0]);
    // The bounding box's corner moves to the page origin.
    assert!(c.starts_with("1 0 0 1 -10 -20 cm"), "{c}");
    assert!(c.contains("0 0 1 0 k"), "CMYK stays CMYK: {c}");
    assert!(c.contains("10 20 m") && c.contains("f"), "{c}");
    assert!(c.contains("1 0 0 RG") && c.contains("2 w") && c.contains("S"), "{c}");
}

#[test]
fn procedures_loops_and_dictionaries_run() {
    let (c, _) = run("/sq { newpath moveto 10 0 rlineto 0 10 rlineto -10 0 rlineto closepath fill } bind def
         0 1 2 { 15 mul 10 add 30 sq } for
         /d 3 dict def d /n 7 put d /n get 7 eq { 0.5 setgray 60 20 5 5 rectfill } if
         { 1 0 div } stopped { 0.25 setgray 70 20 5 5 rectfill } if
         [ 1 2 3 ] { pop } forall  3 { } repeat  0 { 1 add dup 4 ge { exit } if } loop pop");
    assert_eq!(c.lines().filter(|l| *l == "f").count(), 5, "{c}");
    assert!(c.contains("0.5 g") && c.contains("0.25 g"), "{c}");
}

#[test]
fn transforms_curves_and_clips() {
    let (c, _) = run("gsave 60 45 translate 2 2 scale newpath 0 0 10 0 360 arc clip newpath -20 -20 40 40 rectfill grestore 10 20 5 5 rectfill");
    assert!(c.contains(" c"), "arcs become curves: {c}");
    assert!(c.contains("W") && c.contains("q") && c.contains("Q"), "clip in its own q: {c}");
    // The circle is centred at (60, 45) with radius 20 in device space.
    assert!(c.contains("80 45 m"), "{c}");
}

#[test]
fn separation_colours_use_their_tint_transform() {
    let (c, _) = run("[/Separation (Spot) /DeviceCMYK { dup 0 exch 0 } ] setcolorspace 0.5 setcolor 10 20 10 10 rectfill");
    assert!(c.contains("0.5 0 0.5 0 k") || c.contains(".5 0 .5 0 k"), "{c}");
}

#[test]
fn images_and_masks_become_xobjects() {
    let (c, conv) = run("gsave 10 20 translate 20 10 scale 2 1 8 [2 0 0 -1 0 1] <00ff> image grestore
                         gsave 40 20 translate 10 10 scale 0 0 1 setrgbcolor 8 1 true [8 0 0 -1 0 1] <f0> imagemask grestore
                         gsave 60 20 translate 10 10 scale 1 1 8 [1 0 0 -1 0 1] currentfile /ASCIIHexDecode filter false 3 colorimage
ff8000>
grestore");
    assert_eq!(c.matches("Do").count(), 3, "{c}");
    let pdf = String::from_utf8_lossy(&conv.pdf);
    assert!(pdf.contains("/ImageMask true") && pdf.contains("/DeviceRGB") && pdf.contains("/DeviceGray"), "{pdf}");
}

#[test]
fn type3_fonts_draw_their_glyphs() {
    let (c, _) = run("/T3 8 dict dup begin /FontType 3 def /FontMatrix [0.001 0 0 0.001 0 0] def /FontBBox [0 0 1000 1000] def
         /Encoding 256 array def 0 1 255 { Encoding exch /.notdef put } for Encoding 65 /A put
         /BuildChar { pop pop 600 0 0 0 500 500 setcachedevice 0 0 500 500 rectfill } def end def
         /T3 T3 definefont pop /T3 findfont 20 scalefont setfont 10 20 moveto (AA) show");
    // Two glyphs, the second 12 points (600 units at 20 pt) to the right.
    assert_eq!(c.lines().filter(|l| *l == "f").count(), 2, "{c}");
    assert!(c.contains("22 20 m"), "{c}");
}

#[test]
fn a_string_made_a_file_runs_as_a_program() {
    // Adobe's idiom: `(code) 0 () /SubFileDecode filter cvx exec`.
    let (c, _) = run("(0.5 setgray 10 20 5 5 rectfill) 0 () /SubFileDecode filter cvx exec");
    assert!(c.contains("0.5 g"), "{c}");
}

#[test]
fn eexec_sections_run() {
    // `(1 0 0 setrgbcolor 10 20 5 5 rectfill) ` encrypted with the eexec key, as hex.
    let plain = b"1 0 0 setrgbcolor 10 20 5 5 rectfill currentfile closefile\n";
    let mut r: u16 = 55665;
    let mut hex = String::new();
    for &p in [1u8, 2, 3, 4].iter().chain(plain.iter()) {
        let c = p ^ (r >> 8) as u8;
        r = (u16::from(c).wrapping_add(r)).wrapping_mul(52845).wrapping_add(22719);
        hex.push_str(&format!("{c:02x}"));
    }
    let (c, _) = run(&format!("currentfile eexec\n{hex}\n0 1 0 setrgbcolor 20 20 5 5 rectfill"));
    assert!(c.contains("1 0 0 rg") && c.contains("0 1 0 rg"), "the program carries on after the section: {c}");
}

#[test]
fn hostile_programs_end_with_an_error_not_a_hang_or_panic() {
    let limits = Limits { steps: 200_000, memory: 64 << 20 };
    for body in [
        "{ } loop",
        "/f { f } def f",
        "/f { 1 f } def f",
        "{ 1 } loop",
        "1000000000 array",
        "0 1 1e9 { pop } for",
        "[ 1 2",
        "/x 10 dict def x begin x begin",
        "100000 { gsave } repeat",
        "{ { { { { { stop } exec } exec } exec } exec } exec } loop",
        "(unterminated",
        "1 0 idiv",
        "mark { 0 0 moveto } 1000000 { dup exec } repeat",
        "currentfile 100000000 string readstring",
        "/A85 currentfile /ASCII85Decode filter def ~>",
        "0 0 moveto 1 1 1e300 1e300 0 0 curveto stroke",
        "[1e300 0 0 1e300 0 0] concat 0 0 1 1 rectfill",
    ] {
        let _ = eps_to_pdf(&eps(body), limits);
    }
    // Nothing painted and an error: no PDF, so the caller keeps the preview.
    assert!(eps_to_pdf(&eps("1 0 div"), limits).is_err());
    assert!(eps_to_pdf(b"%!PS-Adobe-3.0 EPSF-3.0\n0 0 1 1 rectfill\n", limits).is_err(), "no bounding box");
    assert!(eps_to_pdf(b"not postscript", limits).is_err());
}

#[test]
fn a_file_in_a_name_is_data_until_executed() {
    // cairo keeps its image filter in a name and calls `status` and `flushfile` on it.
    let (c, _) = run("/f (0.5 setgray 10 20 5 5 rectfill) 0 () /SubFileDecode filter def f status pop f cvx exec");
    assert!(c.contains("0.5 g"), "{c}");
}

#[test]
fn smooth_shadings_become_pdf_shadings() {
    let (c, conv) = run("<< /ShadingType 2 /ColorSpace /DeviceCMYK /Coords [10 20 110 20] /Extend [true true]
                           /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0 0] /C1 [0 0 1 0] /N 1 >> >> shfill
                         << /ShadingType 3 /ColorSpace [/Separation (Gold) /DeviceCMYK { dup 0.2 mul exch 0 exch 0 } ]
                           /Coords [60 45 0 60 45 20] /Function { } >> shfill");
    assert_eq!(c.matches(" sh").count(), 2, "{c}");
    let pdf = String::from_utf8_lossy(&conv.pdf);
    assert!(pdf.contains("/ShadingType 2") && pdf.contains("/ShadingType 3"), "{pdf}");
    // CMYK and a spot made of CMYK stay CMYK.
    assert_eq!(pdf.matches("/ColorSpace /DeviceCMYK").count(), 2, "{pdf}");
}

#[test]
fn shading_patterns_fill_their_path() {
    let (c, _) = run("<< /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0]
                         /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >> >> matrix makepattern
                       setpattern 10 20 50 30 rectfill");
    assert!(c.contains("W") && c.contains(" sh"), "clipped to the rectangle, then shaded: {c}");
}

#[test]
fn mesh_shadings_keep_their_vertices() {
    // Two free-form triangles from an array, the second sharing an edge (flag 1).
    let (_, conv) = run("<< /ShadingType 4 /ColorSpace /DeviceRGB
                           /DataSource [0 10 20 1 0 0  0 110 20 0 1 0  0 60 70 0 0 1  1 110 70 1 1 0] >> shfill
                         << /ShadingType 6 /ColorSpace /DeviceGray /DataSource [0 10 20 10 30 10 40 10 50 20 50 30 50 40 50 50 50 50 40 50 30 50 20 40 20 30 20 20 20  0 0.3 0.6 1] >> shfill");
    let pdf = String::from_utf8_lossy(&conv.pdf);
    assert!(pdf.contains("/ShadingType 4") && pdf.contains("/ShadingType 6"), "{pdf}");
    // Malformed meshes draw what they hold, or nothing.
    for bad in [
        "<< /ShadingType 4 /ColorSpace /DeviceRGB /DataSource [0 1] >> shfill",
        "<< /ShadingType 7 /ColorSpace /DeviceGray /DataSource (abc) /BitsPerCoordinate 32 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 1 0 1 0 1] >> shfill 0 0 1 1 rectfill",
    ] {
        let _ = eps_to_pdf(&eps(bad), Limits::default());
    }
}

#[test]
fn type42_fonts_draw_their_truetype_glyphs() {
    use skrifa::MetadataProvider;
    let ttf = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/JetBrainsMono-Regular.ttf")).unwrap();
    let gid = skrifa::FontRef::new(&ttf).unwrap().charmap().map('A').unwrap().to_u32();
    // sfnts strings split at an even length below the string limit.
    let sfnts: String = ttf.chunks(30_000).map(|c| format!("<{}>\n", c.iter().map(|b| format!("{b:02x}")).collect::<String>())).collect();
    let (c, _) = run(&format!(
        "/T42 11 dict dup begin /FontType 42 def /FontMatrix [1 0 0 1 0 0] def /PaintType 0 def /FontBBox [0 0 1 1] def
         /Encoding 256 array def 0 1 255 {{ Encoding exch /.notdef put }} for Encoding 65 /A put
         /CharStrings 2 dict dup /.notdef 0 put dup /A {gid} put def /sfnts [ {sfnts} ] def end definefont
         20 scalefont setfont 10 20 moveto (AA) show"
    ));
    // Two outlined glyphs, filled; the second 12 points on (JetBrains Mono advances 600/1000).
    assert_eq!(c.lines().filter(|l| *l == "f").count(), 2, "{}", &c[..c.len().min(400)]);
}
