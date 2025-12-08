//! GeoJSON Renderer Server
//!
//! A performant Axum web server that receives GeoJSON, renders it on a MapLibre base map
//! with auto-fitted viewport, and saves the result as WebP images.
//!
//! # Usage
//!
//! Start the server:
//! ```bash
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
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use geojson::GeoJson;
use maplibre_native::StaticRenderPool;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use bbox::calculate_bbox;
use storage::ImageStorage;

/// Application state shared across handlers.
struct AppState {
    render_pool: StaticRenderPool,
    storage: ImageStorage,
}

/// Request body for the /render endpoint.
#[derive(Debug, Deserialize)]
struct RenderRequest {
    /// GeoJSON data to render (FeatureCollection, Feature, or Geometry)
    geojson: serde_json::Value,
    /// Optional image width in pixels (default: 512)
    #[serde(default = "default_size")]
    width: u32,
    /// Optional image height in pixels (default: 512)
    #[serde(default = "default_size")]
    height: u32,
    /// Optional padding factor for auto-fit (default: 0.1 = 10%)
    #[serde(default = "default_padding")]
    padding: f64,
}

fn default_size() -> u32 {
    512
}

fn default_padding() -> f64 {
    0.1
}

/// Response from the /render endpoint.
#[derive(Debug, Serialize)]
struct RenderResponse {
    /// Path to the rendered image file
    file: String,
    /// Whether this was a cache hit
    cached: bool,
}

/// Error response.
#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

/// Health check endpoint.
async fn health() -> &'static str {
    "OK"
}

/// Render GeoJSON to an image.
async fn render(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RenderRequest>,
) -> Result<Json<RenderResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Serialize GeoJSON for hashing and rendering
    let geojson_str = serde_json::to_string(&request.geojson).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Invalid JSON: {e}"),
            }),
        )
    })?;

    // Generate hash-based filename for deduplication
    let hash_content = format!(
        "{}:{}:{}:{}",
        geojson_str, request.width, request.height, request.padding
    );
    let filename = ImageStorage::generate_filename(&hash_content);

    // Check if already rendered (cache hit)
    if state.storage.exists(&filename).await {
        let file_path = state.storage.path(&filename);
        return Ok(Json(RenderResponse {
            file: file_path.display().to_string(),
            cached: true,
        }));
    }

    // Parse GeoJSON to calculate bounding box
    let geojson: GeoJson = serde_json::from_value(request.geojson.clone()).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Invalid GeoJSON: {e}"),
            }),
        )
    })?;

    // Calculate bounding box and camera parameters
    let bbox = calculate_bbox(&geojson).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Could not calculate bounding box from GeoJSON".to_string(),
            }),
        )
    })?;

    let (lat, lon) = bbox.center();
    let zoom = bbox.fit_zoom(request.width, request.height, request.padding);

    // Render the image
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

    // Save as WebP
    let file_path = state
        .storage
        .save_webp(image.as_image(), &filename)
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

#[tokio::main]
async fn main() {
    // Initialize the render pool with the base style
    let style_path = base_style_path();
    println!("Loading base style from: {}", style_path.display());

    let render_pool = StaticRenderPool::new(style_path, "geojson-overlay".to_string());

    // Initialize storage
    let output_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("output");
    let storage = ImageStorage::new(&output_dir)
        .await
        .expect("Failed to create output directory");
    println!("Output directory: {}", output_dir.display());

    // Create application state
    let state = Arc::new(AppState {
        render_pool,
        storage,
    });

    // Build router
    let app = Router::new()
        .route("/health", get(health))
        .route("/render", post(render))
        .with_state(state);

    // Start server
    let addr = "127.0.0.1:3000";
    println!("GeoJSON Renderer Server running on http://{addr}");
    println!();
    println!("Example usage:");
    println!(r#"  curl -X POST http://{addr}/render \"#);
    println!(r#"    -H "Content-Type: application/json" \"#);
    println!(
        r#"    -d '{{"geojson": {{"type": "Point", "coordinates": [10, 50]}}}}'"#
    );

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

