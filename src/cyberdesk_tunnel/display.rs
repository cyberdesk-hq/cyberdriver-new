// SPDX-License-Identifier: AGPL-3.0-only
//
// Display endpoints for Cyberdesk's HTTP-over-WS tunnel.
//
// Mirrors Cyberdriver 1.x display behavior closely:
//   GET /computer/display/dimensions
//   GET /computer/display/screenshot
//
// The Cyberdesk-facing screenshot route intentionally returns a fixed
// 1024x768 PNG. Cyberdriver 1.x's local FastAPI endpoint accepted
// width/height/mode query params, but the normal Cyberdesk cloud
// endpoint did not forward those params, so the effective agent/UI
// contract was always 1024x768. Keeping that invariant avoids opening
// a frontend/agent coordinate-scaling surface during the Rust rewrite.

use hbb_common::anyhow::{bail, Context, Result};
use image::{imageops::FilterType, DynamicImage, ImageOutputFormat, RgbaImage};
use scrap::{Capturer, Display, Frame, Pixfmt, TraitCapturer, TraitPixelBuffer};
use std::{
    io::{self, Cursor},
    thread,
    time::Duration,
};

const DEFAULT_SCREENSHOT_WIDTH: u32 = 1024;
const DEFAULT_SCREENSHOT_HEIGHT: u32 = 768;
const CAPTURE_RETRIES: usize = 3;

pub fn dimensions() -> Result<Vec<u8>> {
    let display = primary_display()?;
    Ok(serde_json::to_vec(&serde_json::json!({
        "width": display.width(),
        "height": display.height(),
    }))?)
}

pub fn screenshot() -> Result<Vec<u8>> {
    let (width, height, rgba) = capture_primary_rgba()?;
    let png = encode_scaled_png(width, height, rgba)?;
    Ok(png)
}

fn primary_display() -> Result<Display> {
    Display::primary()
        .or_else(|_| {
            Display::all()?
                .into_iter()
                .next()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no displays found"))
        })
        .context("failed to get primary display")
}

fn capture_primary_rgba() -> Result<(usize, usize, Vec<u8>)> {
    let display = primary_display()?;
    let mut capturer = Capturer::new(display).context("failed to create display capturer")?;
    let width = capturer.width();
    let height = capturer.height();
    let mut last_error = None;

    for _ in 0..CAPTURE_RETRIES {
        match capturer.frame(Duration::from_millis(250)) {
            Ok(Frame::PixelBuffer(pixel_buffer)) => {
                let rgba = rgba_from_pixel_buffer(&pixel_buffer)?;
                return Ok((width, height, rgba));
            }
            Ok(Frame::Texture(_)) => {
                bail!("display capturer returned a GPU texture frame; PNG screenshots require a pixel buffer");
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                last_error = Some(err);
                thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                last_error = Some(err);
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    match last_error {
        Some(err) => Err(err).context("screen capture failed after retries"),
        None => bail!("screen capture failed after retries"),
    }
}

fn rgba_from_pixel_buffer(pixel_buffer: &scrap::PixelBuffer<'_>) -> Result<Vec<u8>> {
    let width = pixel_buffer.width();
    let height = pixel_buffer.height();
    let stride = pixel_buffer
        .stride()
        .first()
        .copied()
        .context("invalid pixel buffer stride")?;

    if pixel_buffer.pixfmt() == Pixfmt::RGBA && stride == width * 4 {
        return Ok(pixel_buffer.data().to_vec());
    }

    if pixel_buffer.pixfmt() == Pixfmt::BGRA && stride != width * 4 {
        let bgra = pixel_buffer.data();
        let mut rgba = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let i = stride * y + 4 * x;
                rgba.extend_from_slice(&[bgra[i + 2], bgra[i + 1], bgra[i], bgra[i + 3]]);
            }
        }
        return Ok(rgba);
    }

    let mut rgba = Vec::new();
    scrap::convert(pixel_buffer, Pixfmt::RGBA, &mut rgba)?;

    if stride == width * 4 || rgba.len() == width * height * 4 {
        Ok(rgba)
    } else {
        bail!(
            "unsupported pixel buffer stride after conversion: stride={stride}, width={width}, height={height}"
        )
    }
}

fn encode_scaled_png(width: usize, height: usize, rgba: Vec<u8>) -> Result<Vec<u8>> {
    let mut image = RgbaImage::from_raw(width as u32, height as u32, rgba)
        .context("failed to construct RGBA screenshot image")?;

    if width as u32 != DEFAULT_SCREENSHOT_WIDTH || height as u32 != DEFAULT_SCREENSHOT_HEIGHT {
        image = image::imageops::resize(
            &image,
            DEFAULT_SCREENSHOT_WIDTH,
            DEFAULT_SCREENSHOT_HEIGHT,
            FilterType::Lanczos3,
        );
    }

    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut cursor, ImageOutputFormat::Png)
        .context("failed to encode screenshot PNG")?;
    Ok(cursor.into_inner())
}
