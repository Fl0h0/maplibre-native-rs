//! Bounding box calculation and camera fitting utilities for GeoJSON data.

use geojson::{GeoJson, Geometry, Value};

/// A geographic bounding box.
#[derive(Debug, Clone, Copy)]
pub struct BoundingBox {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

impl BoundingBox {
    /// Create a new bounding box from coordinates.
    pub fn new(min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> Self {
        Self {
            min_lon,
            min_lat,
            max_lon,
            max_lat,
        }
    }

    /// Get the center point of the bounding box.
    pub fn center(&self) -> (f64, f64) {
        let lat = (self.min_lat + self.max_lat) / 2.0;
        let lon = (self.min_lon + self.max_lon) / 2.0;
        (lat, lon)
    }

    /// Calculate the width in degrees.
    pub fn width(&self) -> f64 {
        self.max_lon - self.min_lon
    }

    /// Calculate the height in degrees.
    pub fn height(&self) -> f64 {
        self.max_lat - self.min_lat
    }

    /// Expand this bounding box to include a point.
    pub fn extend(&mut self, lon: f64, lat: f64) {
        self.min_lon = self.min_lon.min(lon);
        self.min_lat = self.min_lat.min(lat);
        self.max_lon = self.max_lon.max(lon);
        self.max_lat = self.max_lat.max(lat);
    }

    /// Merge another bounding box into this one.
    pub fn merge(&mut self, other: &BoundingBox) {
        self.min_lon = self.min_lon.min(other.min_lon);
        self.min_lat = self.min_lat.min(other.min_lat);
        self.max_lon = self.max_lon.max(other.max_lon);
        self.max_lat = self.max_lat.max(other.max_lat);
    }

    /// Calculate the optimal zoom level to fit this bounding box in a viewport.
    ///
    /// This function calculates the zoom level based on the actual viewport dimensions,
    /// accounting for cases where the output image will be resized after rendering.
    ///
    /// # Arguments
    /// * `viewport_width` - Actual renderer viewport width in pixels
    /// * `viewport_height` - Actual renderer viewport height in pixels
    /// * `output_width` - Desired output image width in pixels
    /// * `output_height` - Desired output image height in pixels
    /// * `padding` - Padding factor (0.0 = no padding, 0.1 = 10% padding on each side)
    ///
    /// The zoom is calculated to ensure the bounding box fits within the output aspect ratio,
    /// then adjusted for the actual viewport dimensions.
    #[allow(clippy::cast_precision_loss)]
    pub fn fit_zoom(
        &self,
        viewport_width: u32,
        viewport_height: u32,
        output_width: u32,
        output_height: u32,
        padding: f64,
    ) -> f64 {
        let bbox_width = self.width();
        let bbox_height = self.height();

        // Handle edge case of single point
        if bbox_width == 0.0 && bbox_height == 0.0 {
            return 14.0; // Default zoom for a single point
        }

        // Apply padding
        let padded_width = bbox_width * (1.0 + padding * 2.0);
        let padded_height = bbox_height * (1.0 + padding * 2.0);

        // Calculate the effective viewport dimensions based on output aspect ratio.
        // After rendering at viewport size, we crop to match the output aspect ratio,
        // then resize to final output dimensions.
        // The zoom must ensure the bbox fits within the cropped region.
        let output_aspect = f64::from(output_width) / f64::from(output_height);
        let viewport_aspect = f64::from(viewport_width) / f64::from(viewport_height);

        // Determine the crop region dimensions (what will be visible after cropping)
        let (effective_width, effective_height) = if output_aspect > viewport_aspect {
            // Output is wider (more landscape) than viewport
            // We keep full viewport width, crop height to match aspect ratio
            let effective_w = f64::from(viewport_width);
            let effective_h = effective_w / output_aspect;
            (effective_w, effective_h)
        } else {
            // Output is taller (more portrait) than viewport
            // We keep full viewport height, crop width to match aspect ratio
            let effective_h = f64::from(viewport_height);
            let effective_w = effective_h * output_aspect;
            (effective_w, effective_h)
        };

        // Calculate zoom level based on Mercator projection
        // World at zoom 0 is 256 pixels wide (or 360 degrees)
        let world_dim = 256.0;

        // Calculate zoom for width and height separately using effective dimensions
        let zoom_x = if padded_width > 0.0 {
            (effective_width / world_dim * 360.0 / padded_width).log2()
        } else {
            20.0
        };

        let zoom_y = if padded_height > 0.0 {
            // Account for Mercator latitude distortion
            let lat_rad = self.center().0.to_radians();
            let mercator_height = padded_height / lat_rad.cos().abs().max(0.01);
            (effective_height / world_dim * 180.0 / mercator_height).log2()
        } else {
            20.0
        };

        // Use the smaller zoom (fits the larger dimension)
        let zoom = zoom_x.min(zoom_y);

        // Clamp to valid zoom levels
        zoom.clamp(0.0, 20.0)
    }
}

/// Calculate the bounding box of a GeoJSON object.
pub fn calculate_bbox(geojson: &GeoJson) -> Option<BoundingBox> {
    let mut bbox: Option<BoundingBox> = None;

    match geojson {
        GeoJson::FeatureCollection(fc) => {
            for feature in &fc.features {
                if let Some(ref geom) = feature.geometry {
                    if let Some(geom_bbox) = geometry_bbox(geom) {
                        match &mut bbox {
                            Some(b) => b.merge(&geom_bbox),
                            None => bbox = Some(geom_bbox),
                        }
                    }
                }
            }
        }
        GeoJson::Feature(feature) => {
            if let Some(ref geom) = feature.geometry {
                bbox = geometry_bbox(geom);
            }
        }
        GeoJson::Geometry(geom) => {
            bbox = geometry_bbox(geom);
        }
    }

    bbox
}

/// Calculate the bounding box of a geometry.
fn geometry_bbox(geometry: &Geometry) -> Option<BoundingBox> {
    let mut bbox: Option<BoundingBox> = None;

    match &geometry.value {
        Value::Point(coords) => {
            bbox = Some(BoundingBox::new(coords[0], coords[1], coords[0], coords[1]));
        }
        Value::MultiPoint(points) => {
            for coords in points {
                extend_bbox(&mut bbox, coords[0], coords[1]);
            }
        }
        Value::LineString(line) => {
            for coords in line {
                extend_bbox(&mut bbox, coords[0], coords[1]);
            }
        }
        Value::MultiLineString(lines) => {
            for line in lines {
                for coords in line {
                    extend_bbox(&mut bbox, coords[0], coords[1]);
                }
            }
        }
        Value::Polygon(polygon) => {
            for ring in polygon {
                for coords in ring {
                    extend_bbox(&mut bbox, coords[0], coords[1]);
                }
            }
        }
        Value::MultiPolygon(polygons) => {
            for polygon in polygons {
                for ring in polygon {
                    for coords in ring {
                        extend_bbox(&mut bbox, coords[0], coords[1]);
                    }
                }
            }
        }
        Value::GeometryCollection(geometries) => {
            for geom in geometries {
                if let Some(geom_bbox) = geometry_bbox(geom) {
                    match &mut bbox {
                        Some(b) => b.merge(&geom_bbox),
                        None => bbox = Some(geom_bbox),
                    }
                }
            }
        }
    }

    bbox
}

/// Helper to extend a bounding box with a point.
fn extend_bbox(bbox: &mut Option<BoundingBox>, lon: f64, lat: f64) {
    match bbox {
        Some(b) => b.extend(lon, lat),
        None => *bbox = Some(BoundingBox::new(lon, lat, lon, lat)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bbox_center() {
        let bbox = BoundingBox::new(-10.0, -5.0, 10.0, 5.0);
        let (lat, lon) = bbox.center();
        assert!((lat - 0.0).abs() < 0.001);
        assert!((lon - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_bbox_point() {
        let geojson: GeoJson =
            serde_json::from_str(r#"{"type": "Point", "coordinates": [10.0, 20.0]}"#).unwrap();

        let bbox = calculate_bbox(&geojson).unwrap();
        assert!((bbox.min_lon - 10.0).abs() < 0.001);
        assert!((bbox.min_lat - 20.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_bbox_polygon() {
        let geojson: GeoJson = serde_json::from_str(
            r#"{
                "type": "Polygon",
                "coordinates": [[[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]]]
            }"#,
        )
        .unwrap();

        let bbox = calculate_bbox(&geojson).unwrap();
        assert!((bbox.min_lon - 0.0).abs() < 0.001);
        assert!((bbox.min_lat - 0.0).abs() < 0.001);
        assert!((bbox.max_lon - 10.0).abs() < 0.001);
        assert!((bbox.max_lat - 10.0).abs() < 0.001);
    }
}
