//! Windows resource files (`.res`) that the build scripts hand to the MSVC
//! linker: the icon group of the application and the version information of
//! each program, which the elevation prompt and the file properties show.

// Each build script uses a part of this file; the tasks binary uses none.
#![allow(dead_code)]

/// Resource type of the version information.
pub const RT_VERSION: u16 = 16;
/// Moveable and pure, as a resource compiler marks version information.
pub const VERSION_FLAGS: u16 = 0x0030;
/// English, United States.
pub const LANGUAGE: u16 = 0x0409;
/// Unicode (UTF-16), the code page of the version strings.
const CODE_PAGE: u16 = 1200;

/// What the version information of one program says.
pub struct VersionInfo<'a> {
    /// The build version, `YYYY.MM.DD.NNNN`.
    pub version: &'a str,
    /// `FileDescription`, the name the elevation prompt shows.
    pub description: &'a str,
    /// `ProductName`.
    pub product: &'a str,
    /// `CompanyName`.
    pub company: &'a str,
    /// `OriginalFilename` and, without its extension, `InternalName`.
    pub file_name: &'a str,
}

/// An empty resource file, which starts with an empty entry that marks its
/// format.
#[must_use]
pub fn new_resource_file() -> Vec<u8> {
    let mut res = Vec::new();
    append(&mut res, 0, 0, 0, 0, &[]);
    res
}

/// One resource entry with a numeric type and a numeric name.
pub fn append(res: &mut Vec<u8>, kind: u16, name: u16, flags: u16, language: u16, data: &[u8]) {
    let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
    res.extend_from_slice(&size.to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes());
    res.extend_from_slice(&0xFFFFu16.to_le_bytes());
    res.extend_from_slice(&kind.to_le_bytes());
    res.extend_from_slice(&0xFFFFu16.to_le_bytes());
    res.extend_from_slice(&name.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes());
    res.extend_from_slice(&flags.to_le_bytes());
    res.extend_from_slice(&language.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes());
    res.extend_from_slice(data);
    pad(res);
}

/// Append the version information of `info` to `res`.
///
/// # Errors
/// Returns why the version cannot be stored in the four 16-bit fields of
/// the fixed version.
pub fn append_version(res: &mut Vec<u8>, info: &VersionInfo<'_>) -> Result<(), String> {
    let data = version_info(info)?;
    append(res, RT_VERSION, 1, VERSION_FLAGS, LANGUAGE, &data);
    Ok(())
}

/// The `VS_VERSION_INFO` block of `info`.
///
/// # Errors
/// As [`append_version`].
pub fn version_info(info: &VersionInfo<'_>) -> Result<Vec<u8>, String> {
    let fields = info
        .version
        .split('.')
        .map(|part| part.parse::<u16>().ok())
        .collect::<Option<Vec<u16>>>()
        .filter(|fields| fields.len() == 4)
        .ok_or_else(|| format!("{:?} does not fit a file version", info.version))?;
    let high = u32::from(fields[0]) << 16 | u32::from(fields[1]);
    let low = u32::from(fields[2]) << 16 | u32::from(fields[3]);
    let mut fixed = Vec::with_capacity(52);
    for value in [
        0xFEEF_04BD_u32, // signature
        0x0001_0000,     // structure version
        high,            // file version
        low,
        high, // product version
        low,
        0x3F,        // flags mask
        0,           // flags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,           // subtype
        0,           // date
        0,
    ] {
        fixed.extend_from_slice(&value.to_le_bytes());
    }
    let internal = info
        .file_name
        .rsplit_once('.')
        .map_or(info.file_name, |(stem, _)| stem);
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", info.company),
        ("FileDescription", info.description),
        ("FileVersion", info.version),
        ("InternalName", internal),
        ("OriginalFilename", info.file_name),
        ("ProductName", info.product),
        ("ProductVersion", info.version),
    ]
    .iter()
    .map(|(key, value)| {
        let text = utf16(value);
        let units = u16::try_from(text.len() / 2).unwrap_or(u16::MAX);
        node(key, &text, units, true, &[])
    })
    .collect();
    let table = node(
        &format!("{LANGUAGE:04X}{CODE_PAGE:04X}"),
        &[],
        0,
        true,
        &strings,
    );
    let string_info = node("StringFileInfo", &[], 0, true, &[table]);
    let mut translation = LANGUAGE.to_le_bytes().to_vec();
    translation.extend_from_slice(&CODE_PAGE.to_le_bytes());
    let var = node("Translation", &translation, 4, false, &[]);
    let var_info = node("VarFileInfo", &[], 0, true, &[var]);
    Ok(node(
        "VS_VERSION_INFO",
        &fixed,
        52,
        false,
        &[string_info, var_info],
    ))
}

/// One block of version information: length, value length, type, key,
/// value and children, each part on a four-byte boundary. The length counts
/// no padding after the last child.
fn node(key: &str, value: &[u8], value_length: u16, text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0u8; 4];
    out.extend_from_slice(&u16::from(text).to_le_bytes());
    out.extend_from_slice(&utf16(key));
    pad(&mut out);
    out.extend_from_slice(value);
    for child in children {
        pad(&mut out);
        out.extend_from_slice(child);
    }
    let length = u16::try_from(out.len()).unwrap_or(u16::MAX);
    out[0..2].copy_from_slice(&length.to_le_bytes());
    out[2..4].copy_from_slice(&value_length.to_le_bytes());
    out
}

/// `text` as UTF-16 with its terminating NUL.
fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn pad(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn info(version: &str) -> VersionInfo<'_> {
        VersionInfo {
            version,
            description: "Compare All",
            product: "Compare All",
            company: "Publisher",
            file_name: "compare-all.exe",
        }
    }

    fn u16_at(bytes: &[u8], at: usize) -> usize {
        usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
    }

    /// The key of the block at `at` and the offset of its first child.
    fn walk(bytes: &[u8], at: usize) -> (String, usize, usize) {
        let length = u16_at(bytes, at);
        let value_length = u16_at(bytes, at + 2);
        let text = u16_at(bytes, at + 4) == 1;
        let mut units = Vec::new();
        let mut cursor = at + 6;
        loop {
            let unit = u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
            cursor += 2;
            if unit == 0 {
                break;
            }
            units.push(unit);
        }
        let cursor = cursor.next_multiple_of(4);
        let value_bytes = if text { value_length * 2 } else { value_length };
        (
            String::from_utf16(&units).unwrap(),
            length,
            (cursor + value_bytes).next_multiple_of(4),
        )
    }

    #[test]
    fn the_version_information_has_the_fixed_version_and_the_strings() {
        let bytes = version_info(&info("2026.10.18.0007")).unwrap();
        let (key, length, first_child) = walk(&bytes, 0);
        assert_eq!(key, "VS_VERSION_INFO");
        assert_eq!(length, bytes.len());
        // The fixed version follows the key at a four-byte boundary.
        let fixed = 40;
        assert_eq!(&bytes[fixed..fixed + 4], &0xFEEF_04BD_u32.to_le_bytes());
        let field = |at: usize| u16_at(&bytes, fixed + at);
        assert_eq!(
            [field(10), field(8), field(14), field(12)],
            [2026, 10, 18, 7]
        );
        let (key, string_length, table) = walk(&bytes, first_child);
        assert_eq!(key, "StringFileInfo");
        let (key, table_length, mut at) = walk(&bytes, table);
        assert_eq!(key, "040904B0");
        let mut strings = Vec::new();
        while at < table + table_length {
            let (key, length, _) = walk(&bytes, at);
            let value = (at + 6 + (key.len() + 1) * 2).next_multiple_of(4);
            let units: Vec<u16> = bytes[value..at + length]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .take_while(|unit| *unit != 0)
                .collect();
            strings.push((key, String::from_utf16(&units).unwrap()));
            at = (at + length).next_multiple_of(4);
        }
        let expected = [
            ("CompanyName", "Publisher"),
            ("FileDescription", "Compare All"),
            ("FileVersion", "2026.10.18.0007"),
            ("InternalName", "compare-all"),
            ("OriginalFilename", "compare-all.exe"),
            ("ProductName", "Compare All"),
            ("ProductVersion", "2026.10.18.0007"),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned()));
        assert_eq!(strings, expected);
        let var_info = (first_child + string_length).next_multiple_of(4);
        let (key, _, var) = walk(&bytes, var_info);
        assert_eq!(key, "VarFileInfo");
        let (key, _, _) = walk(&bytes, var);
        assert_eq!(key, "Translation");
    }

    #[test]
    fn a_version_that_does_not_fit_the_fixed_fields_is_refused() {
        assert!(version_info(&info("2026.10.18.70000")).is_err());
        assert!(version_info(&info("2026.10.18")).is_err());
    }

    #[test]
    fn a_resource_file_entry_is_aligned() {
        let mut res = new_resource_file();
        append_version(&mut res, &info("2026.10.18.0007")).unwrap();
        assert_eq!(res.len() % 4, 0);
        assert_eq!(u16_at(&res, 32 + 10), usize::from(RT_VERSION));
    }
}
