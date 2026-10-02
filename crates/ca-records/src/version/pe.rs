//! The parts of the executable headers a version listing shows.

use crate::bytes::{slice, to_u64, Cursor};
use crate::error::{RecordError, Result};
use crate::limits::Limits;

/// Offset of the field that points at the PE signature.
const E_LFANEW_OFFSET: usize = 0x3C;
/// Optional header magic for a 32 bit image.
const MAGIC_PE32: u16 = 0x010B;
/// Optional header magic for a 64 bit image.
const MAGIC_PE32_PLUS: u16 = 0x020B;
/// Data directory index of the resource table.
const DIR_RESOURCE: usize = 2;
/// Data directory index of the attribute certificate table.
const DIR_CERTIFICATE: usize = 4;
/// Size of one section header.
const SECTION_HEADER_SIZE: usize = 40;
/// Largest section count accepted before the section table is read.
const MAX_SECTIONS: u32 = 4096;

/// Processor a binary is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Machine(pub u16);

impl Machine {
    /// The name of the processor, or the raw code when it has no name here.
    #[must_use]
    pub fn name(self) -> String {
        match self.0 {
            0x0000 => "Unknown".to_owned(),
            0x014C => "x86".to_owned(),
            0x0162 => "MIPS R3000".to_owned(),
            0x0166 => "MIPS R4000".to_owned(),
            0x0169 => "MIPS WCE v2".to_owned(),
            0x01A2 => "Hitachi SH3".to_owned(),
            0x01A6 => "Hitachi SH4".to_owned(),
            0x01C0 => "ARM".to_owned(),
            0x01C2 => "ARM Thumb".to_owned(),
            0x01C4 => "ARM Thumb-2".to_owned(),
            0x01F0 => "PowerPC".to_owned(),
            0x0200 => "Itanium".to_owned(),
            0x0266 => "MIPS16".to_owned(),
            0x0EBC => "EFI byte code".to_owned(),
            0x5032 => "RISC-V 32".to_owned(),
            0x5064 => "RISC-V 64".to_owned(),
            0x8664 => "x64".to_owned(),
            0xAA64 => "ARM64".to_owned(),
            other => format!("0x{other:04X}"),
        }
    }
}

/// Environment a binary runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subsystem(pub u16);

impl Subsystem {
    /// The name of the environment, or the raw code when it has no name here.
    #[must_use]
    pub fn name(self) -> String {
        match self.0 {
            0 => "Unknown".to_owned(),
            1 => "Native".to_owned(),
            2 => "Windows GUI".to_owned(),
            3 => "Windows console".to_owned(),
            5 => "OS/2 console".to_owned(),
            7 => "POSIX console".to_owned(),
            9 => "Windows CE GUI".to_owned(),
            10 => "EFI application".to_owned(),
            11 => "EFI boot service driver".to_owned(),
            12 => "EFI runtime driver".to_owned(),
            13 => "EFI ROM".to_owned(),
            14 => "Xbox".to_owned(),
            16 => "Windows boot application".to_owned(),
            other => format!("0x{other:04X}"),
        }
    }
}

/// Names of the DLL characteristics flags, low bit first.
const DLL_FLAG_NAMES: [(u16, &str); 11] = [
    (0x0020, "High entropy virtual addresses"),
    (0x0040, "Relocatable"),
    (0x0080, "Integrity checks enforced"),
    (0x0100, "Data execution prevention compatible"),
    (0x0200, "No isolation"),
    (0x0400, "No structured exception handling"),
    (0x0800, "No binding"),
    (0x1000, "Runs in an application container"),
    (0x2000, "Windows driver model driver"),
    (0x4000, "Control flow guard"),
    (0x8000, "Terminal server aware"),
];

/// Facts a version listing takes from the headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFacts {
    /// Processor the binary targets.
    pub machine: Machine,
    /// True when the image uses 64 bit addresses.
    pub is_64_bit: bool,
    /// Link time stamp as the header states it, in seconds since the epoch.
    pub time_stamp: u32,
    /// Environment the binary runs under.
    pub subsystem: Subsystem,
    /// Raw DLL characteristics word.
    pub dll_characteristics: u16,
    /// True when the file carries an attribute certificate block.
    ///
    /// Presence only. Nothing here validates the block or decides trust.
    pub has_signature_block: bool,
    /// File offset and size of the resource directory.
    pub resource: Option<Directory>,
}

impl FileFacts {
    /// Names of the DLL characteristics flags that are set.
    #[must_use]
    pub fn dll_flag_names(&self) -> Vec<String> {
        let mut out: Vec<String> = DLL_FLAG_NAMES
            .iter()
            .filter(|(bit, _)| self.dll_characteristics & bit != 0)
            .map(|(_, name)| (*name).to_owned())
            .collect();
        let named: u16 = DLL_FLAG_NAMES
            .iter()
            .map(|(bit, _)| bit)
            .fold(0, |a, b| a | b);
        let rest = self.dll_characteristics & !named;
        if rest != 0 {
            out.push(format!("0x{rest:04X}"));
        }
        out
    }

    /// The word size as a listing shows it.
    #[must_use]
    pub const fn word_size(&self) -> &'static str {
        if self.is_64_bit {
            "64-bit"
        } else {
            "32-bit"
        }
    }
}

/// A resource table position in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Directory {
    /// Address of the table in the loaded image.
    pub virtual_address: u32,
    /// Size of the table.
    pub size: u32,
    /// Offset of the table in the file.
    pub file_offset: usize,
}

/// One entry of the section table, as far as an address map needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Section {
    virtual_address: u32,
    virtual_size: u32,
    raw_offset: u32,
    raw_size: u32,
}

/// The address map and header facts of one binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Image {
    pub(crate) facts: FileFacts,
    sections: Vec<Section>,
}

impl Image {
    /// Translate an address in the loaded image to a file offset.
    pub(crate) fn offset_of(&self, rva: u32) -> Option<usize> {
        for section in &self.sections {
            let end = section.virtual_address.checked_add(section.virtual_size)?;
            if rva >= section.virtual_address && rva < end {
                let delta = rva - section.virtual_address;
                if delta >= section.raw_size {
                    return None;
                }
                return usize::try_from(section.raw_offset.checked_add(delta)?).ok();
            }
        }
        None
    }
}

/// Parse the headers of a Windows binary.
///
/// # Errors
///
/// Returns [`RecordError::Malformed`] when a signature is wrong and
/// [`RecordError::Truncated`] when a header runs past the end of the file.
pub(crate) fn parse(bytes: &[u8], limits: &Limits) -> Result<Image> {
    limits.check_input(to_u64(bytes.len()))?;
    let context = "portable executable header";
    if slice(bytes, 0, 2, context)? != b"MZ" {
        return Err(RecordError::malformed(context, 0));
    }
    let lfanew = u32::from_le_bytes(
        slice(bytes, E_LFANEW_OFFSET, 4, context)?
            .try_into()
            .map_err(|_| RecordError::malformed(context, to_u64(E_LFANEW_OFFSET)))?,
    );
    let pe_offset = usize::try_from(lfanew)
        .map_err(|_| RecordError::malformed(context, to_u64(E_LFANEW_OFFSET)))?;
    if slice(bytes, pe_offset, 4, context)? != b"PE\0\0" {
        return Err(RecordError::malformed(context, to_u64(pe_offset)));
    }

    let mut cursor = Cursor::new(bytes, context);
    cursor.seek(pe_offset + 4)?;
    let machine = Machine(cursor.u16_le()?);
    let section_count = u32::from(cursor.u16_le()?);
    if section_count > MAX_SECTIONS {
        return Err(RecordError::LimitExceeded {
            limit: "sectionCount",
            allowed: u64::from(MAX_SECTIONS),
            requested: u64::from(section_count),
        });
    }
    let time_stamp = cursor.u32_le()?;
    cursor.skip(8)?; // symbol table pointer and count
    let optional_size = usize::from(cursor.u16_le()?);
    cursor.skip(2)?; // characteristics
    let optional_start = cursor.position();

    let magic = cursor.u16_le()?;
    let is_64_bit = match magic {
        MAGIC_PE32 => false,
        MAGIC_PE32_PLUS => true,
        _ => return Err(RecordError::malformed(context, to_u64(optional_start))),
    };
    let subsystem_offset = optional_start + 68;
    let subsystem = Subsystem(u16::from_le_bytes(
        slice(bytes, subsystem_offset, 2, context)?
            .try_into()
            .map_err(|_| RecordError::malformed(context, to_u64(subsystem_offset)))?,
    ));
    let dll_characteristics = u16::from_le_bytes(
        slice(bytes, subsystem_offset + 2, 2, context)?
            .try_into()
            .map_err(|_| RecordError::malformed(context, to_u64(subsystem_offset + 2)))?,
    );
    let directory_count_offset = optional_start + if is_64_bit { 108 } else { 92 };
    let directory_count = u32::from_le_bytes(
        slice(bytes, directory_count_offset, 4, context)?
            .try_into()
            .map_err(|_| RecordError::malformed(context, to_u64(directory_count_offset)))?,
    );
    let directories_offset = directory_count_offset + 4;

    let section_table = optional_start
        .checked_add(optional_size)
        .ok_or_else(|| RecordError::malformed(context, to_u64(optional_start)))?;
    let sections = read_sections(bytes, section_table, section_count)?;

    let resource_dir = read_directory(bytes, directories_offset, directory_count, DIR_RESOURCE)?;
    let certificate = read_directory(bytes, directories_offset, directory_count, DIR_CERTIFICATE)?;

    let image_sections = sections.clone();
    let resource = resource_dir.and_then(|(rva, size)| {
        let probe = Image {
            facts: FileFacts {
                machine,
                is_64_bit,
                time_stamp,
                subsystem,
                dll_characteristics,
                has_signature_block: false,
                resource: None,
            },
            sections: image_sections.clone(),
        };
        probe.offset_of(rva).map(|file_offset| Directory {
            virtual_address: rva,
            size,
            file_offset,
        })
    });

    Ok(Image {
        facts: FileFacts {
            machine,
            is_64_bit,
            time_stamp,
            subsystem,
            dll_characteristics,
            // The certificate table is addressed by file offset, not by an
            // image address, so a non-zero size alone states presence.
            has_signature_block: certificate.is_some_and(|(offset, size)| offset != 0 && size != 0),
            resource,
        },
        sections,
    })
}

fn read_sections(bytes: &[u8], table: usize, count: u32) -> Result<Vec<Section>> {
    let context = "section header";
    let mut sections = Vec::new();
    for index in 0..count {
        let base = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(SECTION_HEADER_SIZE))
            .and_then(|delta| table.checked_add(delta))
            .ok_or_else(|| RecordError::malformed(context, to_u64(table)))?;
        let header = slice(bytes, base, SECTION_HEADER_SIZE, context)?;
        sections.push(Section {
            virtual_size: read_u32(header, 8),
            virtual_address: read_u32(header, 12),
            raw_size: read_u32(header, 16),
            raw_offset: read_u32(header, 20),
        });
    }
    Ok(sections)
}

fn read_directory(
    bytes: &[u8],
    directories_offset: usize,
    count: u32,
    index: usize,
) -> Result<Option<(u32, u32)>> {
    let index_u32 = u32::try_from(index).unwrap_or(u32::MAX);
    if index_u32 >= count {
        return Ok(None);
    }
    let base = directories_offset
        .checked_add(index * 8)
        .ok_or_else(|| RecordError::malformed("data directory", to_u64(directories_offset)))?;
    let entry = slice(bytes, base, 8, "data directory")?;
    let rva = read_u32(entry, 0);
    let size = read_u32(entry, 4);
    if rva == 0 && size == 0 {
        return Ok(None);
    }
    Ok(Some((rva, size)))
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    let mut raw = [0u8; 4];
    for (slot, byte) in raw.iter_mut().zip(data.iter().skip(offset)) {
        *slot = *byte;
    }
    u32::from_le_bytes(raw)
}
