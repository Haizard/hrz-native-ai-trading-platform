//! Document attachments for the skill-authoring interview (`docs/47`).
//!
//! A trading book or course notes cannot ride the interview as typed text: a
//! PDF is bytes, and a book is far longer than a prompt. This module is the
//! boundary between the two -- bytes in from the browser, bounded text out to
//! the interview -- and it owns both refusals honestly: a format it cannot
//! read says so, and a book it had to cut says where.
//!
//! Extraction lives here, not in `ai-agent`: the agent crate stays
//! parser-free, and the dependency rules (`docs/03`) keep PDF parsing off
//! every other target.

use base64::Engine as _;

/// Largest decoded attachment, in bytes.
///
/// Generous enough for a real book PDF; small enough that a browser cannot
/// wedge the socket with a file it should never have sent. The wire form is
/// base64, so the JSON message is ~4/3 of this.
pub const MAX_DOCUMENT_BYTES: usize = 20 * 1024 * 1024;

/// Largest text the interview receives, in characters.
///
/// A whole book does not fit a prompt, and should not: the interview's job is
/// the methodology, and a book states that in its opening chapters and
/// restates it at the end. The cut therefore keeps the head and the tail and
/// says what went missing in the middle.
pub const MAX_DOCUMENT_CHARS: usize = 60_000;

/// Head share of the excerpt budget; the tail gets the rest.
const HEAD_CHARS: usize = 45_000;

/// A document ready for the interview: its name and its bounded text.
#[derive(Debug)]
pub struct PreparedDocument {
    /// The file's name, echoed to the model so it can cite the source.
    pub name: String,
    /// The extracted text, possibly an excerpt with the cut marked inline.
    pub text: String,
}

/// Decode the wire payload and extract the document's text.
///
/// # Errors
/// A human-readable reason for anything the interview cannot use: bad base64,
/// an unsupported extension, an unreadable PDF, or a document with no text in
/// it. The message is written to reach the trader -- it names what to do
/// instead, not just what failed.
pub fn prepare(name: &str, data_base64: &str) -> Result<PreparedDocument, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64)
        .map_err(|_| "the attachment did not decode; attach the file again".to_string())?;
    if bytes.is_empty() {
        return Err(format!("`{name}` is empty; there is nothing to read"));
    }
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "`{name}` is {:.1} MB, over the {} MB attachment limit",
            bytes.len() as f64 / (1024.0 * 1024.0),
            MAX_DOCUMENT_BYTES / (1024 * 1024),
        ));
    }
    let text = extract_text(name, &bytes)?;
    let text = bound(text);
    Ok(PreparedDocument {
        name: name.to_string(),
        text,
    })
}

/// Largest decoded image, in bytes. Matches the chart-screenshot budget:
/// an image the model can read needs far less than this, and a larger one is
/// a client bug, not a bigger ask.
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// Most images on one beat. The pattern needs an example or two; a gallery is
/// prompt weight the answer does not use.
pub const MAX_IMAGES: usize = 3;

/// An image ready for the interview.
#[derive(Debug)]
pub struct PreparedImage {
    /// The file's name.
    pub name: String,
    /// Its media type, validated.
    pub media_type: String,
    /// Base64 data, passed through unchanged -- the LLM layer never handles
    /// raw bytes, so the wire encoding stays the whole way down.
    pub data_base64: String,
}

/// Validate an image attachment: a media type the model can see, a size the
/// prompt can afford. The base64 itself travels untouched.
///
/// # Errors
/// A human-readable reason, worded to reach the trader.
pub fn prepare_image(name: &str, media_type: &str, data_base64: &str) -> Result<PreparedImage, String> {
    if !matches!(media_type, "image/png" | "image/jpeg" | "image/webp" | "image/gif") {
        return Err(format!(
            "`{name}` is `{media_type}`, which the interview cannot show the model; \
             PNG, JPEG, WebP or GIF images work"
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64)
        .map_err(|_| format!("`{name}` did not decode; attach the image again"))?;
    if bytes.is_empty() {
        return Err(format!("`{name}` is empty; there is nothing to show"));
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "`{name}` is {:.1} MB, over the {} MB image limit -- a cropped or downscaled copy works",
            bytes.len() as f64 / (1024.0 * 1024.0),
            MAX_IMAGE_BYTES / (1024 * 1024),
        ));
    }
    Ok(PreparedImage {
        name: name.to_string(),
        media_type: media_type.to_string(),
        data_base64: data_base64.to_string(),
    })
}

/// Pull the text out of the decoded bytes, by the file's extension.
fn extract_text(name: &str, bytes: &[u8]) -> Result<String, String> {
    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let text = match extension.as_str() {
        "pdf" => pdf_extract::extract_text_from_mem(bytes)
            .map_err(|e| format!("`{name}` could not be read as a PDF: {e}"))?,
        "txt" | "md" | "markdown" | "text" | "log" => String::from_utf8_lossy(bytes).into_owned(),
        other => {
            return Err(format!(
                "`{name}` is a `{other}` file, which this interview cannot read; \
                 attach a PDF or a plain-text/markdown file"
            ))
        }
    };
    let normalized = text.replace("\r\n", "\n");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "`{name}` yielded no text -- a scanned book is pictures of pages, and there is \
             nothing for the interview to read; a text export of it would work"
        ));
    }
    Ok(trimmed.to_string())
}

/// Fit the text to the interview's budget, keeping head and tail with the cut
/// marked where the middle went.
fn bound(text: String) -> String {
    if text.chars().count() <= MAX_DOCUMENT_CHARS {
        return text;
    }
    let head: String = text.chars().take(HEAD_CHARS).collect();
    let tail: String = text
        .chars()
        .skip(text.chars().count() - (MAX_DOCUMENT_CHARS - HEAD_CHARS))
        .collect();
    format!(
        "{head}\n\n[... excerpt: the middle of the document was cut for length; this is the \
         beginning and the end ...]\n\n{tail}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Base64 of a plain-text payload, without pulling in a fixture file.
    fn b64(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
    }

    #[test]
    fn a_text_file_arrives_verbatim() {
        let prepared = prepare("notes.txt", &b64("liquidity sweeps happen at equal highs")).unwrap();
        assert_eq!(prepared.name, "notes.txt");
        assert_eq!(prepared.text, "liquidity sweeps happen at equal highs");
    }

    #[test]
    fn markdown_is_accepted_as_text() {
        let prepared = prepare("book.md", &b64("# Order Blocks\nA down candle before the impulse.")).unwrap();
        assert!(prepared.text.contains("Order Blocks"));
    }

    #[test]
    fn an_unknown_extension_is_refused_with_a_way_forward() {
        let err = prepare("book.epub", &b64("whatever")).unwrap_err();
        assert!(err.contains("epub"), "the refusal names what arrived: {err}");
        assert!(err.contains("PDF"), "the refusal names what works: {err}");
    }

    #[test]
    fn a_base64_blob_that_is_not_base64_is_refused() {
        let err = prepare("book.pdf", "this is not base64!!!").unwrap_err();
        assert!(err.contains("did not decode"), "{err}");
    }

    #[test]
    fn an_empty_document_says_so() {
        let err = prepare("empty.txt", &b64("")).unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn whitespace_only_text_reads_as_no_text() {
        let err = prepare("blank.md", &b64("   \n\n  ")).unwrap_err();
        assert!(err.contains("no text"), "{err}");
    }

    #[test]
    fn a_long_document_is_excerpted_with_the_cut_marked() {
        let long = "a".repeat(MAX_DOCUMENT_CHARS + 10_000);
        let prepared = prepare("long.txt", &b64(&long)).unwrap();
        assert!(prepared.text.contains("excerpt"), "the cut is marked");
        assert!(
            prepared.text.chars().count() < long.chars().count(),
            "the excerpt is shorter than the book"
        );
        assert!(
            prepared.text.len() <= MAX_DOCUMENT_CHARS + 200,
            "the excerpt fits the budget plus its marker"
        );
    }

    #[test]
    fn an_image_passes_through_with_its_media_type() {
        let img = prepare_image("pattern.png", "image/png", &b64("not really a png but bytes")).unwrap();
        assert_eq!(img.media_type, "image/png");
        assert_eq!(img.name, "pattern.png");
    }

    #[test]
    fn an_image_media_type_the_model_cannot_see_is_refused() {
        let err = prepare_image("scan.tiff", "image/tiff", &b64("bytes")).unwrap_err();
        assert!(err.contains("tiff"), "{err}");
        assert!(err.contains("PNG"), "{err}");
    }

    #[test]
    fn an_oversize_image_is_refused() {
        let big = vec![0u8; MAX_IMAGE_BYTES + 1];
        let encoded = base64::engine::general_purpose::STANDARD.encode(big);
        let err = prepare_image("huge.png", "image/png", &encoded).unwrap_err();
        assert!(err.contains("image limit"), "{err}");
    }

    #[test]
    fn an_oversize_file_is_refused_before_reading() {
        // 20 MiB of zero bytes encodes to ~27 MB of base64; the refusal must
        // come from the size check, not from a parse attempt.
        let big = vec![0u8; MAX_DOCUMENT_BYTES + 1];
        let encoded = base64::engine::general_purpose::STANDARD.encode(big);
        let err = prepare("huge.pdf", &encoded).unwrap_err();
        assert!(err.contains("limit"), "{err}");
    }
}
