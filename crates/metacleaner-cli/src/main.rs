use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use metacleaner_core::{
    clean, inspect, CleanError, CleanOptions, ImageFormat, InspectOptions, InspectReport,
    DEFAULT_MAX_DECODED_BYTES, DEFAULT_MAX_IMAGE_DIMENSION, DEFAULT_MAX_INPUT_BYTES,
};

mod serve;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum OutputFormatArg {
    Jpeg,
    Png,
    Webp,
    Bmp,
    Gif,
    Tiff,
}

impl From<OutputFormatArg> for ImageFormat {
    fn from(v: OutputFormatArg) -> Self {
        match v {
            OutputFormatArg::Jpeg => ImageFormat::Jpeg,
            OutputFormatArg::Png => ImageFormat::Png,
            OutputFormatArg::Webp => ImageFormat::WebP,
            OutputFormatArg::Bmp => ImageFormat::Bmp,
            OutputFormatArg::Gif => ImageFormat::Gif,
            OutputFormatArg::Tiff => ImageFormat::Tiff,
        }
    }
}

/// Strip EXIF, GPS, XMP, IPTC, C2PA content credentials, and AI-generator
/// signatures (Stable Diffusion parameters, DALL-E/Midjourney/Firefly tags)
/// from images, entirely locally — no network calls, no upload.
#[derive(Parser, Debug)]
#[command(name = "metaclean", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Strip metadata from images (destructive: writes cleaned output).
    Clean(CleanArgs),
    /// Report what metadata is present, without modifying anything.
    Inspect(InspectArgs),
    /// Run a local web UI (loopback-only by default) for drag-and-drop
    /// inspect/clean, with nothing ever leaving this machine.
    Serve(ServeArgs),
}

#[derive(Args, Debug)]
struct ServeArgs {
    /// Address to bind to. Loopback-only by default — change this only if
    /// you understand that anyone who can reach this address can upload
    /// and process files through it.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to listen on.
    #[arg(long, default_value_t = 7878)]
    port: u16,

    /// Don't automatically open a browser tab on startup.
    #[arg(long)]
    no_open: bool,
}

#[derive(Args, Debug)]
struct GuardArgs {
    /// Reject input files larger than this many megabytes, before reading them.
    /// Guards against decompression-bomb-style attacks on untrusted input.
    #[arg(long, default_value_t = DEFAULT_MAX_INPUT_BYTES / (1024 * 1024))]
    max_input_mb: u64,

    /// Reject images whose decoded width or height exceeds this many pixels.
    #[arg(long, default_value_t = DEFAULT_MAX_IMAGE_DIMENSION)]
    max_dimension: u32,
}

#[derive(Args, Debug)]
struct CleanArgs {
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

    /// Apply classical (non-AI) quality enhancement: auto-contrast plus
    /// unsharp-mask sharpening. Off by default — this changes pixel values
    /// beyond what's needed to strip metadata, so it's opt-in.
    #[arg(long)]
    enhance: bool,

    /// Overwrite the input file in place instead of writing a new file.
    #[arg(long, conflicts_with = "out_dir")]
    in_place: bool,

    /// Emit machine-readable JSON instead of one line of text per file.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    guard: GuardArgs,

    /// Reject images that would require decoding more than this many megabytes of pixel data.
    #[arg(long, default_value_t = DEFAULT_MAX_DECODED_BYTES / (1024 * 1024))]
    max_decoded_mb: u64,
}

#[derive(Args, Debug)]
struct InspectArgs {
    /// Image files to inspect (JPEG, PNG, WebP). Accepts multiple.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Emit machine-readable JSON instead of a human-readable report.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    guard: GuardArgs,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match &cli.command {
        Command::Clean(args) => run_clean(args),
        Command::Inspect(args) => run_inspect(args),
        Command::Serve(args) => run_serve(args).await,
    }
}

async fn run_serve(args: &ServeArgs) -> ExitCode {
    let config = serve::ServeConfig {
        host: args.host.clone(),
        port: args.port,
        open_browser: !args.no_open,
    };
    match serve::run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: web server failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_clean(args: &CleanArgs) -> ExitCode {
    let opts = CleanOptions {
        reset_fingerprint: !args.no_fingerprint_reset,
        fingerprint_strength: args.fingerprint_strength,
        fingerprint_fraction: args.fingerprint_fraction,
        jpeg_quality: args.jpeg_quality,
        output_format: args.format.map(Into::into),
        max_input_bytes: Some(args.guard.max_input_mb * 1024 * 1024),
        max_image_dimension: Some(args.guard.max_dimension),
        max_decoded_bytes: Some(args.max_decoded_mb * 1024 * 1024),
        enhance: args.enhance,
    };

    if let Some(dir) = &args.out_dir {
        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!(
                "error: could not create output directory {}: {e}",
                dir.display()
            );
            return ExitCode::FAILURE;
        }
    }

    let mut failures = 0usize;
    let total = args.inputs.len();
    let mut json_results = Vec::new();

    for input_path in &args.inputs {
        match process_one(input_path, &opts, args.out_dir.as_deref(), args.in_place) {
            Ok((out_path, report)) => {
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": true,
                        "output": out_path.display().to_string(),
                        "format": format!("{:?}", report.output_format).to_lowercase(),
                        "width": report.width,
                        "height": report.height,
                        "bytes_in": report.bytes_in,
                        "bytes_out": report.bytes_out,
                        "fingerprint_reset": report.fingerprint_reset,
                        "enhanced": report.enhanced,
                    }));
                } else {
                    println!(
                        "ok   {} -> {} [{:?} {}x{}, {} -> {} bytes, fingerprint reset: {}, enhanced: {}]",
                        input_path.display(),
                        out_path.display(),
                        report.output_format,
                        report.width,
                        report.height,
                        report.bytes_in,
                        report.bytes_out,
                        report.fingerprint_reset,
                        report.enhanced,
                    );
                }
            }
            Err(e) => {
                failures += 1;
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": false,
                        "error": e.to_string(),
                    }));
                } else {
                    eprintln!("fail {}: {e}", input_path.display());
                }
            }
        }
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&json_results).unwrap());
    } else {
        println!(
            "\n{}/{total} images cleaned successfully.",
            total - failures
        );
    }

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
    if let Some(max) = opts.max_input_bytes {
        let size = fs::metadata(input_path)
            .map_err(|e| CliError::Io(input_path.to_path_buf(), e))?
            .len();
        if size > max {
            return Err(CliError::Clean(CleanError::InputTooLarge {
                size: size as usize,
                max,
            }));
        }
    }

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

fn run_inspect(args: &InspectArgs) -> ExitCode {
    let opts = InspectOptions {
        max_input_bytes: Some(args.guard.max_input_mb * 1024 * 1024),
        max_image_dimension: Some(args.guard.max_dimension),
    };

    let mut any_findings = false;
    let mut any_failures = false;
    let mut json_results = Vec::new();

    for input_path in &args.inputs {
        match inspect_one(input_path, &opts) {
            Ok(report) => {
                if !report.is_clean() {
                    any_findings = true;
                }
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": true,
                        "format": format!("{:?}", report.format).to_lowercase(),
                        "width": report.width,
                        "height": report.height,
                        "bytes": report.bytes,
                        "clean": report.is_clean(),
                        "findings": report.findings.iter().map(|f| serde_json::json!({
                            "category": f.category.as_str(),
                            "label": f.label,
                            "size_bytes": f.size_bytes,
                        })).collect::<Vec<_>>(),
                    }));
                } else {
                    print_human_report(input_path, &report);
                }
            }
            Err(e) => {
                any_failures = true;
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": false,
                        "error": e.to_string(),
                    }));
                } else {
                    eprintln!("fail {}: {e}", input_path.display());
                }
            }
        }
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&json_results).unwrap());
    }

    if any_failures || any_findings {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn inspect_one(input_path: &Path, opts: &InspectOptions) -> Result<InspectReport, CliError> {
    if let Some(max) = opts.max_input_bytes {
        let size = fs::metadata(input_path)
            .map_err(|e| CliError::Io(input_path.to_path_buf(), e))?
            .len();
        if size > max {
            return Err(CliError::Clean(CleanError::InputTooLarge {
                size: size as usize,
                max,
            }));
        }
    }

    let bytes = fs::read(input_path).map_err(|e| CliError::Io(input_path.to_path_buf(), e))?;
    inspect(&bytes, opts).map_err(CliError::Clean)
}

fn print_human_report(input_path: &Path, report: &InspectReport) {
    println!(
        "{}  [{:?} {}x{}, {} bytes]",
        input_path.display(),
        report.format,
        report.width,
        report.height,
        report.bytes,
    );
    if report.is_clean() {
        println!("  no metadata findings");
    } else {
        for finding in &report.findings {
            println!(
                "  [{}] {} ({} bytes)",
                finding.category, finding.label, finding.size_bytes
            );
        }
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
