//! The upload gate: what the main server checks, cheaply and without parsing anything complicated, before an uploaded file goes any further.
//!
//! Checked here: a size cap; the real type from the first bytes (PDF, PNG or JPEG; nothing else is a claim document: not a Word file, a web page, an
//! executable, an archive); the picture size of a PNG or JPEG read from its header, so a decompression bomb is refused before anything decodes it;
//! and a plain-byte scan of a PDF for the names of active content. The main server never parses a PDF and never draws a picture: the reader sidecar
//! does that in its own container and repeats the PDF check on the resolved document (a name can be written with `#xx` escapes to hide from a plain
//! scan), so this gate is the cheap first filter, not the only one. Every refusal carries a short code and a reason a person can read.

use sha2::{Digest, Sha256};

pub const MAX_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 40_000_000;
pub const MIN_SIDE: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Pdf,
    Png,
    Jpeg,
}

impl Kind {
    pub fn mime(self) -> &'static str {
        match self {
            Kind::Pdf => "application/pdf",
            Kind::Png => "image/png",
            Kind::Jpeg => "image/jpeg",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Inspected {
    pub kind: Kind,
    pub bytes: usize,
    pub sha256: String,
    pub dims: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    pub code: &'static str,
    pub reason: String,
}

fn reject<T>(code: &'static str, reason: impl Into<String>) -> Result<T, Rejection> {
    Err(Rejection { code, reason: reason.into() })
}

const PDF_NAMES: [&str; 9] = ["/JavaScript", "/JS", "/Launch", "/OpenAction", "/AA", "/EmbeddedFile", "/XFA", "/RichMedia", "/SubmitForm"];

fn png_dims(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 24 || &b[12..16] != b"IHDR" || b[8..12] != [0, 0, 0, 13] {
        return None;
    }
    Some((u32::from_be_bytes(b[16..20].try_into().ok()?), u32::from_be_bytes(b[20..24].try_into().ok()?)))
}

fn jpeg_dims(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if len < 2 {
            return None;
        }
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            if i + 9 > b.len() {
                return None;
            }
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

/// Signatures of other file formats an upload must not also carry (an executable, an archive or jar, a Java class, an Office/OLE container). The idea (a file
/// that starts as one format and contains another is a polyglot) and the strings come from the Attijari mail-attachment YARA rules (`polyglot_file`,
/// `suspicious_exe_in_document`); "MZ" is only meaningful at the start of a region, as that project's CI found ("DMZ" is ordinary text).
const FOREIGN_START: [&[u8]; 5] = [b"MZ", b"\x7fELF", b"PK\x03\x04", b"\xca\xfe\xba\xbe", b"\xd0\xcf\x11\xe0"];
const FOREIGN_ANYWHERE: [&[u8]; 6] = [b"This program cannot be run in DOS mode", b"\x7fELF", b"PK\x03\x04", b"\xca\xfe\xba\xbe", b"\xd0\xcf\x11\xe0", b"META-INF/MANIFEST"];

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// What follows the end of a picture (PNG `IEND` chunk, JPEG last `FFD9`): a camera may add a little; an executable or another document may not.
fn trailer_of(kind: Kind, bytes: &[u8]) -> &[u8] {
    let end = match kind {
        Kind::Png => bytes.windows(4).rposition(|w| w == b"IEND").map(|p| p + 8),
        Kind::Jpeg => bytes.windows(2).rposition(|w| w == [0xFF, 0xD9]).map(|p| p + 2),
        Kind::Pdf => None,
    };
    end.filter(|e| *e <= bytes.len()).map(|e| &bytes[e..]).unwrap_or(&[])
}

fn has_end(kind: Kind, bytes: &[u8]) -> bool {
    match kind {
        Kind::Png => bytes.windows(4).any(|w| w == b"IEND"),
        Kind::Jpeg => bytes.windows(2).any(|w| w == [0xFF, 0xD9]),
        Kind::Pdf => true,
    }
}

fn polyglot(kind: Kind, bytes: &[u8], pdf_at: usize) -> Option<&'static str> {
    if find(bytes, b"This program cannot be run in DOS mode").is_some() {
        return Some("it contains an executable program");
    }
    match kind {
        Kind::Pdf => {
            if pdf_at > 0 && FOREIGN_START.iter().any(|s| bytes.starts_with(s)) {
                return Some("it starts as a different kind of file and only then looks like a PDF");
            }
            if find(bytes, b"META-INF/MANIFEST").is_some() {
                return Some("it contains a Java archive");
            }
        }
        Kind::Png | Kind::Jpeg => {
            let t = trailer_of(kind, bytes);
            if !t.is_empty() && (FOREIGN_START.iter().any(|s| t.starts_with(s)) || FOREIGN_ANYWHERE.iter().any(|s| find(t, s).is_some()) || find(t, b"%PDF-").is_some()) {
                return Some("another file is attached after the end of the picture");
            }
        }
    }
    None
}

pub fn inspect(bytes: &[u8]) -> Result<Inspected, Rejection> {
    if bytes.is_empty() {
        return reject("empty", "the file is empty");
    }
    if bytes.len() > MAX_BYTES {
        return reject("too_large", format!("the file is {} MB; the limit is {} MB", bytes.len() / (1024 * 1024), MAX_BYTES / (1024 * 1024)));
    }
    let sha256 = hex::encode(Sha256::digest(bytes));
    let head = &bytes[..bytes.len().min(1024)];
    let kind = if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Kind::Png
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Kind::Jpeg
    } else if head.windows(5).any(|w| w == b"%PDF-") {
        Kind::Pdf
    } else {
        return reject("unsupported_type", "only PDF, PNG and JPEG files are accepted (the type is read from the file itself, not from its name)");
    };
    let pdf_at = head.windows(5).position(|w| w == b"%PDF-").unwrap_or(0);
    if let Some(why) = polyglot(kind, bytes, pdf_at) {
        return reject("polyglot_file", format!("the file is not a plain {}: {why}", match kind { Kind::Pdf => "PDF", Kind::Png => "PNG picture", Kind::Jpeg => "JPEG picture" }));
    }
    let mut dims = None;
    match kind {
        Kind::Png | Kind::Jpeg => {
            let d = if kind == Kind::Png { png_dims(bytes) } else { jpeg_dims(bytes) };
            let Some((w, h)) = d else { return reject("bad_image", "the picture header cannot be read") };
            if w < MIN_SIDE || h < MIN_SIDE {
                return reject("bad_image", format!("the picture is {w} x {h} pixels: too small to read"));
            }
            if (w as u64) * (h as u64) > MAX_PIXELS {
                return reject("image_too_large", format!("the picture is {w} x {h} pixels; the limit is {} million", MAX_PIXELS / 1_000_000));
            }
            if !has_end(kind, bytes) {
                return reject("bad_image", "the picture has no end marker (cut off or damaged)");
            }
            dims = Some((w, h));
        }
        Kind::Pdf => {
            let found: Vec<&str> = PDF_NAMES.iter().copied().filter(|n| contains_name(bytes, n.as_bytes())).collect();
            if !found.is_empty() {
                return reject("pdf_active_content", format!("the PDF contains active or hidden content ({}); a claim document never needs it", found.join(", ")));
            }
            if contains_name(bytes, b"/Encrypt") {
                return reject("pdf_encrypted", "the PDF is encrypted and cannot be read");
            }
        }
    }
    Ok(Inspected { kind, bytes: bytes.len(), sha256, dims })
}

/// `needle` as a complete PDF name: the next byte must end the name (a delimiter or white space), so `/JS` does not match `/JSON` or `/Jsx`.
fn contains_name(hay: &[u8], needle: &[u8]) -> bool {
    let n = needle.len();
    hay.windows(n + 1).any(|w| &w[..n] == needle && matches!(w[n], b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'<' | b'>' | b'[' | b']' | b'(' | b')' | 0x0c | 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        v.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
        v
    }

    fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0, 0, 0xFF, 0xC0, 0x00, 0x0B, 8];
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&[1, 1, 0x11, 0, 0xFF, 0xD9]);
        v
    }

    #[test]
    fn the_kinds_are_read_from_the_bytes() {
        assert_eq!(inspect(&png(800, 600)).unwrap().kind, Kind::Png);
        assert_eq!(inspect(&png(800, 600)).unwrap().dims, Some((800, 600)));
        assert_eq!(inspect(&jpeg(1200, 900)).unwrap().dims, Some((1200, 900)));
        assert_eq!(inspect(b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n").unwrap().kind, Kind::Pdf);
        assert_eq!(inspect(b"\n\n%PDF-1.7 junk before is tolerated").unwrap().kind, Kind::Pdf);
    }

    #[test]
    fn everything_else_is_refused_with_a_code() {
        for (data, code) in [
            (b"".to_vec(), "empty"),
            (b"MZ\x90\x00 an executable".to_vec(), "unsupported_type"),
            (b"PK\x03\x04 a zip or a docx".to_vec(), "unsupported_type"),
            (b"<html><script>alert(1)</script>".to_vec(), "unsupported_type"),
            (b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(), "unsupported_type"),
            (png(50, 50), "bad_image"),
            (png(9000, 9000), "image_too_large"),
            (png(800, 600)[..20].to_vec(), "bad_image"),
            (png(800, 600)[..33].to_vec(), "bad_image"), // no end chunk: cut off, so nothing proves what follows
            (jpeg(1200, 900)[..jpeg(1200, 900).len() - 2].to_vec(), "bad_image"),
            (jpeg(10, 10), "bad_image"),
            (vec![0xFF, 0xD8, 0xFF], "bad_image"),
        ] {
            assert_eq!(inspect(&data).unwrap_err().code, code, "{:?}", &data[..data.len().min(12)]);
        }
        assert_eq!(inspect(&vec![0u8; MAX_BYTES + 1]).unwrap_err().code, "too_large");
    }

    #[test]
    fn a_pdf_with_active_content_is_refused_but_lookalike_names_are_not() {
        for name in ["/JavaScript", "/JS", "/Launch", "/OpenAction", "/AA", "/EmbeddedFile", "/XFA", "/RichMedia", "/SubmitForm"] {
            let pdf = format!("%PDF-1.4\n1 0 obj\n<< {name} 5 0 R >>\nendobj\n");
            assert_eq!(inspect(pdf.as_bytes()).unwrap_err().code, "pdf_active_content", "{name}");
        }
        assert_eq!(inspect(b"%PDF-1.4\n<< /Type /Catalog /Metadata /JSON /Jsx /AAA /OpenActions >>").unwrap().kind, Kind::Pdf);
        assert_eq!(inspect(b"%PDF-1.4\n<< /Encrypt 3 0 R >>").unwrap_err().code, "pdf_encrypted");
    }

    #[test]
    fn a_file_that_is_two_formats_at_once_is_refused() {
        let mut exe_then_pdf = b"MZ\x90\x00 header".to_vec();
        exe_then_pdf.extend_from_slice(b"\n%PDF-1.4\n<< /Type /Catalog >>");
        let mut png_plus_exe = png(800, 600);
        png_plus_exe.extend_from_slice(b"MZ\x90\x00 payload");
        let mut png_plus_zip = png(800, 600);
        png_plus_zip.extend_from_slice(b"junk PK\x03\x04 zip");
        let mut jpeg_plus_pdf = jpeg(1200, 900);
        jpeg_plus_pdf.extend_from_slice(b"%PDF-1.4 hidden");
        let mut pdf_with_exe = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".to_vec();
        pdf_with_exe.extend_from_slice(b"stream\nThis program cannot be run in DOS mode.\nendstream");
        let pdf_with_jar = b"%PDF-1.4\n<< >>\nstream\nMETA-INF/MANIFEST.MF\nendstream";
        for (name, data) in [("exe then pdf", exe_then_pdf), ("png plus exe", png_plus_exe), ("png plus zip", png_plus_zip), ("jpeg plus pdf", jpeg_plus_pdf), ("pdf with exe", pdf_with_exe), ("pdf with jar", pdf_with_jar.to_vec())] {
            assert_eq!(inspect(&data).unwrap_err().code, "polyglot_file", "{name}");
        }
        // ordinary files stay fine: "DMZ" in text, a few harmless bytes after a picture
        assert_eq!(inspect(b"%PDF-1.4\nthe DMZ network and MZ in text").unwrap().kind, Kind::Pdf);
        let mut padded = png(800, 600);
        padded.extend_from_slice(&[0u8; 12]);
        assert_eq!(inspect(&padded).unwrap().kind, Kind::Png);
    }

    #[test]
    fn the_hash_is_of_the_exact_bytes() {
        let a = inspect(&png(100, 100)).unwrap();
        let mut other = png(100, 100);
        other.push(0);
        assert_ne!(a.sha256, inspect(&other).unwrap().sha256);
        assert_eq!(a.sha256.len(), 64);
    }
}
