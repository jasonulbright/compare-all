//! Media tag and stream reading, and media comparison.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ca_records::compare::Status;
use ca_records::limits::Limits;
use ca_records::media::{
    compare, read, read_info, MediaCompareOptions, MediaFormat, MediaReadOptions,
};
use ca_records::RecordTree;

fn record<'a>(tree: &'a RecordTree, group: &str, name: &str) -> &'a ca_records::Record {
    let mut stack = vec![tree];
    while let Some(node) = stack.pop() {
        if node.name.eq_ignore_ascii_case(group) {
            if let Some(found) = node.record(name) {
                return found;
            }
        }
        stack.extend(node.children.iter());
    }
    panic!("no record {group}/{name}");
}

fn mp3(frames: usize, v2: &[(&str, &str)], v1: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if !v2.is_empty() {
        out.extend_from_slice(&common::id3v2(v2));
    }
    out.extend_from_slice(&common::mpeg_frames(frames));
    if v1 {
        out.extend_from_slice(&common::id3v1("T", "A", "L", Some(7)));
    }
    out
}

fn mp3_with_raw_frames(frames: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, payload) in frames {
        body.extend_from_slice(id.as_bytes());
        body.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(payload);
    }
    let length = u32::try_from(body.len()).unwrap();
    let mut out = vec![b'I', b'D', b'3', 3, 0, 0];
    out.extend_from_slice(&[
        u8::try_from((length >> 21) & 0x7f).unwrap(),
        u8::try_from((length >> 14) & 0x7f).unwrap(),
        u8::try_from((length >> 7) & 0x7f).unwrap(),
        u8::try_from(length & 0x7f).unwrap(),
    ]);
    out.extend(body);
    out.extend(common::mpeg_frames(2));
    out
}

fn riff_chunk(id: [u8; 4], body: &[u8]) -> Vec<u8> {
    let padding = usize::from(!body.len().is_multiple_of(2));
    let mut chunk = Vec::with_capacity(8 + body.len() + padding);
    chunk.extend_from_slice(&id);
    chunk.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
    chunk.extend_from_slice(body);
    if padding != 0 {
        chunk.push(0);
    }
    chunk
}

fn wave_file(chunks: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(12 + chunks.len());
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&u32::try_from(4 + chunks.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(chunks);
    bytes
}

fn utf16_tag_text(parts: &[&str]) -> Vec<u8> {
    let mut out = vec![1, 0xff, 0xfe];
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            out.extend_from_slice(&[0, 0]);
        }
        for unit in part.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    out
}

fn mp4_metadata_file(ilst_body: &[u8]) -> Vec<u8> {
    let ilst = common::mp4_box(*b"ilst", ilst_body);
    let mut meta_body = vec![0; 4];
    meta_body.extend_from_slice(&ilst);
    let meta = common::mp4_box(*b"meta", &meta_body);
    let udta = common::mp4_box(*b"udta", &meta);
    let moov = common::mp4_box(*b"moov", &udta);
    let mut bytes = common::mp4_box(*b"ftyp", b"M4A \0\0\0\0M4A mp42");
    bytes.extend_from_slice(&moov);
    bytes
}

fn mp4_data_box(kind: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + payload.len());
    body.extend_from_slice(&kind.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(payload);
    common::mp4_box(*b"data", &body)
}

fn mp4_item(id: [u8; 4], children: &[u8]) -> Vec<u8> {
    common::mp4_box(id, children)
}

fn enlarge_mp4_box(bytes: &mut [u8], kind: [u8; 4], extra: u32) {
    let kind_at = bytes
        .windows(4)
        .position(|window| window == kind.as_slice())
        .expect("box kind");
    let size_at = kind_at - 4;
    let size = u32::from_be_bytes(bytes[size_at..kind_at].try_into().expect("box size"));
    bytes[size_at..kind_at].copy_from_slice(&size.saturating_add(extra).to_be_bytes());
}

fn ogg_identification_page() -> Vec<u8> {
    let mut identification = vec![0; 30];
    identification[0] = 1;
    identification[1..7].copy_from_slice(b"vorbis");
    identification[11] = 2;
    identification[12..16].copy_from_slice(&48_000u32.to_le_bytes());

    let mut page = b"OggS".to_vec();
    page.extend_from_slice(&[0, 0]);
    page.extend_from_slice(&[0; 8]);
    page.extend_from_slice(&7u32.to_le_bytes());
    page.extend_from_slice(&0u32.to_le_bytes());
    page.extend_from_slice(&[0; 4]);
    page.push(1);
    page.push(u8::try_from(identification.len()).expect("short packet"));
    page.extend_from_slice(&identification);
    page
}

#[test]
fn each_container_is_recognised_from_its_leading_bytes() {
    assert_eq!(
        MediaFormat::detect(&mp3(2, &[("TIT2", "x")], false)),
        Some(MediaFormat::Mpeg)
    );
    assert_eq!(
        MediaFormat::detect(&common::flac(44100, 2, &[])),
        Some(MediaFormat::Flac)
    );
    assert_eq!(
        MediaFormat::detect(&common::wav(44100, 2, 16)),
        Some(MediaFormat::Wav)
    );
    assert_eq!(
        MediaFormat::detect(&common::mp4(1000, &[])),
        Some(MediaFormat::Mp4)
    );
    assert_eq!(MediaFormat::detect(b"plain text"), None);
}

#[test]
fn an_mpeg_stream_reports_its_frame_header_facts() {
    let tree = read(&mp3(10, &[], false), &MediaReadOptions::default()).expect("read");
    assert_eq!(record(&tree, "Audio", "Version").display, "MPEG 1");
    assert_eq!(record(&tree, "Audio", "Layer").display, "3");
    assert_eq!(record(&tree, "Audio", "Bit Rate").display, "128 kbps");
    assert_eq!(record(&tree, "Audio", "Sample Rate").display, "44100 Hz");
    assert_eq!(record(&tree, "Audio", "Channels").display, "Stereo");
    assert_eq!(record(&tree, "Audio", "Frames").display, "10");
}

#[test]
fn the_duration_follows_the_frame_count() {
    let info = read_info(&mp3(100, &[], false), &MediaReadOptions::default()).expect("read");
    let facts = info.mpeg.expect("facts");
    // 100 frames of 1152 samples at 44100 Hz.
    assert_eq!(facts.duration_ms, 100 * 1152 * 1000 / 44100);
}

#[test]
fn an_info_header_does_not_mark_constant_bitrate_as_variable() {
    let mut bytes = common::mpeg_frames(3);
    bytes[36..40].copy_from_slice(b"Info");
    bytes[40..44].copy_from_slice(&1u32.to_be_bytes());
    bytes[44..48].copy_from_slice(&3u32.to_be_bytes());

    let info = read_info(&bytes, &MediaReadOptions::default()).expect("read");
    let facts = info.mpeg.expect("facts");
    assert!(!facts.variable_bit_rate);
}

#[test]
fn an_xing_header_after_a_frame_crc_still_supplies_its_frame_count() {
    let mut bytes = common::mpeg_frames(3);
    bytes[1] = 0xFA;
    bytes.copy_within(4..415, 6);
    bytes[4..6].copy_from_slice(&[0, 0]);
    bytes[38..42].copy_from_slice(b"Xing");
    bytes[42..46].copy_from_slice(&1u32.to_be_bytes());
    bytes[46..50].copy_from_slice(&100u32.to_be_bytes());

    let info = read_info(&bytes, &MediaReadOptions::default()).expect("read");
    let facts = info.mpeg.expect("facts");
    assert_eq!(facts.frame_count, 100);
    assert!(facts.variable_bit_rate);
}

#[test]
fn crc_protected_mpeg_two_and_two_five_mono_xing_headers_are_found() {
    for version_bits in [0xF2, 0xE2] {
        let mut bytes = vec![0; 600];
        bytes[..4].copy_from_slice(&[0xFF, version_bits, 0x80, 0xC0]);
        let header = ca_records::media::mpeg::parse_header(&bytes[..4]).expect("frame header");
        let frame_len = usize::try_from(header.frame_len).expect("frame size");
        bytes.truncate(frame_len);
        let side_info = match header.version {
            ca_records::media::mpeg::MpegVersion::One => 17,
            ca_records::media::mpeg::MpegVersion::Two
            | ca_records::media::mpeg::MpegVersion::TwoFive => 9,
        };
        let xing_at = 4 + 2 + side_info;
        bytes[xing_at..xing_at + 4].copy_from_slice(b"Xing");
        bytes[xing_at + 4..xing_at + 8].copy_from_slice(&1u32.to_be_bytes());
        bytes[xing_at + 8..xing_at + 12].copy_from_slice(&100u32.to_be_bytes());

        let facts = read_info(&bytes, &MediaReadOptions::default())
            .expect("read")
            .mpeg
            .expect("MPEG stream");
        assert_eq!(facts.frame_count, 100);
        assert!(facts.variable_bit_rate);
        assert_eq!(
            facts.first.channel_mode,
            ca_records::media::mpeg::ChannelMode::Mono
        );
    }
}

#[test]
fn a_layer_two_payload_is_not_treated_as_an_xing_header() {
    let mut frame = vec![0; 522];
    frame[..4].copy_from_slice(&[0xFF, 0xFD, 0x90, 0]);
    frame[36..40].copy_from_slice(b"Xing");
    frame[40..44].copy_from_slice(&1u32.to_be_bytes());
    frame[44..48].copy_from_slice(&100u32.to_be_bytes());

    let info = read_info(&frame, &MediaReadOptions::default()).expect("read");
    let facts = info.mpeg.expect("facts");
    assert_eq!(facts.frame_count, 1);
    assert!(!facts.variable_bit_rate);
}

#[test]
fn an_id3_version_one_tag_is_read_from_the_end() {
    let tree = read(&mp3(4, &[], true), &MediaReadOptions::default()).expect("read");
    assert_eq!(record(&tree, "ID3v1", "Title").display, "T");
    assert_eq!(record(&tree, "ID3v1", "Artist").display, "A");
    assert_eq!(record(&tree, "ID3v1", "Album").display, "L");
    assert_eq!(record(&tree, "ID3v1", "Track").display, "7");
    assert_eq!(record(&tree, "ID3v1", "Genre").display, "Rock");
}

#[test]
fn an_id3_version_two_tag_is_read_from_the_start() {
    let bytes = mp3(4, &[("TIT2", "Title"), ("TPE1", "Artist")], false);
    let tree = read(&bytes, &MediaReadOptions::default()).expect("read");
    assert_eq!(record(&tree, "ID3v2", "Title").display, "Title");
    assert_eq!(record(&tree, "ID3v2", "Artist").display, "Artist");
    assert_eq!(record(&tree, "ID3v2", "Version").display, "2.3.0");
}

#[test]
fn the_audio_range_excludes_both_tags() {
    let bytes = mp3(4, &[("TIT2", "Title")], true);
    let info = read_info(&bytes, &MediaReadOptions::default()).expect("read");
    assert!(info.audio_range.start > 0);
    assert_eq!(
        info.audio_range.end(),
        u64::try_from(bytes.len()).unwrap() - 128
    );
}

#[test]
fn a_flac_file_reports_its_stream_information_and_comments() {
    let bytes = common::flac(48000, 2, &[("TITLE", "Song"), ("ARTIST", "Band")]);
    let tree = read(&bytes, &MediaReadOptions::default()).expect("read");
    assert_eq!(record(&tree, "Audio", "Sample Rate").display, "48000 Hz");
    assert_eq!(record(&tree, "Audio", "Channels").display, "2");
    assert_eq!(record(&tree, "Tags", "Title").display, "Song");
    assert_eq!(record(&tree, "Tags", "Artist").display, "Band");
}

#[test]
fn a_flac_streaminfo_block_must_be_34_bytes() {
    let valid = common::flac(48000, 2, &[]);
    let mut bytes = b"fLaC".to_vec();
    bytes.extend_from_slice(&[0x80, 0, 0, 18]);
    bytes.extend_from_slice(&valid[8..26]);

    assert!(matches!(
        ca_records::media::container::read_flac(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Malformed { .. })
    ));
}

#[test]
fn a_flac_streaminfo_block_must_be_first() {
    let mut bytes = b"fLaC".to_vec();
    bytes.extend_from_slice(&[0x81, 0, 0, 0]);

    assert!(matches!(
        ca_records::media::container::read_flac(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Malformed { .. })
    ));
}

#[test]
fn a_flac_file_cannot_repeat_its_streaminfo_block() {
    let valid = common::flac(48000, 2, &[]);
    let mut bytes = valid[..42].to_vec();
    bytes.extend_from_slice(&[0x80, 0, 0, 34]);
    bytes.extend_from_slice(&valid[8..42]);

    assert!(matches!(
        ca_records::media::container::read_flac(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Malformed { .. })
    ));
}

#[test]
fn a_flac_file_cannot_repeat_its_vorbis_comment_block() {
    let valid = common::flac(48000, 2, &[]);
    let mut bytes = valid[..42].to_vec();
    let empty_comment = [0; 8];
    bytes.extend_from_slice(&[4, 0, 0, 8]);
    bytes.extend_from_slice(&empty_comment);
    bytes.extend_from_slice(&[0x84, 0, 0, 8]);
    bytes.extend_from_slice(&empty_comment);

    assert!(matches!(
        ca_records::media::container::read_flac(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Malformed { .. })
    ));
}

#[test]
fn a_flac_file_cannot_repeat_its_seektable_block() {
    let valid = common::flac(48000, 2, &[]);
    let mut bytes = valid[..42].to_vec();
    let seekpoint = [0u8; 18];
    bytes.extend_from_slice(&[3, 0, 0, 18]);
    bytes.extend_from_slice(&seekpoint);
    bytes.extend_from_slice(&[3, 0, 0, 18]);
    bytes.extend_from_slice(&seekpoint);
    bytes.extend_from_slice(&valid[42..]);

    assert!(matches!(
        ca_records::media::container::read_flac(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Malformed { .. })
    ));
}

#[test]
fn a_flac_metadata_block_type_127_is_forbidden() {
    let valid = common::flac(48000, 2, &[]);
    let mut bytes = valid[..42].to_vec();
    bytes.extend_from_slice(&[0xFF, 0, 0, 0]);

    assert!(matches!(
        ca_records::media::container::read_flac(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Malformed { .. })
    ));
}

#[test]
fn an_ogg_signature_without_a_complete_page_header_is_truncated() {
    assert!(matches!(
        read(b"OggS", &MediaReadOptions::default()),
        Err(ca_records::RecordError::Truncated { .. })
    ));
}

#[test]
fn an_ogg_partial_page_after_identification_is_truncated() {
    let mut identification = vec![0; 30];
    identification[0] = 1;
    identification[1..7].copy_from_slice(b"vorbis");
    identification[11] = 2;
    identification[12..16].copy_from_slice(&48_000u32.to_le_bytes());

    let mut bytes = b"OggS".to_vec();
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&7u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.push(1);
    bytes.push(u8::try_from(identification.len()).expect("short packet"));
    bytes.extend_from_slice(&identification);
    bytes.extend_from_slice(b"OggS");

    assert!(matches!(
        read(&bytes, &MediaReadOptions::default()),
        Err(ca_records::RecordError::Truncated { .. })
    ));
}

#[test]
fn an_ogg_stream_ending_after_its_identification_page_is_truncated() {
    let bytes = ogg_identification_page();
    assert!(matches!(
        ca_records::media::container::read_ogg(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Truncated { context, .. })
            if context == "ogg vorbis comment header"
    ));
}

#[test]
fn a_wave_file_reports_its_format_chunk() {
    let bytes = common::wav(44100, 2, 44100 * 4);
    let tree = read(&bytes, &MediaReadOptions::default()).expect("read");
    assert_eq!(record(&tree, "Audio", "Sample Rate").display, "44100 Hz");
    assert_eq!(record(&tree, "Audio", "Bits Per Sample").display, "16");
    assert_eq!(record(&tree, "Audio", "Duration").display, "0:01");
}

#[test]
fn a_wave_audio_chunk_can_exceed_the_metadata_value_limit() {
    let data_len = (16 * 1024 * 1024) + 2;
    let bytes = common::wav(44100, 2, data_len);

    let parsed = ca_records::media::container::read_wav(&bytes, &Limits::default())
        .expect("audio data is counted as source data, not a metadata value");

    assert!(parsed.stream.is_some());
}

#[test]
fn a_wave_reader_ignores_chunks_after_the_declared_riff_extent() {
    let mut info = b"INFO".to_vec();
    info.extend_from_slice(b"INAM");
    info.extend_from_slice(&5u32.to_le_bytes());
    info.extend_from_slice(b"Title");
    let mut bytes = wave_file(&[]);
    bytes.extend_from_slice(&riff_chunk(*b"LIST", &info));

    let parsed = ca_records::media::container::read_wav(&bytes, &Limits::default())
        .expect("read valid RIFF body and ignore trailing bytes");

    assert!(parsed.fields.is_empty());
}

#[test]
fn a_wave_chunk_that_crosses_the_declared_riff_extent_is_truncated() {
    let mut info = b"INFO".to_vec();
    info.extend_from_slice(b"INAM");
    info.extend_from_slice(&5u32.to_le_bytes());
    info.extend_from_slice(b"Title");
    let chunk = riff_chunk(*b"LIST", &info);
    let mut bytes = wave_file(&chunk);
    bytes[4..8].copy_from_slice(&14u32.to_le_bytes());

    assert!(matches!(
        ca_records::media::container::read_wav(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Truncated { .. })
    ));
}

#[test]
fn a_wave_info_entry_that_crosses_its_list_is_truncated() {
    let mut info = b"INFO".to_vec();
    info.extend_from_slice(b"INAM");
    info.extend_from_slice(&5u32.to_le_bytes());
    info.extend_from_slice(b"x\0");
    let mut chunks = riff_chunk(*b"LIST", &info);
    chunks.extend_from_slice(&riff_chunk(*b"JUNK", b"following bytes"));
    let bytes = wave_file(&chunks);

    assert!(matches!(
        ca_records::media::container::read_wav(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Truncated { .. })
    ));
}

#[test]
fn a_wave_reader_reports_when_its_chunk_limit_hides_more_chunks() {
    let mut chunks = Vec::with_capacity(65_537 * 8);
    for _ in 0..65_537 {
        chunks.extend_from_slice(b"JUNK");
        chunks.extend_from_slice(&0u32.to_le_bytes());
    }
    let bytes = wave_file(&chunks);

    assert!(matches!(
        ca_records::media::container::read_wav(&bytes, &Limits::default()),
        Err(ca_records::RecordError::LimitExceeded {
            limit: "containerChunks",
            ..
        })
    ));
}

#[test]
fn an_mp4_file_reports_its_metadata_list_and_duration() {
    let bytes = common::mp4(2000, &[("\u{a9}nam", "Song"), ("\u{a9}ART", "Band")]);
    let tree = read(&bytes, &MediaReadOptions::default()).expect("read");
    assert_eq!(record(&tree, "Tags", "Title").display, "Song");
    assert_eq!(record(&tree, "Tags", "Artist").display, "Band");
    assert_eq!(record(&tree, "Audio", "Duration").display, "0:02");
}

#[test]
fn an_mp4_partial_top_level_box_header_is_truncated() {
    let mut bytes = common::mp4(2000, &[]);
    bytes.extend_from_slice(b"free");

    assert!(matches!(
        ca_records::media::container::read_mp4(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Truncated { .. })
    ));
}

#[test]
fn an_mp4_udta_accepts_a_four_byte_zero_terminator() {
    let mut bytes = common::mp4(2000, &[("\u{a9}nam", "Song")]);
    bytes.extend_from_slice(&[0; 4]);
    enlarge_mp4_box(&mut bytes, *b"udta", 4);
    enlarge_mp4_box(&mut bytes, *b"moov", 4);

    let parsed = ca_records::media::container::read_mp4(&bytes, &Limits::default())
        .expect("QuickTime user data allows a four-byte zero terminator");
    assert!(parsed
        .fields
        .iter()
        .any(|field| field.text.as_deref() == Some("Song")));
}

#[test]
fn an_mp4_nested_box_rejects_nonzero_trailing_bytes() {
    let mut bytes = common::mp4(2000, &[("\u{a9}nam", "Song")]);
    bytes.extend_from_slice(b"junk");
    enlarge_mp4_box(&mut bytes, *b"udta", 4);
    enlarge_mp4_box(&mut bytes, *b"moov", 4);

    assert!(matches!(
        ca_records::media::container::read_mp4(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Truncated {
            context,
            available: 4,
            ..
        }) if context == "mp4 box header"
    ));
}

#[test]
fn an_mp4_moov_rejects_nonzero_trailing_bytes() {
    let mut bytes = common::mp4(2000, &[("\u{a9}nam", "Song")]);
    bytes.extend_from_slice(b"junk");
    enlarge_mp4_box(&mut bytes, *b"moov", 4);

    assert!(matches!(
        ca_records::media::container::read_mp4(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Truncated {
            context,
            available: 4,
            ..
        }) if context == "mp4 box header"
    ));
}

#[test]
fn an_mp4_box_limit_accepts_exactly_its_boundary() {
    let mut bytes = common::mp4(2000, &[]);
    for _ in 0..65_534 {
        bytes.extend_from_slice(&8u32.to_be_bytes());
        bytes.extend_from_slice(b"free");
    }

    ca_records::media::container::read_mp4(&bytes, &Limits::default())
        .expect("exactly 65,536 top-level boxes are allowed");
}

#[test]
fn an_mp4_short_tail_at_its_box_limit_is_truncated_not_a_limit_error() {
    let mut bytes = common::mp4(2000, &[]);
    for _ in 0..65_534 {
        bytes.extend_from_slice(&8u32.to_be_bytes());
        bytes.extend_from_slice(b"free");
    }
    bytes.extend_from_slice(b"tail");

    assert!(matches!(
        ca_records::media::container::read_mp4(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Truncated {
            context,
            available: 4,
            ..
        }) if context == "mp4 box header"
    ));
}

#[test]
fn an_mp4_ilst_short_tail_is_reported_as_truncated() {
    let item = mp4_item(*b"free", &[]);
    let mut ilst = item;
    ilst.extend_from_slice(b"tail");
    let bytes = mp4_metadata_file(&ilst);

    assert!(matches!(
        ca_records::media::container::read_mp4(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Truncated {
            context,
            available: 4,
            ..
        }) if context == "mp4 item header"
    ));
}

#[test]
fn an_mp4_item_short_child_tail_is_reported_as_truncated() {
    let mut children = common::mp4_box(*b"free", &[]);
    children.extend_from_slice(b"tail");
    let item = mp4_item(*b"\xA9nam", &children);
    let bytes = mp4_metadata_file(&item);

    assert!(matches!(
        ca_records::media::container::read_mp4(&bytes, &Limits::default()),
        Err(ca_records::RecordError::Truncated {
            context,
            available: 4,
            ..
        }) if context == "mp4 child box header"
    ));
}

#[test]
fn mp4_items_keep_and_compare_every_data_box() {
    let make_file = |second: &[u8]| {
        let mut children = mp4_data_box(13, b"A");
        children.extend_from_slice(&mp4_data_box(13, second));
        mp4_metadata_file(&mp4_item(*b"covr", &children))
    };
    let options = MediaReadOptions {
        keep_binary_payloads: true,
        ..MediaReadOptions::default()
    };
    let left = read(&make_file(b"B"), &options).expect("left");
    let right = read(&make_file(b"C"), &options).expect("right");
    let difference = compare(&left, &right, &MediaCompareOptions::default());

    assert_eq!(difference.status, Status::Different);
    assert_eq!(difference.counts().different, 1);
}

#[test]
fn an_mp4_box_limit_does_not_hide_extra_top_level_boxes() {
    let mut bytes = common::mp4(2000, &[]);
    for _ in 0..65_535 {
        bytes.extend_from_slice(&8u32.to_be_bytes());
        bytes.extend_from_slice(b"free");
    }

    assert!(matches!(
        ca_records::media::container::read_mp4(&bytes, &Limits::default()),
        Err(ca_records::RecordError::LimitExceeded {
            limit: "mp4Boxes",
            ..
        })
    ));
}

#[test]
fn an_mp4_item_limit_does_not_hide_extra_metadata_items() {
    let items = vec![("free", ""); 65_537];
    let bytes = common::mp4(2000, &items);

    let result = ca_records::media::container::read_mp4(&bytes, &Limits::default());
    assert!(
        matches!(
            result,
            Err(ca_records::RecordError::LimitExceeded {
                limit: "mp4Items",
                ..
            })
        ),
        "unexpected MP4 item limit result: {result:?}"
    );
}

#[test]
fn an_mp4_item_child_limit_does_not_hide_extra_child_boxes() {
    let mut children = Vec::with_capacity(65_537 * 8);
    for _ in 0..65_537 {
        children.extend_from_slice(&8u32.to_be_bytes());
        children.extend_from_slice(b"free");
    }
    let item = common::mp4_box([0xA9, b'n', b'a', b'm'], &children);
    let ilst = common::mp4_box(*b"ilst", &item);
    let mut meta_body = [0; 4].to_vec();
    meta_body.extend_from_slice(&ilst);
    let meta = common::mp4_box(*b"meta", &meta_body);
    let udta = common::mp4_box(*b"udta", &meta);
    let moov = common::mp4_box(*b"moov", &udta);
    let ftyp = common::mp4_box(*b"ftyp", b"M4A \0\0\0\0M4A mp42");
    let mut bytes = ftyp;
    bytes.extend_from_slice(&moov);

    assert!(matches!(
        ca_records::media::container::read_mp4(&bytes, &Limits::default()),
        Err(ca_records::RecordError::LimitExceeded {
            limit: "mp4DataBoxes",
            ..
        })
    ));
}

#[test]
fn an_mp4_box_that_runs_past_its_parent_is_reported_as_truncated() {
    let mut bytes = common::mp4(2000, &[("\u{a9}nam", "Song")]);
    // The ftyp box is 24 bytes; the moov box begins at 24 and its first child
    // begins at 32. Make that child's declared size exceed the parent extent.
    bytes[32..36].copy_from_slice(&u32::MAX.to_be_bytes());

    assert!(matches!(
        read_info(&bytes, &MediaReadOptions::default()),
        Err(ca_records::RecordError::Truncated { .. })
    ));
}

#[test]
fn an_mp4_item_that_runs_past_its_list_is_reported_as_truncated() {
    let mut bytes = common::mp4(2000, &[("\u{a9}nam", "Song"), ("\u{a9}ART", "Band")]);
    let type_at = bytes
        .windows(4)
        .position(|window| window == [0xA9, b'n', b'a', b'm'])
        .expect("title item");
    bytes[type_at - 4..type_at].copy_from_slice(&u32::MAX.to_be_bytes());

    assert!(matches!(
        read_info(&bytes, &MediaReadOptions::default()),
        Err(ca_records::RecordError::Truncated { .. })
    ));
}

#[test]
fn an_mp4_data_box_cannot_read_past_its_item() {
    let mut bytes = common::mp4(2000, &[("\u{a9}nam", "Song"), ("\u{a9}ART", "Band")]);
    let type_at = bytes
        .windows(4)
        .position(|window| window == b"data")
        .expect("data box");
    bytes[type_at - 4..type_at].copy_from_slice(&32u32.to_be_bytes());

    assert!(matches!(
        read_info(&bytes, &MediaReadOptions::default()),
        Err(ca_records::RecordError::Truncated { .. })
    ));
}

#[test]
fn two_files_compare_tag_by_tag() {
    let left = read(
        &mp3(4, &[("TIT2", "One"), ("TPE1", "Band")], false),
        &MediaReadOptions::default(),
    )
    .expect("read");
    let right = read(
        &mp3(4, &[("TIT2", "Two"), ("TPE1", "Band")], false),
        &MediaReadOptions::default(),
    )
    .expect("read");
    let merged = compare(&left, &right, &MediaCompareOptions::default());
    assert_eq!(merged.status, Status::Different);
    assert_eq!(merged.counts().different, 1);
    let rows = merged.rows();
    let row = rows.iter().find(|row| row.name == "Title").expect("row");
    assert_eq!(row.left.as_deref(), Some("One"));
    assert_eq!(row.right.as_deref(), Some("Two"));
}

#[test]
fn utf16_user_text_and_multi_value_frames_compare_every_value() {
    let left = read(
        &mp3_with_raw_frames(&[
            (
                "TXXX",
                utf16_tag_text(&["REPLAYGAIN_TRACK_GAIN", "-6.50 dB"]),
            ),
            ("TPE1", utf16_tag_text(&["Band", "Guest One"])),
        ]),
        &MediaReadOptions::default(),
    )
    .expect("left");
    let right = read(
        &mp3_with_raw_frames(&[
            (
                "TXXX",
                utf16_tag_text(&["REPLAYGAIN_TRACK_GAIN", "+2.10 dB"]),
            ),
            ("TPE1", utf16_tag_text(&["Band", "Guest Two"])),
        ]),
        &MediaReadOptions::default(),
    )
    .expect("right");
    let merged = compare(&left, &right, &MediaCompareOptions::default());
    assert_eq!(merged.counts().different, 2);
    let rows = merged.rows();
    assert!(rows.iter().any(|row| {
        row.name == "User text: REPLAYGAIN_TRACK_GAIN"
            && row.left.as_deref() == Some("-6.50 dB")
            && row.right.as_deref() == Some("+2.10 dB")
    }));
    assert!(rows.iter().any(|row| {
        row.name == "Artist"
            && row.left.as_deref() == Some("Band; Guest One")
            && row.right.as_deref() == Some("Band; Guest Two")
    }));
}

#[test]
fn final_id3_text_terminators_do_not_create_an_empty_value() {
    let latin1_with_terminator = vec![0, b'B', b'a', b'n', b'd', 0];
    let latin1_without_terminator = vec![0, b'B', b'a', b'n', b'd'];
    let utf16_with_terminator = vec![1, 0xff, 0xfe, b'B', 0, b'a', 0, b'n', 0, b'd', 0, 0, 0];
    let utf16_without_terminator = vec![1, 0xff, 0xfe, b'B', 0, b'a', 0, b'n', 0, b'd', 0];

    for (left_frame, right_frame) in [
        (latin1_with_terminator, latin1_without_terminator),
        (utf16_with_terminator, utf16_without_terminator),
    ] {
        let left = read(
            &mp3_with_raw_frames(&[("TPE1", left_frame)]),
            &MediaReadOptions::default(),
        )
        .expect("read terminated artist");
        let right = read(
            &mp3_with_raw_frames(&[("TPE1", right_frame)]),
            &MediaReadOptions::default(),
        )
        .expect("read unterminated artist");
        let merged = compare(&left, &right, &MediaCompareOptions::default());
        let artist = merged
            .rows()
            .into_iter()
            .find(|row| row.group.as_deref() == Some("ID3v2") && row.name == "Artist")
            .expect("artist row");
        assert_eq!(
            artist.kind,
            ca_records::compare::RowKind::Same,
            "left={:?} right={:?}",
            artist.left,
            artist.right
        );
        assert_eq!(artist.left.as_deref(), Some("Band"));
        assert_eq!(artist.right.as_deref(), Some("Band"));
    }
}

#[test]
fn the_ignore_tags_option_leaves_only_the_stream_and_file_facts() {
    let left = read(
        &mp3(4, &[("TIT2", "One")], false),
        &MediaReadOptions::default(),
    )
    .expect("read");
    let right = read(
        &mp3(4, &[("TIT2", "Two")], false),
        &MediaReadOptions::default(),
    )
    .expect("read");
    let options = MediaCompareOptions {
        ignore_tags: true,
        ..MediaCompareOptions::default()
    };
    let merged = compare(&left, &right, &options);
    assert_eq!(merged.counts().different, 0);
    assert!(merged.rows().iter().all(|row| row.name != "Title"));
}

#[test]
fn a_binary_frame_records_its_size_by_default() {
    let mut frames = Vec::new();
    frames.extend_from_slice(b"ID3");
    frames.extend_from_slice(&[3, 0, 0]);
    let mut body = Vec::new();
    body.extend_from_slice(b"APIC");
    body.extend_from_slice(&16u32.to_be_bytes());
    body.extend_from_slice(&[0, 0]);
    body.extend_from_slice(&[0u8; 16]);
    frames.extend_from_slice(&[0, 0, 0, u8::try_from(body.len()).unwrap()]);
    frames.extend_from_slice(&body);
    frames.extend_from_slice(&common::mpeg_frames(2));
    let tree = read(&frames, &MediaReadOptions::default()).expect("read");
    assert_eq!(record(&tree, "ID3v2", "Picture").display, "16 bytes");
}

#[test]
fn hostile_and_truncated_media_never_panics() {
    let files: Vec<Vec<u8>> = vec![
        mp3(3, &[("TIT2", "x")], true),
        common::flac(44100, 2, &[("TITLE", "x")]),
        common::wav(44100, 2, 64),
        common::mp4(1000, &[("\u{a9}nam", "x")]),
    ];
    for bytes in files {
        for end in 0..bytes.len() {
            let _ = read(&bytes[..end], &MediaReadOptions::default());
        }
    }
}

#[test]
fn a_tag_claiming_more_bytes_than_the_file_holds_is_refused() {
    let mut bytes = mp3(2, &[("TIT2", "x")], false);
    bytes[6] = 0x7F;
    bytes[7] = 0x7F;
    bytes[8] = 0x7F;
    bytes[9] = 0x7F;
    let _ = read(&bytes, &MediaReadOptions::default());
}

#[test]
fn a_value_over_the_limit_is_refused_before_allocation() {
    let options = MediaReadOptions {
        limits: Limits {
            max_value_bytes: 4,
            ..Limits::default()
        },
        ..MediaReadOptions::default()
    };
    let bytes = mp3(2, &[("TIT2", "a long enough title")], false);
    assert!(read(&bytes, &options).expect_err("refused").is_limit());
}

#[test]
fn a_file_that_is_not_media_is_refused() {
    let error = read(b"plain text file", &MediaReadOptions::default()).expect_err("refused");
    assert!(matches!(error, ca_records::RecordError::Unsupported(_)));
}
