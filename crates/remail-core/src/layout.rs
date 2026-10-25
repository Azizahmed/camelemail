//! Branded layouts: a header (logo and organisation) and a footer (the
//! sender's signature block) wrapped around outgoing mail.
//!
//! The AI only ever picks design choices (`LayoutStyle`); this module is the
//! one place that writes the markup, so every layout follows the same email
//! rules whatever the model said: nested tables with inline CSS (no `<style>`,
//! classes, scripts or web fonts), 600px at most and fluid below that,
//! web-safe font stacks, a logo with explicit size and alt text, readable
//! colours (WCAG AA), `tel:` and `mailto:` links, and right-to-left blocks for
//! Arabic and Hebrew text.

use anyhow::{bail, Result};
use base64::Engine as _;

use crate::compose::{escape_html, parse_address};
use crate::model::{
    BrandedLayout, ButtonStyle, FooterAlign, FooterLogo, LayoutAlign, LayoutButton, LayoutDivider, LayoutLogo, LayoutPreview, LayoutStyle,
    LogoPosition,
};

/// Layouts per user. Together with the logo limit this keeps the synced
/// `layouts` section (256 KiB) from overflowing.
pub const MAX_LAYOUTS: usize = 6;
/// A logo picture, decoded.
pub const MAX_LOGO_BYTES: usize = 24 * 1024;
/// All layouts together, as stored: six full-size logos fit, and the synced
/// section (256 KiB with its time stamps) still has room.
pub const MAX_TOTAL_BYTES: usize = 220 * 1024;
/// The logo is shown at most this big (CSS pixels); the picture may hold twice the pixels.
const MAX_LOGO_W: u32 = 200;
const MAX_LOGO_H: u32 = 80;
/// In the footer the same picture is shown smaller.
const FOOTER_LOGO_W: u32 = 120;
const FOOTER_LOGO_H: u32 = 48;
/// The company description in the header, and the button's text.
pub const MAX_ABOUT_CHARS: usize = 240;
pub const MAX_BUTTON_CHARS: usize = 30;
/// Widest the layout gets; narrower screens shrink it.
const WIDTH: u32 = 600;
/// Minimum contrast of text on its background (WCAG AA, normal text).
const MIN_CONTRAST: f64 = 4.5;

const FONT: &str = "Arial,'Helvetica Neue',Helvetica,Tahoma,'Segoe UI','Geeza Pro',sans-serif";
const WHITE: Rgb = (255, 255, 255);
const RULE_GREY: &str = "#d5dbe1";

type Rgb = (u8, u8, u8);

// ----------------------------------------------------------------------------
// Colours
// ----------------------------------------------------------------------------

fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.trim().strip_prefix('#')?;
    if !h.is_ascii() {
        return None;
    }
    let v = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok();
    match h.len() {
        6 => Some((v(0, 2)?, v(2, 2)?, v(4, 2)?)),
        3 => {
            let d = |i: usize| v(i, 1).map(|x| x * 17);
            Some((d(0)?, d(1)?, d(2)?))
        }
        _ => None,
    }
}

fn hex((r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn luminance((r, g, b): Rgb) -> f64 {
    let ch = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
}

/// WCAG contrast ratio, 1 to 21.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let m = |x: u8, y: u8| (f64::from(x) + (f64::from(y) - f64::from(x)) * t).round().clamp(0.0, 255.0) as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// `fg` as is when it reads on `bg`; otherwise the nearest shade of it that does
/// (darker on light backgrounds, lighter on dark ones). The flag says it changed.
fn readable(fg: Rgb, bg: Rgb) -> (Rgb, bool) {
    if contrast(fg, bg) >= MIN_CONTRAST {
        return (fg, false);
    }
    let target = if contrast((0, 0, 0), bg) >= contrast(WHITE, bg) { (0, 0, 0) } else { WHITE };
    for step in 1..=20 {
        let c = mix(fg, target, f64::from(step) / 20.0);
        if contrast(c, bg) >= MIN_CONTRAST {
            return (c, true);
        }
    }
    (target, true)
}

/// Text colour for the header, whose background the user chose: the style's
/// own colour when it reads, else the better of white and near-black.
fn on_header(fg: Rgb, bg: Rgb) -> Rgb {
    if contrast(fg, bg) >= MIN_CONTRAST {
        return fg;
    }
    let dark = (17, 24, 39);
    if contrast(dark, bg) >= contrast(WHITE, bg) {
        dark
    } else {
        WHITE
    }
}

/// The style with every colour valid and readable: text and links on the
/// white footer reach 4.5:1 (darkened as little as it takes). Returns the
/// names of the fields that changed.
pub fn sanitize_style(style: &LayoutStyle) -> (LayoutStyle, Vec<String>) {
    let d = LayoutStyle::default();
    let mut adjusted = Vec::new();
    let mut pick = |name: &str, value: &str, fallback: &str, on_white: bool| -> String {
        let (rgb, mut changed) = match parse_hex(value) {
            Some(c) => (c, false),
            None => (parse_hex(fallback).unwrap_or(WHITE), true),
        };
        let rgb = if on_white {
            let (c, ch) = readable(rgb, WHITE);
            changed |= ch;
            c
        } else {
            rgb
        };
        if changed {
            adjusted.push(name.to_string());
        }
        hex(rgb)
    };
    let accent = pick("accent", &style.accent, &d.accent, true);
    let header_bg = pick("headerBg", &style.header_bg, &d.header_bg, false);
    let text_color = pick("textColor", &style.text_color, &d.text_color, true);
    let muted_color = pick("mutedColor", &style.muted_color, &d.muted_color, true);
    let tagline = style
        .tagline
        .as_deref()
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .map(|t| t.chars().take(80).collect::<String>())
        .filter(|t| !t.is_empty());
    (
        LayoutStyle {
            accent,
            header_bg,
            text_color,
            muted_color,
            alignment: style.alignment,
            divider: style.divider,
            logo_position: style.logo_position,
            footer_align: style.footer_align,
            footer_logo: style.footer_logo,
            button_style: style.button_style,
            tagline,
            template: style.template.clone().filter(|t| PRESETS.contains(&t.as_str())),
        },
        adjusted,
    )
}

// ----------------------------------------------------------------------------
// Rendering
// ----------------------------------------------------------------------------

/// Does text start with a right-to-left letter? (The same first-letter rule as `src/lib/bidi.ts`.)
pub fn is_rtl(text: &str) -> bool {
    let Some(c) = text.chars().find(|c| c.is_alphabetic()) else { return false };
    matches!(c as u32, 0x590..=0x8FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFC | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF)
}

/// `+` and digits of a phone number for a `tel:` link (Arabic-Indic digits become ASCII).
fn tel_number(phone: &str) -> String {
    let mut out = String::new();
    for c in phone.trim().chars() {
        match c {
            '0'..='9' => out.push(c),
            '\u{660}'..='\u{669}' => out.push(char::from(b'0' + (c as u32 - 0x660) as u8)),
            '\u{6F0}'..='\u{6F9}' => out.push(char::from(b'0' + (c as u32 - 0x6F0) as u8)),
            '+' if out.is_empty() => out.push('+'),
            _ => {}
        }
    }
    if out.chars().any(|c| c.is_ascii_digit()) {
        out
    } else {
        String::new()
    }
}

fn mail_href(email: &str) -> Option<String> {
    let e = email.trim();
    let ok = e.contains('@') && !e.chars().any(|c| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '&' | '?' | '#'));
    ok.then(|| format!("mailto:{e}"))
}

fn https_url(url: &str) -> Option<&str> {
    let u = url.trim();
    let ok = u.len() <= 500
        && u.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://"))
        && !u.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>'));
    ok.then_some(u)
}

/// Size the logo is shown at: its own size, scaled down to fit the box.
fn logo_size(logo: &LayoutLogo, max_w: u32, max_h: u32) -> (u32, u32) {
    let (w, h) = (logo.width.max(1), logo.height.max(1));
    let scale = (f64::from(max_w) / f64::from(w)).min(f64::from(max_h) / f64::from(h)).min(1.0);
    (((f64::from(w) * scale).round() as u32).max(1), ((f64::from(h) * scale).round() as u32).max(1))
}

fn logo_src(logo: &LayoutLogo) -> Option<String> {
    if let Some(u) = logo.url.as_deref().and_then(https_url) {
        return Some(escape_html(u));
    }
    let mime_ok = matches!(logo.mime.as_str(), "image/png" | "image/jpeg" | "image/gif");
    let data_ok = !logo.data.is_empty() && logo.data.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
    (mime_ok && data_ok).then(|| format!("data:{};base64,{}", logo.mime, logo.data))
}

fn logo_img(l: &BrandedLayout, max_w: u32, max_h: u32) -> Option<String> {
    let logo = l.logo.as_ref()?;
    let src = logo_src(logo)?;
    let (w, h) = logo_size(logo, max_w, max_h);
    let alt = if logo.alt.trim().is_empty() {
        let org = l.org_name.trim();
        match (org.is_empty(), is_rtl(org)) {
            (true, _) => String::new(),
            (false, true) => format!("شعار {org}"),
            (false, false) => format!("{org} logo"),
        }
    } else {
        logo.alt.trim().to_string()
    };
    // The alt text carries the same type as the name next to it, so a blocked picture still reads well.
    Some(format!(
        "<img data-camel-logo=\"1\" src=\"{src}\" width=\"{w}\" height=\"{h}\" alt=\"{}\" style=\"display:block;border:0;outline:none;text-decoration:none;width:{w}px;height:{h}px;max-width:100%;font-family:{FONT};font-size:14px;color:#374151\">",
        escape_html(&alt)
    ))
}

fn logo_plate(img: &str) -> String {
    format!("<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr><td bgcolor=\"#ffffff\" style=\"background-color:#ffffff;padding:6px 10px;border-radius:8px\">{img}</td></tr></table>")
}

fn text_style(size: u32, color: Rgb, bold: bool) -> String {
    format!(
        "font-family:{FONT};font-size:{size}px;line-height:{}px;color:{};font-weight:{}",
        size + size / 3 + 2,
        hex(color),
        if bold { "bold" } else { "normal" }
    )
}

fn align_attr(center: bool, rtl: bool) -> &'static str {
    match (center, rtl) {
        (true, _) => "center",
        (false, true) => "right",
        (false, false) => "left",
    }
}

fn dir_attr(rtl: bool) -> &'static str {
    if rtl {
        "rtl"
    } else {
        "ltr"
    }
}

/// A block as wide as the layout: a full-width table around a centred one of at most 600px.
fn block(rows: &str, bg: Rgb) -> String {
    let bg = hex(bg);
    format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"width:100%;border-collapse:collapse\"><tr><td align=\"center\" style=\"padding:0\"><table role=\"presentation\" width=\"{WIDTH}\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" align=\"center\" bgcolor=\"{bg}\" style=\"width:100%;max-width:{WIDTH}px;border-collapse:collapse;background-color:{bg}\">{rows}</table></td></tr></table>"
    )
}

fn rule_row(px: u32, color: &str) -> String {
    format!("<tr><td height=\"{px}\" bgcolor=\"{color}\" style=\"height:{px}px;line-height:{px}px;font-size:{px}px;background-color:{color}\">&nbsp;</td></tr>")
}

/// Where a button may lead: a public `https://` page or a `mailto:` address.
fn button_href(url: &str) -> Option<String> {
    let u = url.trim();
    if let Some(h) = https_url(u) {
        return Some(h.to_string());
    }
    mail_href(u.strip_prefix("mailto:")?)
}

/// The header's button, built the way mail apps need it: a coloured table cell
/// around a padded link (`mso-padding-alt` keeps the size in Outlook). On a dark
/// header a filled button turns white so it doesn't melt into the background.
fn button_html(b: &LayoutButton, st: &LayoutStyle, bg: Rgb, align: &str) -> Option<String> {
    let href = button_href(&b.url)?;
    let label = b.label.trim();
    if label.is_empty() {
        return None;
    }
    let accent = parse_hex(&st.accent).unwrap_or((11, 92, 173));
    let dark = luminance(bg) < 0.3;
    // Fill, border and text colour.
    let (fill, border, ink) = match (st.button_style, dark) {
        (ButtonStyle::Filled, false) => (accent, accent, WHITE),
        (ButtonStyle::Filled, true) => (WHITE, WHITE, if contrast(bg, WHITE) >= MIN_CONTRAST { bg } else { (17, 24, 39) }),
        // The accent itself, darkened only as far as it takes to read on this header.
        (ButtonStyle::Outline, false) => {
            let c = readable(accent, bg).0;
            (bg, c, c)
        }
        (ButtonStyle::Outline, true) => (bg, WHITE, WHITE),
    };
    let (f, bd) = (hex(fill), hex(border));
    Some(format!(
        "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" align=\"{align}\"><tr><td align=\"center\" bgcolor=\"{f}\" style=\"background-color:{f};border:2px solid {bd};border-radius:6px;mso-padding-alt:10px 22px\"><a href=\"{}\" style=\"display:inline-block;padding:10px 22px;{};text-decoration:none\"><span style=\"color:{}\">{}</span></a></td></tr></table>",
        escape_html(&href),
        text_style(14, ink, true),
        hex(ink),
        escape_html(label)
    ))
}

/// The header: logo, organisation name, tagline, a short description of the
/// company and a button, on the chosen background. Empty when there's neither
/// a name nor a logo.
pub fn render_header(l: &BrandedLayout) -> String {
    let (st, _) = sanitize_style(&l.style);
    let bg = parse_hex(&st.header_bg).unwrap_or(WHITE);
    let org = l.org_name.trim();
    let img = logo_img(l, MAX_LOGO_W, MAX_LOGO_H);
    if org.is_empty() && img.is_none() {
        return String::new();
    }
    // On a dark header the logo sits on a small white plate: pictures are never recoloured, so dark lettering would vanish.
    let img = img.map(|i| if luminance(bg) < 0.3 { logo_plate(&i) } else { i });
    let about = l.about.trim();
    let rtl = is_rtl(org) || (org.is_empty() && is_rtl(about));
    let center = st.alignment == LayoutAlign::Center || st.logo_position == LogoPosition::Center;
    let text = on_header(parse_hex(&st.text_color).unwrap_or((17, 24, 39)), bg);
    let muted = on_header(parse_hex(&st.muted_color).unwrap_or((55, 65, 81)), bg);

    let mut name = String::new();
    if !org.is_empty() {
        name.push_str(&format!("<p style=\"margin:0;{}\">{}</p>", text_style(20, text, true), escape_html(org)));
    }
    if let Some(t) = &st.tagline {
        name.push_str(&format!("<p style=\"margin:4px 0 0;{}\">{}</p>", text_style(14, muted, false), escape_html(t)));
    }
    let align = align_attr(center, rtl);
    let content = match (img, name.is_empty()) {
        (None, _) => name,
        (Some(img), true) => format!("<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" align=\"{align}\"><tr><td>{img}</td></tr></table>"),
        (Some(img), false) if st.logo_position == LogoPosition::Above => format!(
            "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" align=\"{align}\" dir=\"{d}\"><tr><td align=\"{align}\" style=\"padding:0 0 12px 0\">{img}</td></tr><tr><td align=\"{align}\">{name}</td></tr></table>",
            d = dir_attr(rtl)
        ),
        (Some(img), false) => {
            let gap = if rtl { "padding:0 0 0 16px" } else { "padding:0 16px 0 0" };
            format!(
                "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" align=\"{align}\" dir=\"{d}\"><tr><td valign=\"middle\" style=\"{gap}\">{img}</td><td valign=\"middle\" align=\"{ta}\">{name}</td></tr></table>",
                d = dir_attr(rtl),
                ta = align_attr(false, rtl)
            )
        }
    };
    let button = l.button.as_ref().and_then(|b| button_html(b, &st, bg, align));
    // Rows of the header: the name, then the description and the button when there are any.
    let pad_bottom = |last: bool| if last { 20 } else { 0 };
    let mut rows = format!(
        "<tr><td dir=\"{}\" align=\"{align}\" style=\"padding:20px 24px {}px;{}\">{content}</td></tr>",
        dir_attr(rtl),
        pad_bottom(about.is_empty() && button.is_none()),
        text_style(14, text, false)
    );
    if !about.is_empty() {
        let about_rtl = is_rtl(about);
        rows.push_str(&format!(
            "<tr><td dir=\"{}\" align=\"{}\" style=\"padding:12px 24px {}px\"><p style=\"margin:0;{}\">{}</p></td></tr>",
            dir_attr(about_rtl),
            align_attr(center, about_rtl),
            pad_bottom(button.is_none()),
            text_style(14, text, false),
            escape_html(about)
        ));
    }
    if let Some(b) = button {
        rows.push_str(&format!("<tr><td dir=\"{}\" align=\"{align}\" style=\"padding:16px 24px 20px\">{b}</td></tr>", dir_attr(rtl)));
    }
    match st.divider {
        LayoutDivider::None => {}
        LayoutDivider::Line => rows.push_str(&rule_row(1, RULE_GREY)),
        LayoutDivider::Bar => rows.push_str(&rule_row(4, &st.accent)),
    }
    block(&rows, bg)
}

/// A phone number or address is always a left-to-right run, even inside Arabic text.
fn link(href: &str, label: &str, accent: Rgb) -> String {
    let c = hex(accent);
    format!(
        "<a href=\"{}\" style=\"color:{c};text-decoration:underline\"><span dir=\"ltr\" style=\"color:{c};unicode-bidi:embed\">{}</span></a>",
        escape_html(href),
        escape_html(label)
    )
}

/// The footer: the sender's name, position, phone and email as a signature
/// block, with the logo beside or above it, the whole block at the left, centre
/// or right. Empty when there are no details and no logo to show.
pub fn render_footer(l: &BrandedLayout) -> String {
    let (st, _) = sanitize_style(&l.style);
    let accent = parse_hex(&st.accent).unwrap_or((11, 92, 173));
    let text = parse_hex(&st.text_color).unwrap_or((31, 41, 51));
    let muted = parse_hex(&st.muted_color).unwrap_or((82, 96, 109));
    let (name, position, phone, email) = (l.sender_name.trim(), l.position.trim(), l.phone.trim(), l.email.trim());
    let has_info = !(name.is_empty() && position.is_empty() && phone.is_empty() && email.is_empty());
    let logo = (st.footer_logo != FooterLogo::None).then(|| logo_img(l, FOOTER_LOGO_W, FOOTER_LOGO_H)).flatten();
    if !has_info && logo.is_none() {
        return String::new();
    }
    let rtl = is_rtl(name) || (name.is_empty() && is_rtl(position)) || (!has_info && is_rtl(l.org_name.trim()));
    // Left and right follow the reading direction: Arabic starts on the right.
    let (start, end) = if rtl { ("right", "left") } else { ("left", "right") };
    let align = match st.footer_align {
        FooterAlign::Left => start,
        FooterAlign::Center => "center",
        FooterAlign::Right => end,
    };

    let mut who = String::new();
    if !name.is_empty() {
        who.push_str(&format!("<p style=\"margin:0;{}\">{}</p>", text_style(16, text, true), escape_html(name)));
    }
    if !position.is_empty() {
        who.push_str(&format!("<p style=\"margin:2px 0 0;{}\">{}</p>", text_style(14, muted, false), escape_html(position)));
    }
    let mut lines: Vec<String> = Vec::new();
    if !phone.is_empty() {
        let tel = tel_number(phone);
        lines.push(if tel.is_empty() { escape_html(phone) } else { link(&format!("tel:{tel}"), phone, accent) });
    }
    if !email.is_empty() {
        lines.push(match mail_href(email) {
            Some(h) => link(&h, email, accent),
            None => escape_html(email),
        });
    }
    let contact = if lines.is_empty() {
        String::new()
    } else {
        format!("<p style=\"margin:{};{}\">{}</p>", if who.is_empty() { "0" } else { "10px 0 0" }, text_style(14, text, false), lines.join("<br>"))
    };
    let info = format!("{who}{contact}");

    let content = match (logo, st.footer_logo) {
        (Some(img), FooterLogo::Beside) if has_info => {
            let gap = if rtl { "padding:0 0 0 16px" } else { "padding:0 16px 0 0" };
            format!(
                "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" align=\"{align}\" dir=\"{d}\"><tr><td valign=\"top\" style=\"{gap}\">{img}</td><td valign=\"top\" align=\"{start}\" style=\"word-wrap:break-word\">{info}</td></tr></table>",
                d = dir_attr(rtl)
            )
        }
        (Some(img), FooterLogo::Above) if has_info => format!(
            "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" align=\"{align}\" dir=\"{d}\"><tr><td align=\"{align}\" style=\"padding:0 0 12px 0\">{img}</td></tr><tr><td align=\"{align}\" style=\"word-wrap:break-word\">{info}</td></tr></table>",
            d = dir_attr(rtl)
        ),
        (Some(img), _) => format!("<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" align=\"{align}\"><tr><td>{img}</td></tr></table>"),
        (None, _) => info,
    };
    let mut rows = String::new();
    match st.divider {
        LayoutDivider::None => {}
        LayoutDivider::Line => rows.push_str(&rule_row(1, RULE_GREY)),
        LayoutDivider::Bar => rows.push_str(&rule_row(2, &st.accent)),
    }
    rows.push_str(&format!("<tr><td dir=\"{}\" align=\"{align}\" style=\"padding:20px 24px\">{content}</td></tr>", dir_attr(rtl)));
    block(&rows, WHITE)
}

/// Marks of the two pieces that exist for HTML readers only; the plain-text
/// alternative leaves them out (`mime::outgoing_text`).
pub const PREHEADER_MARK: &str = "data-camel-preheader";
pub const LOGO_MARK: &str = "data-camel-logo";

/// Hidden first line that mail apps show next to the subject in the list: the
/// start of the message itself instead of the header's logo and name. The
/// filler keeps the header's words out of the preview.
fn preheader(body_html: &str) -> String {
    let text = crate::mime::html_to_text(body_html);
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(110).collect();
    if text.is_empty() {
        return String::new();
    }
    format!(
        "<div {PREHEADER_MARK}=\"1\" style=\"display:none;font-size:1px;line-height:1px;max-height:0;max-width:0;opacity:0;overflow:hidden;mso-hide:all\">{}{}</div>",
        escape_html(&text),
        "&#847;&zwnj;&nbsp;".repeat(60)
    )
}

/// Header, then the message, then footer: the HTML that is sent.
pub fn apply_layout(body_html: &str, l: &BrandedLayout) -> String {
    let body = block(
        &format!("<tr><td style=\"padding:20px 24px;font-family:{FONT};font-size:15px;line-height:22px\">{body_html}</td></tr>"),
        WHITE,
    );
    format!("{}{}{}{}", preheader(body_html), render_header(l), body, render_footer(l))
}

/// What the UI previews: the two blocks, a sample message inside them, and the
/// colours that were adjusted. `sample_html` is the pretend message body.
pub fn preview(l: &BrandedLayout, sample_html: &str) -> LayoutPreview {
    LayoutPreview {
        header: render_header(l),
        footer: render_footer(l),
        full: apply_layout(sample_html, l),
        adjusted: sanitize_style(&l.style).1,
    }
}

// ----------------------------------------------------------------------------
// Design suggestions (AI and not)
// ----------------------------------------------------------------------------

/// The ready-made templates, in the order "Regenerate" walks through them.
pub const PRESETS: [&str; 6] = ["classic", "band", "tint", "centered", "minimal", "spotlight"];

/// A ready-made template in the logo's colours (the first colour is the brand
/// colour; without a logo the house blue). Unknown names give the classic one.
pub fn preset_style(id: &str, logo_colors: &[String]) -> LayoutStyle {
    let brand = logo_colors.iter().find_map(|c| parse_hex(c)).unwrap_or((11, 92, 173));
    let black = (0, 0, 0);
    let (ink, tint, deep) = (hex(mix(brand, black, 0.82)), hex(mix(brand, WHITE, 0.92)), hex(mix(brand, black, 0.6)));
    let id = PRESETS.iter().find(|p| **p == id).copied().unwrap_or("classic");
    let base = LayoutStyle { accent: hex(brand), text_color: ink, muted_color: "#52606d".into(), template: Some(id.into()), ..LayoutStyle::default() };
    let s = match id {
        // Plain white header, the logo beside the name, a thin rule.
        "classic" => LayoutStyle { divider: LayoutDivider::Line, ..base },
        // A deep brand-coloured band with a bold stripe under it.
        "band" => LayoutStyle { header_bg: deep, divider: LayoutDivider::Bar, ..base },
        // A soft wash of the brand colour; the signature sits under its logo.
        "tint" => LayoutStyle { header_bg: tint, divider: LayoutDivider::Bar, footer_logo: FooterLogo::Above, ..base },
        // Everything centred, logo on top.
        "centered" => LayoutStyle {
            alignment: LayoutAlign::Center,
            logo_position: LogoPosition::Above,
            footer_align: FooterAlign::Center,
            footer_logo: FooterLogo::Above,
            ..base
        },
        // No rules at all, an outline button.
        "minimal" => LayoutStyle { divider: LayoutDivider::None, button_style: ButtonStyle::Outline, ..base },
        // A tinted, centred header; the signature block on the right.
        _ => LayoutStyle {
            header_bg: tint,
            alignment: LayoutAlign::Center,
            logo_position: LogoPosition::Above,
            divider: LayoutDivider::Bar,
            footer_align: FooterAlign::Right,
            button_style: ButtonStyle::Outline,
            ..base
        },
    };
    sanitize_style(&s).0
}

/// A design from the logo's colours. Without AI this is what "Generate" gives,
/// and it is what the AI's choices fall back to. Each `variation` is the next
/// template, so "Regenerate" always looks clearly different.
pub fn suggest_style(logo_colors: &[String], variation: u32) -> LayoutStyle {
    preset_style(PRESETS[variation as usize % PRESETS.len()], logo_colors)
}

/// What the model gets to work from: the details and the logo's colours, as text.
pub fn ai_request(l: &BrandedLayout, variation: u32, wish: &str) -> String {
    let colors = l.logo.as_ref().map(|g| g.colors.join(", ")).unwrap_or_default();
    let mut s = format!(
        "Organisation: {}\nAbout it: {}\nSender: {}, {}\nHas a logo: {}\nLogo colours: {}\nHas a header button: {}\nRequest number: {} (above 0 means the user asked for a different design from the last one: choose clearly different colours, alignment and divider)\n",
        l.org_name.trim(),
        if l.about.trim().is_empty() { "(nothing written)" } else { l.about.trim() },
        l.sender_name.trim(),
        l.position.trim(),
        if l.logo.is_some() { "yes" } else { "no" },
        if colors.is_empty() { "none".into() } else { colors },
        if l.button.is_some() { "yes" } else { "no" },
        variation + 1
    );
    if !wish.trim().is_empty() {
        s.push_str(&format!("What the user asked for: {}\n", wish.trim()));
    }
    s
}

pub fn ai_prompt() -> &'static str {
    "Design the look of a branded email header and footer for the organisation below. Answer with one JSON object and \
     nothing else (no Markdown, no explanation), with exactly these keys: \
     \"accent\" (a #rrggbb colour for links and dividers, taken from the logo colours when there are any), \
     \"header_bg\" (#rrggbb background of the header; white or a very light tint is the safe choice, dark is fine), \
     \"text_color\" (#rrggbb for names, dark), \"muted_color\" (#rrggbb for secondary text, dark grey), \
     \"alignment\" (\"left\" or \"center\"), \"divider\" (\"none\", \"line\" or \"bar\"), \
     \"logo_position\" (\"left\", \"center\" or \"above\"), \
     \"footer_align\" (where the signature block sits: \"left\", \"center\" or \"right\"), \
     \"footer_logo\" (\"none\", \"beside\" or \"above\" the signature details), \
     \"button_style\" (\"filled\" or \"outline\", for the header's button), \
     \"tagline\" (a short line of at most 80 characters under the organisation's name in the header, in the language of \
     the organisation's name, or null). Text must be readable on white. You only choose the design; you never write HTML."
}

/// The model's reply as a style: every field it got right, the rest from `base`.
pub fn parse_ai_style(reply: &str, base: &LayoutStyle) -> LayoutStyle {
    let t = crate::ai::strip_fences(reply);
    let (Some(a), Some(b)) = (t.find('{'), t.rfind('}')) else { return base.clone() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&t[a..=b]) else { return base.clone() };
    let get = |snake: &str, camel: &str| v.get(snake).or_else(|| v.get(camel)).and_then(|x| x.as_str()).map(str::trim);
    let color = |snake: &str, camel: &str, fallback: &str| -> String {
        get(snake, camel).filter(|c| parse_hex(c).is_some()).map(|c| hex(parse_hex(c).unwrap())).unwrap_or_else(|| fallback.to_string())
    };
    let s = LayoutStyle {
        accent: color("accent", "accent", &base.accent),
        header_bg: color("header_bg", "headerBg", &base.header_bg),
        text_color: color("text_color", "textColor", &base.text_color),
        muted_color: color("muted_color", "mutedColor", &base.muted_color),
        alignment: match get("alignment", "alignment") {
            Some("left") => LayoutAlign::Left,
            Some("center") => LayoutAlign::Center,
            _ => base.alignment,
        },
        divider: match get("divider", "divider") {
            Some("none") => LayoutDivider::None,
            Some("line") => LayoutDivider::Line,
            Some("bar") => LayoutDivider::Bar,
            _ => base.divider,
        },
        logo_position: match get("logo_position", "logoPosition") {
            Some("left") => LogoPosition::Left,
            Some("center") => LogoPosition::Center,
            Some("above") => LogoPosition::Above,
            _ => base.logo_position,
        },
        footer_align: match get("footer_align", "footerAlign") {
            Some("left") => FooterAlign::Left,
            Some("center") => FooterAlign::Center,
            Some("right") => FooterAlign::Right,
            _ => base.footer_align,
        },
        footer_logo: match get("footer_logo", "footerLogo") {
            Some("none") => FooterLogo::None,
            Some("beside") => FooterLogo::Beside,
            Some("above") => FooterLogo::Above,
            _ => base.footer_logo,
        },
        button_style: match get("button_style", "buttonStyle") {
            Some("filled") => ButtonStyle::Filled,
            Some("outline") => ButtonStyle::Outline,
            _ => base.button_style,
        },
        tagline: get("tagline", "tagline").map(String::from).filter(|t| !t.is_empty() && t != "null"),
        // A design the model made is its own, not one of the templates.
        template: None,
    };
    sanitize_style(&s).0
}

// ----------------------------------------------------------------------------
// Checks
// ----------------------------------------------------------------------------

fn magic_ok(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        _ => false,
    }
}

/// What a layout may hold; the error says what to change.
pub fn validate(l: &BrandedLayout) -> Result<()> {
    if l.name.trim().is_empty() {
        bail!("Give the layout a name");
    }
    for (what, v, max) in [
        ("The layout name", &l.name, 100),
        ("The organisation name", &l.org_name, 120),
        ("The sender's name", &l.sender_name, 120),
        ("The position", &l.position, 120),
        ("The phone number", &l.phone, 40),
    ] {
        if v.chars().count() > max {
            bail!("{what} is too long");
        }
    }
    if l.org_name.trim().is_empty() && l.logo.is_none() {
        bail!("Add the organisation's name or logo");
    }
    if l.about.chars().count() > MAX_ABOUT_CHARS {
        bail!("The company description is too long (at most {MAX_ABOUT_CHARS} characters)");
    }
    if let Some(b) = &l.button {
        if b.label.trim().is_empty() || b.url.trim().is_empty() {
            bail!("Give the button a text and a link, or remove both");
        }
        if b.label.chars().count() > MAX_BUTTON_CHARS {
            bail!("The button text is too long (at most {MAX_BUTTON_CHARS} characters)");
        }
        if button_href(&b.url).is_none() {
            bail!("The button link must start with https:// or mailto:");
        }
    }
    if !l.email.trim().is_empty() && parse_address(&l.email).is_none_or(|a| a.email.trim() != l.email.trim() || !a.email.contains('@')) {
        bail!("Enter the official email address as name@example.com");
    }
    if let Some(logo) = &l.logo {
        if logo.data.is_empty() && logo.url.as_deref().is_none_or(|u| u.trim().is_empty()) {
            bail!("The logo has no picture");
        }
        if let Some(u) = logo.url.as_deref().filter(|u| !u.trim().is_empty()) {
            if https_url(u).is_none() {
                bail!("The logo address must be a public https:// link");
            }
        }
        if !logo.data.is_empty() {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(logo.data.as_bytes())
                .map_err(|_| anyhow::anyhow!("The logo picture is damaged"))?;
            if bytes.len() > MAX_LOGO_BYTES {
                bail!("The logo is over {} KB; use a smaller picture", MAX_LOGO_BYTES / 1024);
            }
            if !magic_ok(&logo.mime, &bytes) {
                bail!("The logo must be a PNG, JPEG or GIF picture");
            }
        }
        if !(1..=1000).contains(&logo.width) || !(1..=1000).contains(&logo.height) {
            bail!("The logo's size isn't valid");
        }
        if logo.alt.chars().count() > 120 {
            bail!("The logo's description is too long");
        }
        if logo.colors.len() > 4 || logo.colors.iter().any(|c| parse_hex(c).is_none()) {
            bail!("The logo's colours aren't valid");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1x1 transparent PNG.
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

    fn layout() -> BrandedLayout {
        BrandedLayout {
            id: "lay_1".into(),
            account_id: None,
            name: "Work".into(),
            org_name: "Acme & Sons".into(),
            about: "Family bakers since 1990. Fresh bread, every morning.".into(),
            logo: Some(LayoutLogo { data: PNG.into(), mime: "image/png".into(), width: 400, height: 100, colors: vec!["#d2691e".into()], ..Default::default() }),
            button: Some(LayoutButton { label: "Visit our shop".into(), url: "https://acme.test/shop".into() }),
            sender_name: "Dana <Lee>".into(),
            position: "Head of Sales".into(),
            phone: "+1 (555) 010-2030".into(),
            email: "dana@acme.test".into(),
            style: LayoutStyle::default(),
            use_on_replies: false,
            is_default: true,
            updated_at: 0,
        }
    }

    #[test]
    fn markup_follows_the_email_rules() {
        let html = apply_layout("<p>Hello</p>", &layout());
        for banned in ["<script", "<style", "class=", "<link", "javascript:", "<svg", "background-image", "@import"] {
            assert!(!html.contains(banned), "{banned} in {html}");
        }
        assert!(html.contains("width=\"600\"") && html.contains("max-width:600px"));
        assert!(html.contains("role=\"presentation\""));
        // The message sits between the blocks.
        let (h, b, f) = (html.find("Acme &amp; Sons"), html.find("<p>Hello</p>"), html.find("Head of Sales"));
        assert!(h < b && b < f, "{html}");
        // Every tag that carries styling does it inline.
        assert!(html.contains("font-family:Arial"));
    }

    #[test]
    fn user_text_is_escaped() {
        let html = render_footer(&layout());
        assert!(html.contains("Dana &lt;Lee&gt;"), "{html}");
        assert!(!html.contains("<Lee>"));
        let mut l = layout();
        l.org_name = "\"><script>alert(1)</script>".into();
        let h = render_header(&l);
        assert!(!h.contains("<script"), "{h}");
    }

    #[test]
    fn contact_links() {
        let html = render_footer(&layout());
        assert!(html.contains("href=\"tel:+15550102030\""), "{html}");
        assert!(html.contains(">+1 (555) 010-2030<"));
        assert!(html.contains("href=\"mailto:dana@acme.test\""));
        // Arabic-Indic digits dial as ASCII digits.
        assert_eq!(tel_number("+٩٦٦ ٥٠ ١٢٣ ٤٥٦٧"), "+966501234567");
        assert_eq!(tel_number("ext. none"), "");
        let mut l = layout();
        l.phone = "call us".into();
        let html = render_footer(&l);
        assert!(html.contains("call us") && !html.contains("tel:"));
    }

    #[test]
    fn logo_has_size_alt_and_a_blocked_images_fallback() {
        let h = render_header(&layout());
        // 400x100 shown at 200x50 (twice the pixels for sharp screens).
        assert!(h.contains("width=\"200\" height=\"50\""), "{h}");
        assert!(h.contains("alt=\"Acme &amp; Sons logo\""));
        assert!(h.contains("display:block"));
        // The name is text beside the picture, so it stays when images are blocked.
        assert!(h.contains(">Acme &amp; Sons</p>"));
        // A tall logo is limited by its height.
        let mut l = layout();
        l.logo.as_mut().unwrap().width = 100;
        l.logo.as_mut().unwrap().height = 400;
        assert!(render_header(&l).contains("width=\"20\" height=\"80\""));
        // The user's own description wins; a public link replaces the picture.
        l.logo.as_mut().unwrap().alt = "Acme mark".into();
        l.logo.as_mut().unwrap().url = Some("https://acme.test/logo.png".into());
        let h = render_header(&l);
        assert!(h.contains("alt=\"Acme mark\"") && h.contains("src=\"https://acme.test/logo.png\"") && !h.contains("data:image"));
        // Anything but https is ignored.
        l.logo.as_mut().unwrap().url = Some("http://acme.test/logo.png".into());
        assert!(render_header(&l).contains("data:image/png;base64,"));
    }

    #[test]
    fn a_logo_on_a_dark_header_gets_a_white_plate() {
        let mut l = layout();
        l.button = None;
        let light = render_header(&l);
        assert!(!light.contains("padding:6px 10px"), "{light}");
        l.style.header_bg = "#10243f".into();
        let dark = render_header(&l);
        assert!(dark.contains("<td bgcolor=\"#ffffff\" style=\"background-color:#ffffff;padding:6px 10px;border-radius:8px\"><img"), "{dark}");
        // Only the logo is plated, never the name.
        assert_eq!(dark.matches("bgcolor=\"#ffffff\"").count(), 1);
    }

    #[test]
    fn no_logo_means_only_the_name() {
        let mut l = layout();
        l.logo = None;
        let h = render_header(&l);
        assert!(!h.contains("<img") && h.contains("Acme &amp; Sons"));
        l.org_name.clear();
        assert_eq!(render_header(&l), "");
    }

    #[test]
    fn arabic_blocks_run_right_to_left() {
        let mut l = layout();
        l.org_name = "شركة النور".into();
        l.sender_name = "سارة أحمد".into();
        l.position = "مديرة المبيعات".into();
        let h = render_header(&l);
        assert!(h.contains("dir=\"rtl\"") && h.contains("align=\"right\""), "{h}");
        assert!(h.contains("alt=\"شعار شركة النور\""));
        let f = render_footer(&l);
        assert!(f.contains("dir=\"rtl\"") && f.contains("align=\"right\""), "{f}");
        // The English layout stays left to right.
        assert!(render_footer(&layout()).contains("dir=\"ltr\""));
        assert!(is_rtl("  123 שלום") && !is_rtl("Hello مرحبا") && !is_rtl("1234"));
    }

    #[test]
    fn colours_are_made_readable() {
        // Pale yellow links fail on white; the renderer darkens them until they pass.
        let style = LayoutStyle { accent: "#ffe066".into(), text_color: "#cccccc".into(), muted_color: "#aaaaaa".into(), ..LayoutStyle::default() };
        let (fixed, adjusted) = sanitize_style(&style);
        for c in [&fixed.accent, &fixed.text_color, &fixed.muted_color] {
            assert!(contrast(parse_hex(c).unwrap(), WHITE) >= MIN_CONTRAST, "{c}");
        }
        assert_eq!(adjusted, ["accent", "textColor", "mutedColor"]);
        // Colours that already read are left alone, and nothing is reported.
        let (same, adjusted) = sanitize_style(&LayoutStyle::default());
        assert_eq!(same, LayoutStyle { tagline: None, ..LayoutStyle::default() });
        assert!(adjusted.is_empty());
        // Junk becomes the default.
        let (fixed, adjusted) = sanitize_style(&LayoutStyle { accent: "red; background:url(x)".into(), ..LayoutStyle::default() });
        assert_eq!(fixed.accent, LayoutStyle::default().accent);
        assert_eq!(adjusted, ["accent"]);
        // The header's own text reads on its dark background whatever the style says.
        let mut l = layout();
        l.style.header_bg = "#10243f".into();
        let h = render_header(&l);
        assert!(h.contains("color:#ffffff"), "{h}");
        assert!(contrast(on_header(parse_hex("#1f2933").unwrap(), parse_hex("#10243f").unwrap()), parse_hex("#10243f").unwrap()) >= MIN_CONTRAST);
    }

    #[test]
    fn every_suggestion_is_readable_and_they_differ() {
        for colors in [vec![], vec!["#ffe066".into()], vec!["#0b3d91".into()], vec!["#000000".into()], vec!["#ffffff".into()]] {
            let mut seen = Vec::new();
            for v in 0..6 {
                let s = suggest_style(&colors, v);
                for c in [&s.accent, &s.text_color, &s.muted_color] {
                    assert!(contrast(parse_hex(c).unwrap(), WHITE) >= MIN_CONTRAST, "{colors:?} v{v} {c}");
                }
                let bg = parse_hex(&s.header_bg).unwrap();
                assert!(contrast(on_header(parse_hex(&s.text_color).unwrap(), bg), bg) >= MIN_CONTRAST);
                seen.push(s);
            }
            for i in 0..seen.len() {
                for j in i + 1..seen.len() {
                    assert_ne!(seen[i], seen[j], "{colors:?}: {i} and {j}");
                }
            }
        }
    }

    #[test]
    fn model_reply_is_read_strictly() {
        let base = suggest_style(&["#d2691e".into()], 0);
        let reply = "```json\n{\"accent\":\"#0a7d4f\",\"header_bg\":\"#f2faf6\",\"text_color\":\"#102a1f\",\"muted_color\":\"#4a5d55\",\
                     \"alignment\":\"center\",\"divider\":\"bar\",\"logo_position\":\"above\",\"footer_align\":\"right\",\"footer_logo\":\"above\",\"button_style\":\"outline\",\
                     \"tagline\":\"Fresh since 1990\"}\n```";
        let s = parse_ai_style(reply, &base);
        assert_eq!(s.accent, "#0a7d4f");
        assert_eq!((s.alignment, s.divider, s.logo_position), (LayoutAlign::Center, LayoutDivider::Bar, LogoPosition::Above));
        assert_eq!((s.footer_align, s.footer_logo, s.button_style), (FooterAlign::Right, FooterLogo::Above, ButtonStyle::Outline));
        assert_eq!(s.template, None, "the model's design is no template");
        assert_eq!(s.tagline.as_deref(), Some("Fresh since 1990"));
        // Garbage, or no object at all: the suggestion.
        assert_eq!(parse_ai_style("I'd go with blue!", &base), base);
        assert_eq!(parse_ai_style("{not json}", &base), base);
        // Wrong values are dropped one by one; markup in a tagline is only ever text.
        let s = parse_ai_style("{\"accent\":\"url(javascript:1)\",\"divider\":\"wavy\",\"tagline\":\"<b>hi</b>\"}", &base);
        assert_eq!((s.accent.as_str(), s.divider), (base.accent.as_str(), base.divider));
        let mut l = layout();
        l.style = s;
        let h = render_header(&l);
        assert!(h.contains("&lt;b&gt;hi&lt;/b&gt;") && !h.contains("<b>"), "{h}");
        // A colour that fails contrast is fixed on the way in; an overlong tagline is cut.
        let s = parse_ai_style(&format!("{{\"accent\":\"#ffff00\",\"tagline\":\"{}\"}}", "x".repeat(200)), &base);
        assert!(contrast(parse_hex(&s.accent).unwrap(), WHITE) >= MIN_CONTRAST);
        assert_eq!(s.tagline.unwrap().chars().count(), 80);
    }

    #[test]
    fn the_footer_has_the_logo_and_sits_where_you_put_it() {
        let mut l = layout();
        // Default: the block at the left, the logo beside the details, smaller than in the header.
        let f = render_footer(&l);
        assert!(f.contains("width=\"120\" height=\"30\""), "{f}");
        assert_eq!(f.matches("valign=\"top\"").count(), 2, "logo cell and details cell: {f}");
        assert!(f.contains("align=\"left\" dir=\"ltr\"><tr><td valign=\"top\""), "{f}");
        assert!(f.find("<img").unwrap() < f.find("Dana &lt;Lee&gt;").unwrap());
        // Centre, logo above the details.
        l.style.footer_align = FooterAlign::Center;
        l.style.footer_logo = FooterLogo::Above;
        let f = render_footer(&l);
        assert!(f.contains("align=\"center\""), "{f}");
        assert_eq!(f.matches("valign=\"top\"").count(), 0);
        assert!(f.find("<img").unwrap() < f.find("Dana &lt;Lee&gt;").unwrap());
        // Right.
        l.style.footer_align = FooterAlign::Right;
        assert!(render_footer(&l).contains("<td dir=\"ltr\" align=\"right\""));
        // Left and right follow the text: for Arabic, "left" is where reading starts, on the right.
        l.sender_name = "سارة أحمد".into();
        l.style.footer_align = FooterAlign::Left;
        assert!(render_footer(&l).contains("<td dir=\"rtl\" align=\"right\""));
        l.style.footer_align = FooterAlign::Right;
        assert!(render_footer(&l).contains("<td dir=\"rtl\" align=\"left\""));
        // No logo in the footer.
        l.style.footer_logo = FooterLogo::None;
        assert!(!render_footer(&l).contains("<img"));
        // Details gone: only the logo remains; nothing at all when that is off too.
        l.style.footer_logo = FooterLogo::Beside;
        for f in [&mut l.sender_name, &mut l.position, &mut l.phone, &mut l.email] {
            f.clear();
        }
        let only_logo = render_footer(&l);
        assert!(only_logo.contains("<img") && !only_logo.contains("<p "), "{only_logo}");
        l.style.footer_logo = FooterLogo::None;
        assert_eq!(render_footer(&l), "");
    }

    #[test]
    fn the_header_has_a_brief_and_a_button() {
        let mut l = layout();
        l.about = "Bread & <b>cakes</b>, baked daily.".into();
        let h = render_header(&l);
        // The brief sits under the name, as text.
        assert!(h.contains("Bread &amp; &lt;b&gt;cakes&lt;/b&gt;, baked daily."), "{h}");
        assert!(h.find("Acme &amp; Sons").unwrap() < h.find("Bread &amp;").unwrap());
        // The button: a coloured cell around a padded link, with the Outlook padding fallback.
        assert!(h.contains("href=\"https://acme.test/shop\""), "{h}");
        assert!(h.contains("mso-padding-alt:10px 22px") && h.contains("border-radius:6px") && h.contains("display:inline-block"), "{h}");
        assert!(h.contains(">Visit our shop</span></a>"));
        assert!(h.find("Bread &amp;").unwrap() < h.find("Visit our shop").unwrap());
        // Filled: the accent behind white text, and it passes the contrast check.
        let (st, _) = sanitize_style(&l.style);
        assert!(h.contains(&format!("bgcolor=\"{}\"", st.accent)), "{h}");
        assert!(contrast(parse_hex(&st.accent).unwrap(), WHITE) >= MIN_CONTRAST);
        // Outline: the header's own colour behind accent-coloured text and border.
        l.style.button_style = ButtonStyle::Outline;
        let o = render_header(&l);
        assert!(o.contains("bgcolor=\"#ffffff\" style=\"background-color:#ffffff;border:2px solid"), "{o}");
        // On a tinted header the outline stays in the brand colour (darkened to read), not black.
        l.style.header_bg = "#fbf1ea".into();
        l.style.accent = "#d2691e".into();
        let tinted = render_header(&l);
        let (st, _) = sanitize_style(&l.style);
        let bg = parse_hex("#fbf1ea").unwrap();
        let ink = hex(readable(parse_hex(&st.accent).unwrap(), bg).0);
        assert!(tinted.contains(&format!("border:2px solid {ink}")) && ink != "#111827", "{tinted}");
        l.style.header_bg = "#ffffff".into();
        l.style.accent = LayoutStyle::default().accent;
        // On a dark header the button turns white so it doesn't melt into it.
        l.style.button_style = ButtonStyle::Filled;
        l.style.header_bg = "#10243f".into();
        let d = render_header(&l);
        assert!(d.contains("<td align=\"center\" bgcolor=\"#ffffff\" style=\"background-color:#ffffff;border:2px solid #ffffff"), "{d}");
        // A link that isn't https:// or mailto: is never put in the mail; `mailto:` is fine.
        l.style.header_bg = "#ffffff".into();
        l.button = Some(LayoutButton { label: "Click".into(), url: "javascript:alert(1)".into() });
        let bad = render_header(&l);
        assert!(!bad.contains("javascript:") && !bad.contains(">Click<"), "{bad}");
        l.button = Some(LayoutButton { label: "Write to us".into(), url: "mailto:hello@acme.test".into() });
        assert!(render_header(&l).contains("href=\"mailto:hello@acme.test\""));
        // Without them the header is as before; centred headers centre them.
        l.about.clear();
        l.button = None;
        let plain = render_header(&l);
        assert!(!plain.contains("<p style=\"margin:0;font-family:Arial,'Helvetica Neue',Helvetica,Tahoma,'Segoe UI','Geeza Pro',sans-serif;font-size:14px"));
        l.about = "Hi".into();
        l.button = Some(LayoutButton { label: "Go".into(), url: "https://acme.test".into() });
        l.style.alignment = LayoutAlign::Center;
        let c = render_header(&l);
        assert!(c.matches("align=\"center\"").count() >= 5, "{c}");
    }

    #[test]
    fn the_templates_fit_any_logo_and_differ() {
        assert_eq!(PRESETS.len(), 6);
        for colors in [vec![], vec!["#d2691e".to_string()], vec!["#0b3d91".to_string()], vec!["#ffe066".to_string()]] {
            let mut seen: Vec<LayoutStyle> = Vec::new();
            for id in PRESETS {
                let s = preset_style(id, &colors);
                assert_eq!(s.template.as_deref(), Some(id));
                // Already readable: sanitising changes nothing.
                assert_eq!(sanitize_style(&s).1, Vec::<String>::new(), "{id} {colors:?}");
                seen.push(s);
            }
            for i in 0..seen.len() {
                for j in i + 1..seen.len() {
                    assert_ne!(
                        LayoutStyle { template: None, ..seen[i].clone() },
                        LayoutStyle { template: None, ..seen[j].clone() },
                        "{} and {} look the same ({colors:?})",
                        PRESETS[i],
                        PRESETS[j]
                    );
                }
            }
        }
        // Unknown names give the classic one; every template renders into a complete layout.
        assert_eq!(preset_style("no-such", &[]).template.as_deref(), Some("classic"));
        for id in PRESETS {
            let mut l = layout();
            l.style = preset_style(id, &["#d2691e".into()]);
            let html = apply_layout("<p>Hello</p>", &l);
            for part in ["Acme &amp; Sons", "Family bakers", "Visit our shop", "Dana &lt;Lee&gt;", "tel:+15550102030"] {
                assert!(html.contains(part), "{id}: {part}");
            }
            assert_eq!(html.matches("<img").count(), 2, "{id}: the logo in header and footer");
        }
    }

    #[test]
    fn what_a_layout_may_hold() {
        assert!(validate(&layout()).is_ok());
        let bad = |f: &dyn Fn(&mut BrandedLayout)| {
            let mut l = layout();
            f(&mut l);
            validate(&l).unwrap_err().to_string()
        };
        assert!(bad(&|l| l.name = " ".into()).contains("name"));
        assert!(bad(&|l| l.email = "not an email".into()).contains("email"));
        assert!(bad(&|l| l.logo.as_mut().unwrap().mime = "image/svg+xml".into()).contains("PNG"));
        assert!(bad(&|l| l.logo.as_mut().unwrap().data = base64::engine::general_purpose::STANDARD.encode(b"<svg onload=alert(1)>")).contains("PNG"));
        assert!(bad(&|l| l.logo.as_mut().unwrap().data = base64::engine::general_purpose::STANDARD.encode(vec![0x89u8; MAX_LOGO_BYTES + 1])).contains("KB"));
        assert!(bad(&|l| l.logo.as_mut().unwrap().url = Some("javascript:alert(1)".into())).contains("https"));
        assert!(bad(&|l| l.logo.as_mut().unwrap().colors = vec!["red".into()]).contains("colours"));
        assert!(bad(&|l| l.about = "x".repeat(MAX_ABOUT_CHARS + 1)).contains("description"));
        assert!(bad(&|l| l.button = Some(LayoutButton { label: "Go".into(), url: "http://acme.test".into() })).contains("https"));
        assert!(bad(&|l| l.button = Some(LayoutButton { label: "Go".into(), url: "javascript:alert(1)".into() })).contains("https"));
        assert!(bad(&|l| l.button = Some(LayoutButton { label: "".into(), url: "https://acme.test".into() })).contains("text and a link"));
        assert!(bad(&|l| l.button = Some(LayoutButton { label: "x".repeat(MAX_BUTTON_CHARS + 1), url: "https://acme.test".into() })).contains("too long"));
        assert!(bad(&|l| {
            l.org_name.clear();
            l.logo = None;
        })
        .contains("organisation"));
    }
}
