//! Writing WAV files: 32-bit float, or 16- and 24-bit integers with or
//! without dither.

use std::path::Path;

use crate::Result;

/// How samples are stored in the file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    Float32,
    /// 16 or 24 bits, and whether to dither.
    Int {
        bits: u16,
        dither: bool,
    },
}

impl Format {
    pub fn new(bits: u16, dither: bool) -> Result<Self> {
        match (bits, dither) {
            (32, false) => Ok(Self::Float32),
            (32, true) => Err("--dither is for 16- and 24-bit files".into()),
            (16 | 24, _) => Ok(Self::Int { bits, dither }),
            _ => Err(format!("--bits must be 16, 24 or 32, not {bits}").into()),
        }
    }

    pub fn describe(self) -> String {
        match self {
            Self::Float32 => "32-bit float".into(),
            Self::Int {
                bits,
                dither: false,
            } => format!("{bits}-bit"),
            Self::Int { bits, dither: true } => format!("{bits}-bit, dithered"),
        }
    }
}

/// Triangular dither noise, from a fixed seed, so the same render always
/// gives the same file.
struct Dither(u32);

impl Dither {
    fn new() -> Self {
        Self(0x2545_F491)
    }

    /// A number in [0, 1): xorshift32.
    fn uniform(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        f64::from(x) / (f64::from(u32::MAX) + 1.0)
    }

    /// Noise between -1 and 1, most often near 0: the difference of two
    /// uniform numbers.
    fn next(&mut self) -> f64 {
        self.uniform() - self.uniform()
    }
}

/// Turns samples into `bits`-bit integers, rounding to the nearest step
/// after adding dither if asked. Samples beyond full scale are clipped.
fn quantize(samples: &[f32], bits: u16, dither: bool) -> Vec<i32> {
    let scale = f64::from(1u32 << (bits - 1));
    let (min, max) = (-scale, scale - 1.0);
    let mut noise = Dither::new();
    samples
        .iter()
        .map(|&s| {
            let d = if dither { noise.next() } else { 0.0 };
            (f64::from(s) * scale + d).round().clamp(min, max) as i32
        })
        .collect()
}

/// Writes `samples`, interleaved frames of `channels`, to `path`.
pub fn write(
    path: &Path,
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
    format: Format,
) -> Result<()> {
    let (bits_per_sample, sample_format) = match format {
        Format::Float32 => (32, hound::SampleFormat::Float),
        Format::Int { bits, .. } => (bits, hound::SampleFormat::Int),
    };
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample,
        sample_format,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    match format {
        Format::Float32 => {
            for &s in samples {
                writer.write_sample(s)?;
            }
        }
        Format::Int { bits, dither } => {
            for s in quantize(samples, bits, dither) {
                writer.write_sample(s)?;
            }
        }
    }
    writer.finalize()?;
    Ok(())
}

/// How many samples go beyond full scale, where an integer file clips them.
pub fn over_full_scale(samples: &[f32]) -> usize {
    samples.iter().filter(|s| s.abs() > 1.0).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_to_the_nearest_step_and_clips_beyond_full_scale() {
        let samples = [0.0, 0.5, -0.5, -1.0, 1.0, 2.0, -2.0, 1.4 / 32768.0];
        assert_eq!(
            quantize(&samples, 16, false),
            [0, 16384, -16384, -32768, 32767, 32767, -32768, 1]
        );
        assert_eq!(quantize(&[0.5, -1.0], 24, false), [1 << 22, -(1 << 23)]);
        assert_eq!(over_full_scale(&samples), 2);
    }

    #[test]
    fn dither_is_small_centred_and_the_same_every_time() {
        let silence = vec![0.0; 100_000];
        let a = quantize(&silence, 16, true);
        assert_eq!(a, quantize(&silence, 16, true));
        assert!(a.iter().all(|s| (-1..=1).contains(s)));
        let mean = a.iter().map(|&s| f64::from(s)).sum::<f64>() / a.len() as f64;
        assert!(mean.abs() < 0.01, "mean {mean}");
        // Some steps are not zero: the noise is there.
        assert!(a.iter().filter(|&&s| s != 0).count() > 10_000);
        // Half a step between two values comes out as each about as often.
        let half = vec![0.5 / 32768.0; 100_000];
        let ones = quantize(&half, 16, true)
            .iter()
            .filter(|&&s| s == 1)
            .count();
        assert!((40_000..60_000).contains(&ones), "{ones}");
    }

    #[test]
    fn writes_files_that_read_back() {
        let dir = std::env::temp_dir().join(format!("refrain-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let samples = [0.25, -0.5, 0.125, 1.0];
        for (bits, name) in [(16, "a16.wav"), (24, "a24.wav"), (32, "a32.wav")] {
            let path = dir.join(name);
            write(
                &path,
                &samples,
                2,
                48_000,
                Format::new(bits, false).unwrap(),
            )
            .unwrap();
            let mut reader = hound::WavReader::open(&path).unwrap();
            let spec = reader.spec();
            assert_eq!((spec.channels, spec.sample_rate), (2, 48_000));
            assert_eq!(spec.bits_per_sample, bits);
            let back: Vec<f32> = if bits == 32 {
                reader.samples::<f32>().map(|s| s.unwrap()).collect()
            } else {
                let scale = (1u32 << (bits - 1)) as f32;
                reader
                    .samples::<i32>()
                    .map(|s| s.unwrap() as f32 / scale)
                    .collect()
            };
            // Full scale is one step short in an integer file.
            let step = if bits == 32 {
                0.0
            } else {
                1.0 / (1u32 << (bits - 1)) as f32
            };
            for (b, s) in back.iter().zip(samples) {
                assert!((b - s).abs() <= step, "{bits} bits: {b} for {s}");
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(Format::new(32, true).is_err());
        assert!(Format::new(8, false).is_err());
    }
}
