//! Dependency-free PDF writer for text documents (Markdown-ish input).
//! Uses the standard Helvetica fonts with WinAnsi encoding (Latin-1 accents).

const PAGE_W: f32 = 595.0; // A4
const PAGE_H: f32 = 842.0;
const MARGIN: f32 = 56.0;

#[derive(Clone, Copy)]
enum Style {
    Body,
    H1,
    H2,
    H3,
    Bullet,
    Code,
}

impl Style {
    fn size(self) -> f32 {
        match self {
            Style::H1 => 20.0,
            Style::H2 => 15.0,
            Style::H3 => 12.5,
            Style::Code => 9.5,
            _ => 11.0,
        }
    }
    fn font(self) -> &'static str {
        match self {
            Style::H1 | Style::H2 | Style::H3 => "F2",
            Style::Code => "F3",
            _ => "F1",
        }
    }
}

/// Approximate Helvetica advance width (in 1/1000 em).
fn char_width(c: char, style: Style) -> f32 {
    if let Style::Code = style {
        return 600.0;
    }
    let w = match c {
        ' ' => 278.0,
        'i' | 'j' | 'l' | '.' | ',' | ':' | ';' | '\'' | '!' | '|' | 'í' | 'ì' | 'ï' => 240.0,
        'f' | 't' | 'r' | 'I' | '(' | ')' | '[' | ']' | '-' => 333.0,
        'm' | 'w' | 'M' | 'W' | '@' => 860.0,
        c if c.is_ascii_digit() => 556.0,
        c if c.is_uppercase() => 690.0,
        _ => 556.0,
    };
    if matches!(style, Style::H1 | Style::H2 | Style::H3) {
        w * 1.06
    } else {
        w
    }
}

fn text_width(s: &str, style: Style) -> f32 {
    s.chars().map(|c| char_width(c, style)).sum::<f32>() * style.size() / 1000.0
}

/// Map to Windows-1252 bytes; unmappable characters become '?'.
fn to_winansi(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| match c {
            '\u{20}'..='\u{7e}' => c as u8,
            '\u{a0}'..='\u{ff}' => c as u32 as u8,
            '€' => 0x80,
            '‚' => 0x82,
            '„' => 0x84,
            '…' => 0x85,
            '•' => 0x95,
            '–' => 0x96,
            '—' => 0x97,
            '‘' => 0x91,
            '’' => 0x92,
            '“' => 0x93,
            '”' => 0x94,
            '™' => 0x99,
            _ => b'?',
        })
        .collect()
}

fn pdf_string(s: &str) -> Vec<u8> {
    let mut out = vec![b'('];
    for b in to_winansi(s) {
        match b {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(b);
            }
            0x80..=0xff => out.extend_from_slice(format!("\\{:03o}", b).as_bytes()),
            _ => out.push(b),
        }
    }
    out.push(b')');
    out
}

fn wrap(text: &str, style: Style, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let candidate = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if text_width(&candidate, style) <= width || cur.is_empty() {
            // Hard-break words longer than a line.
            if cur.is_empty() && text_width(word, style) > width {
                let mut piece = String::new();
                for ch in word.chars() {
                    piece.push(ch);
                    if text_width(&piece, style) > width {
                        piece.pop();
                        lines.push(std::mem::take(&mut piece));
                        piece.push(ch);
                    }
                }
                cur = piece;
            } else {
                cur = candidate;
            }
        } else {
            lines.push(std::mem::replace(&mut cur, word.to_string()));
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn strip_inline(s: &str) -> String {
    s.replace("**", "").replace("__", "").replace('`', "")
}

/// Render Markdown-ish text to a PDF document.
pub fn render(title: Option<&str>, markdown: &str) -> Vec<u8> {
    let usable = PAGE_W - 2.0 * MARGIN;
    let mut pages: Vec<Vec<u8>> = vec![Vec::new()];
    let mut y = PAGE_H - MARGIN;
    let emit = |pages: &mut Vec<Vec<u8>>, y: &mut f32, line: &str, style: Style, indent: f32, gap_before: f32| {
        let lh = style.size() * 1.35;
        if *y - gap_before - lh < MARGIN {
            pages.push(Vec::new());
            *y = PAGE_H - MARGIN;
        } else {
            *y -= gap_before;
        }
        *y -= lh;
        let page = pages.last_mut().unwrap();
        page.extend_from_slice(format!("BT /{} {} Tf {:.2} {:.2} Td ", style.font(), style.size(), MARGIN + indent, *y).as_bytes());
        page.extend_from_slice(&pdf_string(line));
        page.extend_from_slice(b" Tj ET\n");
    };
    if let Some(t) = title {
        for l in wrap(t, Style::H1, usable) {
            emit(&mut pages, &mut y, &l, Style::H1, 0.0, 0.0);
        }
        y -= 8.0;
    }
    let mut in_code = false;
    for raw in markdown.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            emit(&mut pages, &mut y, if line.is_empty() { " " } else { line }, Style::Code, 10.0, 0.0);
            continue;
        }
        if line.trim().is_empty() {
            y -= 6.0;
            continue;
        }
        let (style, text, indent, gap) = if let Some(t) = line.strip_prefix("### ") {
            (Style::H3, t.to_string(), 0.0, 8.0)
        } else if let Some(t) = line.strip_prefix("## ") {
            (Style::H2, t.to_string(), 0.0, 10.0)
        } else if let Some(t) = line.strip_prefix("# ") {
            (Style::H1, t.to_string(), 0.0, 12.0)
        } else if let Some(t) = line.trim_start().strip_prefix("- ").or_else(|| line.trim_start().strip_prefix("* ")) {
            (Style::Bullet, format!("• {t}"), 12.0, 1.0)
        } else {
            (Style::Body, line.to_string(), 0.0, 2.0)
        };
        let text = strip_inline(&text);
        for (i, l) in wrap(&text, style, usable - indent - if matches!(style, Style::Bullet) { 10.0 } else { 0.0 }).into_iter().enumerate() {
            let extra = if matches!(style, Style::Bullet) && i > 0 { 10.0 } else { 0.0 };
            emit(&mut pages, &mut y, &l, style, indent + extra, if i == 0 { gap } else { 0.0 });
        }
    }

    // Assemble objects: 1 catalog, 2 pages, 3-5 fonts, then (page, content) pairs.
    let n = pages.len();
    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 6 + 2 * i)).collect();
    objects.push(format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes());
    for base in ["Helvetica", "Helvetica-Bold", "Courier"] {
        objects.push(format!("<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding >>").into_bytes());
    }
    for (i, content) in pages.iter().enumerate() {
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W} {PAGE_H}] /Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R >> >> /Contents {} 0 R >>",
                7 + 2 * i
            )
            .into_bytes(),
        );
        let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        stream.extend_from_slice(content);
        stream.extend_from_slice(b"\nendstream");
        objects.push(stream);
    }
    let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(obj);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structure() {
        let pdf = render(Some("Título"), &"# Seção\nTexto com acentuação: ção é à.\n- item\n".repeat(80));
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.starts_with("%PDF-1.4"));
        let count: usize = s.split("/Count ").nth(1).unwrap().split_whitespace().next().unwrap().parse().unwrap();
        assert!(count >= 2, "deveria ter várias páginas: {count}");
        assert!(s.trim_end().ends_with("%%EOF"));
    }

    #[test]
    fn wraps_long_words() {
        let l = wrap(&"x".repeat(500), Style::Body, 200.0);
        assert!(l.len() > 1);
    }
}
