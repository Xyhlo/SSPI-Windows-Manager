use sha2::{Digest, Sha256};
use std::{fs::File, io::{Read, Seek, SeekFrom}, path::Path};

const HEADER_SIZE: usize = 0x1000;
const ENTRY_SIZE: usize = 0x20;
const MAX_ENTRY_COUNT: usize = 65_536;
const MAX_SFO_SIZE: usize = 64 * 1024;
const MAX_ICON_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PkgMeta {
    pub content_id: String,
    pub title_id: String,
    pub category: String,
    pub kind: String,
    pub title: Option<String>,
    pub version: Option<String>,
    pub digest_hex: String,
    pub content_type: u32,
    pub header_sha256: String,
    pub original_size: u64,
    pub file_size: u64,
    pub icon0: Option<Vec<u8>>,
}

#[derive(Clone, Copy)]
struct Entry {
    id: u32,
    encrypted: bool,
    offset: u64,
    size: u64,
}

pub(super) fn read(path: &Path) -> Result<PkgMeta, String> {
    let mut file = File::open(path).map_err(|_| "Unable to open PKG file".to_string())?;
    let file_size = file.metadata().map_err(|_| "Unable to read PKG file size".to_string())?.len();
    if file_size < HEADER_SIZE as u64 {
        return Err("PKG header is truncated".into());
    }

    let mut header = [0u8; HEADER_SIZE];
    file.read_exact(&mut header).map_err(|_| "PKG header is truncated".to_string())?;
    if &header[..4] != b"\x7fCNT" {
        return Err("File is not a PS4 PKG".into());
    }

    let digest = Sha256::digest(&header[..0xfe0]);
    if digest[..] != header[0xfe0..0x1000] {
        return Err("PKG header digest is invalid".into());
    }

    let content_id = std::str::from_utf8(&header[0x40..0x40 + 36])
        .map_err(|_| "PKG content ID is invalid".to_string())?
        .to_string();
    let title_id = title_id_from_content_id(&content_id)
        .ok_or_else(|| "PKG content ID is invalid".to_string())?;

    let declared_size = be_u64(&header, 0x430);
    validate_package_ranges(&header, file_size, declared_size)?;

    // PKG header fields 0x10 and 0x18 are the BE entry count and table offset.
    let entry_count = be_u32(&header, 0x10) as usize;
    let table_offset = be_u32(&header, 0x18) as u64;
    if entry_count > MAX_ENTRY_COUNT {
        return Err("PKG entry table is too large".into());
    }
    let table_size = entry_count.checked_mul(ENTRY_SIZE)
        .ok_or_else(|| "PKG entry table is invalid".to_string())?;
    let table_end = table_offset.checked_add(table_size as u64)
        .ok_or_else(|| "PKG entry table is invalid".to_string())?;
    if table_end > file_size {
        return Err("PKG entry table extends past the file".into());
    }

    let mut table = vec![0u8; table_size];
    if !table.is_empty() {
        file.seek(SeekFrom::Start(table_offset)).map_err(|_| "Unable to read PKG entry table".to_string())?;
        file.read_exact(&mut table).map_err(|_| "PKG entry table is truncated".to_string())?;
    }

    let mut sfo_entry = None;
    let mut icon_entry = None;
    for raw in table.chunks_exact(ENTRY_SIZE) {
        let entry = Entry {
            id: be_u32(raw, 0),
            encrypted: be_u32(raw, 8) & 0x8000_0000 != 0,
            offset: be_u32(raw, 0x10) as u64,
            size: be_u32(raw, 0x14) as u64,
        };
        if entry.offset > file_size || entry.size > file_size - entry.offset {
            return Err("PKG entry extends past the file".into());
        }
        if entry.encrypted {
            continue;
        }
        match entry.id {
            0x1000 if sfo_entry.is_none() => sfo_entry = Some(entry),
            0x1200 if icon_entry.is_none() => icon_entry = Some(entry),
            _ => {}
        }
    }

    let (category, title, version, sfo_title_id) = match sfo_entry {
        Some(entry) if entry.size > 0 => {
            if entry.size > MAX_SFO_SIZE as u64 {
                return Err("PKG param.sfo exceeds the size limit".into());
            }
            let data = read_entry(&mut file, entry)?;
            parse_sfo(&data).ok_or_else(|| "PKG param.sfo is malformed".to_string())?
        }
        _ => (String::new(), None, None, None),
    };
    if sfo_title_id.as_deref().is_some_and(|id| !id.eq_ignore_ascii_case(&title_id)) {
        return Err("PKG param.sfo title ID does not match the package content ID".into());
    }

    let icon0 = match icon_entry {
        Some(entry) if entry.size > 0 && entry.size <= MAX_ICON_SIZE as u64 => {
            let data = read_entry(&mut file, entry)?;
            if is_valid_png(&data) { Some(data) } else { None }
        }
        _ => None,
    };

    let kind = if category.starts_with("gd") {
        "base"
    } else if category.starts_with("gp") {
        "update"
    } else if category.starts_with("ac") {
        "dlc"
    } else {
        "other"
    };

    let mut digest_hex = String::with_capacity(64);
    for byte in &header[0xfe0..0x1000] {
        use std::fmt::Write as _;
        let _ = write!(&mut digest_hex, "{byte:02x}");
    }

    Ok(PkgMeta {
        content_id,
        title_id,
        category,
        kind: kind.into(),
        title,
        version,
        digest_hex,
        // SSPI PkgIntegrity.PackageType reads the BE content type at 0x74.
        content_type: be_u32(&header, 0x74),
        header_sha256: format!("{:x}", Sha256::digest(header)),
        original_size: declared_size,
        file_size,
        icon0,
    })
}

pub(super) fn manifest_json(meta: &PkgMeta, package_url: &str) -> String {
    let url = serde_json::to_string(package_url).expect("a string can always be serialized as JSON");
    format!(
        "{{\"originalFileSize\":{},\"packageDigest\":\"{}\",\"numberOfSplitFiles\":1,\"pieces\":[{{\"url\":{},\"fileOffset\":0,\"fileSize\":{},\"hashValue\":\"0000000000000000000000000000000000000000\"}}]}}",
        meta.original_size, meta.digest_hex, url, meta.file_size
    )
}

pub(super) fn is_valid_png(bytes: &[u8]) -> bool {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 45 || bytes.len() > MAX_ICON_SIZE || !bytes.starts_with(SIGNATURE) {
        return false;
    }

    let mut cursor = 8usize;
    let mut first = true;
    let mut saw_idat = false;
    while cursor + 12 <= bytes.len() {
        let length = be_u32(bytes, cursor) as usize;
        let Some(chunk_end) = cursor.checked_add(12).and_then(|n| n.checked_add(length)) else {
            return false;
        };
        if chunk_end > bytes.len() {
            return false;
        }
        let kind = &bytes[cursor + 4..cursor + 8];
        let data_start = cursor + 8;
        let data_end = data_start + length;
        let expected_crc = be_u32(bytes, data_end);
        if crc32(&bytes[cursor + 4..data_end]) != expected_crc {
            return false;
        }

        if first {
            if kind != b"IHDR" || length != 13 || be_u32(bytes, data_start) == 0 || be_u32(bytes, data_start + 4) == 0 {
                return false;
            }
            first = false;
        } else if kind == b"IHDR" {
            return false;
        }

        if kind == b"IDAT" {
            saw_idat = true;
        }
        cursor = chunk_end;
        if kind == b"IEND" {
            return length == 0 && saw_idat && cursor == bytes.len();
        }
    }
    false
}

fn read_entry(file: &mut File, entry: Entry) -> Result<Vec<u8>, String> {
    let size = usize::try_from(entry.size).map_err(|_| "PKG entry is too large".to_string())?;
    let mut data = vec![0u8; size];
    file.seek(SeekFrom::Start(entry.offset)).map_err(|_| "Unable to read PKG entry".to_string())?;
    file.read_exact(&mut data).map_err(|_| "PKG entry is truncated".to_string())?;
    Ok(data)
}

fn validate_package_ranges(header: &[u8], file_size: u64, declared_size: u64) -> Result<(), String> {
    let body_offset = be_u64(header, 0x20);
    let body_size = be_u64(header, 0x28);
    let pfs_offset = be_u64(header, 0x410);
    let pfs_size = be_u64(header, 0x418);
    let no_data_license = be_u32(header, 0x74) == 0x1c
        && declared_size == 0
        && pfs_offset == 0
        && pfs_size == 0
        && body_offset >= HEADER_SIZE as u64
        && body_offset <= file_size
        && body_size > 0
        && body_size == file_size - body_offset;

    if !no_data_license && declared_size != file_size {
        return Err("PKG declared size does not match the file".into());
    }
    if body_offset > file_size
        || body_size > file_size - body_offset
        || (body_size > 0 && body_offset < HEADER_SIZE as u64)
        || pfs_offset > file_size
        || pfs_size > file_size - pfs_offset
        || (pfs_size > 0 && pfs_offset < HEADER_SIZE as u64)
    {
        return Err("PKG data range extends past the file".into());
    }
    Ok(())
}

pub(super) fn parse_sfo(data: &[u8]) -> Option<(String, Option<String>, Option<String>, Option<String>)> {
    if data.len() < 20 || &data[..4] != b"\0PSF" {
        return None;
    }
    let key_offset = le_u32(data, 8) as usize;
    let value_offset = le_u32(data, 12) as usize;
    let count = le_u32(data, 16) as usize;
    let entries_end = 20usize.checked_add(count.checked_mul(16)?)?;
    if entries_end > data.len() || key_offset < entries_end || key_offset > value_offset || value_offset > data.len() {
        return None;
    }
    let key_table = &data[key_offset..value_offset];
    let mut category = String::new();
    let mut title = None;
    let mut version = None;
    let mut title_id = None;

    for entry in data[20..entries_end].chunks_exact(16) {
        let name_offset = le_u16(entry, 0) as usize;
        let format = le_u16(entry, 2);
        let length = le_u32(entry, 4) as usize;
        let max_length = le_u32(entry, 8) as usize;
        let relative_value = le_u32(entry, 12) as usize;
        if length > max_length || name_offset >= key_table.len() {
            return None;
        }
        let key_tail = &key_table[name_offset..];
        let key_end = key_tail.iter().position(|byte| *byte == 0)?;
        let key = std::str::from_utf8(&key_tail[..key_end]).ok()?;
        let value_start = value_offset.checked_add(relative_value)?;
        let value_end = value_start.checked_add(length)?;
        let raw_value = data.get(value_start..value_end)?;

        if format != 0x0204 || !matches!(key, "CATEGORY" | "TITLE" | "APP_VER" | "TITLE_ID") {
            continue;
        }
        let string_end = raw_value.iter().position(|byte| *byte == 0).unwrap_or(raw_value.len());
        let value = std::str::from_utf8(&raw_value[..string_end]).ok()?.trim().to_string();
        match key {
            "CATEGORY" => category = value,
            "TITLE" if !value.is_empty() => title = Some(value),
            "APP_VER" if !value.is_empty() => version = Some(value),
            "TITLE_ID" if !value.is_empty() => title_id = Some(value),
            _ => {}
        }
    }
    Some((category, title, version, title_id))
}

fn title_id_from_content_id(content_id: &str) -> Option<String> {
    let bytes = content_id.as_bytes();
    if bytes.len() != 36 || bytes[6] != b'-' || bytes[16] != b'_' || bytes[19] != b'-' {
        return None;
    }
    if !bytes[..6].iter().all(u8::is_ascii_alphanumeric)
        || !bytes[7..16].iter().all(u8::is_ascii_alphanumeric)
        || !bytes[17..19].iter().all(u8::is_ascii_alphanumeric)
        || !bytes[20..].iter().all(u8::is_ascii_alphanumeric)
    {
        return None;
    }
    let title_id = &content_id[7..16];
    if !title_id[..4].eq_ignore_ascii_case("CUSA") || !title_id[4..].bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(title_id.to_string())
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn be_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("four-byte field"))
}

fn be_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(bytes[offset..offset + 8].try_into().expect("eight-byte field"))
}

fn le_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("two-byte field"))
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four-byte field"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    fn temp_pkg(bytes: &[u8]) -> PathBuf {
        let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = crate::test_output_root().join(format!("sspi-pkg-meta-{}-{id}.pkg", std::process::id()));
        fs::write(&path, bytes).unwrap();
        path
    }

    fn put_be(bytes: &mut [u8], offset: usize, value: u64, width: usize) {
        for index in 0..width {
            bytes[offset + width - 1 - index] = (value >> (index * 8)) as u8;
        }
    }

    fn put_le(bytes: &mut [u8], offset: usize, value: u32, width: usize) {
        for index in 0..width {
            bytes[offset + index] = (value >> (index * 8)) as u8;
        }
    }

    fn sfo(category: &str, title: &str) -> Vec<u8> {
        let keys = ["CATEGORY", "TITLE", "APP_VER"];
        let values = [category, title, "1.03"];
        let key_offset = 20 + keys.len() * 16;
        let key_size: usize = keys.iter().map(|key| key.len() + 1).sum();
        let value_offset = key_offset + key_size;
        let value_size: usize = values.iter().map(|value| value.len() + 1).sum();
        let mut result = vec![0u8; value_offset + value_size];
        result[..4].copy_from_slice(b"\0PSF");
        put_le(&mut result, 8, key_offset as u32, 4);
        put_le(&mut result, 12, value_offset as u32, 4);
        put_le(&mut result, 16, keys.len() as u32, 4);
        let mut key_at = 0usize;
        let mut value_at = 0usize;
        for index in 0..keys.len() {
            let entry = 20 + index * 16;
            put_le(&mut result, entry, key_at as u32, 2);
            put_le(&mut result, entry + 2, 0x0204, 2);
            put_le(&mut result, entry + 4, (values[index].len() + 1) as u32, 4);
            put_le(&mut result, entry + 8, (values[index].len() + 1) as u32, 4);
            put_le(&mut result, entry + 12, value_at as u32, 4);
            let key = format!("{}\0", keys[index]);
            result[key_offset + key_at..key_offset + key_at + key.len()].copy_from_slice(key.as_bytes());
            let value = format!("{}\0", values[index]);
            result[value_offset + value_at..value_offset + value_at + value.len()].copy_from_slice(value.as_bytes());
            key_at += key.len();
            value_at += value.len();
        }
        result
    }

    fn add_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let crc = crc32(&png[png.len() - data.len() - 4..]);
        png.extend_from_slice(&crc.to_be_bytes());
    }

    fn tiny_png() -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = vec![0u8; 13];
        ihdr[..4].copy_from_slice(&1u32.to_be_bytes());
        ihdr[4..8].copy_from_slice(&1u32.to_be_bytes());
        ihdr[8] = 8;
        ihdr[9] = 6;
        add_chunk(&mut png, b"IHDR", &ihdr);
        add_chunk(&mut png, b"IDAT", &[0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00, 0x05, 0x00, 0x01]);
        add_chunk(&mut png, b"IEND", &[]);
        png
    }

    fn package(include_sfo: bool) -> Vec<u8> {
        let sfo_data = sfo("gd", "Example Game");
        let icon = tiny_png();
        let count = if include_sfo { 2 } else { 1 };
        let table_offset = 0x1000usize;
        let sfo_offset = 0x1100usize;
        let icon_offset = 0x1400usize;
        let mut pkg = vec![0u8; 0x2000];
        let pkg_len = pkg.len();
        pkg[..4].copy_from_slice(b"\x7fCNT");
        put_be(&mut pkg, 0x10, count as u64, 4);
        put_be(&mut pkg, 0x18, table_offset as u64, 4);
        pkg[0x40..0x40 + 36].copy_from_slice(b"UP0000-CUSA12345_00-ABCDEFGHIJKLMNOP");
        put_be(&mut pkg, 0x74, 0x1a, 4);
        put_be(&mut pkg, 0x20, 0x1000, 8);
        put_be(&mut pkg, 0x28, (pkg_len - 0x1000) as u64, 8);
        put_be(&mut pkg, 0x430, pkg_len as u64, 8);
        if include_sfo {
            put_be(&mut pkg, table_offset, 0x1000, 4);
            put_be(&mut pkg, table_offset + 0x10, sfo_offset as u64, 4);
            put_be(&mut pkg, table_offset + 0x14, sfo_data.len() as u64, 4);
            pkg[sfo_offset..sfo_offset + sfo_data.len()].copy_from_slice(&sfo_data);
        }
        let icon_entry = table_offset + (count - 1) * ENTRY_SIZE;
        put_be(&mut pkg, icon_entry, 0x1200, 4);
        put_be(&mut pkg, icon_entry + 0x10, icon_offset as u64, 4);
        put_be(&mut pkg, icon_entry + 0x14, icon.len() as u64, 4);
        pkg[icon_offset..icon_offset + icon.len()].copy_from_slice(&icon);
        let digest = Sha256::digest(&pkg[..0xfe0]);
        pkg[0xfe0..0x1000].copy_from_slice(&digest);
        pkg
    }

    #[test]
    fn reads_header_sfo_and_icon() {
        let bytes = package(true);
        let path = temp_pkg(&bytes);
        let result = read(&path).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(result, PkgMeta {
            content_id: "UP0000-CUSA12345_00-ABCDEFGHIJKLMNOP".into(),
            title_id: "CUSA12345".into(),
            category: "gd".into(),
            kind: "base".into(),
            title: Some("Example Game".into()),
            version: Some("1.03".into()),
            digest_hex: hex(&Sha256::digest(&bytes[..0xfe0])),
            content_type: 0x1a,
            header_sha256: hex(&Sha256::digest(&bytes[..0x1000])),
            original_size: bytes.len() as u64,
            file_size: bytes.len() as u64,
            icon0: Some(tiny_png()),
        });
    }

    #[test]
    fn malformed_and_truncated_packages_are_rejected() {
        for bytes in [vec![0u8; 32], vec![0u8; 0x1000], {
            let mut bytes = package(true);
            put_be(&mut bytes, 0x18, 0xffff_fff0, 4);
            let digest = Sha256::digest(&bytes[..0xfe0]);
            bytes[0xfe0..0x1000].copy_from_slice(&digest);
            bytes
        }, {
            let mut bytes = package(true);
            put_be(&mut bytes, 0x1010, 0x3000, 4);
            let digest = Sha256::digest(&bytes[..0xfe0]);
            bytes[0xfe0..0x1000].copy_from_slice(&digest);
            bytes
        }] {
            let path = temp_pkg(&bytes);
            assert!(read(&path).is_err());
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn missing_sfo_is_allowed() {
        let bytes = package(false);
        let path = temp_pkg(&bytes);
        let result = read(&path).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(result.category, "");
        assert_eq!(result.kind, "other");
        assert_eq!(result.title, None);
        assert_eq!(result.version, None);
    }

    #[test]
    fn content_type_and_header_sha256_include_the_complete_header() {
        let mut bytes = package(true);
        put_be(&mut bytes, 0x74, 0x01020304, 4);
        let digest = Sha256::digest(&bytes[..0xfe0]); bytes[0xfe0..0x1000].copy_from_slice(&digest);
        let path = temp_pkg(&bytes); let meta = read(&path).unwrap(); let _ = fs::remove_file(path);
        assert_eq!(meta.content_type, 0x01020304);
        assert_eq!(meta.header_sha256, hex(&Sha256::digest(&bytes[..0x1000])));
        assert_eq!(meta.digest_hex, hex(&digest)); assert_ne!(meta.digest_hex, meta.header_sha256);
        assert_eq!(meta.header_sha256.len(), 64);
    }

    #[test]
    fn manifest_json_is_exact() {
        let meta = PkgMeta {
            content_id: String::new(), title_id: String::new(), category: String::new(), kind: String::new(),
            title: None, version: None, digest_hex: "ab".repeat(32), content_type: 0x1a, header_sha256: "cd".repeat(32), original_size: 123, file_size: 100, icon0: None,
        };
        assert_eq!(manifest_json(&meta, "http://192.168.1.2:9115/pkg/0123456789abcdef.pkg"),
            "{\"originalFileSize\":123,\"packageDigest\":\"abababababababababababababababababababababababababababababababab\",\"numberOfSplitFiles\":1,\"pieces\":[{\"url\":\"http://192.168.1.2:9115/pkg/0123456789abcdef.pkg\",\"fileOffset\":0,\"fileSize\":100,\"hashValue\":\"0000000000000000000000000000000000000000\"}]}");
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
