// SPDX-License-Identifier: GPL-3.0-or-later

use anyhow::Result;

use super::{App, EntryKind};

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// Handle a bracketed-paste event. If the pasted text looks like one or more
/// image file paths (e.g. drag-and-drop from a file manager), read and attach
/// them; otherwise insert the text verbatim.
pub(super) fn handle_paste(app: &mut App, text: String) {
    // Terminals send \r or \r\n in bracketed paste; normalise to \n so the
    // multi-line input area renders correctly.
    let text = text.replace("\r\n", "\n").replace('\r', "\n");

    // Terminals deliver drag-and-drop as pasted file paths, one per line.
    // Check if *every* non-empty line is an image file that exists on disk.
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let all_images = !lines.is_empty()
        && lines.iter().all(|line| {
            let path = std::path::Path::new(line.trim());
            path.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
                && path.is_file()
        });

    if all_images {
        for line in &lines {
            let path = std::path::Path::new(line.trim());
            match attach_image_file(app, path) {
                Ok(chip) => {
                    app.editor.input.insert_str(app.editor.cursor, &chip);
                    app.editor.cursor += chip.len();
                }
                Err(e) => {
                    let msg = format!("image: {e}");
                    app.push_entry(EntryKind::Error, &msg);
                }
            }
        }
        app.editor.comp_candidates.clear();
    } else {
        app.editor.input.insert_str(app.editor.cursor, &text);
        app.editor.cursor += text.len();
        app.editor.comp_candidates.clear();
    }
}

/// Read an image file from disk, base64-encode it, attach to `app.attached_images`,
/// and return the `[Image #N]` chip string.
fn attach_image_file(app: &mut App, path: &std::path::Path) -> Result<String> {
    let bytes =
        std::fs::read(path).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "image/png",
    };
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let data_url = format!("data:{mime};base64,{b64}");
    app.attached_images.push(data_url);
    let n = app.attached_images.len();
    Ok(format!("[Image #{n}]"))
}

/// Read the system clipboard. If it contains an image, base64-encode it and
/// attach it to the current message; otherwise insert any text at the cursor.
pub(super) fn paste_clipboard(app: &mut App) {
    use arboard::Clipboard;
    let Ok(mut clip) = Clipboard::new() else {
        app.push_entry(EntryKind::Error, "clipboard: cannot open");
        return;
    };

    // Try image first.
    if let Ok(img) = clip.get_image() {
        // Encode as PNG into a `data:` URL.
        let mut png_buf: Vec<u8> = Vec::new();
        if image_encode_png(
            std::io::Cursor::new(&mut png_buf),
            img.width as u32,
            img.height as u32,
            &img.bytes,
        )
        .is_err()
        {
            app.push_entry(EntryKind::Error, "clipboard: failed to encode image");
            return;
        }
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);
        let data_url = format!("data:image/png;base64,{b64}");
        app.attached_images.push(data_url);
        let n = app.attached_images.len();
        let chip = format!("[Image #{n}]");
        app.editor.input.insert_str(app.editor.cursor, &chip);
        app.editor.cursor += chip.len();
        app.editor.comp_candidates.clear();
        return;
    }

    // Fall back to text.
    if let Ok(text) = clip.get_text()
        && !text.is_empty()
    {
        app.editor.input.insert_str(app.editor.cursor, &text);
        app.editor.cursor += text.len();
        app.editor.comp_candidates.clear();
    }
}

/// Encode raw RGBA bytes into a PNG buffer.
fn image_encode_png(
    writer: impl std::io::Write,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<()> {
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut w = encoder.write_header()?;
    w.write_image_data(rgba)?;
    w.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_encode_png_writes_png_signature() {
        let mut out = Vec::new();
        let rgba = [255, 0, 0, 255];

        image_encode_png(&mut out, 1, 1, &rgba).unwrap();

        assert_eq!(&out[..8], b"\x89PNG\r\n\x1a\n");
    }
}
