//! Pure parsers and naming helpers for the SSPI PS4 FTP inbox protocol.
//!
//! This module intentionally uses only the Rust standard library so the wire
//! formats can also be tested directly with `rustc --test`.
#![allow(dead_code)]

const FNV1A64_OFFSET: u64 = 0xcbf29ce484222325;
const FNV1A64_PRIME: u64 = 0x100000001b3;
const MAX_INBOX_NAME_BYTES: usize = 240;

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A64_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV1A64_PRIME);
    }
    hash
}

pub fn claim_file_name(name: &str, size: u64, mtime: i64) -> String {
    format!("{:016x}-{size}-{mtime}.claim", fnv1a64(name.as_bytes()))
}

pub fn claim_prefix(name: &str) -> String {
    format!("{:016x}-", fnv1a64(name.as_bytes()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxKind {
    Primary,
    Continuation,
}

fn ends_with_ascii_case_insensitive(name: &[u8], suffix: &[u8]) -> bool {
    name.len() > suffix.len()
        && name[name.len() - suffix.len()..]
            .eq_ignore_ascii_case(suffix)
}

fn ascii_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

// Parse the prefix the same way C strtol(..., 10) does for the ASCII input
// bytes used in filenames. The returned offset includes leading whitespace
// and an optional sign, as strtol's end pointer does.
fn parse_c_decimal_prefix(input: &[u8]) -> Option<(usize, i64)> {
    let mut at = 0;
    while at < input.len() && ascii_whitespace(input[at]) {
        at += 1;
    }
    let number_start = at;
    if at < input.len() && matches!(input[at], b'+' | b'-') {
        at += 1;
    }
    let digits_start = at;
    while at < input.len() && input[at].is_ascii_digit() {
        at += 1;
    }
    if at == digits_start {
        return None;
    }
    let number = std::str::from_utf8(&input[number_start..at]).ok()?.parse().ok()?;
    Some((at, number))
}

// Returns the byte offset of the `.part` marker and its parsed volume number.
// The caller first establishes a `.rar` suffix, just as gs_inbox_kind does.
fn part_rar_suffix(name: &[u8]) -> Option<(usize, i64)> {
    if !ends_with_ascii_case_insensitive(name, b".rar") || name.len() < 5 {
        return None;
    }
    let last_candidate = name.len() - 5;
    if last_candidate < 5 {
        return None;
    }
    for number_start in (5..=last_candidate).rev() {
        let marker_start = number_start - 5;
        if !name[marker_start..number_start].eq_ignore_ascii_case(b".part") {
            continue;
        }
        let Some((consumed, number)) = parse_c_decimal_prefix(&name[number_start..]) else {
            continue;
        };
        if consumed == name.len() - 4 - number_start && consumed <= 6 {
            return Some((marker_start, number));
        }
    }
    None
}

pub fn inbox_kind(name: &str) -> Option<InboxKind> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_INBOX_NAME_BYTES || bytes[0] == b'.' {
        return None;
    }

    for suffix in [b".pkg".as_slice(), b".zip", b".7z", b".7zip"] {
        if ends_with_ascii_case_insensitive(bytes, suffix) {
            return Some(InboxKind::Primary);
        }
    }

    if ends_with_ascii_case_insensitive(bytes, b".rar") {
        if let Some((_, number)) = part_rar_suffix(bytes) {
            return match number {
                1 => Some(InboxKind::Primary),
                n if n > 1 => Some(InboxKind::Continuation),
                _ => None,
            };
        }
        return Some(InboxKind::Primary);
    }

    if bytes.len() > 4
        && bytes[bytes.len() - 4] == b'.'
        && (bytes[bytes.len() - 3] | 0x20) >= b'r'
        && (bytes[bytes.len() - 3] | 0x20) <= b'z'
        && bytes[bytes.len() - 2].is_ascii_digit()
        && bytes[bytes.len() - 1].is_ascii_digit()
    {
        return Some(InboxKind::Continuation);
    }
    None
}

pub fn set_key(name: &str) -> Option<(String, char)> {
    let kind = inbox_kind(name)?;
    let bytes = name.as_bytes();
    let (stem_end, style) = if ends_with_ascii_case_insensitive(bytes, b".rar") {
        if let Some((marker_start, _)) = part_rar_suffix(bytes) {
            (marker_start, 'P')
        } else {
            (bytes.len() - 4, 'R')
        }
    } else if kind == InboxKind::Continuation {
        (bytes.len() - 4, 'R')
    } else {
        (bytes.len(), 'F')
    };
    Some((name[..stem_end].to_ascii_lowercase(), style))
}

pub fn validate_final_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("File name cannot be empty.".to_owned());
    }
    if name.len() > MAX_INBOX_NAME_BYTES {
        return Err("File name is too long for the PS4 inbox.".to_owned());
    }
    if name.starts_with('.') {
        return Err("File name cannot start with a dot.".to_owned());
    }
    if name.chars().any(|ch| {
        ch.is_control() || matches!(ch, '/' | '\\' | ':')
    }) {
        return Err("File name contains an invalid character.".to_owned());
    }
    if inbox_kind(name).is_none() {
        return Err("File type is not supported by the PS4 inbox.".to_owned());
    }
    Ok(())
}

fn split_supported_suffix(original: &str) -> Option<(&str, &str, InboxKind)> {
    let bytes = original.as_bytes();
    if let Some((marker_start, number)) = part_rar_suffix(bytes) {
        let kind = match number {
            1 => InboxKind::Primary,
            n if n > 1 => InboxKind::Continuation,
            _ => return None,
        };
        return Some((&original[..marker_start], &original[marker_start..], kind));
    }

    for suffix in [b".rar".as_slice(), b".zip", b".7zip", b".7z", b".pkg"] {
        if ends_with_ascii_case_insensitive(bytes, suffix) {
            let start = bytes.len() - suffix.len();
            let kind = InboxKind::Primary;
            return Some((&original[..start], &original[start..], kind));
        }
    }

    if bytes.len() > 4
        && bytes[bytes.len() - 4] == b'.'
        && (bytes[bytes.len() - 3] | 0x20) >= b'r'
        && (bytes[bytes.len() - 3] | 0x20) <= b'z'
        && bytes[bytes.len() - 2].is_ascii_digit()
        && bytes[bytes.len() - 1].is_ascii_digit()
    {
        return Some((
            &original[..bytes.len() - 4],
            &original[bytes.len() - 4..],
            InboxKind::Continuation,
        ));
    }
    None
}

pub fn final_name(original: &str, tag: &str, generation: u32) -> Result<String, String> {
    if tag.is_empty() || tag.len() > 16 || !tag.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err("Tag must contain 1 to 16 ASCII letters or digits.".to_owned());
    }
    let Some((stem, suffix, expected_kind)) = split_supported_suffix(original) else {
        return Err(format!("Unsupported file type for the PS4 inbox: {original}"));
    };

    let mut safe_stem = String::with_capacity(stem.len());
    for ch in stem.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            safe_stem.push('_');
        } else {
            safe_stem.push(ch);
        }
    }
    safe_stem = safe_stem
        .trim_start_matches(|ch| ch == '.' || ch == ' ')
        .trim_end_matches(|ch| ch == ' ' || ch == '.')
        .to_owned();
    if safe_stem.is_empty() {
        safe_stem.push_str("package");
    }

    let marker = if generation == 0 {
        format!("-{tag}")
    } else {
        format!("-{tag}-{generation}")
    };
    let max_stem_bytes = MAX_INBOX_NAME_BYTES.saturating_sub(marker.len() + suffix.len());
    while safe_stem.len() > max_stem_bytes {
        let Some((boundary, _)) = safe_stem.char_indices().next_back() else {
            break;
        };
        safe_stem.truncate(boundary);
    }
    if safe_stem.is_empty() {
        safe_stem.push_str("package");
    }

    let result = format!("{safe_stem}{marker}{suffix}");
    validate_final_name(&result)?;
    if inbox_kind(&result) != Some(expected_kind) {
        return Err("Generated file name does not match its archive volume.".to_owned());
    }
    Ok(result)
}

pub fn temp_name(final_name: &str) -> String {
    format!(".{final_name}.uploading")
}

pub fn receipt_name(job: &str) -> String {
    format!("installed-{job}.txt")
}

pub fn b64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        encoded.push(ALPHABET[(first >> 2) as usize] as char);
        encoded.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            encoded.push(ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(ALPHABET[(third & 0x3f) as usize] as char);
        } else {
            encoded.push('=');
        }
    }
    encoded
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(Vec::new());
    }
    let bytes = text.as_bytes();
    let data_len = bytes.iter().position(|byte| *byte == b'=').unwrap_or(bytes.len());
    let padding = bytes.len() - data_len;
    if padding > 2 || bytes[data_len..].iter().any(|byte| *byte != b'=') {
        return None;
    }
    match padding {
        0 if data_len % 4 == 1 => return None,
        1 if bytes.len() % 4 != 0 || data_len % 4 != 3 => return None,
        2 if bytes.len() % 4 != 0 || data_len % 4 != 2 => return None,
        _ => {}
    }

    let mut values = Vec::with_capacity(data_len);
    for byte in &bytes[..data_len] {
        values.push(base64_value(*byte)?);
    }
    let mut decoded = Vec::with_capacity(data_len * 3 / 4);
    let mut at = 0;
    while at < values.len() {
        let remain = values.len() - at;
        let a = values[at];
        let b = *values.get(at + 1)?;
        decoded.push((a << 2) | (b >> 4));
        if remain > 2 {
            let c = values[at + 2];
            decoded.push((b << 4) | (c >> 2));
            if remain > 3 {
                let d = values[at + 3];
                decoded.push((c << 6) | d);
            }
        }
        at += 4;
    }
    Some(decoded)
}

pub fn names_job(text: &str, job: &str) -> bool {
    !job.is_empty() && (text.contains(job) || text.contains(&b64_encode(job.as_bytes())))
}

fn protocol_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
    text.split_terminator('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

fn decoded_text(text: &str) -> Option<String> {
    Some(String::from_utf8_lossy(&b64_decode(text)?).into_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heartbeat {
    pub version: String,
    pub dotnet_ticks: i64,
    pub caps: Vec<(String, String)>,
}

impl Heartbeat {
    pub fn cap(&self, key: &str) -> Option<&str> {
        self.caps
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    pub fn unix_seconds(&self) -> i64 {
        self.dotnet_ticks
            .saturating_sub(621_355_968_000_000_000)
            / 10_000_000
    }

    pub fn inbox_ready(&self) -> bool {
        self.cap("ftpinbox") == Some("1")
            && self.cap("transfer") == Some("1")
            && self.cap("bgft") == Some("1")
    }
}

pub fn parse_heartbeat(text: &str) -> Option<Heartbeat> {
    let lines = protocol_lines(text);
    let version = *lines.first()?;
    if version.is_empty() {
        return None;
    }
    let dotnet_ticks = lines.get(1)?.parse().ok()?;
    let mut caps = Vec::new();
    for token in lines.get(2)?.split_whitespace() {
        if let Some((key, value)) = token.split_once('=') {
            caps.push((key.to_owned(), value.to_owned()));
        }
    }
    Some(Heartbeat {
        version: version.to_owned(),
        dotnet_ticks,
        caps,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub owner: String,
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    pub job: String,
    pub state: String,
    pub count: Option<u64>,
    pub total: Option<u64>,
}

pub fn parse_claim(text: &str) -> Option<Claim> {
    let lines = protocol_lines(text);
    Some(Claim {
        owner: lines.first()?.to_string(),
        name: lines.get(1)?.to_string(),
        size: lines.get(2)?.parse().ok()?,
        mtime: lines.get(3)?.parse().ok()?,
        job: lines.get(4).copied().unwrap_or("").to_owned(),
        state: lines.get(5).copied().unwrap_or("").to_owned(),
        count: match lines.get(6) {
            Some(line) => Some(line.parse().ok()?),
            None => None,
        },
        total: match lines.get(7) {
            Some(line) => Some(line.parse().ok()?),
            None => None,
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageStatus {
    pub job: String,
    pub state: String,
    pub done: i64,
    pub total: i64,
    pub error: String,
    pub active: i64,
    pub auto_install: bool,
    pub wire_bytes: i64,
    pub generation: String,
    pub build: String,
}

fn optional_i64(lines: &[&str], index: usize) -> Option<i64> {
    match lines.get(index) {
        Some(line) => line.parse().ok(),
        None => Some(0),
    }
}

fn optional_string(lines: &[&str], index: usize) -> String {
    lines.get(index).copied().unwrap_or("").to_owned()
}

pub fn parse_stage_status(text: &str) -> Option<StageStatus> {
    let lines = protocol_lines(text);
    if *lines.first()? != "1" || lines.len() < 7 {
        return None;
    }
    Some(StageStatus {
        job: decoded_text(lines[1])?,
        state: lines[2].to_owned(),
        done: lines[3].parse().ok()?,
        total: lines[4].parse().ok()?,
        error: decoded_text(lines[6])?,
        active: optional_i64(&lines, 7)?,
        auto_install: lines.get(8).is_some_and(|line| *line == "1"),
        wire_bytes: optional_i64(&lines, 9)?,
        generation: optional_string(&lines, 10),
        build: optional_string(&lines, 11),
    })
}

pub fn parse_stage_job_id(text: &str) -> Option<String> {
    let lines = protocol_lines(text);
    if lines.first()?.parse::<i64>().ok()? < 1 {
        return None;
    }
    decoded_text(lines.get(1)?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingletonStatus {
    pub job: String,
    pub state: String,
}

pub fn parse_singleton_status(text: &str) -> Option<SingletonStatus> {
    let lines = protocol_lines(text);
    if *lines.first()? != "1" {
        return None;
    }
    Some(SingletonStatus {
        job: decoded_text(lines.get(1)?)?,
        state: lines.get(2)?.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReceipt {
    pub job: String,
    pub content_id: String,
    pub total: i64,
    pub generation: String,
}

pub fn parse_install_receipt(text: &str) -> Option<InstallReceipt> {
    let lines = protocol_lines(text);
    if *lines.first()? != "1" {
        return None;
    }
    Some(InstallReceipt {
        job: decoded_text(lines.get(1)?)?,
        content_id: decoded_text(lines.get(2)?)?,
        total: lines.get(3)?.parse().ok()?,
        generation: lines.get(4)?.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    pub name: String,
    pub size: Option<u64>,
    pub dir: bool,
}

fn dot_entry(name: &str) -> bool {
    name == "." || name == ".."
}

fn parse_mlsd(line: &str, split_at: usize) -> Option<ListEntry> {
    let facts = &line[..split_at];
    let name = &line[split_at + 2..];
    let mut entry_type = None;
    let mut size = None;
    for fact in facts.split(';') {
        if fact.is_empty() {
            continue;
        }
        let (key, value) = fact.split_once('=')?;
        if key.eq_ignore_ascii_case("type") {
            entry_type = Some(value);
        } else if key.eq_ignore_ascii_case("size") {
            size = Some(value.parse::<u64>().ok()?);
        }
    }
    if dot_entry(name) {
        return None;
    }
    let dir = match entry_type?.to_ascii_lowercase().as_str() {
        "file" => false,
        "dir" => true,
        "cdir" | "pdir" => return None,
        _ => return None,
    };
    Some(ListEntry {
        name: name.to_owned(),
        size,
        dir,
    })
}

fn unix_fields_and_name(line: &str) -> Option<(Vec<&str>, &str)> {
    let bytes = line.as_bytes();
    let mut fields = Vec::with_capacity(8);
    let mut at = 0;
    while fields.len() < 8 {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if at == bytes.len() {
            return None;
        }
        let start = at;
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(&line[start..at]);
    }
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    if at == bytes.len() {
        return None;
    }
    Some((fields, &line[at..]))
}

pub fn parse_list_line(line: &str) -> Option<ListEntry> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.trim_start().starts_with("total") {
        return None;
    }
    if let Some(split_at) = line.find("; ") {
        return parse_mlsd(line, split_at);
    }

    let (fields, raw_name) = unix_fields_and_name(line)?;
    let first = fields.first()?.as_bytes().first().copied()?;
    let dir = match first {
        b'd' => true,
        b'l' | b'-' => false,
        _ => return None,
    };
    let size = fields.get(4)?.parse::<u64>().ok()?;
    let name = if first == b'l' {
        raw_name.split_once(" -> ").map_or(raw_name, |(name, _)| name)
    } else {
        raw_name
    };
    if name.is_empty() || dot_entry(name) {
        return None;
    }
    Some(ListEntry {
        name: name.to_owned(),
        size: Some(size),
        dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_and_claim_names_match_protocol() {
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a64(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
        assert_eq!(claim_file_name("a", 12, -3), "af63dc4c8601ec8c-12--3.claim");
        assert_eq!(claim_prefix("a"), "af63dc4c8601ec8c-");
    }

    #[test]
    fn inbox_kinds_follow_console_name_rules() {
        use InboxKind::{Continuation as C, Primary as P};
        let cases = [
            ("a.pkg", Some(P)),
            ("A.PKG", Some(P)),
            (".a.pkg", None),
            ("a.pkg.uploading", None),
            (".pkg", None),
            ("x.part1.rar", Some(P)),
            ("x.part01.rar", Some(P)),
            ("x.part2.rar", Some(C)),
            ("x.part0.rar", None),
            ("x.part1234567.rar", Some(P)),
            ("x.rar", Some(P)),
            ("x.r00", Some(C)),
            ("x.S12", Some(C)),
            ("x.q00", None),
        ];
        for (name, expected) in cases {
            assert_eq!(inbox_kind(name), expected, "{name}");
        }
        assert_eq!(inbox_kind(&format!("{}.pkg", "a".repeat(236))), Some(P));
        assert_eq!(inbox_kind(&format!("{}.pkg", "a".repeat(237))), None);
        assert_eq!(inbox_kind("x.part 1.rar"), Some(P));
        assert_eq!(inbox_kind("x.part+2.rar"), Some(C));
        assert_eq!(inbox_kind("x.part-1.rar"), None);
        assert_eq!(inbox_kind("x.part123456.rar"), Some(C));
    }

    #[test]
    fn set_keys_group_all_rar_volume_styles() {
        assert_eq!(set_key("Game.part1.rar"), Some(("game".to_owned(), 'P')));
        assert_eq!(set_key("game.PART2.rar"), Some(("game".to_owned(), 'P')));
        assert_eq!(set_key("Game.rar"), Some(("game".to_owned(), 'R')));
        assert_eq!(set_key("Game.r00"), Some(("game".to_owned(), 'R')));
        assert_eq!(set_key("Game.pkg"), Some(("game.pkg".to_owned(), 'F')));
        assert_eq!(set_key("x.part0.rar"), None);
    }

    #[test]
    fn names_are_validated_and_tagged_without_breaking_volume_kind() {
        assert_eq!(
            final_name("Game.part1.rar", "1a2b3c4d", 0).unwrap(),
            "Game-1a2b3c4d.part1.rar"
        );
        assert_eq!(
            final_name("Game.part1.rar", "1a2b3c4d", 2).unwrap(),
            "Game-1a2b3c4d-2.part1.rar"
        );
        let continuation = final_name("Game.r05", "1a2b3c4d", 0).unwrap();
        assert_eq!(continuation, "Game-1a2b3c4d.r05");
        assert_eq!(inbox_kind(&continuation), Some(InboxKind::Continuation));
        assert_eq!(
            final_name(".hidden.pkg", "1a2b3c4d", 0).unwrap(),
            "hidden-1a2b3c4d.pkg"
        );
        assert_eq!(
            final_name("a:b*c.pkg", "1a2b3c4d", 0).unwrap(),
            "a_b_c-1a2b3c4d.pkg"
        );
        assert!(final_name("x.txt", "1a2b3c4d", 0).is_err());
        assert!(final_name("x.pkg", "bad-tag", 0).is_err());
        assert!(final_name("x.part0.rar", "tag", 0).is_err());

        let long = final_name(&format!("{}.pkg", "界".repeat(100)), "tag", 0).unwrap();
        assert!(long.len() <= 240);
        assert!(long.ends_with("-tag.pkg"));
        assert!(std::str::from_utf8(long.as_bytes()).is_ok());

        let first = final_name("Game.part1.rar", "aaa", 0).unwrap();
        let second = final_name("game.part2.rar", "aaa", 0).unwrap();
        let other_tag = final_name("Game.part1.rar", "bbb", 0).unwrap();
        assert_eq!(set_key(&first), set_key(&second));
        assert_ne!(set_key(&first), set_key(&other_tag));
        assert_eq!(validate_final_name("clean.pkg"), Ok(()));
        assert_eq!(
            validate_final_name(&format!("{}.pkg", "a".repeat(237))).unwrap_err(),
            "File name is too long for the PS4 inbox."
        );
        assert!(validate_final_name("bad:name.pkg").is_err());
    }

    #[test]
    fn helper_names_and_base64_are_standard() {
        assert_eq!(temp_name("game.pkg"), ".game.pkg.uploading");
        assert_eq!(inbox_kind(&temp_name("game.pkg")), None);
        assert_eq!(receipt_name("ftp_abc"), "installed-ftp_abc.txt");
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_decode(" Zm9v\r\n"), Some(b"foo".to_vec()));
        assert_eq!(b64_decode("Zg"), Some(b"f".to_vec()));
        assert_eq!(b64_decode(""), Some(Vec::new()));
        assert_eq!(b64_decode("Z=g="), None);
        assert_eq!(b64_decode("A"), None);
        assert!(names_job("queued ftp_abc status", "ftp_abc"));
        assert!(names_job("job=ZnRwX2FiYw==", "ftp_abc"));
        assert!(!names_job("anything", ""));
    }

    #[test]
    fn heartbeat_parser_reads_caps_and_time() {
        let text = "resident\r\n638355968000000000\r\nhost=shell pid=5 ftpinbox=1 transfer=1 bgft=1 v=2 api=3\r\n";
        let heartbeat = parse_heartbeat(text).unwrap();
        assert_eq!(heartbeat.version, "resident");
        assert_eq!(heartbeat.unix_seconds(), 1_700_000_000);
        assert_eq!(heartbeat.cap("host"), Some("shell"));
        assert!(heartbeat.inbox_ready());
        assert!(!parse_heartbeat("resident\n0\nftpinbox=1 transfer=1 bgft=0\n")
            .unwrap()
            .inbox_ready());
        assert!(parse_heartbeat("\n123\na=b\n").is_none());
        assert!(parse_heartbeat("resident\nnope\na=b\n").is_none());
    }

    #[test]
    fn claim_parser_accepts_c_body_and_optional_fields() {
        let text = "resident\nGame-1a2b3c4d.pkg\n123\n1700000000\nftp_abc\nqueued\n1\n123\n";
        assert_eq!(
            parse_claim(text),
            Some(Claim {
                owner: "resident".to_owned(),
                name: "Game-1a2b3c4d.pkg".to_owned(),
                size: 123,
                mtime: 1_700_000_000,
                job: "ftp_abc".to_owned(),
                state: "queued".to_owned(),
                count: Some(1),
                total: Some(123),
            })
        );
        assert_eq!(
            parse_claim("resident\nx.pkg\n7\n-3\n\ninstalled\n"),
            Some(Claim {
                owner: "resident".to_owned(),
                name: "x.pkg".to_owned(),
                size: 7,
                mtime: -3,
                job: "".to_owned(),
                state: "installed".to_owned(),
                count: None,
                total: None,
            })
        );
        assert!(parse_claim("owner\nname\nnot-a-number\n0\n").is_none());
        assert!(parse_claim("owner\nname\n1\n").is_none());
    }

    #[test]
    fn staged_status_job_singleton_and_receipt_formats_parse() {
        let encoded_job = b64_encode(b"ftp_abc");
        let encoded_error = b64_encode(b"bad archive");
        let status_body = format!(
            "1\n{encoded_job}\nfeeding\n42\n100\n0\n{encoded_error}\n2\n1\n500\ngen-a\nbuild-1\n1700000000\n"
        );
        assert_eq!(
            parse_stage_status(&status_body),
            Some(StageStatus {
                job: "ftp_abc".to_owned(),
                state: "feeding".to_owned(),
                done: 42,
                total: 100,
                error: "bad archive".to_owned(),
                active: 2,
                auto_install: true,
                wire_bytes: 500,
                generation: "gen-a".to_owned(),
                build: "build-1".to_owned(),
            })
        );
        assert_eq!(
            parse_stage_status(&format!("1\n{encoded_job}\nqueued\n0\n8\n0\n\n")),
            Some(StageStatus {
                job: "ftp_abc".to_owned(),
                state: "queued".to_owned(),
                done: 0,
                total: 8,
                error: "".to_owned(),
                active: 0,
                auto_install: false,
                wire_bytes: 0,
                generation: "".to_owned(),
                build: "".to_owned(),
            })
        );
        assert!(parse_stage_status("2\nabc\nqueued\n0\n1\n0\n\n").is_none());
        assert!(parse_stage_status("1\n!!\nqueued\n0\n1\n0\n\n").is_none());

        assert_eq!(
            parse_stage_job_id(&format!("18\n{encoded_job}\n")),
            Some("ftp_abc".to_owned())
        );
        assert!(parse_stage_job_id(&format!("0\n{encoded_job}\n")).is_none());

        assert_eq!(
            parse_singleton_status(&format!("1\n{encoded_job}\nextracting\n")),
            Some(SingletonStatus {
                job: "ftp_abc".to_owned(),
                state: "extracting".to_owned(),
            })
        );
        let content = b64_encode(b"CUSA12345");
        assert_eq!(
            parse_install_receipt(&format!("1\n{encoded_job}\n{content}\n12345\ngen-a\n")),
            Some(InstallReceipt {
                job: "ftp_abc".to_owned(),
                content_id: "CUSA12345".to_owned(),
                total: 12345,
                generation: "gen-a".to_owned(),
            })
        );
        assert!(parse_install_receipt("1\n\n\nnope\ngen\n").is_none());
    }

    #[test]
    fn unix_list_and_mlsd_lines_keep_names_and_metadata() {
        assert_eq!(
            parse_list_line("type=file;size=123;modify=20260926132000; Game.pkg"),
            Some(ListEntry {
                name: "Game.pkg".to_owned(),
                size: Some(123),
                dir: false,
            })
        );
        assert_eq!(
            parse_list_line("TYPE=DIR;modify=x; Folder With Spaces"),
            Some(ListEntry {
                name: "Folder With Spaces".to_owned(),
                size: None,
                dir: true,
            })
        );
        assert_eq!(parse_list_line("type=cdir; ."), None);
        assert_eq!(parse_list_line("type=pdir; .."), None);
        assert_eq!(
            parse_list_line("-rw-rw-rw- 1 root root 12345 Sep 26 13:20 name with spaces.pkg"),
            Some(ListEntry {
                name: "name with spaces.pkg".to_owned(),
                size: Some(12345),
                dir: false,
            })
        );
        assert_eq!(
            parse_list_line("drwxr-xr-x 2 root root 4096 Sep 26 13:20 games"),
            Some(ListEntry {
                name: "games".to_owned(),
                size: Some(4096),
                dir: true,
            })
        );
        assert_eq!(
            parse_list_line("lrwxrwxrwx 1 root root 4 Sep 26 13:20 alias -> target name"),
            Some(ListEntry {
                name: "alias".to_owned(),
                size: Some(4),
                dir: false,
            })
        );
        assert_eq!(parse_list_line("total 3"), None);
        assert_eq!(parse_list_line("-rw-r--r-- 1 root root nope Sep 26 13:20 file"), None);
        assert_eq!(parse_list_line("not a directory listing"), None);
    }
}
