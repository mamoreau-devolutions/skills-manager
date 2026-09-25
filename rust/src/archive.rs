//! Strict zip archive reader (port of archive.ts). Rejects unsafe paths,
//! links, encryption, multi-disk archives and inconsistent headers.

use std::io::Read;

const ZIP_LOCAL_FILE_HEADER: u32 = 0x04034b50;
const ZIP_CENTRAL_DIRECTORY_HEADER: u32 = 0x02014b50;
const ZIP_END_OF_CENTRAL_DIRECTORY: u32 = 0x06054b50;
const ZIP64_END_OF_CENTRAL_DIRECTORY: u32 = 0x06064b50;
const ZIP64_END_OF_CENTRAL_DIRECTORY_LOCATOR: u32 = 0x07064b50;
const ZIP_END_MIN_SIZE: usize = 22;
const ZIP_MAX_COMMENT_SIZE: usize = 0xffff;
const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

pub struct ArchiveLimits {
    pub max_extracted_bytes: u64,
    pub max_entries: u64,
}

#[derive(Debug, Clone)]
pub enum ArchiveError {
    /// Security/limit validation failure (surfaced to the user).
    Validation(String),
    /// Malformed archive.
    Invalid(String),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::Validation(s) | ArchiveError::Invalid(s) => write!(f, "{}", s),
        }
    }
}

fn inv<T>(s: &str) -> Result<T, ArchiveError> {
    Err(ArchiveError::Invalid(s.to_string()))
}

fn ensure_range(buf: &[u8], offset: u64, len: u64, label: &str) -> Result<(), ArchiveError> {
    if offset
        .checked_add(len)
        .map(|e| e > buf.len() as u64)
        .unwrap_or(true)
    {
        return Err(ArchiveError::Invalid(format!(
            "Invalid zip archive: {} is out of bounds",
            label
        )));
    }
    Ok(())
}

fn u16le(b: &[u8], o: usize) -> Result<u16, ArchiveError> {
    b.get(o..o + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| ArchiveError::Invalid("Invalid zip archive".into()))
}

fn u32le(b: &[u8], o: usize) -> Result<u32, ArchiveError> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| ArchiveError::Invalid("Invalid zip archive".into()))
}

fn u64le(b: &[u8], o: u64, label: &str) -> Result<u64, ArchiveError> {
    ensure_range(b, o, 8, label)?;
    let o = o as usize;
    let v = u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    if v > 9_007_199_254_740_991 {
        return Err(ArchiveError::Invalid(format!(
            "Invalid zip archive: {} exceeds the safe integer range",
            label
        )));
    }
    Ok(v)
}

fn find_end_of_central_directory(buf: &[u8]) -> Option<usize> {
    if buf.len() < ZIP_END_MIN_SIZE {
        return None;
    }
    let min = buf
        .len()
        .saturating_sub(ZIP_MAX_COMMENT_SIZE + ZIP_END_MIN_SIZE);
    let mut offset = buf.len() - ZIP_END_MIN_SIZE;
    loop {
        if u32le(buf, offset).ok() == Some(ZIP_END_OF_CENTRAL_DIRECTORY) {
            let comment = u16le(buf, offset + 20).unwrap_or(0) as usize;
            if offset + ZIP_END_MIN_SIZE + comment == buf.len() {
                return Some(offset);
            }
        }
        if offset == min {
            return None;
        }
        offset -= 1;
    }
}

struct CentralDirectory {
    entries: u64,
    offset: u64,
    size: u64,
    trailer_offset: u64,
}

fn read_central_directory(buf: &[u8], end: usize) -> Result<CentralDirectory, ArchiveError> {
    let disk = u16le(buf, end + 4)?;
    let cd_disk = u16le(buf, end + 6)?;
    let entries_on_disk = u16le(buf, end + 8)?;
    let total = u16le(buf, end + 10)?;
    let size = u32le(buf, end + 12)?;
    let offset = u32le(buf, end + 16)?;
    let zip64 =
        entries_on_disk == 0xffff || total == 0xffff || size == 0xffffffff || offset == 0xffffffff;
    if !zip64 {
        if disk != 0 || cd_disk != 0 || entries_on_disk != total {
            return inv("Multi-disk zip archives are not supported");
        }
        return Ok(CentralDirectory {
            entries: total as u64,
            offset: offset as u64,
            size: size as u64,
            trailer_offset: end as u64,
        });
    }
    if disk != 0 || cd_disk != 0 {
        return inv("Multi-disk zip archives are not supported");
    }
    if end < 20 {
        return inv("Invalid zip archive: zip64 locator is out of bounds");
    }
    let locator = end - 20;
    ensure_range(buf, locator as u64, 20, "zip64 locator")?;
    if u32le(buf, locator)? != ZIP64_END_OF_CENTRAL_DIRECTORY_LOCATOR {
        return inv("Invalid zip64 locator");
    }
    if u32le(buf, locator + 4)? != 0 || u32le(buf, locator + 16)? != 1 {
        return inv("Multi-disk zip archives are not supported");
    }
    let z64 = u64le(buf, locator as u64 + 8, "zip64 end offset")?;
    ensure_range(buf, z64, 56, "zip64 end of central directory")?;
    let z = z64 as usize;
    if u32le(buf, z)? != ZIP64_END_OF_CENTRAL_DIRECTORY {
        return inv("Invalid zip64 end of central directory");
    }
    let record_size = u64le(buf, z64 + 4, "zip64 end size")?;
    if record_size < 44 {
        return inv("Invalid zip64 end of central directory");
    }
    ensure_range(buf, z64, record_size + 12, "zip64 end of central directory")?;
    if z64 + record_size + 12 != locator as u64 {
        return inv("Invalid zip64 end of central directory");
    }
    if u32le(buf, z + 16)? != 0 || u32le(buf, z + 20)? != 0 {
        return inv("Multi-disk zip archives are not supported");
    }
    let on_disk = u64le(buf, z64 + 24, "zip64 entries on disk")?;
    let total = u64le(buf, z64 + 32, "zip64 total entries")?;
    if on_disk != total {
        return inv("Multi-disk zip archives are not supported");
    }
    Ok(CentralDirectory {
        entries: total,
        size: u64le(buf, z64 + 40, "zip64 central directory size")?,
        offset: u64le(buf, z64 + 48, "zip64 central directory offset")?,
        trailer_offset: z64,
    })
}

fn normalize_archive_path(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.contains('\0') {
        return None;
    }
    let path = raw.replace('\\', "/");
    if path.starts_with('/') {
        return None;
    }
    let mut chars = path.chars();
    if let (Some(a), Some(':')) = (chars.next(), chars.next()) {
        if a.is_ascii_alphabetic() {
            return None;
        }
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.contains(&"..") {
        return None;
    }
    let normalized = parts
        .iter()
        .filter(|p| !p.is_empty() && **p != ".")
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    if normalized.is_empty() && !path.ends_with('/') {
        return None;
    }
    Some(if path.ends_with('/') && !normalized.is_empty() {
        format!("{}/", normalized)
    } else {
        normalized
    })
}

fn find_extra_field(
    buf: &[u8],
    extra_offset: usize,
    extra_len: usize,
    target: u16,
) -> Result<Option<(usize, usize)>, ArchiveError> {
    let end = extra_offset + extra_len;
    let mut o = extra_offset;
    while o < end {
        if o + 4 > end {
            return inv("Invalid zip extra field");
        }
        let id = u16le(buf, o)?;
        let size = u16le(buf, o + 2)? as usize;
        let data = o + 4;
        ensure_range(buf, data as u64, size as u64, "zip extra field")?;
        if data + size > end {
            return inv("Invalid zip extra field");
        }
        if id == target {
            return Ok(Some((data, size)));
        }
        o = data + size;
    }
    Ok(None)
}

fn decode_file_name(
    bytes: &[u8],
    is_utf8: bool,
    unicode_extra: Option<&[u8]>,
) -> Result<String, ArchiveError> {
    if is_utf8 {
        return String::from_utf8(bytes.to_vec()).map_err(|_| {
            ArchiveError::Invalid("The encoded data was not valid for encoding utf-8".into())
        });
    }
    if let Some(extra) = unicode_extra {
        if extra.len() >= 5
            && extra[0] == 1
            && u32::from_le_bytes([extra[1], extra[2], extra[3], extra[4]])
                == crc32fast::hash(bytes)
        {
            return String::from_utf8(extra[5..].to_vec()).map_err(|_| {
                ArchiveError::Invalid("The encoded data was not valid for encoding utf-8".into())
            });
        }
    }
    let high: Vec<char> = CP437_HIGH.chars().collect();
    Ok(bytes
        .iter()
        .map(|b| {
            if *b < 0x80 {
                *b as char
            } else {
                high[(*b - 0x80) as usize]
            }
        })
        .collect())
}

/// Read every file entry of a zip archive into memory.
pub fn read_zip_archive(
    buf: &[u8],
    limits: &ArchiveLimits,
) -> Result<Vec<(String, Vec<u8>)>, ArchiveError> {
    let end = find_end_of_central_directory(buf)
        .ok_or_else(|| ArchiveError::Invalid("Invalid zip archive".into()))?;
    let cd = read_central_directory(buf, end)?;
    if cd.entries > limits.max_entries {
        return Err(ArchiveError::Validation(format!(
            "Archive contains too many files ({}). Maximum is {}.",
            cd.entries, limits.max_entries
        )));
    }
    ensure_range(buf, cd.offset, cd.size, "central directory")?;
    if cd.offset + cd.size > cd.trailer_offset {
        return inv("Invalid zip archive: central directory overlaps archive trailer");
    }
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut extracted: u64 = 0;
    let mut offset = cd.offset as usize;
    for _ in 0..cd.entries {
        ensure_range(buf, offset as u64, 46, "central directory entry")?;
        if u32le(buf, offset)? != ZIP_CENTRAL_DIRECTORY_HEADER {
            return inv("Invalid zip central directory entry");
        }
        let flags = u16le(buf, offset + 8)?;
        let method = u16le(buf, offset + 10)?;
        let checksum = u32le(buf, offset + 16)?;
        let mut compressed_size = u32le(buf, offset + 20)? as u64;
        let mut uncompressed_size = u32le(buf, offset + 24)? as u64;
        let name_len = u16le(buf, offset + 28)? as usize;
        let extra_len = u16le(buf, offset + 30)? as usize;
        let comment_len = u16le(buf, offset + 32)? as usize;
        let mut disk_start = u16le(buf, offset + 34)? as u64;
        let external = u32le(buf, offset + 38)?;
        let mut local_offset = u32le(buf, offset + 42)? as u64;
        let variable = name_len + extra_len + comment_len;
        ensure_range(
            buf,
            offset as u64 + 46,
            variable as u64,
            "central directory entry data",
        )?;
        let name_start = offset + 46;
        let extra_offset = name_start + name_len;

        let needs_zip64 = uncompressed_size == 0xffffffff
            || compressed_size == 0xffffffff
            || local_offset == 0xffffffff
            || disk_start == 0xffff;
        if !needs_zip64 {
            if disk_start != 0 {
                return inv("Multi-disk zip archives are not supported");
            }
        } else {
            let (zo, zl) = find_extra_field(buf, extra_offset, extra_len, 0x0001)?
                .ok_or_else(|| ArchiveError::Invalid("Invalid zip64 extra field".into()))?;
            let extra = &buf[zo..zo + zl];
            let mut vo = 0usize;
            let mut next = |label: &str| -> Result<u64, ArchiveError> {
                if vo + 8 > extra.len() {
                    return Err(ArchiveError::Invalid(format!(
                        "Invalid zip64 extra field: missing {}",
                        label
                    )));
                }
                let v = u64le(extra, vo as u64, &format!("zip64 {}", label))?;
                vo += 8;
                Ok(v)
            };
            if uncompressed_size == 0xffffffff {
                uncompressed_size = next("uncompressed size")?;
            }
            if compressed_size == 0xffffffff {
                compressed_size = next("compressed size")?;
            }
            if local_offset == 0xffffffff {
                local_offset = next("local header offset")?;
            }
            if disk_start == 0xffff {
                if vo + 4 > extra.len() {
                    return inv("Invalid zip64 extra field: missing disk start");
                }
                disk_start = u32le(extra, vo)? as u64;
            }
            if disk_start != 0 {
                return inv("Multi-disk zip archives are not supported");
            }
        }

        let central_name = &buf[name_start..name_start + name_len];
        let unicode_extra =
            find_extra_field(buf, extra_offset, extra_len, 0x7075)?.map(|(o, l)| &buf[o..o + l]);
        let raw_name = decode_file_name(central_name, flags & 0x800 != 0, unicode_extra)?;
        let file_name = normalize_archive_path(&raw_name).ok_or_else(|| {
            ArchiveError::Validation(format!("Archive contains unsafe path: {}", raw_name))
        })?;
        if flags & 0x1 != 0 {
            return Err(ArchiveError::Validation(
                "Encrypted zip entries are not supported".into(),
            ));
        }
        let file_type = (external >> 16) & 0o170000;
        if file_type != 0 && file_type != 0o100000 && file_type != 0o040000 {
            return Err(ArchiveError::Validation(
                "Archive links are not supported".into(),
            ));
        }
        let is_dir = raw_name.replace('\\', "/").ends_with('/') || file_type == 0o040000;
        offset += 46 + variable;

        extracted += uncompressed_size;
        if extracted > limits.max_extracted_bytes {
            return Err(ArchiveError::Validation(format!(
                "Archive extracts to more than {} bytes.",
                limits.max_extracted_bytes
            )));
        }
        if is_dir {
            continue;
        }

        ensure_range(buf, local_offset, 30, "local file header")?;
        let lo = local_offset as usize;
        if u32le(buf, lo)? != ZIP_LOCAL_FILE_HEADER {
            return inv("Invalid zip local file header");
        }
        let local_flags = u16le(buf, lo + 6)?;
        let local_method = u16le(buf, lo + 8)?;
        let local_name_len = u16le(buf, lo + 26)? as usize;
        let local_extra_len = u16le(buf, lo + 28)? as usize;
        ensure_range(
            buf,
            lo as u64 + 30,
            (local_name_len + local_extra_len) as u64,
            "local file header data",
        )?;
        let local_name = &buf[lo + 30..lo + 30 + local_name_len];
        if local_flags != flags || local_method != method || local_name != central_name {
            return inv("Zip local header does not match central directory");
        }
        let data_offset = (lo + 30 + local_name_len + local_extra_len) as u64;
        ensure_range(buf, data_offset, compressed_size, "file data")?;
        if data_offset + compressed_size > cd.offset {
            return inv("Invalid zip archive: file data overlaps central directory");
        }
        let compressed = &buf[data_offset as usize..(data_offset + compressed_size) as usize];
        let contents: Vec<u8> = match method {
            0 => compressed.to_vec(),
            8 => {
                let mut out = Vec::new();
                let decoder = flate2::read::DeflateDecoder::new(compressed);
                decoder
                    .take(uncompressed_size + 1)
                    .read_to_end(&mut out)
                    .map_err(|e| ArchiveError::Invalid(e.to_string()))?;
                out
            }
            m => {
                return Err(ArchiveError::Invalid(format!(
                    "Unsupported zip compression method: {}",
                    m
                )))
            }
        };
        if contents.len() as u64 != uncompressed_size {
            return inv("Zip entry size mismatch");
        }
        if crc32fast::hash(&contents) != checksum {
            return inv("Zip entry checksum mismatch");
        }
        if let Some(existing) = files.iter_mut().find(|(n, _)| *n == file_name) {
            existing.1 = contents;
        } else {
            files.push((file_name, contents));
        }
    }
    if offset as u64 != cd.offset + cd.size {
        return inv("Invalid zip central directory size");
    }
    Ok(files)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Build a minimal stored (uncompressed) zip for tests.
    pub fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let offset = out.len() as u32;
            let crc = crc32fast::hash(data);
            let mut local = Vec::new();
            local.extend_from_slice(&ZIP_LOCAL_FILE_HEADER.to_le_bytes());
            local.extend_from_slice(&20u16.to_le_bytes());
            local.extend_from_slice(&0u16.to_le_bytes()); // flags
            local.extend_from_slice(&0u16.to_le_bytes()); // method
            local.extend_from_slice(&[0, 0, 0, 0]); // time/date
            local.extend_from_slice(&crc.to_le_bytes());
            local.extend_from_slice(&(data.len() as u32).to_le_bytes());
            local.extend_from_slice(&(data.len() as u32).to_le_bytes());
            local.extend_from_slice(&(name.len() as u16).to_le_bytes());
            local.extend_from_slice(&0u16.to_le_bytes());
            local.extend_from_slice(name.as_bytes());
            local.extend_from_slice(data);
            out.extend_from_slice(&local);

            central.extend_from_slice(&ZIP_CENTRAL_DIRECTORY_HEADER.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&[0, 0, 0, 0]);
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&ZIP_END_OF_CENTRAL_DIRECTORY.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn limits() -> ArchiveLimits {
        ArchiveLimits {
            max_extracted_bytes: 1 << 20,
            max_entries: 100,
        }
    }

    #[test]
    fn reads_stored_zip() {
        let z = build_zip(&[("skill/SKILL.md", b"hello"), ("skill/ref/a.txt", b"x")]);
        let files = read_zip_archive(&z, &limits()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "skill/SKILL.md");
        assert_eq!(files[0].1, b"hello");
    }

    #[test]
    fn rejects_traversal_and_limits() {
        let z = build_zip(&[("../evil", b"x")]);
        assert!(matches!(
            read_zip_archive(&z, &limits()),
            Err(ArchiveError::Validation(_))
        ));
        let z = build_zip(&[("a", b"x"), ("b", b"y")]);
        let tight = ArchiveLimits {
            max_extracted_bytes: 100,
            max_entries: 1,
        };
        assert!(matches!(
            read_zip_archive(&z, &tight),
            Err(ArchiveError::Validation(_))
        ));
        assert!(read_zip_archive(b"not a zip", &limits()).is_err());
    }
}
