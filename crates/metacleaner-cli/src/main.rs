use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use metacleaner_core::{
    clean, inspect, CleanError, CleanOptions, ImageFormat, InspectOptions, InspectReport,
    DEFAULT_MAX_DECODED_BYTES, DEFAULT_MAX_IMAGE_DIMENSION, DEFAULT_MAX_INPUT_BYTES,
};

mod ai_upscale;
mod serve;

/// Expand `inputs` into a flat file list. Files are passed through as-is
/// (explicitly naming a file always processes it, regardless of
/// extension). A directory is only accepted when `recursive` is set — a
/// destructive/reporting tool should never silently turn a mistyped
/// directory argument into "process everything under here" — in which
/// case it's walked recursively and every entry matching `is_supported`
/// is included; non-matching files are counted and reported once at the
/// end rather than spamming a line per skip.
fn expand_inputs(
    inputs: &[PathBuf],
    recursive: bool,
    is_supported: impl Fn(&Path) -> bool,
) -> Result<Vec<PathBuf>, CliError> {
    let mut out = Vec::new();
    let mut skipped = 0usize;

    for p in inputs {
        if p.is_dir() {
            if !recursive {
                return Err(CliError::Text(format!(
                    "{} is a directory; pass --recursive/-r to process directories",
                    p.display()
                )));
            }
            for entry in walkdir::WalkDir::new(p) {
                let entry =
                    entry.map_err(|e| CliError::Text(format!("walking {}: {e}", p.display())))?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.into_path();
                if is_supported(&path) {
                    out.push(path);
                } else {
                    skipped += 1;
                }
            }
        } else {
            out.push(p.clone());
        }
    }

    if skipped > 0 {
        eprintln!(
            "note: skipped {skipped} file(s) with an unrecognized extension while expanding directory input"
        );
    }

    Ok(out)
}

fn is_image_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "bmp" | "gif" | "tif" | "tiff")
    )
}

fn is_text_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref(),
        Some("txt" | "md" | "markdown" | "text" | "html" | "htm" | "svg")
    )
}

fn is_doc_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref(),
        Some("docx" | "xlsx" | "pptx")
    )
}

fn is_pdf_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref(),
        Some("pdf")
    )
}

fn is_mp3(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref(),
        Some("mp3")
    )
}

fn is_mp4ish(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref(),
        Some("mp4" | "m4a" | "m4v" | "mov")
    )
}

fn is_media_extension(path: &Path) -> bool {
    is_mp3(path) || is_mp4ish(path)
}

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
    ///
    /// Removes EXIF, GPS, XMP, IPTC, C2PA content credentials, and
    /// AI-generator signatures (Stable Diffusion, DALL-E, Midjourney,
    /// Adobe Firefly) by fully decoding and re-encoding the image, so
    /// every non-pixel segment is dropped by construction.
    Clean(CleanArgs),
    /// Report what metadata is present in an image, without modifying it.
    Inspect(InspectArgs),
    /// Strip hidden Unicode and AI-identifying artifacts from text files
    /// (destructive: writes cleaned output).
    ///
    /// Removes invisible-Unicode steganography (zero-width characters,
    /// bidi overrides, Unicode Tag block, variation-selector smuggling)
    /// from any .txt/.md/.html/.svg file; additionally strips AI-key
    /// Markdown frontmatter (.md), identifying <meta> tags and comments
    /// (.html/.htm), and editor metadata/comments (.svg).
    CleanText(CleanTextArgs),
    /// Report hidden Unicode and AI-identifying artifacts in a text file,
    /// without modifying it.
    ///
    /// Covers the same ground as clean-text: invisible-Unicode
    /// steganography everywhere, plus Markdown frontmatter keys (.md),
    /// <meta> tags/comments (.html/.htm), and editor metadata/comments
    /// (.svg).
    InspectText(InspectTextArgs),
    /// Strip identifying metadata from an Office document (destructive:
    /// writes cleaned output).
    ///
    /// Removes author, company, last-modified-by, and custom tracking
    /// properties from a DOCX/XLSX/PPTX file's docProps/*.xml, leaving
    /// document content byte-for-byte untouched.
    CleanDoc(CleanDocArgs),
    /// Report what identifying metadata is present in a DOCX/XLSX/PPTX
    /// file, without modifying it.
    InspectDoc(InspectDocArgs),
    /// Strip identifying metadata from a PDF file (destructive: writes
    /// cleaned output).
    ///
    /// Removes the /Info dictionary (Author, Producer, Creator, Title,
    /// Subject, Keywords, dates) and every XMP metadata stream, wherever
    /// in the object graph it appears. Scope note: page content, form
    /// fields, embedded file attachments, and JavaScript actions are not
    /// touched — see the metacleaner-pdf crate docs for why.
    CleanPdf(CleanPdfArgs),
    /// Report what identifying metadata is present in a PDF file, without
    /// modifying it.
    InspectPdf(InspectPdfArgs),
    /// Strip identifying metadata from an MP3/MP4/M4A/MOV file
    /// (destructive: writes cleaned output).
    ///
    /// Removes every ID3v2 frame and the legacy ID3v1 trailer from MP3
    /// (artist, album, comment, encoder tag, embedded artwork), or the
    /// iTunes-style ilst metadata item list and chapter data from
    /// MP4/M4A/MOV (title, artist, encoder, embedded artwork). Audio/
    /// video sample data is untouched. Scope note: mp4ameta targets the
    /// standard iTunes-style dictionary — see the metacleaner-media crate
    /// docs for what that doesn't cover.
    CleanMedia(CleanMediaArgs),
    /// Report what identifying metadata is present in an MP3/MP4/M4A/MOV
    /// file, without modifying it.
    InspectMedia(InspectMediaArgs),
    /// Run a local web UI for drag-and-drop inspect/clean.
    ///
    /// Loopback-only by default — nothing ever leaves this machine.
    Serve(ServeArgs),
    /// Generate a shell completion script, printed to stdout.
    ///
    /// Packaging/maintainer tool, hidden from the primary command list.
    /// Example: `metaclean completions bash > /etc/bash_completion.d/metaclean`.
    #[command(hide = true)]
    Completions {
        /// Shell to generate a completion script for.
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Generate the metaclean(1) man page (roff), printed to stdout.
    ///
    /// Packaging/maintainer tool, hidden from the primary command list.
    /// Example: `metaclean man > /usr/share/man/man1/metaclean.1`.
    #[command(hide = true)]
    Man,
}

#[derive(Args, Debug)]
struct CleanDocArgs {
    /// DOCX/XLSX/PPTX files (or, with --recursive, directories) to clean.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Directory to write cleaned files into (default: alongside each input, suffixed "-clean").
    #[arg(short, long)]
    out_dir: Option<PathBuf>,

    /// Overwrite the input file in place instead of writing a new file.
    #[arg(long, conflicts_with = "out_dir")]
    in_place: bool,

    /// Emit machine-readable JSON instead of one line of text per file.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    recurse: RecurseArgs,
}

#[derive(Args, Debug)]
struct InspectDocArgs {
    /// DOCX/XLSX/PPTX files (or, with --recursive, directories) to inspect.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Emit machine-readable JSON instead of a human-readable report.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    recurse: RecurseArgs,
}

#[derive(Args, Debug)]
struct CleanPdfArgs {
    /// PDF files (or, with --recursive, directories) to clean.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Directory to write cleaned files into (default: alongside each input, suffixed "-clean").
    #[arg(short, long)]
    out_dir: Option<PathBuf>,

    /// Overwrite the input file in place instead of writing a new file.
    #[arg(long, conflicts_with = "out_dir")]
    in_place: bool,

    /// Reject input files larger than this many megabytes, before reading them.
    #[arg(long, default_value_t = metacleaner_pdf::DEFAULT_MAX_INPUT_BYTES / (1024 * 1024))]
    max_input_mb: u64,

    /// Emit machine-readable JSON instead of one line of text per file.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    recurse: RecurseArgs,
}

#[derive(Args, Debug)]
struct InspectPdfArgs {
    /// PDF files (or, with --recursive, directories) to inspect.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Emit machine-readable JSON instead of a human-readable report.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    recurse: RecurseArgs,
}

#[derive(Args, Debug)]
struct CleanMediaArgs {
    /// MP3/MP4/M4A/MOV files (or, with --recursive, directories) to clean.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Directory to write cleaned files into (default: alongside each input, suffixed "-clean").
    #[arg(short, long)]
    out_dir: Option<PathBuf>,

    /// Overwrite the input file in place instead of writing a new file.
    #[arg(long, conflicts_with = "out_dir")]
    in_place: bool,

    /// Reject input files larger than this many megabytes, before reading them.
    #[arg(long, default_value_t = metacleaner_media::DEFAULT_MAX_INPUT_BYTES / (1024 * 1024))]
    max_input_mb: u64,

    /// Emit machine-readable JSON instead of one line of text per file.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    recurse: RecurseArgs,
}

#[derive(Args, Debug)]
struct InspectMediaArgs {
    /// MP3/MP4/M4A/MOV files (or, with --recursive, directories) to inspect.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Emit machine-readable JSON instead of a human-readable report.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    recurse: RecurseArgs,
}

#[derive(Args, Debug)]
struct CleanTextArgs {
    /// Text files (or, with --recursive, directories) to clean.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Directory to write cleaned files into (default: alongside each input, suffixed "-clean").
    #[arg(short, long)]
    out_dir: Option<PathBuf>,

    /// Overwrite the input file in place instead of writing a new file.
    #[arg(long, conflicts_with = "out_dir")]
    in_place: bool,

    /// Also strip the zero-width joiner (U+200D). Off by default because
    /// it's legitimate and common (it's what joins emoji into
    /// family/profession sequences) — only enable this if you specifically
    /// need to defeat a watermarking scheme that uses it.
    #[arg(long)]
    strip_zero_width_joiner: bool,

    /// Don't strip bidirectional-control characters (LRE/RLE/RLO/etc.).
    /// Leave this off unless you have text that legitimately depends on
    /// explicit bidi embedding.
    #[arg(long)]
    keep_bidi_controls: bool,

    /// Don't strip the Unicode Tag block (U+E0000-U+E007F). This block has
    /// no legitimate use in ordinary text — only disable this for testing.
    #[arg(long)]
    keep_unicode_tags: bool,

    /// Don't strip supplementary-plane variation selectors
    /// (U+E0100-U+E01EF). Disable if your text uses CJK Ideographic
    /// Variation Database selectors legitimately.
    #[arg(long)]
    keep_variation_selectors: bool,

    /// Normalize curly quotes to straight quotes, em/en-dashes to a plain
    /// hyphen, and non-breaking spaces to a regular space. Off by
    /// default — unlike every other pass here, this changes ordinary
    /// rendered characters, not just hidden ones — but these specific
    /// characters are genuine, common AI-tool typographic artifacts, so
    /// normalizing them doubles as removing a provenance signal.
    #[arg(long)]
    normalize_typography: bool,

    /// Emit machine-readable JSON instead of one line of text per file.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    recurse: RecurseArgs,
}

#[derive(Args, Debug)]
struct InspectTextArgs {
    /// Text files (or, with --recursive, directories) to inspect.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Also report advisory AI-writing-style indicators (stock vocabulary
    /// and phrases, unusually uniform sentence length, elevated em-dash
    /// usage). Off by default. This is a linter-style hint for reviewing
    /// your own draft, not a score or a verdict — see the printed caveat.
    #[arg(long)]
    ai_style: bool,

    /// Emit machine-readable JSON instead of a human-readable report.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    recurse: RecurseArgs,
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
struct RecurseArgs {
    /// Also accept directories in the input list, walking them
    /// recursively and auto-detecting each file's type by extension. Off
    /// by default: since this is a destructive/reporting tool, a
    /// directory argument is rejected unless you opt in explicitly,
    /// rather than silently expanding to everything underneath it.
    #[arg(short = 'r', long)]
    recursive: bool,
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
    /// Image files (or, with --recursive, directories) to clean.
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

    /// Upscale by this factor (e.g. 2.0 doubles each side) using Lanczos3
    /// resampling before any other processing. Classical resampling, not
    /// AI super-resolution — smooths existing pixels, doesn't invent detail.
    #[arg(long)]
    upscale: Option<f32>,

    /// Upscale 4x using a real AI super-resolution model (Real-ESRGAN,
    /// bundled) instead of classical resampling. This model HALLUCINATES
    /// plausible detail that wasn't in the original — genuinely improves
    /// low-res images, but the output is no longer a strictly faithful
    /// representation of the source pixels. Meant for small/low-res
    /// images; capped at 1600px per side on input.
    #[arg(long, conflicts_with = "upscale")]
    ai_upscale: bool,

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

    #[command(flatten)]
    recurse: RecurseArgs,
}

#[derive(Args, Debug)]
struct InspectArgs {
    /// Image files (or, with --recursive, directories) to inspect.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Emit machine-readable JSON instead of a human-readable report.
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    guard: GuardArgs,

    #[command(flatten)]
    recurse: RecurseArgs,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match &cli.command {
        Command::Clean(args) => run_clean(args),
        Command::Inspect(args) => run_inspect(args),
        Command::CleanText(args) => run_clean_text(args),
        Command::InspectText(args) => run_inspect_text(args),
        Command::CleanDoc(args) => run_clean_doc(args),
        Command::InspectDoc(args) => run_inspect_doc(args),
        Command::CleanPdf(args) => run_clean_pdf(args),
        Command::InspectPdf(args) => run_inspect_pdf(args),
        Command::CleanMedia(args) => run_clean_media(args),
        Command::InspectMedia(args) => run_inspect_media(args),
        Command::Serve(args) => run_serve(args).await,
        Command::Completions { shell } => run_completions(*shell),
        Command::Man => run_man(),
    }
}

fn run_completions(shell: Shell) -> ExitCode {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
    ExitCode::SUCCESS
}

fn run_man() -> ExitCode {
    let cmd = Cli::command();
    let man = clap_mangen::Man::new(cmd);
    match man.render(&mut std::io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: could not render man page: {e}");
            ExitCode::FAILURE
        }
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
    let inputs = match expand_inputs(&args.inputs, args.recurse.recursive, is_image_extension) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

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
        upscale_factor: args.upscale,
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

    let mut ai_upscaler = if args.ai_upscale {
        match metacleaner_ai::AiUpscaler::load() {
            Ok(u) => Some(u),
            Err(e) => {
                eprintln!("error: could not load AI upscale model: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    let mut failures = 0usize;
    let total = inputs.len();
    let mut json_results = Vec::new();

    for input_path in &inputs {
        match process_one(
            input_path,
            &opts,
            args.out_dir.as_deref(),
            args.in_place,
            ai_upscaler.as_mut(),
        ) {
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
                        "ai_upscaled": args.ai_upscale,
                    }));
                } else {
                    println!(
                        "ok   {} -> {} [{:?} {}x{}, {} -> {} bytes, fingerprint reset: {}, enhanced: {}{}]",
                        input_path.display(),
                        out_path.display(),
                        report.output_format,
                        report.width,
                        report.height,
                        report.bytes_in,
                        report.bytes_out,
                        report.fingerprint_reset,
                        report.enhanced,
                        if args.ai_upscale { ", AI-upscaled" } else { "" },
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
    ai_upscaler: Option<&mut metacleaner_ai::AiUpscaler>,
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
    let bytes = match ai_upscaler {
        Some(upscaler) => {
            ai_upscale::apply_ai_upscale(upscaler, &bytes).map_err(CliError::AiUpscale)?
        }
        None => bytes,
    };
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
    let inputs = match expand_inputs(&args.inputs, args.recurse.recursive, is_image_extension) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let opts = InspectOptions {
        max_input_bytes: Some(args.guard.max_input_mb * 1024 * 1024),
        max_image_dimension: Some(args.guard.max_dimension),
    };

    let mut any_findings = false;
    let mut any_failures = false;
    let mut json_results = Vec::new();

    for input_path in &inputs {
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

fn run_clean_text(args: &CleanTextArgs) -> ExitCode {
    let inputs = match expand_inputs(&args.inputs, args.recurse.recursive, is_text_extension) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let opts = metacleaner_text::CleanTextOptions {
        strip_zero_width: true,
        strip_zero_width_joiner: args.strip_zero_width_joiner,
        strip_bidi_controls: !args.keep_bidi_controls,
        strip_unicode_tags: !args.keep_unicode_tags,
        strip_variation_selector_supplement: !args.keep_variation_selectors,
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
    let total = inputs.len();
    let mut json_results = Vec::new();

    for input_path in &inputs {
        match process_text_one(
            input_path,
            &opts,
            args.out_dir.as_deref(),
            args.in_place,
            args.normalize_typography,
        ) {
            Ok((out_path, report, frontmatter_removed, html_removed, svg_removed, typography_removed)) => {
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": true,
                        "output": out_path.display().to_string(),
                        "chars_in": report.chars_in,
                        "chars_out": report.chars_out,
                        "removed": report.removed.iter().map(|f| serde_json::json!({
                            "category": f.category.as_str(),
                            "codepoint": format!("U+{:04X}", f.codepoint),
                            "count": f.count,
                        })).collect::<Vec<_>>(),
                        "frontmatter_keys_removed": frontmatter_removed.iter().map(|f| f.key.clone()).collect::<Vec<_>>(),
                        "html_findings_removed": html_removed.iter().map(|f| serde_json::json!({
                            "kind": f.kind.as_str(),
                            "label": f.label,
                        })).collect::<Vec<_>>(),
                        "svg_findings_removed": svg_removed.iter().map(|f| serde_json::json!({
                            "kind": f.kind.as_str(),
                            "label": f.label,
                        })).collect::<Vec<_>>(),
                        "typography_normalized": typography_removed.iter().map(|f| serde_json::json!({
                            "kind": f.kind.as_str(),
                            "codepoint": format!("U+{:04X}", f.codepoint),
                            "count": f.count,
                        })).collect::<Vec<_>>(),
                    }));
                } else {
                    let fm_note = if frontmatter_removed.is_empty() {
                        String::new()
                    } else {
                        format!(
                            ", frontmatter keys removed: {}",
                            frontmatter_removed
                                .iter()
                                .map(|f| f.key.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    let html_note = if html_removed.is_empty() {
                        String::new()
                    } else {
                        let (meta_count, comment_count) =
                            html_removed
                                .iter()
                                .fold((0usize, 0usize), |(m, c), f| match f.kind {
                                    metacleaner_text::HtmlFindingKind::Meta => (m + 1, c),
                                    metacleaner_text::HtmlFindingKind::Comment => (m, c + 1),
                                });
                        format!(
                            ", HTML: {meta_count} meta tag(s), {comment_count} comment(s) removed"
                        )
                    };
                    let svg_note = if svg_removed.is_empty() {
                        String::new()
                    } else {
                        let (meta_count, attr_count, comment_count) = svg_removed.iter().fold(
                            (0usize, 0usize, 0usize),
                            |(m, a, c), f| match f.kind {
                                metacleaner_text::SvgFindingKind::MetadataElement => {
                                    (m + 1, a, c)
                                }
                                metacleaner_text::SvgFindingKind::NamespacedAttr => {
                                    (m, a + 1, c)
                                }
                                metacleaner_text::SvgFindingKind::Comment => (m, a, c + 1),
                            },
                        );
                        format!(
                            ", SVG: {meta_count} metadata element(s), {attr_count} namespaced attr(s), {comment_count} comment(s) removed"
                        )
                    };
                    let typography_note = if typography_removed.is_empty() {
                        String::new()
                    } else {
                        let total: usize = typography_removed.iter().map(|f| f.count).sum();
                        format!(", typography normalized: {total} character(s)")
                    };
                    println!(
                        "ok   {} -> {} [{} -> {} chars, {} removed{}{}{}{}]",
                        input_path.display(),
                        out_path.display(),
                        report.chars_in,
                        report.chars_out,
                        report.chars_in - report.chars_out,
                        fm_note,
                        html_note,
                        svg_note,
                        typography_note,
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
            "\n{}/{total} text files cleaned successfully.",
            total - failures
        );
    }

    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn is_markdown(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_lowercase),
        Some(ext) if ext == "md" || ext == "markdown"
    )
}

fn is_html(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_lowercase),
        Some(ext) if ext == "html" || ext == "htm"
    )
}

fn is_svg(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_lowercase),
        Some(ext) if ext == "svg"
    )
}

#[allow(clippy::type_complexity)]
fn process_text_one(
    input_path: &Path,
    opts: &metacleaner_text::CleanTextOptions,
    out_dir: Option<&Path>,
    in_place: bool,
    normalize_typography: bool,
) -> Result<
    (
        PathBuf,
        metacleaner_text::CleanTextReport,
        Vec<metacleaner_text::FrontmatterFinding>,
        Vec<metacleaner_text::HtmlFinding>,
        Vec<metacleaner_text::SvgFinding>,
        Vec<metacleaner_text::TypographyFinding>,
    ),
    CliError,
> {
    let bytes = fs::read(input_path).map_err(|e| CliError::Io(input_path.to_path_buf(), e))?;
    let text = String::from_utf8(bytes)
        .map_err(|e| CliError::Text(format!("not valid UTF-8 text: {e}")))?;

    let (text, frontmatter_removed) = if is_markdown(input_path) {
        let (stripped, fm_report) = metacleaner_text::strip_frontmatter(&text);
        (stripped, fm_report.removed)
    } else {
        (text, Vec::new())
    };

    let (text, html_removed) = if is_html(input_path) {
        let (stripped, html_report) = metacleaner_text::strip_html(&text);
        (stripped, html_report.findings)
    } else {
        (text, Vec::new())
    };

    let (text, svg_removed) = if is_svg(input_path) {
        let (stripped, svg_report) = metacleaner_text::strip_svg(&text);
        (stripped, svg_report.findings)
    } else {
        (text, Vec::new())
    };

    let (text, typography_removed) = if normalize_typography {
        let (normalized, typo_report) = metacleaner_text::normalize_typography(&text);
        (normalized, typo_report.findings)
    } else {
        (text, Vec::new())
    };

    let (cleaned, report) = metacleaner_text::clean_text(&text, opts);

    let out_path = if in_place {
        input_path.to_path_buf()
    } else {
        text_destination_path(input_path, out_dir)
    };

    fs::write(&out_path, cleaned.as_bytes()).map_err(|e| CliError::Io(out_path.clone(), e))?;
    Ok((
        out_path,
        report,
        frontmatter_removed,
        html_removed,
        svg_removed,
        typography_removed,
    ))
}

fn text_destination_path(input: &Path, out_dir: Option<&Path>) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let ext = input
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "txt".to_string());
    let file_name = format!("{stem}-clean.{ext}");

    match out_dir {
        Some(dir) => dir.join(file_name),
        None => input.with_file_name(file_name),
    }
}

fn run_inspect_text(args: &InspectTextArgs) -> ExitCode {
    let inputs = match expand_inputs(&args.inputs, args.recurse.recursive, is_text_extension) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut any_findings = false;
    let mut any_failures = false;
    let mut json_results = Vec::new();

    for input_path in &inputs {
        match inspect_text_one(input_path, args.ai_style) {
            Ok((report, fm_report, html_report, svg_report, typography_report, ai_style_report)) => {
                if !report.is_clean()
                    || !fm_report.is_clean()
                    || !html_report.is_clean()
                    || !svg_report.is_clean()
                    || !typography_report.is_clean()
                {
                    any_findings = true;
                }
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": true,
                        "char_count": report.char_count,
                        "clean": report.is_clean() && fm_report.is_clean() && html_report.is_clean() && svg_report.is_clean() && typography_report.is_clean(),
                        "findings": report.findings.iter().map(|f| serde_json::json!({
                            "category": f.category.as_str(),
                            "codepoint": format!("U+{:04X}", f.codepoint),
                            "count": f.count,
                        })).collect::<Vec<_>>(),
                        "frontmatter_findings": fm_report.removed.iter().map(|f| serde_json::json!({
                            "key": f.key,
                            "value": f.value,
                        })).collect::<Vec<_>>(),
                        "html_findings": html_report.findings.iter().map(|f| serde_json::json!({
                            "kind": f.kind.as_str(),
                            "label": f.label,
                            "value": f.value,
                        })).collect::<Vec<_>>(),
                        "svg_findings": svg_report.findings.iter().map(|f| serde_json::json!({
                            "kind": f.kind.as_str(),
                            "label": f.label,
                            "value": f.value,
                        })).collect::<Vec<_>>(),
                        "typography_findings": typography_report.findings.iter().map(|f| serde_json::json!({
                            "kind": f.kind.as_str(),
                            "codepoint": format!("U+{:04X}", f.codepoint),
                            "count": f.count,
                        })).collect::<Vec<_>>(),
                        "ai_style_findings": ai_style_report.as_ref().map(|r| r.findings.iter().map(|f| serde_json::json!({
                            "category": f.category.as_str(),
                            "label": f.label,
                            "detail": f.detail,
                        })).collect::<Vec<_>>()),
                        "ai_style_caveat": ai_style_report.as_ref().map(|_| metacleaner_text::FALSE_POSITIVE_CAVEAT),
                    }));
                } else {
                    println!("{}  [{} chars]", input_path.display(), report.char_count);
                    if report.is_clean()
                        && fm_report.is_clean()
                        && html_report.is_clean()
                        && svg_report.is_clean()
                        && typography_report.is_clean()
                    {
                        println!("  no invisible/steganography-relevant characters found");
                    } else {
                        for finding in &report.findings {
                            println!(
                                "  [{}] U+{:04X} x{}",
                                finding.category, finding.codepoint, finding.count
                            );
                        }
                        for finding in &fm_report.removed {
                            println!("  [frontmatter] {} = {}", finding.key, finding.value);
                        }
                        for finding in &html_report.findings {
                            println!(
                                "  [{}] {} = {}",
                                finding.kind.as_str(),
                                finding.label,
                                finding.value
                            );
                        }
                        for finding in &svg_report.findings {
                            println!(
                                "  [svg:{}] {} = {}",
                                finding.kind.as_str(),
                                finding.label,
                                finding.value
                            );
                        }
                        for finding in &typography_report.findings {
                            println!(
                                "  [typography:{}] U+{:04X} x{}",
                                finding.kind.as_str(),
                                finding.codepoint,
                                finding.count
                            );
                        }
                    }
                    if let Some(ai_style_report) = &ai_style_report {
                        if ai_style_report.is_clean() {
                            println!("  ai-style: no advisory patterns found");
                        } else {
                            println!("  ai-style ({}):", metacleaner_text::FALSE_POSITIVE_CAVEAT);
                            for finding in &ai_style_report.findings {
                                println!(
                                    "    [{}] {} {}",
                                    finding.category.as_str(),
                                    finding.label,
                                    finding.detail
                                );
                            }
                        }
                    }
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

fn inspect_text_one(
    input_path: &Path,
    ai_style: bool,
) -> Result<
    (
        metacleaner_text::InspectTextReport,
        metacleaner_text::FrontmatterReport,
        metacleaner_text::HtmlReport,
        metacleaner_text::SvgReport,
        metacleaner_text::TypographyReport,
        Option<metacleaner_text::AiStyleReport>,
    ),
    CliError,
> {
    let bytes = fs::read(input_path).map_err(|e| CliError::Io(input_path.to_path_buf(), e))?;
    let text = String::from_utf8(bytes)
        .map_err(|e| CliError::Text(format!("not valid UTF-8 text: {e}")))?;
    let fm_report = if is_markdown(input_path) {
        metacleaner_text::inspect_frontmatter(&text)
    } else {
        metacleaner_text::FrontmatterReport {
            had_frontmatter: false,
            removed: Vec::new(),
        }
    };
    let html_report = if is_html(input_path) {
        metacleaner_text::inspect_html(&text)
    } else {
        metacleaner_text::HtmlReport::default()
    };
    let svg_report = if is_svg(input_path) {
        metacleaner_text::inspect_svg(&text)
    } else {
        metacleaner_text::SvgReport::default()
    };
    let typography_report = metacleaner_text::inspect_typography(&text);
    let ai_style_report = ai_style.then(|| metacleaner_text::inspect_ai_style(&text));
    Ok((
        metacleaner_text::inspect_text(&text),
        fm_report,
        html_report,
        svg_report,
        typography_report,
        ai_style_report,
    ))
}

fn run_clean_doc(args: &CleanDocArgs) -> ExitCode {
    let inputs = match expand_inputs(&args.inputs, args.recurse.recursive, is_doc_extension) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let opts = metacleaner_docs::OoxmlOptions::default();

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
    let total = inputs.len();
    let mut json_results = Vec::new();

    for input_path in &inputs {
        match process_doc_one(input_path, &opts, args.out_dir.as_deref(), args.in_place) {
            Ok((out_path, report)) => {
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": true,
                        "output": out_path.display().to_string(),
                        "bytes_in": report.bytes_in,
                        "bytes_out": report.bytes_out,
                        "stripped_parts": report.stripped_parts,
                    }));
                } else {
                    println!(
                        "ok   {} -> {} [{} -> {} bytes, stripped: {}]",
                        input_path.display(),
                        out_path.display(),
                        report.bytes_in,
                        report.bytes_out,
                        report.stripped_parts.join(", "),
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
            "\n{}/{total} documents cleaned successfully.",
            total - failures
        );
    }

    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn process_doc_one(
    input_path: &Path,
    opts: &metacleaner_docs::OoxmlOptions,
    out_dir: Option<&Path>,
    in_place: bool,
) -> Result<(PathBuf, metacleaner_docs::OoxmlCleanReport), CliError> {
    let size = fs::metadata(input_path)
        .map_err(|e| CliError::Io(input_path.to_path_buf(), e))?
        .len();
    if size > opts.max_input_bytes {
        return Err(CliError::Text(format!(
            "input is {size} bytes, which exceeds the {}-byte limit",
            opts.max_input_bytes
        )));
    }

    let bytes = fs::read(input_path).map_err(|e| CliError::Io(input_path.to_path_buf(), e))?;
    let (cleaned, report) =
        metacleaner_docs::clean_ooxml(&bytes, opts).map_err(|e| CliError::Text(e.to_string()))?;

    let out_path = if in_place {
        input_path.to_path_buf()
    } else {
        doc_destination_path(input_path, out_dir)
    };

    fs::write(&out_path, &cleaned).map_err(|e| CliError::Io(out_path.clone(), e))?;
    Ok((out_path, report))
}

fn doc_destination_path(input: &Path, out_dir: Option<&Path>) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let ext = input
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "docx".to_string());
    let file_name = format!("{stem}-clean.{ext}");

    match out_dir {
        Some(dir) => dir.join(file_name),
        None => input.with_file_name(file_name),
    }
}

fn run_inspect_doc(args: &InspectDocArgs) -> ExitCode {
    let inputs = match expand_inputs(&args.inputs, args.recurse.recursive, is_doc_extension) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut any_findings = false;
    let mut any_failures = false;
    let mut json_results = Vec::new();

    for input_path in &inputs {
        match inspect_doc_one(input_path) {
            Ok(report) => {
                if !report.is_clean() {
                    any_findings = true;
                }
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": true,
                        "clean": report.is_clean(),
                        "findings": report.findings.iter().map(|f| serde_json::json!({
                            "part": f.part,
                            "field": f.field,
                            "value": f.value,
                        })).collect::<Vec<_>>(),
                    }));
                } else {
                    println!("{}", input_path.display());
                    if report.is_clean() {
                        println!("  no identifying metadata found");
                    } else {
                        for finding in &report.findings {
                            println!("  [{}] {} = {}", finding.part, finding.field, finding.value);
                        }
                    }
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

fn inspect_doc_one(input_path: &Path) -> Result<metacleaner_docs::InspectOoxmlReport, CliError> {
    let bytes = fs::read(input_path).map_err(|e| CliError::Io(input_path.to_path_buf(), e))?;
    metacleaner_docs::inspect_ooxml(&bytes, &metacleaner_docs::OoxmlOptions::default())
        .map_err(|e| CliError::Text(e.to_string()))
}

fn run_clean_pdf(args: &CleanPdfArgs) -> ExitCode {
    let inputs = match expand_inputs(&args.inputs, args.recurse.recursive, is_pdf_extension) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let opts = metacleaner_pdf::PdfOptions {
        max_input_bytes: args.max_input_mb * 1024 * 1024,
        ..Default::default()
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
    let total = inputs.len();
    let mut json_results = Vec::new();

    for input_path in &inputs {
        match process_pdf_one(input_path, &opts, args.out_dir.as_deref(), args.in_place) {
            Ok((out_path, report)) => {
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": true,
                        "output": out_path.display().to_string(),
                        "bytes_in": report.bytes_in,
                        "bytes_out": report.bytes_out,
                        "stripped": report.stripped,
                    }));
                } else {
                    let stripped = if report.stripped.is_empty() {
                        "nothing found".to_string()
                    } else {
                        report.stripped.join(", ")
                    };
                    println!(
                        "ok   {} -> {} [{} -> {} bytes, stripped: {}]",
                        input_path.display(),
                        out_path.display(),
                        report.bytes_in,
                        report.bytes_out,
                        stripped,
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
            "\n{}/{total} PDF files cleaned successfully.",
            total - failures
        );
    }

    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn process_pdf_one(
    input_path: &Path,
    opts: &metacleaner_pdf::PdfOptions,
    out_dir: Option<&Path>,
    in_place: bool,
) -> Result<(PathBuf, metacleaner_pdf::PdfCleanReport), CliError> {
    let bytes = fs::read(input_path).map_err(|e| CliError::Io(input_path.to_path_buf(), e))?;
    let (cleaned, report) =
        metacleaner_pdf::clean_pdf(&bytes, opts).map_err(|e| CliError::Text(e.to_string()))?;

    let out_path = if in_place {
        input_path.to_path_buf()
    } else {
        pdf_destination_path(input_path, out_dir)
    };

    fs::write(&out_path, &cleaned).map_err(|e| CliError::Io(out_path.clone(), e))?;
    Ok((out_path, report))
}

fn pdf_destination_path(input: &Path, out_dir: Option<&Path>) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let file_name = format!("{stem}-clean.pdf");

    match out_dir {
        Some(dir) => dir.join(file_name),
        None => input.with_file_name(file_name),
    }
}

fn run_inspect_pdf(args: &InspectPdfArgs) -> ExitCode {
    let inputs = match expand_inputs(&args.inputs, args.recurse.recursive, is_pdf_extension) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut any_findings = false;
    let mut any_failures = false;
    let mut json_results = Vec::new();

    for input_path in &inputs {
        match inspect_pdf_one(input_path) {
            Ok(report) => {
                if !report.is_clean() {
                    any_findings = true;
                }
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": true,
                        "clean": report.is_clean(),
                        "findings": report.findings.iter().map(|f| serde_json::json!({
                            "location": f.location,
                            "field": f.field,
                            "value": f.value,
                        })).collect::<Vec<_>>(),
                    }));
                } else {
                    println!("{}", input_path.display());
                    if report.is_clean() {
                        println!("  no identifying metadata found");
                    } else {
                        for finding in &report.findings {
                            println!(
                                "  [{}] {} = {}",
                                finding.location, finding.field, finding.value
                            );
                        }
                    }
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

fn inspect_pdf_one(input_path: &Path) -> Result<metacleaner_pdf::InspectPdfReport, CliError> {
    let bytes = fs::read(input_path).map_err(|e| CliError::Io(input_path.to_path_buf(), e))?;
    metacleaner_pdf::inspect_pdf(&bytes, &metacleaner_pdf::PdfOptions::default())
        .map_err(|e| CliError::Text(e.to_string()))
}

fn run_clean_media(args: &CleanMediaArgs) -> ExitCode {
    let inputs = match expand_inputs(&args.inputs, args.recurse.recursive, is_media_extension) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let opts = metacleaner_media::MediaOptions {
        max_input_bytes: args.max_input_mb * 1024 * 1024,
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
    let total = inputs.len();
    let mut json_results = Vec::new();

    for input_path in &inputs {
        match process_media_one(input_path, &opts, args.out_dir.as_deref(), args.in_place) {
            Ok((out_path, bytes_in, bytes_out, stripped)) => {
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": true,
                        "output": out_path.display().to_string(),
                        "bytes_in": bytes_in,
                        "bytes_out": bytes_out,
                        "stripped": stripped,
                    }));
                } else {
                    let stripped_note = if stripped.is_empty() {
                        "nothing found".to_string()
                    } else {
                        stripped.join(", ")
                    };
                    println!(
                        "ok   {} -> {} [{} -> {} bytes, stripped: {}]",
                        input_path.display(),
                        out_path.display(),
                        bytes_in,
                        bytes_out,
                        stripped_note,
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
            "\n{}/{total} media files cleaned successfully.",
            total - failures
        );
    }

    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[allow(clippy::type_complexity)]
fn process_media_one(
    input_path: &Path,
    opts: &metacleaner_media::MediaOptions,
    out_dir: Option<&Path>,
    in_place: bool,
) -> Result<(PathBuf, usize, usize, Vec<String>), CliError> {
    let bytes = fs::read(input_path).map_err(|e| CliError::Io(input_path.to_path_buf(), e))?;
    let bytes_in = bytes.len();

    let (cleaned, stripped) = if is_mp3(input_path) {
        metacleaner_media::clean_mp3(&bytes, opts).map_err(|e| CliError::Text(e.to_string()))?
    } else {
        metacleaner_media::clean_mp4(&bytes, opts).map_err(|e| CliError::Text(e.to_string()))?
    };
    let bytes_out = cleaned.len();

    let out_path = if in_place {
        input_path.to_path_buf()
    } else {
        media_destination_path(input_path, out_dir)
    };

    fs::write(&out_path, &cleaned).map_err(|e| CliError::Io(out_path.clone(), e))?;
    Ok((out_path, bytes_in, bytes_out, stripped))
}

fn media_destination_path(input: &Path, out_dir: Option<&Path>) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let ext = input
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "mp3".to_string());
    let file_name = format!("{stem}-clean.{ext}");

    match out_dir {
        Some(dir) => dir.join(file_name),
        None => input.with_file_name(file_name),
    }
}

fn run_inspect_media(args: &InspectMediaArgs) -> ExitCode {
    let inputs = match expand_inputs(&args.inputs, args.recurse.recursive, is_media_extension) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut any_findings = false;
    let mut any_failures = false;
    let mut json_results = Vec::new();

    for input_path in &inputs {
        match inspect_media_one(input_path) {
            Ok(findings) => {
                if !findings.is_empty() {
                    any_findings = true;
                }
                if args.json {
                    json_results.push(serde_json::json!({
                        "file": input_path.display().to_string(),
                        "ok": true,
                        "clean": findings.is_empty(),
                        "findings": findings.iter().map(|f| serde_json::json!({
                            "location": f.location,
                            "field": f.field,
                            "value": f.value,
                        })).collect::<Vec<_>>(),
                    }));
                } else {
                    println!("{}", input_path.display());
                    if findings.is_empty() {
                        println!("  no identifying metadata found");
                    } else {
                        for finding in &findings {
                            println!(
                                "  [{}] {} = {}",
                                finding.location, finding.field, finding.value
                            );
                        }
                    }
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

fn inspect_media_one(input_path: &Path) -> Result<Vec<metacleaner_media::MediaFinding>, CliError> {
    let bytes = fs::read(input_path).map_err(|e| CliError::Io(input_path.to_path_buf(), e))?;
    let opts = metacleaner_media::MediaOptions::default();
    if is_mp3(input_path) {
        metacleaner_media::inspect_mp3(&bytes, &opts).map_err(|e| CliError::Text(e.to_string()))
    } else {
        metacleaner_media::inspect_mp4(&bytes, &opts).map_err(|e| CliError::Text(e.to_string()))
    }
}

#[derive(Debug)]
enum CliError {
    Io(PathBuf, std::io::Error),
    Clean(CleanError),
    AiUpscale(String),
    Text(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Io(path, e) => write!(f, "I/O error on {}: {e}", path.display()),
            CliError::Clean(e) => write!(f, "{e}"),
            CliError::AiUpscale(e) => write!(f, "{e}"),
            CliError::Text(e) => write!(f, "{e}"),
        }
    }
}
