//! Local web UI: a tiny HTTP server, bound to loopback by default, that
//! serves an embedded single-page app and exposes `/api/inspect` and
//! `/api/clean` over the same `metacleaner-core` functions the CLI uses.
//!
//! Everything the browser needs (HTML/CSS/JS) is compiled into the binary
//! via `include_str!` — no assets directory needs to ship alongside it,
//! which matters once this is packaged as a single apt-installable binary.
//! Nothing here ever makes an outbound network call; the server only
//! answers requests, it never initiates them.

use std::collections::HashMap;
use std::net::SocketAddr;

use axum::extract::{DefaultBodyLimit, Multipart};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use metacleaner_core::{clean, inspect, CleanOptions, ImageFormat, InspectOptions};

const INDEX_HTML: &str = include_str!("../assets/index.html");
const STYLE_CSS: &str = include_str!("../assets/style.css");
const APP_JS: &str = include_str!("../assets/app.js");

pub struct ServeConfig {
    pub host: String,
    pub port: u16,
    pub open_browser: bool,
}

pub async fn run(config: ServeConfig) -> std::io::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/style.css", get(style_css))
        .route("/app.js", get(app_js))
        .route("/api/inspect", post(api_inspect))
        .route("/api/clean", post(api_clean))
        // Belt-and-suspenders network-level cap, on top of the
        // application-level max_input_bytes check clean()/inspect() do
        // themselves — reject an oversized body before it's even buffered.
        .layer(DefaultBodyLimit::max(
            metacleaner_core::DEFAULT_MAX_INPUT_BYTES as usize,
        ));

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .map_err(std::io::Error::other)?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let url = format!("http://{addr}");
    println!("metacleaner web UI running at {url}");
    println!("(bound to {}; press Ctrl+C to stop)", config.host);

    if config.open_browser {
        let _ = open::that(&url);
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
}

async fn style_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLE_CSS,
    )
}

async fn app_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
}

/// A parsed multipart request: the uploaded file's bytes/name, plus any
/// other text fields sent alongside it.
struct Upload {
    file_name: String,
    file_bytes: Vec<u8>,
    fields: HashMap<String, String>,
}

async fn parse_upload(mut multipart: Multipart) -> Result<Upload, String> {
    let mut file_name = None;
    let mut file_bytes = None;
    let mut fields = HashMap::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| format!("invalid upload: {e}"))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            file_name = Some(field.file_name().unwrap_or("upload").to_string());
            file_bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| format!("failed to read upload: {e}"))?
                    .to_vec(),
            );
        } else {
            let value = field
                .text()
                .await
                .map_err(|e| format!("invalid field {name}: {e}"))?;
            fields.insert(name, value);
        }
    }

    Ok(Upload {
        file_name: file_name.ok_or_else(|| "missing \"file\" field".to_string())?,
        file_bytes: file_bytes.ok_or_else(|| "missing \"file\" field".to_string())?,
        fields,
    })
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn json_error(status: StatusCode, message: impl std::fmt::Display) -> Response {
    json_response(
        status,
        serde_json::json!({ "ok": false, "error": message.to_string() }),
    )
}

async fn api_inspect(multipart: Multipart) -> Response {
    let upload = match parse_upload(multipart).await {
        Ok(u) => u,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
    };

    match inspect(&upload.file_bytes, &InspectOptions::default()) {
        Ok(report) => json_response(
            StatusCode::OK,
            serde_json::json!({
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
            }),
        ),
        Err(e) => json_error(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

async fn api_clean(multipart: Multipart) -> Response {
    let upload = match parse_upload(multipart).await {
        Ok(u) => u,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
    };

    let opts = match options_from_fields(&upload.fields) {
        Ok(o) => o,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, e),
    };

    match clean(&upload.file_bytes, &opts) {
        Ok(cleaned) => {
            let stem = std::path::Path::new(&upload.file_name)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "image".to_string());
            let out_name = format!("{stem}-clean.{}", cleaned.report.output_format.extension());
            json_response(
                StatusCode::OK,
                serde_json::json!({
                    "ok": true,
                    "filename": out_name,
                    "mime": mime_for(cleaned.report.output_format),
                    "format": format!("{:?}", cleaned.report.output_format).to_lowercase(),
                    "width": cleaned.report.width,
                    "height": cleaned.report.height,
                    "bytes_in": cleaned.report.bytes_in,
                    "bytes_out": cleaned.report.bytes_out,
                    "fingerprint_reset": cleaned.report.fingerprint_reset,
                    "data_base64": BASE64.encode(&cleaned.bytes),
                }),
            )
        }
        Err(e) => json_error(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

fn options_from_fields(fields: &HashMap<String, String>) -> Result<CleanOptions, String> {
    let mut opts = CleanOptions::default();

    if let Some(v) = fields.get("reset_fingerprint") {
        opts.reset_fingerprint = v == "true";
    }
    if let Some(v) = fields.get("fingerprint_strength") {
        opts.fingerprint_strength = v
            .parse()
            .map_err(|_| "invalid fingerprint_strength".to_string())?;
    }
    if let Some(v) = fields.get("fingerprint_fraction") {
        opts.fingerprint_fraction = v
            .parse()
            .map_err(|_| "invalid fingerprint_fraction".to_string())?;
    }
    if let Some(v) = fields.get("jpeg_quality") {
        opts.jpeg_quality = v.parse().map_err(|_| "invalid jpeg_quality".to_string())?;
    }
    if let Some(v) = fields.get("format") {
        opts.output_format = Some(match v.as_str() {
            "jpeg" => ImageFormat::Jpeg,
            "png" => ImageFormat::Png,
            "webp" => ImageFormat::WebP,
            "bmp" => ImageFormat::Bmp,
            "gif" => ImageFormat::Gif,
            "tiff" => ImageFormat::Tiff,
            other => return Err(format!("unknown output format \"{other}\"")),
        });
    }

    Ok(opts)
}

fn mime_for(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Png => "image/png",
        ImageFormat::WebP => "image/webp",
        ImageFormat::Bmp => "image/bmp",
        ImageFormat::Gif => "image/gif",
        ImageFormat::Tiff => "image/tiff",
    }
}
