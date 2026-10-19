//! BitTorrent magnet links: parsing, and choosing the one file a package row names inside a torrent.

const PACKAGE_EXTENSIONS: [&str; 9] = ["pkg", "ffpkg", "rar", "zip", "7z", "7zip", "exfat", "ffpfsc", "iso"];

pub(super) fn is_magnet(text: &str) -> bool {
    text.trim_start().get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("magnet:?"))
}

/// The torrent's v1 info hash as 40 lowercase hex characters. A magnet with no `urn:btih`
/// topic, two different ones, or control characters is rejected.
pub(super) fn info_hash(text: &str) -> Option<String> {
    if text.chars().any(char::is_control) {
        return None;
    }
    let text = text.trim();
    if !is_magnet(text) || text.len() > 8192 || text.contains(' ') {
        return None;
    }
    let mut found: Option<String> = None;
    for (key, value) in url::form_urlencoded::parse(text[8..].as_bytes()) {
        if !key.eq_ignore_ascii_case("xt") {
            continue;
        }
        let Some(topic) = value.get(..9).filter(|p| p.eq_ignore_ascii_case("urn:btih:")).map(|_| &value[9..]) else {
            continue;
        };
        let hash = match topic.len() {
            40 if topic.chars().all(|c| c.is_ascii_hexdigit()) => topic.to_ascii_lowercase(),
            32 => base32_hex(topic)?,
            _ => return None,
        };
        if found.as_ref().is_some_and(|other| *other != hash) {
            return None;
        }
        found = Some(hash);
    }
    found
}

pub(super) fn valid(text: &str) -> bool {
    info_hash(text).is_some()
}

/// The `dn` display name, without control characters, at most 180 characters.
pub(super) fn display_name(text: &str) -> Option<String> {
    if !is_magnet(text) {
        return None;
    }
    url::form_urlencoded::parse(text.trim()[8..].as_bytes())
        .find(|(key, _)| key.eq_ignore_ascii_case("dn"))
        .map(|(_, value)| value.chars().filter(|c| !c.is_control()).take(180).collect::<String>().trim().to_string())
        .filter(|name| !name.is_empty())
}

fn base32_hex(text: &str) -> Option<String> {
    let mut bits: u64 = 0;
    let mut count = 0;
    let mut out = String::with_capacity(40);
    for c in text.chars() {
        let value = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u64 - 'A' as u64,
            c @ '2'..='7' => c as u64 - '2' as u64 + 26,
            _ => return None,
        };
        bits = (bits << 5) | value;
        count += 5;
        while count >= 8 {
            count -= 8;
            out.push_str(&format!("{:02x}", (bits >> count) & 0xff));
        }
    }
    (out.len() == 40).then_some(out)
}

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

pub(super) fn is_package_file(path: &str) -> bool {
    let name = base_name(path).to_ascii_lowercase();
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    PACKAGE_EXTENSIONS.contains(&extension)
        // Old-style RAR volumes: game.r00, game.r01 ...
        || extension.len() == 3 && extension.starts_with('r') && extension[1..].chars().all(|c| c.is_ascii_digit())
}

/// Picks the file a package row asks for from a torrent's `(path, size)` list.
/// `wanted` is the row's file name; without one, the torrent must hold exactly one package file.
pub(super) fn pick(files: &[(String, u64)], wanted: Option<&str>, size: Option<u64>) -> Result<usize, String> {
    let wanted = wanted.map(str::trim).filter(|name| !name.is_empty());
    let sized = |indexes: Vec<usize>| -> Option<usize> {
        match indexes.len() {
            0 => None,
            1 => Some(indexes[0]),
            _ => size
                .and_then(|size| indexes.iter().copied().find(|&i| files[i].1 == size))
                .or(Some(indexes[0])),
        }
    };
    if let Some(wanted) = wanted {
        let target = base_name(wanted);
        let named: Vec<usize> = (0..files.len())
            .filter(|&i| base_name(&files[i].0).eq_ignore_ascii_case(target))
            .collect();
        if let Some(index) = sized(named) {
            return Ok(index);
        }
    }
    let packages: Vec<usize> = (0..files.len()).filter(|&i| is_package_file(&files[i].0)).collect();
    if let Some(size) = size.filter(|size| *size > 0) {
        let matching: Vec<usize> = packages.iter().copied().filter(|&i| files[i].1 == size).collect();
        if matching.len() == 1 {
            return Ok(matching[0]);
        }
    }
    if wanted.is_none() && packages.len() == 1 {
        return Ok(packages[0]);
    }
    let listed: Vec<&str> = packages.iter().take(4).map(|&i| base_name(&files[i].0)).collect();
    Err(match (wanted, packages.len()) {
        (_, 0) => "The torrent holds no package or archive file".into(),
        (Some(wanted), _) => format!("The torrent has no file named {wanted}. It holds: {}", listed.join(", ")),
        (None, count) => format!("The torrent holds {count} package files; choose a release that names one ({})", listed.join(", ")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c";

    #[test]
    fn parses_hex_and_base32_hashes() {
        let magnet = format!("magnet:?xt=urn:btih:{}&dn=Big+Buck+Bunny&tr=udp%3A%2F%2Fexample", HEX.to_uppercase());
        assert_eq!(info_hash(&magnet).as_deref(), Some(HEX));
        assert_eq!(display_name(&magnet).as_deref(), Some("Big Buck Bunny"));
        assert_eq!(
            info_hash("magnet:?xt=urn:btih:3WBFL3G4PSSV7MF37AJSHWDQMLNR63I4").as_deref(),
            Some(HEX)
        );
        assert!(valid(&format!("MAGNET:?dn=x&xt=urn:btih:{HEX}")));
    }

    #[test]
    fn rejects_malformed_magnets() {
        assert!(!valid("magnet:?dn=only-a-name"));
        assert!(!valid(&format!("magnet:?xt=urn:btih:{}", &HEX[..39])));
        assert!(!valid(&format!("magnet:?xt=urn:btih:{HEX}&xt=urn:btih:{}", "a".repeat(40))));
        assert!(valid(&format!("magnet:?xt=urn:btih:{HEX}&xt=urn:btih:{}", HEX.to_uppercase())));
        assert!(!valid(&format!("magnet:?xt=urn:btih:{HEX}\n")));
        assert!(!valid(&format!("https://example.com/?xt=urn:btih:{HEX}")));
        assert!(!valid(&format!("magnet:?xt=urn:sha1:{HEX}")));
    }

    #[test]
    fn picks_the_named_file() {
        let files = vec![
            ("Game/readme.txt".to_string(), 10),
            ("Game/Update/UP0001-CUSA00001_00-X_1.pkg".to_string(), 500),
            ("Game/Base/UP0001-CUSA00001_00-X_0.PKG".to_string(), 900),
        ];
        assert_eq!(pick(&files, Some("up0001-cusa00001_00-x_0.pkg"), None), Ok(2));
        assert_eq!(pick(&files, Some("renamed.pkg"), Some(500)), Ok(1));
        assert!(pick(&files, Some("missing.pkg"), None).unwrap_err().contains("no file named missing.pkg"));
        assert!(pick(&files, None, None).unwrap_err().contains("2 package files"));
        assert_eq!(pick(&files[..2], None, None), Ok(1));
    }

    #[test]
    fn duplicate_names_prefer_the_expected_size() {
        let files = vec![("A/x.pkg".to_string(), 1), ("B/x.pkg".to_string(), 2)];
        assert_eq!(pick(&files, Some("x.pkg"), Some(2)), Ok(1));
        assert_eq!(pick(&files, Some("x.pkg"), None), Ok(0));
    }

    #[test]
    fn recognises_package_files() {
        for name in ["a.pkg", "b.part1.rar", "c.r00", "d.7z", "e.exfat", "f.ffpfsc", "g.ffpkg"] {
            assert!(is_package_file(name), "{name}");
        }
        for name in ["a.nfo", "b.txt", "c.jpg", "pkg"] {
            assert!(!is_package_file(name), "{name}");
        }
    }
}
