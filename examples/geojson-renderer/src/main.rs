//! Run with defaults (style: base-style.json, output: ./output):
//! ```bash
//! cargo run --example geojson-renderer
//! ```
//!
//! Override via CLI:
//! ```bash
//! cargo run --example geojson-renderer -- \
//!   --style /path/to/style.json \
//!   --output /path/to/output-dir
//! ```
//!
//! Or via environment variables:
//! ```bash
//! GEOJSON_RENDERER_STYLE_PATH=/path/to/style.json \
//! GEOJSON_RENDERER_OUTPUT_DIR=/path/to/output-dir \
//! cargo run --example geojson-renderer
//! ```
//!
//! Send a render request:
//! ```bash
//! curl -X POST http://localhost:3000/render \
//!   -H "Content-Type: application/json" \
//!   -d '{"geojson": {"type": "Point", "coordinates": [0, 0]}}'
//! ```

mod bbox;
mod storage;

use axum::{
    extract::{Json, State},
    http::StatusCode,
    routing::{get, post},
    Router,
};
use clap::Parser;
use geojson::GeoJson;
use image::imageops::{self, FilterType};
use maplibre_native::StaticRenderPool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use bbox::calculate_bbox;
use storage::ImageStorage;

/// Default viewport dimensions for the renderer.
const VIEWPORT_WIDTH: u32 = 1024;
const VIEWPORT_HEIGHT: u32 = 1024;

/// Application state shared across handlers.
struct AppState {
    render_pool: StaticRenderPool,
    storage: ImageStorage,
}

#[derive(Debug, Deserialize)]
struct RenderRequest {
    geojson: serde_json::Value,
    #[serde(default = "default_size")]
    width: u32,
    #[serde(default = "default_size")]
    height: u32,
    #[serde(default = "default_padding")]
    padding: f64,
}

fn default_size() -> u32 {
    512
}

fn default_padding() -> f64 {
    0.1
}

#[derive(Debug, Serialize)]
struct RenderResponse {
    file: String,
    cached: bool,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Parser, Debug)]
#[command(name = "geojson-renderer", version, about = "GeoJSON Renderer Server")]
struct Cli {
    #[arg(long, env = "GEOJSON_RENDERER_STYLE_PATH")]
    style: Option<PathBuf>,

    #[arg(long, env = "GEOJSON_RENDERER_OUTPUT_DIR")]
    output: Option<PathBuf>,
}

async fn health() -> &'static str {
    "OK"
}

async fn render(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RenderRequest>,
) -> Result<Json<RenderResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Validate dimensions - don't allow upscaling beyond viewport
    let viewport_width = state.render_pool.width();
    let viewport_height = state.render_pool.height();

    if request.width > viewport_width || request.height > viewport_height {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!(
                    "Requested dimensions {}x{} exceed viewport dimensions {}x{}. Upscaling is not supported.",
                    request.width, request.height, viewport_width, viewport_height
                ),
            }),
        ));
    }

    // Serialize GeoJSON for hashing and rendering
    let geojson_str = serde_json::to_string(&request.geojson).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Invalid JSON: {e}"),
            }),
        )
    })?;

    let hash_content = format!(
        "{}:{}:{}:{}",
        geojson_str, request.width, request.height, request.padding
    );
    let filename = ImageStorage::generate_filename(&hash_content);

    if state.storage.exists(&filename).await {
        let file_path = state.storage.path(&filename);
        return Ok(Json(RenderResponse {
            file: file_path.display().to_string(),
            cached: true,
        }));
    }

    let geojson: GeoJson = serde_json::from_value(request.geojson.clone()).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Invalid GeoJSON: {e}"),
            }),
        )
    })?;

    let bbox = calculate_bbox(&geojson).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Could not calculate bounding box from GeoJSON".to_string(),
            }),
        )
    })?;

    let (lat, lon) = bbox.center();
    let zoom = bbox.fit_zoom(
        viewport_width,
        viewport_height,
        request.width,
        request.height,
        request.padding,
    );

    let image = state
        .render_pool
        .render_static(geojson_str, lat, lon, zoom, 0.0, 0.0)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Rendering failed: {e}"),
                }),
            )
        })?;

    // Crop and resize image to requested dimensions if different from viewport
    let final_image = if request.width != viewport_width || request.height != viewport_height {
        // Calculate crop dimensions to match output aspect ratio
        let output_aspect = f64::from(request.width) / f64::from(request.height);
        let viewport_aspect = f64::from(viewport_width) / f64::from(viewport_height);

        let (crop_width, crop_height) = if output_aspect > viewport_aspect {
            // Output is wider - keep full width, crop height
            let crop_w = viewport_width;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let crop_h = (f64::from(viewport_width) / output_aspect).round() as u32;
            (crop_w, crop_h)
        } else {
            // Output is taller - keep full height, crop width
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let crop_w = (f64::from(viewport_height) * output_aspect).round() as u32;
            let crop_h = viewport_height;
            (crop_w, crop_h)
        };

        // Calculate crop offset (center crop)
        let crop_x = (viewport_width - crop_width) / 2;
        let crop_y = (viewport_height - crop_height) / 2;

        // Crop from center
        let cropped = imageops::crop_imm(image.as_image(), crop_x, crop_y, crop_width, crop_height)
            .to_image();

        // Resize to final output dimensions
        imageops::resize(
            &cropped,
            request.width,
            request.height,
            FilterType::Lanczos3,
        )
    } else {
        image.as_image().clone()
    };

    // Save as WebP
    let file_path = state
        .storage
        .save_webp(&final_image, &filename)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to save image: {e}"),
                }),
            )
        })?;

    Ok(Json(RenderResponse {
        file: file_path.display().to_string(),
        cached: false,
    }))
}

fn base_style_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("base-style.json")
}

fn default_output_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("output")
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let style_path = cli.style.unwrap_or_else(base_style_path);
    println!("Loading base style from: {}", style_path.display());

    let render_pool = StaticRenderPool::new(
        style_path,
        "geojson-overlay".to_string(),
        VIEWPORT_WIDTH,
        VIEWPORT_HEIGHT,
    );

    let output_dir = cli.output.unwrap_or_else(default_output_dir);
    let storage = ImageStorage::new(&output_dir)
        .await
        .expect("Failed to create output directory");
    println!("Output directory: {}", output_dir.display());

    let state = Arc::new(AppState {
        render_pool,
        storage,
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/render", post(render))
        .with_state(state);

    let addr = "127.0.0.1:3000";
    println!("GeoJSON Renderer Server running on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
