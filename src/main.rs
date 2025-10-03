// SPDX-FileCopyrightText: 2025 René Kijewski <crates.io@k6i.de>
// SPDX-License-Identifier: MIT OR Apache-2.0 OR ISC

use std::fs::{File, OpenOptions};
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::num::NonZeroU8;
use std::path::PathBuf;
use std::process::exit;
use std::str::FromStr;

use clap::Parser;
use image::codecs::png::{CompressionType, FilterType, PngDecoder, PngEncoder};
use image::{ColorType, ExtendedColorType, ImageDecoder, ImageEncoder, ImageError};
use memmap2::MmapOptions;
use oxipng::{Deflaters, Options, PngError, StripChunks, optimize_from_memory};

fn main() -> Result<(), Error> {
    let args = Args::parse();
    if args.output.is_none() && !args.force {
        eprintln!(
            "\
            You have to supply an output path, or supply the '--force' option.\n\
            \n\
            For more information, try '--help'."
        );
        exit(1);
    }

    // open input and output files

    let file = match OpenOptions::new()
        .read(true)
        .write(args.output.is_none())
        .open(&args.input)
    {
        Ok(input) => input,
        Err(err) => return Err(Error::OpenRead(err, args.input)),
    };
    let input = match unsafe { MmapOptions::new().map(&file) } {
        Ok(input) => input,
        Err(err) => return Err(Error::Map(err, args.input)),
    };

    let mut output = if let Some(path) = args.output {
        drop(file);
        match OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .create_new(!args.force)
            .open(&path)
        {
            Ok(file) => Output::NewFile(file, path),
            Err(err) => return Err(Error::OpenWrite(err, path)),
        }
    } else {
        Output::Inplace(file)
    };

    // read input

    let input = match PngDecoder::new(Cursor::new(input)) {
        Ok(input) => input,
        Err(err) => return Err(Error::Header(err, args.input)),
    };

    let target_colors = match TargetColors::try_from(input.color_type()) {
        Ok(target_colors) => target_colors,
        Err(color_type) => return Err(Error::ColorType(color_type, args.input)),
    };

    let (width, height) = input.dimensions();
    let mut image = vec![0; input.total_bytes().try_into().unwrap()];
    if let Err(err) = input.read_image(&mut image) {
        return Err(Error::Read(err, args.input));
    }

    // re-encode image

    args.bits.run(&mut image, target_colors);

    let mut encoded = Vec::new();
    PngEncoder::new_with_quality(&mut encoded, CompressionType::Fast, FilterType::NoFilter)
        .write_image(&{ image }, width, height, target_colors.into())
        .map_err(Error::Encode)?;

    let options = Options {
        strip: StripChunks::All,
        deflate: Deflaters::Zopfli {
            iterations: args.iterations,
        },
        fast_evaluation: false,
        timeout: Some(args.timeout.into()),
        ..Options::from_preset(6)
    };
    let optimized = optimize_from_memory(&{ encoded }, &options).map_err(Error::Optimize)?;

    // write output

    let file = match &mut output {
        Output::Inplace(file) => match file.seek(SeekFrom::Start(0)) {
            Ok(_) => file,
            Err(err) => return Err(Error::Seek(err, args.input)),
        },
        Output::NewFile(file, _) => file,
    };
    if let Err(err) = file.write_all(optimized.as_slice()) {
        let path = match output {
            Output::Inplace(_) => args.input,
            Output::NewFile(_, path) => path,
        };
        return Err(Error::Write(err, path));
    }
    if let Output::Inplace(file) = output
        && let Err(err) = file.set_len(optimized.len().try_into().unwrap())
    {
        return Err(Error::Truncate(err, args.input));
    }

    Ok(())
}

enum Output {
    Inplace(File),
    NewFile(File, PathBuf),
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
    fn run(self, bytes: &mut [u8], target_colors: TargetColors) {
        use SignificantBits::*;
        use TargetColors::*;

        let func = match (target_colors, self) {
            (_, Bits8) => return,
            (L8 | Rgb8, Bits1) => no_alpha::<0b1000_0000>,
            (L8 | Rgb8, Bits2) => no_alpha::<0b1100_0000>,
            (L8 | Rgb8, Bits3) => no_alpha::<0b1110_0000>,
            (L8 | Rgb8, Bits4) => no_alpha::<0b1111_0000>,
            (L8 | Rgb8, Bits5) => no_alpha::<0b1111_1000>,
            (L8 | Rgb8, Bits6) => no_alpha::<0b1111_1100>,
            (L8 | Rgb8, Bits7) => no_alpha::<0b1111_1110>,
            (La8, Bits1) => la8::<0b1000_0000>,
            (La8, Bits2) => la8::<0b1100_0000>,
            (La8, Bits3) => la8::<0b1110_0000>,
            (La8, Bits4) => la8::<0b1111_0000>,
            (La8, Bits5) => la8::<0b1111_1000>,
            (La8, Bits6) => la8::<0b1111_1100>,
            (La8, Bits7) => la8::<0b1111_1110>,
            (Rgba8, Bits1) => rgba::<0b1000_0000>,
            (Rgba8, Bits2) => rgba::<0b1100_0000>,
            (Rgba8, Bits3) => rgba::<0b1110_0000>,
            (Rgba8, Bits4) => rgba::<0b1111_0000>,
            (Rgba8, Bits5) => rgba::<0b1111_1000>,
            (Rgba8, Bits6) => rgba::<0b1111_1100>,
            (Rgba8, Bits7) => rgba::<0b1111_1110>,
        };
        func(bytes);

        fn no_alpha<const MASK: u8>(bytes: &mut [u8]) {
            for byte in bytes {
                mask_bits::<MASK>(byte);
            }
        }

        fn rgba<const MASK: u8>(bytes: &mut [u8]) {
            let (chunks, _) = bytes.as_chunks_mut::<4>();
            for chunk in chunks {
                mask_bits::<MASK>(&mut chunk[0]);
                mask_bits::<MASK>(&mut chunk[1]);
                mask_bits::<MASK>(&mut chunk[2]);
            }
        }

        fn la8<const MASK: u8>(bytes: &mut [u8]) {
            let (chunks, _) = bytes.as_chunks_mut::<2>();
            for chunk in chunks {
                mask_bits::<MASK>(&mut chunk[0]);
            }
        }

        #[inline(always)]
        fn mask_bits<const MASK: u8>(byte: &mut u8) {
            *byte = (*byte & MASK) | const { (!MASK) >> 1 };
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TargetColors {
    L8,
    La8,
    Rgb8,
    Rgba8,
}

impl From<TargetColors> for ExtendedColorType {
    fn from(value: TargetColors) -> ExtendedColorType {
        match value {
            TargetColors::L8 => ExtendedColorType::L8,
            TargetColors::La8 => ExtendedColorType::La8,
            TargetColors::Rgb8 => ExtendedColorType::Rgb8,
            TargetColors::Rgba8 => ExtendedColorType::Rgba8,
        }
    }
}

impl TryFrom<ColorType> for TargetColors {
    type Error = ColorType;

    fn try_from(value: ColorType) -> Result<Self, Self::Error> {
        match value {
            ColorType::L8 => Ok(Self::L8),
            ColorType::La8 => Ok(Self::La8),
            ColorType::Rgb8 => Ok(Self::Rgb8),
            ColorType::Rgba8 => Ok(Self::Rgba8),
            value => Err(value),
        }
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
    /// write to (default: overwrite input)
    output: Option<PathBuf>,
    /// overwrite existing output file if exists
    #[clap(long, short, action)]
    force: bool,
    /// number of significant bits to keep
    #[arg(long, short, default_value = "6")]
    bits: SignificantBits,
    /// compression iterations
    #[clap(long, short, default_value = "15")]
    iterations: NonZeroU8,
    /// maximum amount of time to spend on optimizations
    #[clap(long, short, default_value = "30s")]
    timeout: humantime::Duration,
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
    /// Could not seek to the start of {1:?}.
    Seek(#[source] std::io::Error, PathBuf),
    /// Could not empty output file {1:?}.
    Truncate(#[source] std::io::Error, PathBuf),
}
