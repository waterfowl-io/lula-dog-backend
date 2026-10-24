//! The local bucket: a plain directory tree that stands in for the object
//! store ingestion runners will write to later. Raw source files and crawled
//! page text are archived here before they are parsed, so the original is
//! always on hand for re-processing or debugging.
//!
//! Layout: `sources/<source>/<YYYY-MM-DD>/<original-file-name>` for imported
//! files, `crawl/<domain>/<YYYY-MM-DD>/<page>.txt` for crawled pages. The
//! root is configurable (`BUCKET_DIR`, default `data/bucket`) and gitignored.

use std::path::{Path, PathBuf};

use anyhow::Context;

#[derive(Debug, Clone)]
pub struct Bucket {
    root: PathBuf,
}

impl Bucket {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Copy a local file unchanged into `sources/<source>/<date>/<name>`,
    /// where `<name>` is `original_path`'s file name. Returns the path
    /// relative to the bucket root, for storage in `company_sources`.
    pub fn store_source_file(
        &self,
        source: &str,
        date: &str,
        original_path: &Path,
    ) -> anyhow::Result<String> {
        let name = original_path
            .file_name()
            .context("source file path has no file name")?;
        let relative = Path::new("sources").join(source).join(date).join(name);
        let dest = self.root.join(&relative);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(original_path, &dest)
            .with_context(|| format!("copying {} into the bucket", original_path.display()))?;
        Ok(relative.to_string_lossy().into_owned())
    }

    /// Write crawled page text to `crawl/<domain>/<date>/<page>.txt`. Returns
    /// the path relative to the bucket root.
    pub fn write_crawl_text(
        &self,
        domain: &str,
        date: &str,
        page: &str,
        text: &str,
    ) -> anyhow::Result<String> {
        let relative = Path::new("crawl")
            .join(domain)
            .join(date)
            .join(format!("{page}.txt"));
        let dest = self.root.join(&relative);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, text)
            .with_context(|| format!("writing crawl text to {}", dest.display()))?;
        Ok(relative.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_a_source_file_and_returns_its_relative_path() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("filings.csv");
        std::fs::write(&input, "a,b\n1,2\n")?;

        let bucket = Bucket::new(dir.path().join("bucket"));
        let relative = bucket.store_source_file("bizapedia", "2026-10-23", &input)?;

        assert_eq!(relative, "sources/bizapedia/2026-10-23/filings.csv");
        let stored = std::fs::read_to_string(dir.path().join("bucket").join(relative))?;
        assert_eq!(stored, "a,b\n1,2\n");
        Ok(())
    }

    #[test]
    fn writes_crawl_text() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let bucket = Bucket::new(dir.path().join("bucket"));
        let relative =
            bucket.write_crawl_text("example.com", "2026-10-23", "home", "hello world")?;

        assert_eq!(relative, "crawl/example.com/2026-10-23/home.txt");
        let stored = std::fs::read_to_string(dir.path().join("bucket").join(relative))?;
        assert_eq!(stored, "hello world");
        Ok(())
    }
}
