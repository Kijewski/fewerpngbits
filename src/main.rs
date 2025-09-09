// SPDX-FileCopyrightText: 2025 René Kijewski <crates.io@k6i.de>
// SPDX-License-Identifier: MIT OR Apache-2.0 OR ISC

use std::fs::OpenOptions;
use std::io::{Cursor, Write};
use std::num::NonZeroU8;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use clap::Parser;
use image::codecs::png::{CompressionType, FilterType, PngDecoder, PngEncoder};
use image::{ColorType, ExtendedColorType, ImageDecoder, ImageEncoder, ImageError};
use memmap2::MmapOptions;
use oxipng::{Deflaters, Options, PngError, StripChunks, optimize_from_memory};

fn main() -> Result<(), Error> {
    let args = Args::parse();

    let input = match OpenOptions::new().read(true).open(&args.input) {
        Ok(input) => input,
        Err(err) => return Err(Error::OpenRead(err, args.input)),
    };
    let input = match unsafe { MmapOptions::new().map(&{ input }) } {
        Ok(input) => input,
        Err(err) => return Err(Error::Map(err, args.input)),
    };
    let input = match PngDecoder::new(Cursor::new(input)) {
        Ok(input) => input,
        Err(err) => return Err(Error::Header(err, args.input)),
    };

    let color_type = match input.color_type() {
        ColorType::L8 => ExtendedColorType::L8,
        ColorType::La8 => ExtendedColorType::La8,
        ColorType::Rgb8 => ExtendedColorType::Rgb8,
        ColorType::Rgba8 => ExtendedColorType::Rgba8,
        color_type => return Err(Error::ColorType(color_type, args.input)),
    };

    let (width, height) = input.dimensions();
    let mut image = vec![0; input.total_bytes().try_into().unwrap()];
    input
        .read_image(&mut image)
        .map_err(|err| Error::Read(err, args.input))?;
    args.bits.run(&mut image);

    // Re-encoding should not fail. Open the output file first to error out early.

    let mut output = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .create_new(!args.force)
        .open(&args.output)
    {
        Ok(output) => output,
        Err(err) => return Err(Error::OpenWrite(err, args.output)),
    };

    let mut encoded = Vec::new();
    PngEncoder::new_with_quality(&mut encoded, CompressionType::Fast, FilterType::NoFilter)
        .write_image(&{ image }, width, height, color_type)
        .map_err(Error::Encode)?;

    let options = Options {
        strip: StripChunks::All,
        deflate: Deflaters::Zopfli {
            iterations: NonZeroU8::new(50).unwrap(),
        },
        fast_evaluation: false,
        timeout: Some(Duration::from_secs(30)),
        ..Options::from_preset(6)
    };
    let optimized = optimize_from_memory(&{ encoded }, &options).map_err(Error::Optimize)?;

    output
        .write_all(&{ optimized })
        .map_err(|err| Error::Write(err, args.output))
}

#[derive(Debug, Clone, Copy)]
enum SignificantBits {
    Bits1,
    Bits2,
    Bits3,
    Bits4,
    Bits5,
    Bits6,
    Bits7,
    Bits8,
}

impl SignificantBits {
    fn run(self, bytes: &mut [u8]) {
        let mask_bits = match self {
            SignificantBits::Bits1 => mask_bits::<0b1000_0000>,
            SignificantBits::Bits2 => mask_bits::<0b1100_0000>,
            SignificantBits::Bits3 => mask_bits::<0b1110_0000>,
            SignificantBits::Bits4 => mask_bits::<0b1111_0000>,
            SignificantBits::Bits5 => mask_bits::<0b1111_1000>,
            SignificantBits::Bits6 => mask_bits::<0b1111_1100>,
            SignificantBits::Bits7 => mask_bits::<0b1111_1110>,
            SignificantBits::Bits8 => return,
        };
        mask_bits(bytes);
    }
}

#[allow(clippy::manual_div_ceil)] // easier to understand what is going on
fn mask_bits<const MASK: u8>(bytes: &mut [u8]) {
    for byte in bytes {
        *byte = (*byte & MASK) | const { (!MASK + 1) / 2 };
    }
}

impl FromStr for SignificantBits {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim_ascii() {
            "1" => Ok(Self::Bits1),
            "2" => Ok(Self::Bits2),
            "3" => Ok(Self::Bits3),
            "4" => Ok(Self::Bits4),
            "5" => Ok(Self::Bits5),
            "6" => Ok(Self::Bits6),
            "7" => Ok(Self::Bits7),
            "8" => Ok(Self::Bits8),
            _ => Err("expected value between 1 and 8"),
        }
    }
}

git_testament::git_testament_macros!(git);

/// Optimize a PNG by masking the lower bits of each channel.
#[derive(Debug, Parser)]
#[clap(version = git_testament!())]
struct Args {
    /// read from
    input: PathBuf,
    /// write to
    output: PathBuf,
    /// number of significant bits to keep
    #[arg(long, short, default_value = "6")]
    bits: SignificantBits,
    /// overwrite existing file
    #[clap(long, short, action)]
    force: bool,
}

#[derive(pretty_error_debug::Debug, thiserror::Error, displaydoc::Display)]
enum Error {
    /// Could not open {1:?} for reading.
    OpenRead(#[source] std::io::Error, PathBuf),
    /// Could not map {1:?} for reading.
    Map(#[source] std::io::Error, PathBuf),
    /// Could not decode image header of {1:?}.
    Header(#[source] ImageError, PathBuf),
    /// Color type {0:?} of {1:?} is not supported. Only L8, La8, Rgb8 and Rgba8 are.
    ColorType(ColorType, PathBuf),
    /// Could not read image data of {1:?}.
    Read(#[source] ImageError, PathBuf),
    /// Could not encode image.
    Encode(#[source] ImageError),
    /// Could not optimize image.
    Optimize(#[source] PngError),
    /// Could not open {1:?} for writing.
    OpenWrite(#[source] std::io::Error, PathBuf),
    /// Could not write image {1:?}.
    Write(#[source] std::io::Error, PathBuf),
}
