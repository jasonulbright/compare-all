//! Minimal PE32+ files for the version and signature checks.

#![allow(clippy::unwrap_used, dead_code)]

const HEADERS: usize = 0x200;
const OPTIONAL: usize = 0x98;

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// A PE32+ file with one section per entry of `sections`, then `overlay`,
/// then, when given, an attribute certificate table holding `certificate`.
pub fn pe(sections: &[&[u8]], overlay: &[u8], certificate: Option<&[u8]>) -> Vec<u8> {
    let mut bytes = vec![0u8; HEADERS];
    bytes[..2].copy_from_slice(b"MZ");
    put32(&mut bytes, 0x3C, 0x80);
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    let coff = 0x84;
    put16(&mut bytes, coff, 0x8664);
    put16(&mut bytes, coff + 2, u16::try_from(sections.len()).unwrap());
    put16(&mut bytes, coff + 16, 240);
    put16(&mut bytes, OPTIONAL, 0x20B);
    put32(&mut bytes, OPTIONAL + 108, 16);
    let table = OPTIONAL + 240;
    for (index, data) in sections.iter().enumerate() {
        let start = bytes.len();
        bytes.extend_from_slice(data);
        while bytes.len() % 0x200 != 0 {
            bytes.push(0);
        }
        let entry = table + index * 40;
        let size = u32::try_from(bytes.len() - start).unwrap();
        put32(&mut bytes, entry + 16, size);
        put32(&mut bytes, entry + 20, u32::try_from(start).unwrap());
    }
    bytes.extend_from_slice(overlay);
    if let Some(certificate) = certificate {
        while bytes.len() % 8 != 0 {
            bytes.push(0);
        }
        let start = bytes.len();
        let length = u32::try_from(8 + certificate.len()).unwrap();
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&0x0200u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(certificate);
        while bytes.len() % 8 != 0 {
            bytes.push(0);
        }
        let size = u32::try_from(bytes.len() - start).unwrap();
        let directory = OPTIONAL + 112 + 4 * 8;
        put32(&mut bytes, directory, u32::try_from(start).unwrap());
        put32(&mut bytes, directory + 4, size);
    }
    bytes
}
