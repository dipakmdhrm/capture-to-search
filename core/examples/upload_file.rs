//! Stage an upload page for an existing image and report what it contains.
//!
//! The dev tool for "did the capture survive the pipeline". Prints the image's
//! facts, writes the page the browser would receive, and optionally opens it.
//!
//! ```text
//! cargo run -p capture-core --example upload_file -- /path/to/shot.png [--open]
//! ```

use capture_core::{config, lens};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: upload_file <image> [--open]");
    let open_it = std::env::args().any(|a| a == "--open");

    let original = std::fs::read(&path)?;
    let facts = lens::inspect(&original)?;
    println!(
        "source     {}x{}  {} bytes  blank={}",
        facts.width,
        facts.height,
        facts.bytes,
        facts.looks_blank()
    );

    let prepared = lens::downscale(&original, config::DEFAULT_MAX_UPLOAD_EDGE)?;
    let after = lens::inspect(&prepared)?;
    println!(
        "uploaded   {}x{}  {} bytes  (re-encoded: {})",
        after.width,
        after.height,
        after.bytes,
        prepared != original
    );

    let page = lens::upload_page(&prepared, config::DEFAULT_LENS_ENDPOINT);
    let out = capture_core::paths::new_upload_page_path()?;
    capture_core::paths::write_private(&out, page.as_bytes())?;
    println!("staged     {} ({} KB)", out.display(), page.len() / 1024);

    if open_it {
        open::that_detached(&out)?;
        println!("opened in your browser");
    } else {
        println!("re-run with --open to send it to Google Lens");
    }
    Ok(())
}
