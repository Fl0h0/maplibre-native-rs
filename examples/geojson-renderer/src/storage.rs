//! WebP encoding and hash-based file storage for rendered images.

use image::ImageFormat;
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::PathBuf;

/// Storage manager for rendered images.
pub struct ImageStorage {
    output_dir: PathBuf,
}

impl ImageStorage {
    /// Create a new image storage with the given output directory.
    ///
    /// Creates the directory if it doesn't exist.
    pub async fn new(output_dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let output_dir = output_dir.into();
        tokio::fs::create_dir_all(&output_dir).await?;
        Ok(Self { output_dir })
    }

    /// Generate a hash-based filename from the request content.
    ///
    /// This enables deduplication - identical requests will produce the same filename.
    pub fn generate_filename(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let hash = hasher.finalize();
        format!("{}.webp", hex::encode(&hash[..16])) // Use first 16 bytes (32 hex chars)
    }

    /// Check if an image already exists for the given hash.
    pub async fn exists(&self, filename: &str) -> bool {
        self.output_dir.join(filename).exists()
    }

    /// Get the full path for a filename.
    pub fn path(&self, filename: &str) -> PathBuf {
        self.output_dir.join(filename)
    }

    /// Save an image as WebP format.
    ///
    /// Returns the path where the image was saved.
    pub async fn save_webp(
        &self,
        image: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
        filename: &str,
    ) -> std::io::Result<PathBuf> {
        let path = self.output_dir.join(filename);

        // Encode to WebP
        let mut buffer = Cursor::new(Vec::new());
        image
            .write_to(&mut buffer, ImageFormat::WebP)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        // Write to file
        tokio::fs::write(&path, buffer.into_inner()).await?;

        Ok(path)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_filename() {
        let content = r#"{"type": "Point", "coordinates": [0, 0]}"#;
        let filename = ImageStorage::generate_filename(content);
        assert!(filename.ends_with(".webp"));
        assert_eq!(filename.len(), 32 + 5); // 32 hex chars + ".webp"
    }

    #[test]
    fn test_deterministic_hash() {
        let content = "test content";
        let hash1 = ImageStorage::generate_filename(content);
        let hash2 = ImageStorage::generate_filename(content);
        assert_eq!(hash1, hash2);
    }
}

