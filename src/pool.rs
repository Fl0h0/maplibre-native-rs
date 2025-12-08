//! Simple rendering pool for thread-safe [MapLibre Native](https://maplibre.org/projects/native/) rendering.
//!
//! This module provides a minimal thread-safe rendering pool that prevents
//! segmentation faults when used concurrently.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() {
//! use maplibre_native::SingleThreadedRenderPool;
//! use std::path::PathBuf;
//!
//! // Get the global pool instance
//! let pool = SingleThreadedRenderPool::global_pool();
//!
//! // Render a tile with a MapLibre style
//! let style_path = PathBuf::from("path/to/style.json");
//! let image = pool.render_tile(style_path.clone(), 10, 512, 384).await.unwrap();
//!
//! // The pool automatically handles style caching - subsequent renders
//! // with the same style will be faster
//! let another_tile = pool.render_tile(style_path.clone(), 10, 513, 384).await.unwrap();
//! # }
//! ```

use std::path::PathBuf;
use std::sync::{mpsc, LazyLock};
use std::thread;

use tokio::sync::oneshot;

use crate::renderer::{Image, ImageRendererBuilder, RenderingError};

/// Rendering request sent to the pool.
struct RenderRequest {
    style_path: PathBuf,
    z: u8,
    x: u32,
    y: u32,
    response: oneshot::Sender<Result<Image, SingleThreadedRenderPoolError>>,
}

/// A thread-safe rendering pool that serializes [MapLibre Native](https://maplibre.org/projects/native/) tile rendering
/// operations through a single worker thread.
///
/// Prevents segmentation faults by ensuring all rendering operations are handled
/// sequentially. Automatically loads and caches styles as needed.
///
/// Use [`SingleThreadedRenderPool::global_pool`] to access the shared instance.
#[derive(Debug, Clone)]
pub struct SingleThreadedRenderPool {
    rendering_requests: mpsc::Sender<RenderRequest>,
}

impl SingleThreadedRenderPool {
    /// Create a new rendering pool
    ///
    /// Purposely not public to prevent accidental misuse.
    pub(crate) fn new() -> Self {
        let (tx, rx) = mpsc::channel::<RenderRequest>();

        thread::spawn(move || {
            let mut renderer = ImageRendererBuilder::default().build_tile_renderer();
            let mut current_style: Option<PathBuf> = None;

            while let Ok(request) = rx.recv() {
                // Load style if it is different from current
                if current_style.as_ref() != Some(&request.style_path) {
                    if let Err(e) = renderer.load_style_from_path(&request.style_path) {
                        let _ = request
                            .response
                            .send(Err(SingleThreadedRenderPoolError::IOError(e)));
                        continue;
                    }
                    current_style = Some(request.style_path.clone());
                }
                // TODO: handle style changing on the disk

                // Render the tile
                let result = renderer
                    .render_tile(request.z, request.x, request.y)
                    .map_err(SingleThreadedRenderPoolError::RenderingError);
                let _ = request.response.send(result);
            }
        });

        Self {
            rendering_requests: tx,
        }
    }

    /// Render an encoded tile [`Image`] asynchronously in a centralised pool
    ///
    /// # Errors
    ///
    /// If the rendering fails, the response channel is dropped, or the request fails to send.
    pub async fn render_tile(
        &self,
        style_path: PathBuf,
        z: u8,
        x: u32,
        y: u32,
    ) -> Result<Image, SingleThreadedRenderPoolError> {
        let (response_tx, response_rx) = oneshot::channel();

        self.rendering_requests
            .send(RenderRequest {
                style_path,
                z,
                x,
                y,
                response: response_tx,
            })
            .map_err(|_| SingleThreadedRenderPoolError::FailedToSendRequest)?;

        response_rx
            .await
            .map_err(|_| SingleThreadedRenderPoolError::FailedToReceiveResponse)?
    }

    /// Get the global rendering pool instance.
    #[must_use]
    pub fn global_pool() -> &'static SingleThreadedRenderPool {
        static GLOBAL_POOL: LazyLock<SingleThreadedRenderPool> =
            LazyLock::new(SingleThreadedRenderPool::new);

        &GLOBAL_POOL
    }
}

/// Errors that can occur in the single-threaded render pool.
#[derive(thiserror::Error, Debug)]
pub enum SingleThreadedRenderPoolError {
    /// An I/O error occurred during rendering operations.
    #[error(transparent)]
    IOError(#[from] std::io::Error),

    /// A rendering error occurred during map rendering.
    #[error(transparent)]
    RenderingError(#[from] RenderingError),

    /// Failed to send a rendering request to the worker thread.
    #[error("Failed to send request to rendering thread")]
    FailedToSendRequest,

    /// Failed to receive a response from the worker thread.
    #[error("Failed to receive response from rendering thread")]
    FailedToReceiveResponse,
}

/// Request to render a static map with GeoJSON overlay.
struct StaticRenderRequest {
    /// GeoJSON string to set on the source
    geojson: String,
    /// Source ID in the style to update with the GeoJSON
    source_id: String,
    /// Camera latitude
    lat: f64,
    /// Camera longitude
    lon: f64,
    /// Camera zoom level
    zoom: f64,
    /// Camera bearing (rotation)
    bearing: f64,
    /// Camera pitch (tilt)
    pitch: f64,
    /// Response channel
    response: oneshot::Sender<Result<Image, StaticRenderPoolError>>,
}

/// A thread-safe rendering pool for static map images with dynamic GeoJSON overlays.
///
/// This pool is optimized for rendering GeoJSON data on a base map:
/// - Loads the base style once at initialization
/// - Updates only the GeoJSON source data per request (no style reload)
/// - Efficiently re-renders with different camera positions
///
/// # Example
///
/// ```no_run
/// # async fn example() {
/// use maplibre_native::StaticRenderPool;
/// use std::path::PathBuf;
///
/// // Create a pool with a base style containing a GeoJSON source
/// let pool = StaticRenderPool::new(
///     PathBuf::from("base-style.json"),
///     "geojson-overlay".to_string(),
/// );
///
/// // Render with GeoJSON data
/// let geojson = r#"{"type": "FeatureCollection", "features": []}"#;
/// let image = pool.render_static(geojson.to_string(), 0.0, 0.0, 2.0, 0.0, 0.0).await.unwrap();
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct StaticRenderPool {
    rendering_requests: mpsc::Sender<StaticRenderRequest>,
    source_id: String,
}

impl StaticRenderPool {
    /// Create a new static rendering pool with the given base style.
    ///
    /// The base style should contain a GeoJSON source with the given `source_id`
    /// and appropriate layers to render the GeoJSON features.
    ///
    /// # Arguments
    /// * `style_path` - Path to the base style JSON file
    /// * `source_id` - ID of the GeoJSON source in the style to update
    #[must_use]
    pub fn new(style_path: PathBuf, source_id: String) -> Self {
        let (tx, rx) = mpsc::channel::<StaticRenderRequest>();
        let source_id_clone = source_id.clone();

        thread::spawn(move || {
            let mut renderer = ImageRendererBuilder::default().build_static_renderer();

            // Load the base style once at startup
            if let Err(e) = renderer.load_style_from_path(&style_path) {
                // Log error but keep thread alive to handle requests gracefully
                #[cfg(feature = "log")]
                log::error!("Failed to load base style: {e}");
                // Drain requests with error responses
                while let Ok(request) = rx.recv() {
                    let _ = request.response.send(Err(StaticRenderPoolError::IOError(
                        std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            format!("Base style not loaded: {e}"),
                        ),
                    )));
                }
                return;
            }

            // Do a warmup render to ensure the style is fully loaded
            // This blocks until the style, sources, and layers are initialized
            let _ = renderer.render_static(0.0, 0.0, 1.0, 0.0, 0.0);
            #[cfg(feature = "log")]
            log::info!("StaticRenderPool warmup complete, style fully loaded");

            while let Ok(request) = rx.recv() {
                // Update the GeoJSON source data
                if let Err(e) =
                    renderer.set_geojson_source_data(&request.source_id, &request.geojson)
                {
                    let _ = request
                        .response
                        .send(Err(StaticRenderPoolError::RenderingError(e)));
                    continue;
                }

                // Render the static image
                let result = renderer
                    .render_static(
                        request.lat,
                        request.lon,
                        request.zoom,
                        request.bearing,
                        request.pitch,
                    )
                    .map_err(StaticRenderPoolError::RenderingError);
                let _ = request.response.send(result);
            }
        });

        Self {
            rendering_requests: tx,
            source_id: source_id_clone,
        }
    }

    /// Render a static map image with the given GeoJSON data and camera position.
    ///
    /// # Arguments
    /// * `geojson` - GeoJSON string (FeatureCollection, Feature, or Geometry)
    /// * `lat` - Camera latitude
    /// * `lon` - Camera longitude
    /// * `zoom` - Camera zoom level
    /// * `bearing` - Camera bearing (rotation in degrees)
    /// * `pitch` - Camera pitch (tilt in degrees)
    ///
    /// # Errors
    /// Returns an error if rendering fails or the request cannot be processed.
    pub async fn render_static(
        &self,
        geojson: String,
        lat: f64,
        lon: f64,
        zoom: f64,
        bearing: f64,
        pitch: f64,
    ) -> Result<Image, StaticRenderPoolError> {
        let (response_tx, response_rx) = oneshot::channel();

        self.rendering_requests
            .send(StaticRenderRequest {
                geojson,
                source_id: self.source_id.clone(),
                lat,
                lon,
                zoom,
                bearing,
                pitch,
                response: response_tx,
            })
            .map_err(|_| StaticRenderPoolError::FailedToSendRequest)?;

        response_rx
            .await
            .map_err(|_| StaticRenderPoolError::FailedToReceiveResponse)?
    }

    /// Get the source ID used for GeoJSON data.
    #[must_use]
    pub fn source_id(&self) -> &str {
        &self.source_id
    }
}

/// Errors that can occur in the static render pool.
#[derive(thiserror::Error, Debug)]
pub enum StaticRenderPoolError {
    /// An I/O error occurred during rendering operations.
    #[error(transparent)]
    IOError(#[from] std::io::Error),

    /// A rendering error occurred during map rendering.
    #[error(transparent)]
    RenderingError(#[from] RenderingError),

    /// Failed to send a rendering request to the worker thread.
    #[error("Failed to send request to rendering thread")]
    FailedToSendRequest,

    /// Failed to receive a response from the worker thread.
    #[error("Failed to receive response from rendering thread")]
    FailedToReceiveResponse,
}
