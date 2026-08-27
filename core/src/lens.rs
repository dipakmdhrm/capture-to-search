//! Hand a capture to Google Lens by staging an upload page for the browser.
//!
//! # Why the browser uploads, not us
//!
//! The obvious design is for the daemon to POST the capture itself, read the
//! `303` redirect, and open the resulting URL. That produces a valid-looking
//! URL - correct `vsrid`, correct `vsdim` - and it does not work.
//!
//! Google scopes the uploaded image to the *client session that uploaded it*.
//! The results URL carries `gsessionid`/`lsessionid` minted for the uploading
//! HTTP client, and opening it from a different client shows the search shell
//! with an empty query image. Presenting those session ids with no matching
//! cookies is refused outright with `403`. Verified end to end: a daemon-side
//! upload is always empty in the browser, including a freshly minted URL, so it
//! is session binding rather than expiry.
//!
//! So we do what `lens.google.com` itself does: the *browser* performs the
//! upload. We stage a self-contained HTML page that rebuilds the capture as a
//! `File`, attaches it to a real file input, and submits a normal form POST.
//! The browser sends its own cookies and follows the redirect, so the session
//! that uploads is the session that views the results.

use anyhow::{Context, Result};
use base64::Engine;
use image::imageops::FilterType;

/// What we are about to upload, for logging and sanity checks.
#[derive(Debug, Clone, Copy)]
pub struct ImageFacts {
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    /// Every pixel identical - a blank capture, which Lens renders as nothing.
    pub uniform: bool,
    /// Highest alpha in the image. `0` means fully transparent, which also
    /// renders as nothing.
    pub max_alpha: u8,
}

impl ImageFacts {
    /// Would this image look empty to a viewer?
    pub fn looks_blank(&self) -> bool {
        self.uniform || self.max_alpha == 0
    }
}

/// Decode an image and report what it contains.
pub fn inspect(bytes: &[u8]) -> Result<ImageFacts> {
    let img = image::load_from_memory(bytes).context("decoding capture")?;
    let rgba = img.to_rgba8();
    let first = rgba.pixels().next().copied();
    Ok(ImageFacts {
        width: img.width(),
        height: img.height(),
        bytes: bytes.len(),
        uniform: rgba.pixels().all(|p| Some(*p) == first),
        max_alpha: rgba.pixels().map(|p| p[3]).max().unwrap_or(0),
    })
}

/// Scale the longest edge down to `max_edge`, re-encoding as PNG.
///
/// The Lens web client downscales before posting, and an oversized upload is
/// the likeliest cause of a rejection - a raw 4K screenshot is several MB.
/// Images already within the limit are passed through untouched rather than
/// re-encoded, which keeps text screenshots crisp.
pub fn downscale(bytes: &[u8], max_edge: u32) -> Result<Vec<u8>> {
    let img = image::load_from_memory(bytes).context("decoding capture")?;
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= max_edge {
        return Ok(bytes.to_vec());
    }
    // Lanczos3: screenshots are mostly text and UI edges, where cheaper filters
    // visibly smear glyphs and cost Lens recognition accuracy.
    let resized = img.resize(max_edge, max_edge, FilterType::Lanczos3);
    let mut out = std::io::Cursor::new(Vec::new());
    resized
        .write_to(&mut out, image::ImageFormat::Png)
        .context("re-encoding capture as PNG")?;
    tracing::debug!(
        "downscaled capture {}x{} -> {}x{}",
        w,
        h,
        resized.width(),
        resized.height()
    );
    Ok(out.into_inner())
}

/// Build the self-contained page that makes the browser upload `png`.
///
/// No external resources: a strict `file://` page with the image inlined, so it
/// works with no network fetch of its own and nothing to leak beyond the POST
/// the user asked for.
pub fn upload_page(png: &[u8], endpoint: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    // The endpoint is user-configurable, so it lands in an HTML attribute and
    // must be escaped even though the default is a fixed constant.
    let action = html_escape(&form_action(endpoint));
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="referrer" content="no-referrer">
<title>Capture to Search</title>
<style>
  body {{ font-family: system-ui, sans-serif; margin: 0;
         display: flex; align-items: center; justify-content: center;
         min-height: 100vh; background: #fff; color: #222; }}
  @media (prefers-color-scheme: dark) {{ body {{ background: #1c1c1c; color: #eee; }} }}
  .box {{ text-align: center; }}
  button {{ font: inherit; padding: .6rem 1.2rem; margin-top: 1rem; cursor: pointer; }}
</style>
</head>
<body>
<div class="box">
  <p id="status">Sending your capture to Google Lens...</p>
  <form id="f" method="POST" enctype="multipart/form-data" action="{action}">
    <input type="file" id="img" name="encoded_image" hidden>
    <noscript><p>JavaScript is required to send the capture.</p></noscript>
    <button id="go" type="button" hidden>Continue to Google Lens</button>
  </form>
</div>
<script>
(function () {{
  var status = document.getElementById("status");
  var form = document.getElementById("f");
  var go = document.getElementById("go");
  try {{
    // Rebuild the PNG as a File and attach it to a genuine file input. A plain
    // form navigation then POSTs it with the browser's own cookies, so the
    // upload and the results page share one session - which is the whole point.
    var bin = atob("{encoded}");
    var buf = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) {{ buf[i] = bin.charCodeAt(i); }}
    var dt = new DataTransfer();
    dt.items.add(new File([buf], "capture.png", {{ type: "image/png" }}));
    document.getElementById("img").files = dt.files;
  }} catch (e) {{
    status.textContent = "Could not prepare the capture: " + e;
    return;
  }}
  // Manual fallback if the automatic submit is blocked for any reason.
  go.hidden = false;
  go.addEventListener("click", function () {{ form.submit(); }});
  form.submit();
}})();
</script>
</body>
</html>
"#
    )
}

/// The form target: the configured endpoint plus the language parameter,
/// preserving any query the user already put on it.
fn form_action(endpoint: &str) -> String {
    let separator = if endpoint.contains('?') { '&' } else { '?' };
    format!("{endpoint}{separator}hl=en")
}

/// Escape the characters that matter inside a double-quoted HTML attribute.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn oversized_capture_is_scaled_to_the_limit() {
        let out = downscale(&png_of(3840, 2160), 1600).unwrap();
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!(img.width().max(img.height()), 1600);
        // Aspect ratio must survive, or Lens sees a distorted image.
        assert_eq!(img.width(), 1600);
        assert_eq!(img.height(), 900);
    }

    #[test]
    fn small_capture_passes_through_byte_for_byte() {
        let original = png_of(800, 600);
        assert_eq!(downscale(&original, 1600).unwrap(), original);
    }

    #[test]
    fn portrait_capture_scales_on_its_long_edge() {
        let out = downscale(&png_of(1000, 4000), 1600).unwrap();
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!(img.height(), 1600);
        assert_eq!(img.width(), 400);
    }

    #[test]
    fn garbage_input_is_an_error_not_a_panic() {
        assert!(downscale(b"not an image at all", 1600).is_err());
    }

    #[test]
    fn blank_captures_are_detected() {
        let blank = png_of(200, 200);
        assert!(inspect(&blank).unwrap().looks_blank());

        let mut img = image::RgbaImage::from_pixel(200, 200, image::Rgba([255, 255, 255, 255]));
        img.put_pixel(10, 10, image::Rgba([0, 0, 0, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        assert!(!inspect(&out.into_inner()).unwrap().looks_blank());
    }

    #[test]
    fn fully_transparent_capture_counts_as_blank() {
        let img = image::RgbaImage::from_pixel(50, 50, image::Rgba([255, 0, 0, 0]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        let facts = inspect(&out.into_inner()).unwrap();
        assert_eq!(facts.max_alpha, 0);
        assert!(facts.looks_blank());
    }

    #[test]
    fn inspect_reports_real_dimensions() {
        let facts = inspect(&png_of(640, 480)).unwrap();
        assert_eq!((facts.width, facts.height), (640, 480));
    }

    #[test]
    fn staged_page_carries_the_exact_capture() {
        // The browser reconstructs the PNG from this base64. If it does not
        // decode back to the original bytes, Lens receives a corrupt image.
        let png = png_of(120, 90);
        let page = upload_page(&png, "https://lens.google.com/v3/upload");
        let start = page.find("atob(\"").expect("page must embed the image") + 6;
        let end = start + page[start..].find('"').unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&page[start..end])
            .expect("embedded payload must be valid base64");
        assert_eq!(decoded, png);
    }

    #[test]
    fn staged_page_posts_multipart_to_the_endpoint() {
        // These four are the contract with Lens; break any and the upload
        // silently stops working.
        let page = upload_page(&png_of(10, 10), "https://lens.google.com/v3/upload");
        assert!(page.contains(r#"method="POST""#));
        assert!(page.contains(r#"enctype="multipart/form-data""#));
        assert!(page.contains(r#"name="encoded_image""#));
        assert!(page.contains("https://lens.google.com/v3/upload?hl=en"));
    }

    #[test]
    fn endpoint_with_existing_query_keeps_it() {
        let page = upload_page(&png_of(10, 10), "https://example.test/up?a=1");
        assert!(page.contains("https://example.test/up?a=1&amp;hl=en"));
    }

    #[test]
    fn endpoint_cannot_break_out_of_the_attribute() {
        // The endpoint comes from user-editable config; an unescaped quote would
        // let it inject markup into the staged page.
        let page = upload_page(
            &png_of(10, 10),
            r#"https://x.test/"><script>bad()</script>"#,
        );
        assert!(!page.contains("<script>bad()</script>"));
        assert!(page.contains("&quot;&gt;&lt;script&gt;"));
    }

    #[test]
    fn staged_page_is_self_contained() {
        // A file:// page that reached out to the network would be both a
        // privacy leak and a way for the upload to break offline.
        let page = upload_page(&png_of(10, 10), "https://lens.google.com/v3/upload");
        assert!(!page.contains("<script src"));
        assert!(!page.contains("<link "));
        assert!(!page.contains("@import"));
    }
}
