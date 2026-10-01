//! PS4 system-theme packages.
//!
//! The Theme Studio renders every image; this module checks them, lays out the
//! theme the way the console expects, compresses the animated background into
//! an RAF scene and hands the folder to the bundled packager, which writes a
//! fake-signed system-theme PKG (IRO tag 2). Installing that PKG through the
//! receiver puts it under Settings > Themes. The layout, the RAF scene and the
//! size limits follow PS4 Ultimate Theme Creator (MIT), which verified them on
//! hardware; see build/themepack-cli/NOTICE.txt.
use super::*;
use image::{imageops::FilterType, DynamicImage, GenericImageView, ImageFormat, RgbaImage};
use std::{collections::BTreeMap, process::Stdio};

const BACKGROUND: (u32, u32) = (1920, 1080);
const PREVIEW: (u32, u32) = (740, 416);
const ICON0: (u32, u32) = (512, 512);
const CONTENT_ICON: (u32, u32) = (512, 512);
const FUNCTION_ICON: (u32, u32) = (128, 128);
const FUNCTION_GLOW: (u32, u32) = (152, 152);
/// Resolutions offered for the animated background (the budget is in bytes).
const ANIMATION_SIZES: [(u32, u32); 3] = [(1280, 720), (960, 540), (640, 360)];
/// Measured on a console: more .dds bytes than this give CE-38196-7.
const RAF_BYTES: usize = 6 * 1024 * 1024;
/// Separate cap on the number of frames (48 applies, 54 fails).
const RAF_FRAMES: usize = 48;
const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;
pub(super) const CONTENT_ICONS: [&str; 10] = ["browser", "disc", "discoverlay", "folder", "gallery", "library", "livefromps", "shareplay", "tvvideo", "usbmusic"];
pub(super) const FUNCTION_ICONS: [&str; 9] = ["community", "event", "friend", "message", "notification", "party", "power", "setting", "trophy"];

/// A single textured plane filling the RAF camera's view (yfov 29 at depth
/// 2.146484 x 100). These are the exact bytes PS4 Ultimate Theme Creator's
/// `mdx.quad_model(node_name="plane", half_w=0.98688, half_h=0.55512)` writes;
/// the geometry never changes, so the verified output is embedded as is.
const PLANE_MDX: &str = "58444d2e30302e314d5350000000000002001000100000001000000094010000100018001800000018000000840100006d6f64656c2d3000838014000000000000000000e3a556c35075e242848018000100000000000000ee5f0940000000000000c04011001c001c0000001c0000001c000000526f6f744e6f6465000000001100180018000000180000004c000000706c616e65000000408408000010110044841400f30435bf0000000000000000f304353f4b8410000000c8420000c8420000c8427f84080000101200120018001800000018000000bc000000706c616e65000000130018001800000018000000440000006d6573682d300000c084080000201600c184080000101400e0841c0003000000060000000100000000000100020000000200030014001c002800000060000000600000006172726179732d3000000000230090040e00000004000000e5bb4b4071380081000000000000e5bb4b4071b8008100000000003ce53b4b4071b800810000003c003ce53b4b40713800810000003c000016001c001c0000001c0000001c0000006d6174657269616c2d300000";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThemeColors {
    pub theme_color: u8,
    pub font: String,
    pub font_shadow: String,
    pub focus: String,
    pub home_dimmer: String,
    pub function_dimmer: String,
    pub title_dimmer: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FunctionIcon { pub icon: String, pub glow: String }
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThemeAnimation { pub width: u32, pub height: u32, pub wait: f64, pub frames: Vec<String> }
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThemeBuildRequest {
    pub title: String,
    /// 16 characters [A-Z0-9]; the same label replaces the same theme.
    pub label: String,
    /// PNG images as base64 (a data: prefix is accepted).
    pub home: String,
    pub function: Option<String>,
    pub preview: Option<String>,
    pub icon0: Option<String>,
    #[serde(default)]
    pub content_icons: BTreeMap<String, String>,
    #[serde(default)]
    pub function_icons: BTreeMap<String, FunctionIcon>,
    pub colors: ThemeColors,
    pub animation: Option<ThemeAnimation>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThemeBuildResult {
    pub path: String,
    pub size: u64,
    pub content_id: String,
    pub title: String,
    pub frames: usize,
    pub scene_bytes: usize,
    pub files: usize,
}

fn decode_png(label: &str, data: &str) -> Result<DynamicImage, String> {
    let data = data.split_once(',').filter(|(head, _)| head.starts_with("data:")).map_or(data, |(_, body)| body);
    if data.len() > MAX_IMAGE_BYTES * 4 / 3 + 4 { return Err(format!("{label} is too large")); }
    let bytes = BASE64.decode(data.trim()).map_err(|_| format!("{label} is not valid base64"))?;
    image::load_from_memory_with_format(&bytes, ImageFormat::Png).map_err(|_| format!("{label} is not a readable PNG"))
}
fn sized(label: &str, image: DynamicImage, size: (u32, u32)) -> Result<DynamicImage, String> {
    if image.dimensions() != size { return Err(format!("{label} must be {}x{} (got {}x{})", size.0, size.1, image.width(), image.height())); }
    Ok(image)
}
/// Largest file a tile, icon or thumbnail may be. A theme whose tile PNGs were 257–307 KB never
/// became usable on a PS4, while ones up to 127 KB applied, so these stay within that.
const ICON_BYTES: usize = 128 * 1024;
/// Backgrounds of up to 1.7 MB applied on a console.
const BACKGROUND_BYTES: usize = 1_700_000;
/// IDAT chunk size libpng and ffmpeg write; the console's own theme files use small chunks too.
const IDAT_CHUNK: usize = 8192;

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 { crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 }; }
    }
    !crc
}
/// The same PNG with its image data re-split into `IDAT_CHUNK`-byte chunks.
fn split_idat(png: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || "PNG encoding produced an unreadable file".to_string();
    if png.len() < 8 { return Err(bad()); }
    let (mut before, mut data, mut after, mut at) = (Vec::new(), Vec::new(), Vec::new(), 8usize);
    while at + 12 <= png.len() {
        let length = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        let end = at.checked_add(12 + length).filter(|&e| e <= png.len()).ok_or_else(bad)?;
        let kind = &png[at + 4..at + 8];
        if kind == b"IDAT" { data.extend_from_slice(&png[at + 8..at + 8 + length]); }
        else if data.is_empty() { before.extend_from_slice(&png[at..end]); }
        else { after.extend_from_slice(&png[at..end]); }
        at = end;
    }
    let mut out = Vec::with_capacity(png.len() + data.len() / IDAT_CHUNK * 12 + 12);
    out.extend_from_slice(&png[..8]);
    out.extend_from_slice(&before);
    for part in data.chunks(IDAT_CHUNK) {
        out.extend_from_slice(&(part.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(b"IDAT");
        out.extend_from_slice(part);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out.extend_from_slice(&after);
    Ok(out)
}
/// Every theme image is written as 8-bit RGBA, backgrounds included: that is what PS4 Ultimate
/// Theme Creator's console-verified themes carry (its encoder forces rgba for every PNG).
fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, String> {
    use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
    use image::{ExtendedColorType, ImageEncoder};
    let (w, h) = image.dimensions();
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Best, PngFilter::Adaptive)
        .write_image(image.to_rgba8().as_raw(), w, h, ExtendedColorType::Rgba8)
        .map_err(|e| format!("PNG encoding failed: {e}"))?;
    split_idat(&out)
}
/// Rounds every channel to `levels` steps: soft glows and gradients compress far better and look the same at tile size.
fn posterize(image: &DynamicImage, levels: u32) -> DynamicImage {
    let step = 256 / levels;
    let mut rgba = image.to_rgba8();
    for value in rgba.iter_mut() { *value = ((u32::from(*value) + step / 2) / step * step).min(255) as u8; }
    DynamicImage::ImageRgba8(rgba)
}
/// A theme PNG within `budget` bytes.
fn png(label: &str, image: &DynamicImage, budget: usize) -> Result<Vec<u8>, String> {
    let mut bytes = encode_png(image)?;
    for levels in [64, 32, 16, 8] {
        if bytes.len() <= budget { break; }
        bytes = encode_png(&posterize(image, levels))?;
    }
    if bytes.len() > budget { return Err(format!("{label} is still {} KB after compression; the PS4 needs it under {} KB. Use a simpler image.", bytes.len() / 1024, budget / 1024)); }
    Ok(bytes)
}
fn argb(label: &str, value: &str) -> Result<String, String> {
    let valid = value.len() == 9 && value.starts_with('#') && value[1..].chars().all(|c| c.is_ascii_hexdigit());
    if valid { Ok(value.to_ascii_uppercase()) } else { Err(format!("{label} must be an #AARRGGBB colour")) }
}
pub(super) fn theme_xml(label: &str, colors: &ThemeColors) -> Result<String, String> {
    if colors.theme_color > 7 { return Err("Theme colour must be 0 to 7".into()); }
    Ok([
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>".to_string(),
        "<theme>".into(),
        "    <format-version>4.0</format-version>".into(),
        format!("    <themecolor>{}</themecolor>", colors.theme_color),
        format!("    <label>{}</label>", label.to_ascii_lowercase()),
        format!("    <fontcolor>{}</fontcolor>", argb("Font colour", &colors.font)?),
        format!("    <fontshadowcolor>{}</fontshadowcolor>", argb("Font shadow", &colors.font_shadow)?),
        format!("    <focuscolor>{}</focuscolor>", argb("Focus colour", &colors.focus)?),
        format!("    <homescreen-dimmer>{}</homescreen-dimmer>", argb("Home dimmer", &colors.home_dimmer)?),
        format!("    <functionscreen-dimmer>{}</functionscreen-dimmer>", argb("Function screen dimmer", &colors.function_dimmer)?),
        format!("    <titlename-dimmer>{}</titlename-dimmer>", argb("Title dimmer", &colors.title_dimmer)?),
        // True without sound/bgm_home.at9 makes the console play its own music.
        "    <homebgm-enable>False</homebgm-enable>".into(),
        "</theme>".into(),
        String::new(),
    ].join("\n"))
}

/// index.xml of scene/background.raf: one plane, one material and actor per
/// frame, and a looping sequence that hands visibility to the next frame.
pub(super) fn raf_scene(frames: usize, wait: f64) -> String {
    let wait = format!("{}", (wait.clamp(0.02, 5.0) * 1000.).round() / 1000.);
    let id = |i: usize| format!("f{i:02}");
    let mut lines = vec![
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_string(),
        "<raf angle_unit=\"degree\">".into(), String::new(),
        "\t<system camera_switch_mode=\"vertical\" layer_switch_distance=\"0\"/>".into(), String::new(),
        "\t<camera id=\"camera1\" type=\"perspective\" yfov=\"29\" znear=\"0.1\" zfar=\"300\" position=\"0 0 0\" direction=\"0 0 -1\" up=\"0 1 0\"/>".into(),
        "\t<light id=\"light1\" type=\"ambient\" color=\"1 1 1\"/>".into(), String::new(),
        // The model is referenced as .fbx although the file on disk is .mdx.
        "\t<model id=\"plane\" file=\"plane.fbx\"/>".into(), String::new(),
    ];
    for i in 0..frames {
        lines.push(format!("\t<material id=\"{}\" effect=\"pure_texture\">", id(i)));
        lines.push(format!("\t\t<texture file=\"{i:02}.dds\"/>"));
        lines.push("\t</material>".into());
    }
    lines.push(String::new());
    for i in 0..frames {
        lines.push(format!("\t<actor id=\"{0}\" model=\"plane\" material=\"{0}\" color=\"1 1 1 {1}\" position=\"0 0 9\" zsort=\"back_to_front\"/>", id(i), u8::from(i == 0)));
    }
    lines.push(String::new());
    lines.push("\t<sequence>".into());
    for i in 0..frames {
        lines.push(format!("\t\t<case wait=\"{wait}\">"));
        lines.push(format!("\t\t\t<actor id=\"{}\" color=\"1 1 1 0\"/>", id(i)));
        lines.push(format!("\t\t\t<actor id=\"{}\" color=\"1 1 1 1\"/>", id((i + 1) % frames)));
        lines.push("\t\t</case>".into());
    }
    lines.extend(["\t</sequence>".into(), String::new(), "</raf>".into(), String::new()]);
    lines.join("\n")
}

fn quantize(c: [f32; 3]) -> (u16, [f32; 3]) {
    let r = (c[0].clamp(0., 255.) * 31. / 255.).round() as u16;
    let g = (c[1].clamp(0., 255.) * 63. / 255.).round() as u16;
    let b = (c[2].clamp(0., 255.) * 31. / 255.).round() as u16;
    ((r << 11) | (g << 5) | b, [f32::from((r << 3) | (r >> 2)), f32::from((g << 2) | (g >> 4)), f32::from((b << 3) | (b >> 2))])
}
/// The ends of a pixel set along its principal colour axis (power iteration).
fn principal_ends(px: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let n = px.len().max(1) as f32;
    let mut mean = [0f32; 3];
    for p in px { for c in 0..3 { mean[c] += p[c] / n; } }
    let mut cov = [[0f32; 3]; 3];
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in px {
        let d = [p[0] - mean[0], p[1] - mean[1], p[2] - mean[2]];
        for i in 0..3 { lo[i] = lo[i].min(d[i]); hi[i] = hi[i].max(d[i]); for j in 0..3 { cov[i][j] += d[i] * d[j]; } }
    }
    let normalize = |v: [f32; 3]| { let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt(); if n > 1e-6 { Some([v[0] / n, v[1] / n, v[2] / n]) } else { None } };
    let mut axis = normalize([hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]).unwrap_or([1., 0., 0.]);
    for _ in 0..8 {
        let next = [0, 1, 2].map(|i| cov[i][0] * axis[0] + cov[i][1] * axis[1] + cov[i][2] * axis[2]);
        match normalize(next) { Some(v) => axis = v, None => break }
    }
    let (mut tmin, mut tmax) = (f32::MAX, f32::MIN);
    for p in px {
        let t = (p[0] - mean[0]) * axis[0] + (p[1] - mean[1]) * axis[1] + (p[2] - mean[2]) * axis[2];
        tmin = tmin.min(t); tmax = tmax.max(t);
    }
    ([0, 1, 2].map(|c| mean[c] + axis[c] * tmax), [0, 1, 2].map(|c| mean[c] + axis[c] * tmin))
}
/// One opaque 4x4 block to BC1: endpoints on the block's principal colour
/// axis, indices chosen against the decoded palette.
fn bc1_block(px: &[[f32; 3]; 16]) -> [u8; 8] {
    let (high, low) = principal_ends(px);
    let (pa, ca) = quantize(high);
    let (pb, cb) = quantize(low);
    // Opaque blocks want colour0 > colour1 (four-colour mode).
    let ((p0, c0), (p1, c1)) = if pa >= pb { ((pa, ca), (pb, cb)) } else { ((pb, cb), (pa, ca)) };
    let four = p0 > p1;
    let palette = if four {
        [c0, c1, [0, 1, 2].map(|c| (2. * c0[c] + c1[c]) / 3.), [0, 1, 2].map(|c| (c0[c] + 2. * c1[c]) / 3.)]
    } else {
        [c0, c1, [0, 1, 2].map(|c| (c0[c] + c1[c]) / 2.), [0.; 3]]
    };
    let mut bits = 0u32;
    for (i, p) in px.iter().enumerate() {
        let mut best = (f32::MAX, 0u32);
        // Index 3 is transparent in three-colour mode; opaque pixels never use it.
        for (k, q) in palette.iter().enumerate().take(if four { 4 } else { 3 }) {
            let d = (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2);
            if d < best.0 { best = (d, k as u32); }
        }
        bits |= best.1 << (2 * i);
    }
    let mut out = [0u8; 8];
    out[..2].copy_from_slice(&p0.to_le_bytes());
    out[2..4].copy_from_slice(&p1.to_le_bytes());
    out[4..].copy_from_slice(&bits.to_le_bytes());
    out
}
/// Opaque RGBA image to a DXT1 .dds without mipmaps (alpha is flattened on black).
pub(super) fn dds_dxt1(image: &RgbaImage) -> Vec<u8> {
    let (w, h) = image.dimensions();
    let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
    let payload = (bw * bh * 8) as usize;
    let mut out = Vec::with_capacity(128 + payload);
    out.extend_from_slice(b"DDS ");
    // DDSD_CAPS | HEIGHT | WIDTH | PIXELFORMAT | LINEARSIZE; no mipmap flag.
    for value in [124u32, 0x1 | 0x2 | 0x4 | 0x1000 | 0x80000, h, w, payload as u32, 0, 0] { out.extend_from_slice(&value.to_le_bytes()); }
    out.extend_from_slice(&[0; 44]);
    for value in [32u32, 0x4] { out.extend_from_slice(&value.to_le_bytes()); }
    out.extend_from_slice(b"DXT1");
    out.extend_from_slice(&[0; 20]);
    for value in [0x1000u32, 0, 0, 0, 0] { out.extend_from_slice(&value.to_le_bytes()); }
    debug_assert_eq!(out.len(), 128);
    for by in 0..bh {
        for bx in 0..bw {
            let mut block = [[0f32; 3]; 16];
            for y in 0..4 {
                for x in 0..4 {
                    // Edge blocks replicate the last row/column.
                    let p = image.get_pixel((bx * 4 + x).min(w - 1), (by * 4 + y).min(h - 1));
                    let a = f32::from(p[3]) / 255.;
                    block[(y * 4 + x) as usize] = [f32::from(p[0]) * a, f32::from(p[1]) * a, f32::from(p[2]) * a];
                }
            }
            out.extend_from_slice(&bc1_block(&block));
        }
    }
    out
}

/// A 4x4 block with transparent pixels, in BC1's three-colour mode (colour0 <= colour1):
/// opaque pixels pick from colour0, colour1 and their midpoint, the rest take index 3,
/// which decodes as transparent black.
fn bc1_alpha_block(px: &[[f32; 3]; 16], clear: u16) -> [u8; 8] {
    let opaque: Vec<[f32; 3]> = (0..16).filter(|i| clear & (1 << i) == 0).map(|i| px[i]).collect();
    let (mut p0, mut p1, mut c0, mut c1) = (0u16, 0u16, [0f32; 3], [0f32; 3]);
    if !opaque.is_empty() {
        let (high, low) = principal_ends(&opaque);
        let ((pa, ca), (pb, cb)) = (quantize(high), quantize(low));
        ((p0, c0), (p1, c1)) = if pa <= pb { ((pa, ca), (pb, cb)) } else { ((pb, cb), (pa, ca)) };
    }
    let palette = [c0, c1, [0, 1, 2].map(|c| (c0[c] + c1[c]) / 2.)];
    let mut bits = 0u32;
    for (i, p) in px.iter().enumerate() {
        let index = if clear & (1 << i) != 0 { 3 } else {
            let mut best = (f32::MAX, 0u32);
            for (k, q) in palette.iter().enumerate() {
                let d = (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2);
                if d < best.0 { best = (d, k as u32); }
            }
            best.1
        };
        bits |= index << (2 * i);
    }
    let mut out = [0u8; 8];
    out[..2].copy_from_slice(&p0.to_le_bytes());
    out[2..4].copy_from_slice(&p1.to_le_bytes());
    out[4..].copy_from_slice(&bits.to_le_bytes());
    out
}
/// A title icon as the `icon0.dds` the PS4 home screen draws: the same header the console's
/// own icons carry (DXT1, one mip level), with a mask's transparent pixels as BC1's one-bit
/// alpha. The image must be square with a side divisible by 4.
pub(super) fn icon_dds(image: &RgbaImage) -> Vec<u8> {
    let (w, h) = image.dimensions();
    let payload = (w / 4 * (h / 4) * 8) as usize;
    let mut out = Vec::with_capacity(128 + payload);
    out.extend_from_slice(b"DDS ");
    // CAPS | HEIGHT | WIDTH | PIXELFORMAT | MIPMAPCOUNT | LINEARSIZE, one level.
    for value in [124u32, 0xa1007, h, w, payload as u32, 0, 1] { out.extend_from_slice(&value.to_le_bytes()); }
    out.extend_from_slice(&[0; 44]);
    for value in [32u32, 0x4] { out.extend_from_slice(&value.to_le_bytes()); }
    out.extend_from_slice(b"DXT1");
    out.extend_from_slice(&[0; 20]);
    // COMPLEX | TEXTURE | MIPMAP, as the console's files have it.
    for value in [0x40_1008u32, 0, 0, 0, 0] { out.extend_from_slice(&value.to_le_bytes()); }
    for by in 0..h / 4 {
        for bx in 0..w / 4 {
            let (mut block, mut clear) = ([[0f32; 3]; 16], 0u16);
            for y in 0..4 {
                for x in 0..4 {
                    let p = image.get_pixel(bx * 4 + x, by * 4 + y);
                    let i = (y * 4 + x) as usize;
                    if p[3] < 128 { clear |= 1 << i; }
                    block[i] = [f32::from(p[0]), f32::from(p[1]), f32::from(p[2])];
                }
            }
            out.extend_from_slice(&if clear == 0 { bc1_block(&block) } else { bc1_alpha_block(&block, clear) });
        }
    }
    out
}

fn random_label() -> String {
    let seed = Sha256::digest(format!("{}-{:?}", Uuid::new_v4(), SystemTime::now()));
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    format!("SSPI{}", seed.iter().take(12).map(|b| CHARS[*b as usize % CHARS.len()] as char).collect::<String>())
}
pub(super) fn valid_label(label: &str) -> bool { label.len() == 16 && label.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) }

fn locate_packager() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("SSPI_THEMEPACK_ENGINE") {
        let path = PathBuf::from(value);
        if path.is_file() { return Some(path); }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let bundled = dir.join("resources").join("themepack").join("themepack-cli.exe");
    if bundled.is_file() { return Some(bundled); }
    // Development builds run from Build-Output/Windows Manager/cargo/<profile>/.
    let dev = dir.parent()?.parent()?.join("themepack-engine").join("themepack-cli.exe");
    dev.is_file().then_some(dev)
}

struct Staged { files: usize, frames: usize, scene_bytes: usize }
fn stage_theme(stage: &Path, request: &ThemeBuildRequest) -> Result<Staged, String> {
    let write = |relative: &str, bytes: &[u8]| -> Result<(), String> {
        let path = stage.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| format!("Could not create {relative}: {e}"))?;
        std::fs::write(&path, bytes).map_err(|e| format!("Could not write {relative}: {e}"))
    };
    let home = sized("Home background", decode_png("Home background", &request.home)?, BACKGROUND)?;
    let function = match &request.function { Some(data) => sized("Function screen background", decode_png("Function screen background", data)?, BACKGROUND)?, None => home.clone() };
    let preview = match &request.preview { Some(data) => sized("Theme preview", decode_png("Theme preview", data)?, PREVIEW)?, None => home.resize_exact(PREVIEW.0, PREVIEW.1, FilterType::Lanczos3) };
    let icon0 = match &request.icon0 {
        Some(data) => sized("Theme icon", decode_png("Theme icon", data)?, ICON0)?,
        None => home.crop_imm((BACKGROUND.0 - BACKGROUND.1) / 2, 0, BACKGROUND.1, BACKGROUND.1).resize_exact(ICON0.0, ICON0.1, FilterType::Lanczos3),
    };
    write("texture/background/homescreen.png", &png("Home background", &home, BACKGROUND_BYTES)?)?;
    write("texture/background/functionscreen.png", &png("Function screen background", &function, BACKGROUND_BYTES)?)?;
    write("texture/preview.png", &png("Theme preview", &preview, ICON_BYTES)?)?;
    write("sce_sys/icon0.png", &png("Theme icon", &icon0, ICON_BYTES)?)?;
    write("sce_sys/pic1.png", &png("Home background", &home, BACKGROUND_BYTES)?)?;
    let mut files = 5;
    for (name, data) in &request.content_icons {
        if !CONTENT_ICONS.contains(&name.as_str()) { return Err(format!("Unknown system icon {name}")); }
        let label = format!("{name} icon");
        write(&format!("texture/content_icon/{name}.png"), &png(&label, &sized(&label, decode_png(&label, data)?, CONTENT_ICON)?, ICON_BYTES)?)?;
        files += 1;
    }
    for (name, icon) in &request.function_icons {
        if !FUNCTION_ICONS.contains(&name.as_str()) { return Err(format!("Unknown function icon {name}")); }
        let label = format!("{name} function icon");
        write(&format!("texture/function_icon/{name}.png"), &png(&label, &sized(&label, decode_png(&label, &icon.icon)?, FUNCTION_ICON)?, ICON_BYTES)?)?;
        write(&format!("texture/function_icon/{name}_glow.png"), &png(&label, &sized(&label, decode_png(&label, &icon.glow)?, FUNCTION_GLOW)?, ICON_BYTES)?)?;
        files += 2;
    }
    write("theme.xml", theme_xml(&request.label, &request.colors)?.as_bytes())?;
    files += 1;
    let (mut frames, mut scene_bytes) = (0, 0);
    if let Some(animation) = &request.animation {
        let size = (animation.width, animation.height);
        if !ANIMATION_SIZES.contains(&size) { return Err(format!("Animated backgrounds must be 1280x720, 960x540 or 640x360 (got {}x{})", size.0, size.1)); }
        frames = animation.frames.len();
        if !(2..=RAF_FRAMES).contains(&frames) { return Err(format!("An animated background needs 2 to {RAF_FRAMES} frames (got {frames})")); }
        let frame_bytes = 128 + (size.0 as usize / 4) * (size.1 as usize / 4) * 8;
        scene_bytes = frame_bytes * frames;
        if scene_bytes > RAF_BYTES {
            return Err(format!("{frames} frames at {}x{} take {:.2} MB; the console limit is 6 MB. Use fewer frames or a smaller size.", size.0, size.1, scene_bytes as f64 / 1_048_576.));
        }
        let decoded = animation.frames.iter().enumerate()
            .map(|(i, data)| decode_png(&format!("Frame {}", i + 1), data).and_then(|image| sized(&format!("Frame {}", i + 1), image, size)).map(|image| image.to_rgba8()))
            .collect::<Result<Vec<_>, _>>()?;
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
        let encoded: Vec<Vec<u8>> = std::thread::scope(|scope| {
            let chunk = decoded.len().div_ceil(threads);
            let workers: Vec<_> = decoded.chunks(chunk).map(|part| scope.spawn(move || part.iter().map(dds_dxt1).collect::<Vec<_>>())).collect();
            workers.into_iter().flat_map(|worker| worker.join().unwrap_or_default()).collect()
        });
        if encoded.len() != frames { return Err("Animated background compression failed".into()); }
        for (i, dds) in encoded.iter().enumerate() { write(&format!("scene/background.raf/{i:02}.dds"), dds)?; }
        let plane: Vec<u8> = (0..PLANE_MDX.len()).step_by(2).map(|i| u8::from_str_radix(&PLANE_MDX[i..i + 2], 16).unwrap()).collect();
        write("scene/background.raf/plane.mdx", &plane)?;
        write("scene/background.raf/index.xml", raf_scene(frames, animation.wait).as_bytes())?;
        files += frames + 2;
    }
    Ok(Staged { files, frames, scene_bytes })
}

#[tauri::command]
pub(super) async fn build_ps4_theme(app: AppHandle, request: ThemeBuildRequest) -> Result<ThemeBuildResult, String> {
    let title: String = request.title.trim().chars().filter(|c| !c.is_control()).collect();
    if title.is_empty() || title.len() > 127 { return Err("Give the theme a name (up to 127 bytes)".into()); }
    let label = if valid_label(&request.label) { request.label.clone() } else { random_label() };
    let content_id = format!("UP9000-CUSA00000_00-{label}");
    let packager = locate_packager().ok_or("The theme packager is missing. Reinstall SSPI Windows (resources/themepack).")?;
    let root = app.path().app_local_data_dir().map_err(|e| e.to_string())?.join("themes").join(&label);
    let request = ThemeBuildRequest { label: label.clone(), ..request };
    let stage = root.join("stage");
    let staged = {
        let stage = stage.clone();
        tokio::task::spawn_blocking(move || {
            if stage.exists() { std::fs::remove_dir_all(&stage).map_err(|e| format!("Could not clear the previous theme build: {e}"))?; }
            stage_theme(&stage, &request)
        }).await.map_err(|e| e.to_string())??
    };
    let output = root.join(format!("{content_id}.pkg"));
    let mut command = tokio::process::Command::new(&packager);
    crate::fpkg::use_bundled_dotnet(&mut command);
    command.arg("build").arg(&stage).arg(&output).arg("--content-id").arg(&content_id).arg("--title").arg(&title)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let run = tokio::time::timeout(Duration::from_secs(180), command.output()).await
        .map_err(|_| "The theme packager did not finish within 3 minutes".to_string())?
        .map_err(|e| format!("Could not start the theme packager: {e}"))?;
    if !run.status.success() {
        let error = String::from_utf8_lossy(&run.stderr).trim().to_string();
        let runtime = error.contains("You must install") || error.contains(".NET") || run.status.code() == Some(-2147450730);
        return Err(if runtime { "Building PS4 themes needs the .NET 9 runtime (or newer). Install it from Microsoft, then try again.".into() }
            else { format!("The theme packager failed: {}", if error.is_empty() { "no details" } else { &error }) });
    }
    let reply: Value = serde_json::from_slice(&run.stdout).map_err(|_| "The theme packager returned an unreadable result".to_string())?;
    if reply["ok"] != true { return Err("The theme packager did not confirm the package".into()); }
    let size = std::fs::metadata(&output).map_err(|_| "The built theme package is missing".to_string())?.len();
    Ok(ThemeBuildResult { path: output.to_string_lossy().into_owned(), size, content_id, title, frames: staged.frames, scene_bytes: staged.scene_bytes, files: staged.files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn decode_block(block: &[u8]) -> [[u8; 3]; 16] {
        let p0 = u16::from_le_bytes([block[0], block[1]]);
        let p1 = u16::from_le_bytes([block[2], block[3]]);
        let bits = u32::from_le_bytes(block[4..8].try_into().unwrap());
        let rgb = |p: u16| { let (r, g, b) = ((p >> 11) & 31, (p >> 5) & 63, p & 31); [f32::from((r << 3) | (r >> 2)), f32::from((g << 2) | (g >> 4)), f32::from((b << 3) | (b >> 2))] };
        let (c0, c1) = (rgb(p0), rgb(p1));
        let pal = if p0 > p1 { [c0, c1, [0, 1, 2].map(|c| (2. * c0[c] + c1[c]) / 3.), [0, 1, 2].map(|c| (c0[c] + 2. * c1[c]) / 3.)] }
            else { [c0, c1, [0, 1, 2].map(|c| (c0[c] + c1[c]) / 2.), [0.; 3]] };
        std::array::from_fn(|i| pal[((bits >> (2 * i)) & 3) as usize].map(|v| v.round() as u8))
    }

    /// Pixel i of a BC1 block is transparent when the block is in three-colour mode and uses index 3.
    fn transparent(block: &[u8], i: usize) -> bool {
        let (p0, p1) = (u16::from_le_bytes([block[0], block[1]]), u16::from_le_bytes([block[2], block[3]]));
        p0 <= p1 && (u32::from_le_bytes(block[4..8].try_into().unwrap()) >> (2 * i)) & 3 == 3
    }

    #[test]
    fn theme_pngs_use_small_idat_chunks_and_fit_the_budget() {
        // Glow-like gradients with the low-bit noise canvas blurs leave behind: hard for PNG until posterized.
        let mut seed = 7u32;
        let noisy = RgbaImage::from_fn(512, 512, |x, y| {
            let mut n = || { seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223); (seed >> 30) as u8 };
            let (dx, dy) = (x as f32 - 256., y as f32 - 256.);
            let glow = (255. * (-(dx * dx + dy * dy) / 30000.).exp()) as u8;
            image::Rgba([glow.saturating_add(n()), ((x / 2) as u8).saturating_add(n()), ((y / 2) as u8).saturating_add(n()), 200u8.saturating_add(n())])
        });
        let image = DynamicImage::ImageRgba8(noisy);
        let raw = encode_png(&image).unwrap();
        assert!(raw.len() > ICON_BYTES, "the fixture should start over budget ({})", raw.len());
        let bytes = png("Test tile", &image, ICON_BYTES).unwrap();
        assert!(bytes.len() <= ICON_BYTES, "{}", bytes.len());
        let (mut at, mut idat) = (8usize, 0);
        while at + 12 <= bytes.len() {
            let length = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let body = &bytes[at + 4..at + 8 + length];
            assert_eq!(crc32(body), u32::from_be_bytes(bytes[at + 8 + length..at + 12 + length].try_into().unwrap()));
            if &bytes[at + 4..at + 8] == b"IDAT" { idat += 1; assert!(length <= IDAT_CHUNK); }
            at += 12 + length;
        }
        assert!(idat > 1);
        let decoded = image::load_from_memory_with_format(&bytes, ImageFormat::Png).unwrap();
        assert_eq!(decoded.dimensions(), (512, 512));
        assert!(png("Test tile", &image, 1024).unwrap_err().contains("under 1 KB"));
    }

    #[test]
    fn icon_dds_matches_the_console_header_and_keeps_the_mask() {
        // The first 32 header bytes of a retail icon0.dds (512x512 DXT1, one level).
        let stock = "444453207c00000007100a000002000000020000000002000000000001000000";
        let mut image = RgbaImage::from_pixel(512, 512, image::Rgba([230, 40, 120, 255]));
        for (x, y, p) in image.enumerate_pixels_mut() {
            if (x as f32 - 255.5).hypot(y as f32 - 255.5) > 250. { *p = image::Rgba([0, 0, 0, 0]); }
        }
        let dds = icon_dds(&image);
        assert_eq!(dds.len(), 131_200);
        assert_eq!(dds[..32].iter().map(|b| format!("{b:02x}")).collect::<String>(), stock);
        assert_eq!(&dds[76..88], &[32, 0, 0, 0, 4, 0, 0, 0, b'D', b'X', b'T', b'1']);
        assert_eq!(&dds[108..112], &0x40_1008u32.to_le_bytes());
        let block = |bx: usize, by: usize| &dds[128 + (by * 128 + bx) * 8..][..8];
        // A corner block is fully clear, the centre is opaque and keeps its colour, and an edge
        // block mixes both pixel by pixel.
        assert!((0..16).all(|i| transparent(block(0, 0), i)));
        assert!((0..16).all(|i| !transparent(block(64, 64), i)));
        for got in decode_block(block(64, 64)) { assert!((i32::from(got[0]) - 230).abs() <= 8 && (i32::from(got[1]) - 40).abs() <= 8 && (i32::from(got[2]) - 120).abs() <= 8); }
        for by in 0..128usize { for bx in 0..128usize { for i in 0..16 {
            let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
            assert_eq!(transparent(block(bx, by), i), image.get_pixel(x as u32, y as u32)[3] < 128, "pixel {x},{y}");
        } } }
        // Clear blocks decode their opaque pixels from the opaque colours only.
        let edge = (0..128).find(|&bx| { let b = block(bx, 64); (0..16).any(|i| transparent(b, i)) && (0..16).any(|i| !transparent(b, i)) }).unwrap();
        for (i, got) in decode_block(block(edge, 64)).iter().enumerate() {
            if !transparent(block(edge, 64), i) { assert!((i32::from(got[0]) - 230).abs() <= 8 && (i32::from(got[2]) - 120).abs() <= 8); }
        }
    }

    #[test]
    fn scene_matches_the_verified_generator() {
        // SHA-256 of raf.build_scene(["00.dds", "01.dds", "02.dds"], 0.1) from PS4 Ultimate Theme Creator.
        assert_eq!(sha256_hex(raf_scene(3, 0.1).as_bytes()), "df47246616f4685f5b556fa90367a5441467d5d20962ecf1ca3e9aa020f2d6a3");
        assert!(raf_scene(24, 0.06).contains("<case wait=\"0.06\">"));
        assert!(raf_scene(2, 1.0).contains("<case wait=\"1\">"));
    }

    #[test]
    fn plane_model_is_the_verified_quad() {
        let plane: Vec<u8> = (0..PLANE_MDX.len()).step_by(2).map(|i| u8::from_str_radix(&PLANE_MDX[i..i + 2], 16).unwrap()).collect();
        assert_eq!(plane.len(), 420);
        assert!(plane.starts_with(b"XDM.00.1MSP\0\0\0\0\0"));
        assert_eq!(sha256_hex(&plane), "2bf819275efa411ab87a95967797d856ce8c209980cb288921e95c1aa68585e2");
    }

    #[test]
    fn dxt1_is_a_valid_opaque_dds() {
        let mut image = RgbaImage::new(10, 6);
        for (x, y, p) in image.enumerate_pixels_mut() { *p = image::Rgba([(x * 25) as u8, (y * 40) as u8, 200, 255]); }
        let dds = dds_dxt1(&image);
        assert_eq!(&dds[..4], b"DDS ");
        assert_eq!(&dds[84..88], b"DXT1");
        assert_eq!(u32::from_le_bytes(dds[12..16].try_into().unwrap()), 6);
        assert_eq!(u32::from_le_bytes(dds[16..20].try_into().unwrap()), 10);
        assert_eq!(dds.len(), 128 + 3 * 2 * 8);
        // A two-way gradient cannot be exact in four colours; the average stays close.
        let mut total = 0i64;
        for (index, block) in dds[128..].chunks(8).enumerate() {
            let (bx, by) = ((index % 3) as u32, (index / 3) as u32);
            // Opaque blocks must stay in four-colour mode (or be flat).
            assert!(u16::from_le_bytes([block[0], block[1]]) >= u16::from_le_bytes([block[2], block[3]]));
            for (i, got) in decode_block(block).iter().enumerate() {
                let p = image.get_pixel((bx * 4 + i as u32 % 4).min(9), (by * 4 + i as u32 / 4).min(5));
                for c in 0..3 { total += i64::from((i32::from(got[c]) - i32::from(p[c])).abs()); }
            }
        }
        assert!(total / (6 * 16 * 3) <= 12, "mean error {}", total / (6 * 16 * 3));
        // A one-way gradient lies on the block axis and stays within a few levels.
        let mut ramp = RgbaImage::new(16, 4);
        for (x, _, p) in ramp.enumerate_pixels_mut() { *p = image::Rgba([(x * 16) as u8, 90, 30, 255]); }
        for (index, block) in dds_dxt1(&ramp)[128..].chunks(8).enumerate() {
            for (i, got) in decode_block(block).iter().enumerate() {
                let want = (index as i32 * 4 + i as i32 % 4) * 16;
                assert!((i32::from(got[0]) - want).abs() <= 12, "ramp block {index} pixel {i}: {got:?}");
                assert!((i32::from(got[1]) - 90).abs() <= 4 && (i32::from(got[2]) - 30).abs() <= 6);
            }
        }
        // A flat block encodes exactly.
        let flat = RgbaImage::from_pixel(4, 4, image::Rgba([16, 120, 240, 255]));
        let block = &dds_dxt1(&flat)[128..];
        for got in decode_block(block) { for (c, want) in [16u8, 120, 240].iter().enumerate() { assert!((i32::from(got[c]) - i32::from(*want)).abs() <= 4); } }
    }

    #[test]
    fn theme_xml_validates_colours() {
        let colors = ThemeColors { theme_color: 0, font: "#ffffffff".into(), font_shadow: "#FF000000".into(), focus: "#FF1E9BFF".into(),
            home_dimmer: "#00FFFFFF".into(), function_dimmer: "#00FFFFFF".into(), title_dimmer: "#00FFFFFF".into() };
        let xml = theme_xml("SSPIABCDEFGH1234", &colors).unwrap();
        assert!(xml.contains("<format-version>4.0</format-version>") && xml.contains("<fontcolor>#FFFFFFFF</fontcolor>"));
        assert!(xml.contains("<label>sspiabcdefgh1234</label>") && xml.contains("<homebgm-enable>False</homebgm-enable>"));
        assert!(theme_xml("X", &ThemeColors { focus: "#123".into(), ..colors.clone() }).is_err());
        assert!(theme_xml("X", &ThemeColors { theme_color: 8, ..colors }).is_err());
    }

    fn b64png(image: &RgbaImage) -> String {
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image.clone()).write_to(&mut out, ImageFormat::Png).unwrap();
        BASE64.encode(out.into_inner())
    }
    fn wallpaper(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let glow = (-((u - 0.25).powi(2) + (v - 1.05).powi(2)) * 3.0).exp();
            image::Rgba([(10. + 40. * glow) as u8, (18. + 60. * glow + 20. * (1. - v)) as u8, (60. + 120. * glow + 50. * (1. - v)) as u8, 255])
        })
    }
    fn bubble(frame: &mut RgbaImage, cx: f32, cy: f32, r: f32, alpha: f32) {
        let (w, h) = frame.dimensions();
        let (x0, x1) = ((cx - r - 2.).max(0.) as u32, ((cx + r + 2.) as u32).min(w - 1));
        let (y0, y1) = ((cy - r - 2.).max(0.) as u32, ((cy + r + 2.) as u32).min(h - 1));
        for y in y0..=y1 { for x in x0..=x1 {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / r;
            if d > 1.05 { continue; }
            // Soft rim plus a faint fill and a highlight, like a soap bubble.
            let rim = (1. - ((d - 0.92).abs() / 0.1)).clamp(0., 1.);
            let hl = (1. - (((x as f32 - (cx - r * 0.35)).powi(2) + (y as f32 - (cy - r * 0.35)).powi(2)).sqrt() / (r * 0.28))).clamp(0., 1.);
            let a = (0.08 + 0.65 * rim + 0.7 * hl).min(1.) * alpha * if d > 1. { (1.05 - d) / 0.05 } else { 1. };
            let p = frame.get_pixel_mut(x, y);
            for c in 0..3 { p[c] = (f32::from(p[c]) * (1. - a) + [200., 230., 255.][c] * a) as u8; }
        } }
    }

    /// Writes a complete animated sample theme stage for console testing:
    /// SSPI_THEME_SAMPLE_DIR=<dir> cargo test --lib export_sample_theme -- --ignored
    #[test]
    #[ignore]
    fn export_sample_theme() {
        let dir = PathBuf::from(std::env::var("SSPI_THEME_SAMPLE_DIR").expect("set SSPI_THEME_SAMPLE_DIR"));
        let home = wallpaper(1920, 1080);
        let (fw, fh, frames) = (960u32, 540u32, 24usize);
        let base = wallpaper(fw, fh);
        // Each bubble rises a fixed distance per loop and fades at both ends,
        // so frame 23 flows into frame 0 without a jump.
        let bubbles: Vec<(f32, f32, f32, f32, f32)> = (0..34).map(|i| {
            let f = i as f32;
            ((f * 97.13) % fw as f32, 120. + (f * 53.7) % (fh as f32 - 60.), 6. + (f * 7.3) % 22., (f * 0.137) % 1., 40. + (f * 13.) % 70.)
        }).collect();
        let animation = (0..frames).map(|n| {
            let mut frame = base.clone();
            for &(x, y, r, phase, rise) in &bubbles {
                let t = (n as f32 / frames as f32 + phase) % 1.;
                let alpha = (t * 4.).min((1. - t) * 4.).min(1.);
                bubble(&mut frame, x + (t * std::f32::consts::TAU).sin() * 6., y - rise * t, r, alpha);
            }
            b64png(&frame)
        }).collect();
        let mut globe = RgbaImage::new(512, 512);
        for (x, y, p) in globe.enumerate_pixels_mut() {
            let d = ((x as f32 - 256.).powi(2) + (y as f32 - 256.).powi(2)).sqrt();
            if d <= 236. {
                let line = (d - 150.).abs() < 9. || (x as f32 - 256.).abs() < 7. || (y as f32 - 256.).abs() < 7. || ((x as f32 - 256.).abs() / 0.55 + 0.).hypot(y as f32 - 256.) < 158. && ((x as f32 - 256.).abs() / 0.55).hypot(y as f32 - 256.) > 142.;
                *p = if line && d < 170. { image::Rgba([255, 255, 255, 255]) } else { image::Rgba([30, 110, 235, 255]) };
            }
        }
        let request = ThemeBuildRequest {
            title: "SSPI Bubbles".into(), label: "SSPIBUBBLES00001".into(), home: b64png(&home), function: None, preview: None, icon0: None,
            content_icons: BTreeMap::from([("browser".to_string(), b64png(&globe))]), function_icons: BTreeMap::new(),
            colors: ThemeColors { theme_color: 0, font: "#FFFFFFFF".into(), font_shadow: "#FF000000".into(), focus: "#FF7FD4FF".into(),
                home_dimmer: "#00FFFFFF".into(), function_dimmer: "#00FFFFFF".into(), title_dimmer: "#00FFFFFF".into() },
            animation: Some(ThemeAnimation { width: fw, height: fh, wait: 0.1, frames: animation }),
        };
        if dir.exists() { std::fs::remove_dir_all(&dir).unwrap(); }
        let staged = stage_theme(&dir, &request).unwrap();
        assert_eq!(staged.frames, 24);
        assert!(staged.scene_bytes <= RAF_BYTES);
        println!("staged {} files, {} scene bytes", staged.files, staged.scene_bytes);
    }

    #[test]
    fn labels_are_sixteen_upper_alphanumerics() {
        let label = random_label();
        assert!(valid_label(&label) && label.starts_with("SSPI"));
        assert!(!valid_label("sspi0000000000000") && !valid_label("SSPI-00000000000") && !valid_label("SSPI"));
    }
}
