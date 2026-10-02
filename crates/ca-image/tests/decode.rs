//! Decoding: round trips, metadata, hostile input and the size limits.

#![allow(clippy::unwrap_used, clippy::panic)]

mod fixtures;

use ca_image::{decode_bytes, decode_bytes_as, DecodeOptions, Error, Limits, SourceFormat};
use std::io::Cursor;

#[test]
fn every_encodable_format_round_trips_its_size() {
    let options = DecodeOptions::default();
    for (name, bytes) in fixtures::all_encoded(8, 5) {
        let named = SourceFormat::from_extension(name);
        let decoded = decode_bytes_as(&bytes, named.as_ref(), &options)
            .unwrap_or_else(|error| panic!("{name} failed to decode: {error}"));
        assert_eq!(decoded.image.width(), 8, "{name} width");
        assert_eq!(decoded.image.height(), 5, "{name} height");
        assert_eq!(decoded.metadata.width, 8, "{name} reported width");
        assert!(decoded.metadata.bits_per_pixel > 0, "{name} bit depth");
    }
}

#[test]
fn a_signatureless_format_needs_its_name() {
    let source = fixtures::gradient(6, 4);
    let bytes = fixtures::encode(&source, image::ImageFormat::Tga);
    let guessed = decode_bytes(&bytes, &DecodeOptions::default());
    assert!(guessed.is_err(), "a TGA file resolved without its name");
    let named =
        decode_bytes_as(&bytes, Some(&SourceFormat::Tga), &DecodeOptions::default()).unwrap();
    assert_eq!((named.image.width(), named.image.height()), (6, 4));
}

#[test]
fn a_png_round_trips_its_exact_pixels() {
    let source = fixtures::gradient(4, 3);
    let bytes = fixtures::encode(&source, image::ImageFormat::Png);
    let decoded = decode_bytes(&bytes, &DecodeOptions::default()).unwrap();
    assert_eq!(decoded.image.pixels(), source.as_raw().as_slice());
    assert_eq!(decoded.metadata.format, SourceFormat::Png);
    assert_eq!(decoded.metadata.color, ca_image::ColorKind::Rgba);
    assert_eq!(
        decoded.metadata.orientation,
        ca_image::decode::Orientation::Identity
    );
}

#[test]
fn a_reader_decodes_the_same_bytes() {
    let source = fixtures::gradient(4, 3);
    let bytes = fixtures::encode(&source, image::ImageFormat::Png);
    let from_reader =
        ca_image::decode_reader(Cursor::new(&bytes), &DecodeOptions::default()).unwrap();
    let from_bytes = decode_bytes(&bytes, &DecodeOptions::default()).unwrap();
    assert_eq!(from_reader.image, from_bytes.image);
}

#[test]
fn truncated_input_returns_an_error_for_every_format() {
    let options = DecodeOptions::default();
    let mut cases: Vec<(String, Vec<u8>)> = fixtures::all_encoded(32, 32)
        .into_iter()
        .map(|(name, bytes)| {
            let cut = bytes.len() / 3;
            (name.to_owned(), bytes[..cut].to_vec())
        })
        .collect();
    cases.push(("webp".to_owned(), fixtures::broken_webp()));

    for (name, bytes) in cases {
        let named = SourceFormat::from_extension(&name);
        let outcome = decode_bytes_as(&bytes, named.as_ref(), &options);
        assert!(
            outcome.is_err(),
            "{name} accepted truncated input of {} bytes",
            bytes.len()
        );
    }
}

#[test]
fn corrupt_input_never_panics() {
    let options = DecodeOptions::default();
    for (name, bytes) in fixtures::all_encoded(24, 24) {
        for cut in [2usize, 5, 11, 23] {
            let mut damaged = bytes.clone();
            for index in (cut..damaged.len()).step_by(cut) {
                damaged[index] ^= 0xFF;
            }
            // The only requirement is a decision, not a particular one: the
            // decoder must not abort the process on damaged input.
            let named = SourceFormat::from_extension(name);
            let outcome = decode_bytes_as(&damaged, named.as_ref(), &options);
            assert!(
                outcome.is_ok() || outcome.is_err(),
                "{name} produced neither outcome"
            );
        }
    }
}

#[test]
fn a_declared_size_above_the_limit_is_rejected_before_decoding() {
    let bytes = fixtures::png_declaring(60_000, 60_000);
    let options = DecodeOptions {
        limits: Limits {
            max_width: 4096,
            max_height: 4096,
            ..Limits::default()
        },
        count_frames: false,
        ..DecodeOptions::default()
    };
    let outcome = decode_bytes(&bytes, &options);
    assert!(
        matches!(
            outcome,
            Err(Error::DimensionsTooLarge { width: 60_000, .. } | Error::LimitExceeded(_))
        ),
        "unexpected outcome: {outcome:?}"
    );
}

#[test]
fn webp_metadata_over_the_byte_limit_is_rejected_before_the_metadata_read() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&68u32.to_le_bytes());
    bytes.extend_from_slice(b"WEBP");
    bytes.extend_from_slice(b"VP8X");
    bytes.extend_from_slice(&10u32.to_le_bytes());
    bytes.extend_from_slice(&[0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend_from_slice(b"VP8L");
    bytes.extend_from_slice(&13u32.to_le_bytes());
    bytes.extend_from_slice(&[0x2f, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    bytes.push(0);
    bytes.extend_from_slice(b"EXIF");
    bytes.extend_from_slice(&1_610_612_736u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 16]);
    assert_eq!(bytes.len(), 76);

    let options = DecodeOptions {
        limits: Limits {
            max_decoded_bytes: 1_073_741_824,
            ..Limits::default()
        },
        ..DecodeOptions::default()
    };
    let result = decode_bytes_as(&bytes, Some(&SourceFormat::WebP), &options);
    assert!(
        matches!(
            &result,
            Err(Error::TooLarge {
                needed: 1_610_612_736,
                limit: 1_073_741_824,
                unit: "bytes"
            })
        ),
        "unexpected decode outcome: {result:?}"
    );
}

#[test]
fn animated_webp_is_charged_for_all_live_canvas_buffers() {
    let bytes = animated_webp(4096, 4096);
    let result = decode_bytes_as(
        &bytes,
        Some(&SourceFormat::WebP),
        &DecodeOptions {
            limits: Limits {
                max_decoded_bytes: 64 * 1024 * 1024,
                ..Limits::default()
            },
            ..DecodeOptions::default()
        },
    );
    assert!(
        matches!(
            result,
            Err(Error::TooLarge {
                needed: 201_326_592,
                limit: 67_108_864,
                unit: "bytes"
            })
        ),
        "unexpected decode outcome: {result:?}"
    );
}

fn animated_webp(width: u32, height: u32) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, name: [u8; 4], body: &[u8]) {
        out.extend_from_slice(&name);
        out.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        out.extend_from_slice(body);
        if !body.len().is_multiple_of(2) {
            out.push(0);
        }
    }
    fn u24(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&[
            u8::try_from(value & 0xff).unwrap(),
            u8::try_from((value >> 8) & 0xff).unwrap(),
            u8::try_from((value >> 16) & 0xff).unwrap(),
        ]);
    }

    let mut chunks = Vec::new();
    let mut extended = vec![0x02, 0, 0, 0];
    u24(&mut extended, width - 1);
    u24(&mut extended, height - 1);
    chunk(&mut chunks, *b"VP8X", &extended);
    chunk(&mut chunks, *b"ANIM", &[0; 6]);

    let mut frame = Vec::new();
    u24(&mut frame, 0);
    u24(&mut frame, 0);
    u24(&mut frame, width - 1);
    u24(&mut frame, height - 1);
    u24(&mut frame, 100);
    frame.push(0);
    let packed_dimensions = (width - 1) | ((height - 1) << 14);
    let mut lossless = vec![0x2f];
    lossless.extend_from_slice(&packed_dimensions.to_le_bytes());
    lossless.extend_from_slice(&[0; 8]);
    chunk(&mut frame, *b"VP8L", &lossless);
    chunk(&mut chunks, *b"ANMF", &frame);

    let mut webp = b"RIFF".to_vec();
    let riff_size = u32::try_from(chunks.len() + 4).unwrap();
    webp.extend_from_slice(&riff_size.to_le_bytes());
    webp.extend_from_slice(b"WEBP");
    webp.extend_from_slice(&chunks);
    webp
}

#[test]
fn an_offset_gif_frame_is_charged_for_both_live_buffers() {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&16_384u16.to_le_bytes());
    bytes.extend_from_slice(&12_288u16.to_le_bytes());
    bytes.extend_from_slice(&[0x80, 0, 0, 0, 0, 0, 255, 255, 255]);
    bytes.push(0x2c);
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&16_383u16.to_le_bytes());
    bytes.extend_from_slice(&12_288u16.to_le_bytes());
    bytes.extend_from_slice(&[0, 2, 0, 0x3b]);
    assert_eq!(bytes.len(), 32);

    let result = decode_bytes_as(&bytes, Some(&SourceFormat::Gif), &DecodeOptions::default());
    assert!(
        matches!(
            &result,
            Err(Error::TooLarge {
                needed: 1_610_612_736,
                limit: 1_073_741_824,
                unit: "bytes"
            })
        ),
        "unexpected decode outcome: {result:?}"
    );
}

#[test]
fn a_declared_pixel_count_above_the_limit_is_rejected_before_decoding() {
    let bytes = fixtures::png_declaring(20_000, 20_000);
    let options = DecodeOptions {
        limits: Limits {
            max_width: u32::MAX,
            max_height: u32::MAX,
            max_pixels: 1_000_000,
            max_decoded_bytes: u64::MAX,
        },
        count_frames: false,
        ..DecodeOptions::default()
    };
    let outcome = decode_bytes(&bytes, &options);
    assert!(
        matches!(outcome, Err(Error::TooLarge { unit: "pixels", .. })),
        "unexpected outcome: {outcome:?}"
    );
}

#[test]
fn a_declared_byte_count_above_the_limit_is_rejected_before_decoding() {
    let bytes = fixtures::png_declaring(20_000, 20_000);
    let options = DecodeOptions {
        limits: Limits {
            max_width: u32::MAX,
            max_height: u32::MAX,
            max_pixels: u64::MAX,
            max_decoded_bytes: 1024,
        },
        count_frames: false,
        ..DecodeOptions::default()
    };
    let outcome = decode_bytes(&bytes, &options);
    assert!(
        matches!(
            outcome,
            Err(Error::TooLarge { unit: "bytes", .. } | Error::LimitExceeded(_))
        ),
        "unexpected outcome: {outcome:?}"
    );
}

#[test]
fn a_header_only_png_within_the_limits_reports_missing_data() {
    let bytes = fixtures::png_header_only(8, 8);
    let outcome = decode_bytes(&bytes, &DecodeOptions::default());
    assert!(outcome.is_err(), "a PNG without image data decoded");
}

#[test]
fn a_zero_sized_image_is_rejected() {
    let bytes = fixtures::png_header_only(0, 8);
    let outcome = decode_bytes(&bytes, &DecodeOptions::default());
    assert!(outcome.is_err(), "a zero width image decoded");
}

#[test]
fn an_unrecognized_format_reports_unsupported() {
    let outcome = decode_bytes(b"not an image at all, just text", &DecodeOptions::default());
    match outcome {
        Err(error) => {
            assert!(error.is_unsupported(), "unexpected error: {error}");
            assert!(error.is_bad_input());
        }
        Ok(_) => panic!("plain text decoded as an image"),
    }
}

#[test]
fn a_sixteen_bit_source_reports_the_precision_it_lost() {
    let bytes = fixtures::png_sixteen_bit(0x0101, 0, 0);
    let decoded = decode_bytes(&bytes, &DecodeOptions::default()).unwrap();
    assert_eq!(decoded.metadata.fidelity.bits_per_channel, 16);
    assert!(decoded.metadata.fidelity.precision_reduced);
    assert!(decoded.metadata.fidelity.reduced());

    let eight_bit = fixtures::encode(&fixtures::gradient(2, 2), image::ImageFormat::Png);
    let decoded = decode_bytes(&eight_bit, &DecodeOptions::default()).unwrap();
    assert_eq!(decoded.metadata.fidelity.bits_per_channel, 8);
    assert!(!decoded.metadata.fidelity.precision_reduced);
    assert!(!decoded.metadata.fidelity.reduced());
}

#[test]
fn truvision_targa_extensions_resolve_to_the_tga_decoder() {
    for extension in ["tga", "icb", "vda", "vst", "win"] {
        assert_eq!(
            ca_image::decode::SourceFormat::from_extension(extension),
            Some(ca_image::decode::SourceFormat::Tga),
            "{extension} did not resolve to the TGA decoder"
        );
    }
}

#[test]
fn a_difference_below_eight_bits_is_reported_as_equal_at_eight_bits() {
    let left = decode_bytes(
        &fixtures::png_sixteen_bit(0x0101, 0, 0),
        &DecodeOptions::default(),
    )
    .unwrap();
    let right = decode_bytes(
        &fixtures::png_sixteen_bit(0x0102, 0, 0),
        &DecodeOptions::default(),
    )
    .unwrap();
    assert_eq!(left.image.pixel(0, 0), right.image.pixel(0, 0));

    let options = ca_image::CompareOptions {
        left_fidelity: left.metadata.fidelity,
        right_fidelity: right.metadata.fidelity,
        ..ca_image::CompareOptions::default()
    };
    let result =
        ca_image::compare(&left.image, &right.image, &options, &ca_image::NeverCancel).unwrap();
    assert!(result.totals.is_identical(false));
    assert!(
        result.totals.equal_at_eight_bits,
        "the reduced precision was not reported"
    );
    assert!(result.left_fidelity.precision_reduced);
    assert!(result.right_fidelity.precision_reduced);
}

#[test]
fn two_eight_bit_sources_do_not_claim_reduced_precision() {
    let bytes = fixtures::encode(&fixtures::gradient(2, 2), image::ImageFormat::Png);
    let decoded = decode_bytes(&bytes, &DecodeOptions::default()).unwrap();
    let options = ca_image::CompareOptions {
        left_fidelity: decoded.metadata.fidelity,
        right_fidelity: decoded.metadata.fidelity,
        ..ca_image::CompareOptions::default()
    };
    let result = ca_image::compare(
        &decoded.image,
        &decoded.image,
        &options,
        &ca_image::NeverCancel,
    )
    .unwrap();
    assert!(result.totals.is_identical(false));
    assert!(!result.totals.equal_at_eight_bits);
}

#[test]
fn one_sixteen_bit_side_is_enough_to_qualify_an_equal_result() {
    let left = decode_bytes(
        &fixtures::png_sixteen_bit(257, 0, 0),
        &DecodeOptions::default(),
    )
    .unwrap();
    let eight_bit = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 0, 0, 255]));
    let right = decode_bytes(
        &fixtures::encode(&eight_bit, image::ImageFormat::Png),
        &DecodeOptions::default(),
    )
    .unwrap();
    let options = ca_image::CompareOptions {
        left_fidelity: left.metadata.fidelity,
        right_fidelity: right.metadata.fidelity,
        ..ca_image::CompareOptions::default()
    };
    let result =
        ca_image::compare(&left.image, &right.image, &options, &ca_image::NeverCancel).unwrap();
    assert!(result.totals.is_identical(false));
    assert!(result.totals.equal_at_eight_bits);
}

#[test]
fn reduced_precision_does_not_qualify_a_different_result_as_equal() {
    let left = decode_bytes(
        &fixtures::png_sixteen_bit(257, 0, 0),
        &DecodeOptions::default(),
    )
    .unwrap();
    let eight_bit = image::RgbaImage::from_pixel(1, 1, image::Rgba([2, 0, 0, 255]));
    let right = decode_bytes(
        &fixtures::encode(&eight_bit, image::ImageFormat::Png),
        &DecodeOptions::default(),
    )
    .unwrap();
    let options = ca_image::CompareOptions {
        left_fidelity: left.metadata.fidelity,
        right_fidelity: right.metadata.fidelity,
        ..ca_image::CompareOptions::default()
    };
    let result =
        ca_image::compare(&left.image, &right.image, &options, &ca_image::NeverCancel).unwrap();
    assert!(!result.totals.is_identical(false));
    assert!(!result.totals.equal_at_eight_bits);
}

#[test]
fn frames_are_counted_only_when_asked() {
    let frames: Vec<image::Frame> = (0..3)
        .map(|index| {
            let mut frame = fixtures::gradient(4, 4);
            for pixel in frame.pixels_mut() {
                pixel.0[0] = index * 40;
            }
            image::Frame::new(frame)
        })
        .collect();
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        encoder.encode_frames(frames).unwrap();
    }

    let quiet = decode_bytes(&bytes, &DecodeOptions::default()).unwrap();
    assert_eq!(quiet.metadata.frame_count, None);

    let counted = decode_bytes(
        &bytes,
        &DecodeOptions {
            count_frames: true,
            ..DecodeOptions::default()
        },
    )
    .unwrap();
    assert_eq!(counted.metadata.frame_count, Some(3));
    assert_eq!(counted.image.width(), 4);
}
