//! Just enough XML for S3's error and multipart documents.

/// The text of the first `<name>` element, unescaped.
pub fn tag(doc: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = doc.find(&open)? + open.len();
    let end = doc[start..].find(&format!("</{name}>"))? + start;
    Some(unescape(&doc[start..end]))
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_error_codes_and_upload_ids() {
        let err = "<?xml version=\"1.0\"?><Error><Code>SlowDown</Code><Message>Please &amp; wait</Message></Error>";
        assert_eq!(tag(err, "Code").as_deref(), Some("SlowDown"));
        assert_eq!(tag(err, "Message").as_deref(), Some("Please & wait"));
        assert_eq!(tag(err, "UploadId"), None);
        let up = "<InitiateMultipartUploadResult><Bucket>b</Bucket><UploadId>abc.123</UploadId></InitiateMultipartUploadResult>";
        assert_eq!(tag(up, "UploadId").as_deref(), Some("abc.123"));
    }

    #[test]
    fn escapes_markup_in_etags() {
        assert_eq!(escape("\"a&b\""), "\"a&amp;b\"");
    }
}
