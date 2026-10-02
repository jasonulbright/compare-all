//! Media tags and stream facts.
//!
//! The reader recognises a bare MPEG audio stream with ID3 tags, and the FLAC,
//! Ogg, MP4 and RIFF WAVE containers. It reads tag fields and the stream facts
//! a listing shows. It never decodes audio.
//!
//! Every length in a tag or a container comes from the file, so every one is
//! range checked before use and every allocation is checked against
//! [`crate::limits::Limits`].

pub mod container;
pub mod id3;
pub mod mpeg;

use crate::bytes::to_u64;
use crate::compare::{compare_trees, AlignOptions, TreeDiff};
use crate::error::{RecordError, Result};
use crate::limits::{Limits, Unknown};
use crate::record::{ByteRange, Record, RecordTree, RecordValue};
use serde::{Deserialize, Serialize};

pub use container::{ContainerStream, ContainerTags};
pub use id3::{Id3v1, Id3v2, TagField};
pub use mpeg::{ChannelMode, FrameHeader, MpegVersion, StreamFacts};

/// Largest number of MPEG frames counted while measuring a stream.
const MAX_FRAMES: u64 = 2_000_000;

/// Group name of the ID3 version 1 tag.
const GROUP_ID3V1: &str = "ID3v1";
/// Group name of the ID3 version 2 tag.
const GROUP_ID3V2: &str = "ID3v2";
/// Group name of a container's own metadata.
const GROUP_TAGS: &str = "Tags";
/// Group name of the stream facts.
const GROUP_AUDIO: &str = "Audio";
/// Group name of the file facts.
const GROUP_FILE: &str = "File";

/// Which container a file uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFormat {
    /// A bare MPEG audio stream, with or without ID3 tags.
    Mpeg,
    /// A FLAC stream.
    Flac,
    /// An Ogg stream.
    Ogg,
    /// An MP4 or AAC container.
    Mp4,
    /// A RIFF WAVE file.
    Wav,
}

impl MediaFormat {
    /// The name shown in a listing.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Mpeg => "MPEG audio",
            Self::Flac => "FLAC",
            Self::Ogg => "Ogg",
            Self::Mp4 => "MP4",
            Self::Wav => "WAVE",
        }
    }

    /// Recognise a container from the leading bytes.
    #[must_use]
    pub fn detect(data: &[u8]) -> Option<Self> {
        if data.get(..4) == Some(b"fLaC".as_slice()) {
            return Some(Self::Flac);
        }
        if data.get(..4) == Some(b"OggS".as_slice()) {
            return Some(Self::Ogg);
        }
        if data.get(4..8) == Some(b"ftyp".as_slice()) {
            return Some(Self::Mp4);
        }
        if data.get(..4) == Some(b"RIFF".as_slice()) && data.get(8..12) == Some(b"WAVE".as_slice())
        {
            return Some(Self::Wav);
        }
        if data.get(..3) == Some(b"ID3".as_slice()) {
            return Some(Self::Mpeg);
        }
        if data.len() >= 2 && data[0] == 0xFF && data[1] & 0xE0 == 0xE0 {
            return Some(Self::Mpeg);
        }
        // An ID3 version 1 tag alone still names an MPEG audio file.
        if data.len() >= 128
            && data.get(data.len() - 128..data.len() - 125) == Some(b"TAG".as_slice())
        {
            return Some(Self::Mpeg);
        }
        None
    }
}

/// How a media file is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
// The struct is a settings record: each switch is one independent option, and
// grouping them into sub structures would change the settings document.
#[allow(clippy::struct_excessive_bools)]
pub struct MediaReadOptions {
    /// Add the stream facts group.
    pub include_stream_facts: bool,
    /// Add the tag groups.
    pub include_tags: bool,
    /// Keep the bytes of a binary tag field, so a viewer can open them.
    ///
    /// Off records the size only, which keeps a file full of cover art small
    /// in memory.
    pub keep_binary_payloads: bool,
    /// Count every MPEG frame to measure the stream.
    ///
    /// Off trusts a variable bit rate header where the file carries one.
    pub count_all_frames: bool,
    /// Allocation ceilings applied while reading.
    pub limits: Limits,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: Unknown,
}

impl Default for MediaReadOptions {
    fn default() -> Self {
        Self {
            include_stream_facts: true,
            include_tags: true,
            keep_binary_payloads: false,
            count_all_frames: true,
            limits: Limits::default(),
            unknown: Unknown::new(),
        }
    }
}

/// Options of a media comparison.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaCompareOptions {
    /// Rules shared by every record comparison.
    pub align: AlignOptions,
    /// Leave the tag groups out of the comparison.
    ///
    /// The stream facts and the file facts still compare, so two files with the
    /// same audio and different tags come out the same.
    pub ignore_tags: bool,
    /// Leave the stream facts out of the comparison.
    pub ignore_stream_facts: bool,
    /// Fields written by another build, preserved verbatim.
    #[serde(flatten, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unknown: Unknown,
}

/// What a media read produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaInfo {
    /// Container the file uses.
    pub format: MediaFormat,
    /// Size of the file in bytes.
    pub file_len: u64,
    /// Range of the audio payload, tags excluded.
    pub audio_range: ByteRange,
    /// MPEG stream facts, for a bare MPEG stream.
    pub mpeg: Option<StreamFacts>,
    /// Container stream facts, for every other container.
    pub container: Option<ContainerStream>,
    /// ID3 version 1 tag.
    pub id3v1: Option<Id3v1>,
    /// ID3 version 2 tag.
    pub id3v2: Option<Id3v2>,
    /// Metadata a container carries in its own form.
    pub container_tags: Vec<TagField>,
}

/// Read a media file into a record tree.
///
/// # Errors
///
/// Returns [`RecordError::Unsupported`] for a container this build does not
/// read, [`RecordError::Malformed`] when a signature is wrong,
/// [`RecordError::Truncated`] when a structure claims more bytes than the file
/// holds, and [`RecordError::LimitExceeded`] when a limit refuses the work.
pub fn read(bytes: &[u8], options: &MediaReadOptions) -> Result<RecordTree> {
    let info = read_info(bytes, options)?;
    Ok(build_tree(&info, options))
}

/// Read a media file without building a tree.
///
/// # Errors
///
/// Same as [`read`].
pub fn read_info(bytes: &[u8], options: &MediaReadOptions) -> Result<MediaInfo> {
    let limits = &options.limits;
    limits.check_input(to_u64(bytes.len()))?;
    let Some(format) = MediaFormat::detect(bytes) else {
        return Err(RecordError::unsupported(
            "the file is not a media container this build reads",
        ));
    };
    let file_len = to_u64(bytes.len());
    let mut info = MediaInfo {
        format,
        file_len,
        audio_range: ByteRange::new(0, file_len),
        mpeg: None,
        container: None,
        id3v1: None,
        id3v2: None,
        container_tags: Vec::new(),
    };

    match format {
        MediaFormat::Mpeg => {
            let mut start = 0usize;
            if let Some((_, len)) = id3::v2_span(bytes) {
                info.id3v2 = id3::read_v2(bytes, limits)?;
                start = len.min(bytes.len());
            }
            let mut end = bytes.len();
            info.id3v1 = id3::read_v1(bytes);
            if info.id3v1.is_some() {
                end = end.saturating_sub(128);
            }
            let audio = bytes.get(start..end.max(start)).unwrap_or_default();
            info.audio_range = ByteRange::new(to_u64(start), to_u64(audio.len()));
            if options.include_stream_facts {
                let max = if options.count_all_frames {
                    MAX_FRAMES
                } else {
                    1
                };
                info.mpeg = mpeg::scan(audio, to_u64(start), max)?;
            }
        }
        MediaFormat::Flac => {
            let tags = container::read_flac(bytes, limits)?;
            info.container = tags.stream;
            info.container_tags = tags.fields;
        }
        MediaFormat::Ogg => {
            let tags = container::read_ogg(bytes, limits)?;
            info.container = tags.stream;
            info.container_tags = tags.fields;
        }
        MediaFormat::Mp4 => {
            let tags = container::read_mp4(bytes, limits)?;
            info.container = tags.stream;
            info.container_tags = tags.fields;
        }
        MediaFormat::Wav => {
            let tags = container::read_wav(bytes, limits)?;
            info.container = tags.stream;
            info.container_tags = tags.fields;
        }
    }

    if !options.keep_binary_payloads {
        drop_payloads(&mut info);
    }
    Ok(info)
}

fn drop_payloads(info: &mut MediaInfo) {
    let lists: [&mut Vec<TagField>; 1] = [&mut info.container_tags];
    for list in lists {
        for field in list.iter_mut() {
            if let Some(bytes) = field.binary.take() {
                field.text = Some(format!("{} bytes", bytes.len()));
            }
        }
    }
    if let Some(tag) = info.id3v2.as_mut() {
        for field in &mut tag.frames {
            if let Some(bytes) = field.binary.take() {
                field.text = Some(format!("{} bytes", bytes.len()));
            }
        }
    }
}

fn build_tree(info: &MediaInfo, options: &MediaReadOptions) -> RecordTree {
    let mut root = RecordTree::new(String::new(), "Media".to_owned());

    let mut file = RecordTree::new(GROUP_FILE.to_owned(), GROUP_FILE.to_owned());
    file.push(Record::new(
        GROUP_FILE,
        "Format",
        "ENUM",
        RecordValue::Text(info.format.name().to_owned()),
    ));
    file.push(Record::new(
        GROUP_FILE,
        "Size",
        "INTEGER",
        RecordValue::Integer(i128::from(info.file_len)),
    ));
    file.push(
        Record::new(
            GROUP_FILE,
            "Audio Range",
            "RANGE",
            RecordValue::Integer(i128::from(info.audio_range.len)),
        )
        .with_display(format!(
            "{}..{}",
            info.audio_range.start,
            info.audio_range.end()
        ))
        .with_source(info.audio_range),
    );
    root.push_child(file);

    if options.include_stream_facts {
        let audio = audio_group(info);
        if !audio.records.is_empty() {
            root.push_child(audio);
        }
    }

    if options.include_tags {
        if let Some(tag) = &info.id3v1 {
            root.push_child(tag_group(GROUP_ID3V1, &tag.fields));
        }
        if let Some(tag) = &info.id3v2 {
            let mut group = tag_group(GROUP_ID3V2, &tag.frames);
            group.push(Record::new(
                GROUP_ID3V2,
                "Version",
                "VERSION",
                RecordValue::Text(format!("2.{}.{}", tag.major, tag.revision)),
            ));
            root.push_child(group);
        }
        if !info.container_tags.is_empty() {
            root.push_child(tag_group(GROUP_TAGS, &info.container_tags));
        }
    }

    root.sort_by_name();
    root
}

fn audio_group(info: &MediaInfo) -> RecordTree {
    if let Some(facts) = info.mpeg {
        return mpeg_group(&facts);
    }
    if let Some(stream) = info.container {
        return container_group(&stream);
    }
    RecordTree::new(GROUP_AUDIO.to_owned(), GROUP_AUDIO.to_owned())
}

fn mpeg_group(facts: &StreamFacts) -> RecordTree {
    let mut audio = RecordTree::new(GROUP_AUDIO.to_owned(), GROUP_AUDIO.to_owned());
    audio.push(Record::new(
        GROUP_AUDIO,
        "Version",
        "ENUM",
        RecordValue::Text(facts.first.version.name().to_owned()),
    ));
    audio.push(Record::new(
        GROUP_AUDIO,
        "Layer",
        "INTEGER",
        RecordValue::Integer(i128::from(facts.first.layer)),
    ));
    audio.push(
        Record::new(
            GROUP_AUDIO,
            "Bit Rate",
            "INTEGER",
            RecordValue::Integer(i128::from(facts.average_bit_rate)),
        )
        .with_display(format!(
            "{} kbps{}",
            facts.average_bit_rate / 1000,
            if facts.variable_bit_rate {
                " (variable)"
            } else {
                ""
            }
        )),
    );
    audio.push(
        Record::new(
            GROUP_AUDIO,
            "Sample Rate",
            "INTEGER",
            RecordValue::Integer(i128::from(facts.first.sample_rate)),
        )
        .with_display(format!("{} Hz", facts.first.sample_rate)),
    );
    audio.push(
        Record::new(
            GROUP_AUDIO,
            "Channels",
            "INTEGER",
            RecordValue::Integer(i128::from(facts.first.channel_mode.count())),
        )
        .with_display(facts.first.channel_mode.name().to_owned()),
    );
    audio.push(Record::new(
        GROUP_AUDIO,
        "Frames",
        "INTEGER",
        RecordValue::Integer(i128::from(facts.frame_count)),
    ));
    audio.push(
        Record::new(
            GROUP_AUDIO,
            "Duration",
            "DURATION",
            RecordValue::Integer(i128::from(facts.duration_ms)),
        )
        .with_display(duration_text(facts.duration_ms)),
    );
    audio
}

fn container_group(stream: &ContainerStream) -> RecordTree {
    let mut audio = RecordTree::new(GROUP_AUDIO.to_owned(), GROUP_AUDIO.to_owned());
    audio.push(
        Record::new(
            GROUP_AUDIO,
            "Sample Rate",
            "INTEGER",
            RecordValue::Integer(i128::from(stream.sample_rate)),
        )
        .with_display(format!("{} Hz", stream.sample_rate)),
    );
    audio.push(Record::new(
        GROUP_AUDIO,
        "Channels",
        "INTEGER",
        RecordValue::Integer(i128::from(stream.channels)),
    ));
    if stream.bits_per_sample > 0 {
        audio.push(Record::new(
            GROUP_AUDIO,
            "Bits Per Sample",
            "INTEGER",
            RecordValue::Integer(i128::from(stream.bits_per_sample)),
        ));
    }
    if stream.bit_rate > 0 {
        audio.push(
            Record::new(
                GROUP_AUDIO,
                "Bit Rate",
                "INTEGER",
                RecordValue::Integer(i128::from(stream.bit_rate)),
            )
            .with_display(format!("{} kbps", stream.bit_rate / 1000)),
        );
    }
    audio.push(
        Record::new(
            GROUP_AUDIO,
            "Duration",
            "DURATION",
            RecordValue::Integer(i128::from(stream.duration_ms)),
        )
        .with_display(duration_text(stream.duration_ms)),
    );
    audio
}

fn tag_group(path: &str, fields: &[TagField]) -> RecordTree {
    let mut group = RecordTree::new(path.to_owned(), path.to_owned());
    for field in fields {
        let name = if field.name == "Frame" || field.name == "Field" {
            field.id.clone()
        } else {
            field.name.clone()
        };
        let (value, display) = match (&field.text, &field.binary) {
            (Some(text), _) => (RecordValue::Text(text.clone()), text.clone()),
            (None, Some(bytes)) => (
                RecordValue::Bytes(bytes.clone()),
                format!("{} bytes", bytes.len()),
            ),
            (None, None) => (RecordValue::Empty, String::new()),
        };
        group.push(
            Record::new(path, name, field.id.clone(), value)
                .with_display(display)
                .with_source(field.source),
        );
    }
    group
}

fn duration_text(ms: u64) -> String {
    let seconds = ms / 1000;
    let minutes = seconds / 60;
    format!("{}:{:02}", minutes, seconds % 60)
}

/// Compare two media trees.
///
/// The option to leave tags out drops the tag groups from both sides before
/// the comparison, so an orphan tag group does not reappear as a difference.
#[must_use]
pub fn compare(left: &RecordTree, right: &RecordTree, options: &MediaCompareOptions) -> TreeDiff {
    if !options.ignore_tags && !options.ignore_stream_facts {
        return compare_trees(left, right, &options.align);
    }
    let left = filtered(left, options);
    let right = filtered(right, options);
    compare_trees(&left, &right, &options.align)
}

fn filtered(tree: &RecordTree, options: &MediaCompareOptions) -> RecordTree {
    let mut out = tree.clone();
    out.children.retain(|child| {
        let is_tag = matches!(child.name.as_str(), GROUP_ID3V1 | GROUP_ID3V2 | GROUP_TAGS);
        let is_audio = child.name == GROUP_AUDIO;
        !((options.ignore_tags && is_tag) || (options.ignore_stream_facts && is_audio))
    });
    out
}
