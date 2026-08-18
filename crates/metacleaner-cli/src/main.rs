use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use metacleaner_core::{clean, CleanError, CleanOptions, ImageFormat};

#[derive(Copy, Clone, Debug, ValueEnum)]
enum OutputFormatArg {
    Jpeg,
    Png,
    Webp,
}

impl From<OutputFormatArg> for ImageFormat {
    fn from(v: OutputFormatArg) -> Self {
        match v {
            OutputFormatArg::Jpeg => ImageFormat::Jpeg,
            OutputFormatArg::Png => ImageFormat::Png,
            OutputFormatArg::Webp => ImageFormat::WebP,
        }
    }
}

/// Strip EXIF, GPS, XMP, IPTC, C2PA content credentials, and AI-generator
/// signatures (Stable Diffusion parameters, DALL-E/Midjourney/Firefly tags)
/// from images, entirely locally — no network calls, no upload.
#[derive(Parser, Debug)]
#[command(name = "metaclean", version, about)]
struct Cli {
    /// Image files to clean (JPEG, PNG, WebP). Accepts multiple for batch processing.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Directory to write cleaned files into (default: alongside each input, suffixed "-clean").
    #[arg(short, long)]
    out_dir: Option<PathBuf>,

    /// Skip the invisible pixel-fingerprint reset (metadata is still stripped).
    #[arg(long)]
    no_fingerprint_reset: bool,

    /// Max per-channel RGB delta used by the fingerprint reset (1-2 recommended, invisible to the eye).
    #[arg(long, default_value_t = 2)]
    fingerprint_strength: u8,

    /// Fraction of pixels touched by the fingerprint reset, 0.0-1.0.
    #[arg(long, default_value_t = 0.25)]
    fingerprint_fraction: f32,

    /// JPEG re-encode quality, 1-100. Ignored for PNG/WebP.
    #[arg(long, default_value_t = 92)]
    jpeg_quality: u8,

    /// Force a specific output container instead of keeping each input's own format.
    #[arg(long, value_enum)]
    format: Option<OutputFormatArg>,

    /// Overwrite the input file in place instead of writing a new file.
    #[arg(long, conflicts_with = "out_dir")]
    in_place: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let opts = CleanOptions {
        reset_fingerprint: !cli.no_fingerprint_reset,
        fingerprint_strength: cli.fingerprint_strength,
        fingerprint_fraction: cli.fingerprint_fraction,
        jpeg_quality: cli.jpeg_quality,
        output_format: cli.format.map(Into::into),
    };

    if let Some(dir) = &cli.out_dir {
        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!(
                "error: could not create output directory {}: {e}",
                dir.display()
            );
            return ExitCode::FAILURE;
        }
    }

    let mut failures = 0usize;
    let total = cli.inputs.len();

    for input_path in &cli.inputs {
        match process_one(input_path, &opts, cli.out_dir.as_deref(), cli.in_place) {
            Ok((out_path, report)) => {
                println!(
                    "ok   {} -> {} [{:?} {}x{}, {} -> {} bytes, fingerprint reset: {}]",
                    input_path.display(),
                    out_path.display(),
                    report.output_format,
                    report.width,
                    report.height,
                    report.bytes_in,
                    report.bytes_out,
                    report.fingerprint_reset,
                );
            }
            Err(e) => {
                eprintln!("fail {}: {e}", input_path.display());
                failures += 1;
            }
        }
    }

    println!(
        "\n{}/{total} images cleaned successfully.",
        total - failures
    );

    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn process_one(
    input_path: &Path,
    opts: &CleanOptions,
    out_dir: Option<&Path>,
    in_place: bool,
) -> Result<(PathBuf, metacleaner_core::CleanReport), CliError> {
    let bytes = fs::read(input_path).map_err(|e| CliError::Io(input_path.to_path_buf(), e))?;
    let cleaned = clean(&bytes, opts).map_err(CliError::Clean)?;

    let out_path = if in_place {
        input_path.to_path_buf()
    } else {
        destination_path(input_path, out_dir, cleaned.report.output_format)
    };

    fs::write(&out_path, &cleaned.bytes).map_err(|e| CliError::Io(out_path.clone(), e))?;
    Ok((out_path, cleaned.report))
}

fn destination_path(input: &Path, out_dir: Option<&Path>, format: ImageFormat) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".to_string());
    let file_name = format!("{stem}-clean.{}", format.extension());

    match out_dir {
        Some(dir) => dir.join(file_name),
        None => input.with_file_name(file_name),
    }
}

#[derive(Debug)]
enum CliError {
    Io(PathBuf, std::io::Error),
    Clean(CleanError),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Io(path, e) => write!(f, "I/O error on {}: {e}", path.display()),
            CliError::Clean(e) => write!(f, "{e}"),
        }
    }
}
