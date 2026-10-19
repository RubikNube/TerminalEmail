use base64::{Engine, alphabet, engine::general_purpose};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    pub unread: u32,
    pub role: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Message {
    pub uid: u32,
    pub folder: String,
    pub uid_validity: u32,
    pub message_id: String,
    pub references: Vec<String>,
    pub from: String,
    pub reply_to: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub date: String,
    pub timestamp: i64,
    pub seen: bool,
    pub starred: bool,
    pub body: Option<String>,
    #[serde(default)]
    pub html_body: Option<String>,
    #[serde(default)]
    pub html_loaded: bool,
    pub attachments: Vec<Attachment>,
}

/// Mail and folder names are untrusted terminal input. Preserve printable Unicode,
/// tabs, and newlines, but drop control characters including ESC, OSC, and C1.
pub fn safe_text(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// Decode IMAP modified UTF-7 for display only; mailbox identities stay encoded.
/// Keep malformed sequences verbatim and sanitize after decoding terminal controls.
pub fn folder_display_name(value: &str) -> String {
    let engine =
        general_purpose::GeneralPurpose::new(&alphabet::IMAP_MUTF7, general_purpose::NO_PAD);
    let mut decoded = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(start) = remaining.find('&') {
        decoded.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        let Some(end) = remaining.find('-') else {
            break;
        };
        let encoded = &remaining[1..end];
        if encoded.is_empty() {
            decoded.push('&');
        } else {
            let text = engine.decode(encoded).ok().and_then(|bytes| {
                let (pairs, remainder) = bytes.as_chunks::<2>();
                if !remainder.is_empty() {
                    return None;
                }
                let units: Vec<_> = pairs.iter().map(|pair| u16::from_be_bytes(*pair)).collect();
                String::from_utf16(&units).ok()
            });
            decoded.push_str(text.as_deref().unwrap_or(&remaining[..=end]));
        }
        remaining = &remaining[end + 1..];
    }
    decoded.push_str(remaining);
    safe_text(&decoded)
}

pub fn parse_message(raw: &[u8], uid: u32, folder: &str, validity: u32) -> Message {
    let mut result = Message {
        uid,
        folder: folder.into(),
        uid_validity: validity,
        ..Default::default()
    };
    if let Some(mail) = mail_parser::MessageParser::default().parse(raw) {
        let addresses = |address: Option<&mail_parser::Address<'_>>| {
            address
                .map(|a| {
                    a.iter()
                        .filter_map(|a| {
                            a.address().map(|email| {
                                email
                                    .parse::<lettre::Address>()
                                    .map(|address| {
                                        lettre::message::Mailbox::new(
                                            a.name().map(str::to_owned),
                                            address,
                                        )
                                        .to_string()
                                    })
                                    .unwrap_or_else(|_| email.to_owned())
                            })
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        };
        result.from = safe_text(&addresses(mail.from()));
        result.reply_to = safe_text(&addresses(mail.reply_to()));
        result.to = safe_text(&addresses(mail.to()));
        result.cc = safe_text(&addresses(mail.cc()));
        result.subject = safe_text(mail.subject().unwrap_or("(no subject)"));
        result.message_id = mail.message_id().unwrap_or_default().into();
        if let Some(date) = mail.date() {
            result.timestamp = date.to_timestamp();
            result.date = chrono::DateTime::from_timestamp(result.timestamp, 0)
                .map(|date| date.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
        }
        result.references = mail
            .references()
            .as_text_list()
            .unwrap_or_default()
            .iter()
            .map(|s| s.to_string())
            .collect();
        if let Some(id) = mail.in_reply_to().as_text() {
            result.references.push(id.to_owned());
        }
        // body_html() also synthesizes HTML for plain-text messages. Keep only
        // actual HTML MIME parts so plain-text mail gets the correct notice.
        result.html_body = mail
            .html_body
            .iter()
            .find_map(|&id| match &mail.part(id)?.body {
                mail_parser::PartType::Html(html) => Some(html.to_string()),
                _ => None,
            });
        result.html_loaded = true;
        result.body = mail.body_text(0).map(|s| safe_text(&s));
        if result.body.is_none() {
            result.body = mail.body_html(0).map(|html| {
                safe_text(&html2text::from_read(html.as_bytes(), 100).unwrap_or_default())
            });
        }
        for (index, part) in mail.attachments().enumerate() {
            use mail_parser::MimeHeaders;
            result.attachments.push(Attachment {
                name: safe_text(
                    part.attachment_name()
                        .unwrap_or(&format!("attachment-{}", index + 1)),
                ),
                content_type: part
                    .content_type()
                    .map(|ct| ct.c_type.to_string())
                    .unwrap_or_else(|| "application/octet-stream".into()),
                data: part.contents().to_vec(),
            });
        }
    }
    result
}

/// Union connected References/Message-ID components, including shared missing parents.
pub fn threads(messages: &[Message]) -> Vec<Vec<usize>> {
    use std::collections::HashMap;
    let mut parent: Vec<usize> = (0..messages.len()).collect();
    fn root(parent: &mut [usize], mut n: usize) -> usize {
        while parent[n] != n {
            parent[n] = parent[parent[n]];
            n = parent[n];
        }
        n
    }
    let mut identifiers: HashMap<&str, usize> = HashMap::new();
    for (i, message) in messages.iter().enumerate() {
        for id in std::iter::once(message.message_id.as_str())
            .chain(message.references.iter().map(String::as_str))
            .filter(|s| !s.is_empty())
        {
            if let Some(&other) = identifiers.get(id) {
                let a = root(&mut parent, i);
                let b = root(&mut parent, other);
                parent[a] = b;
            } else {
                identifiers.insert(id, i);
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..messages.len() {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    for group in &mut groups {
        group.sort_by_key(|&i| (messages[i].timestamp, messages[i].uid));
    }
    groups.sort_by_key(|group| {
        std::cmp::Reverse(
            group
                .iter()
                .map(|&i| (messages[i].timestamp, messages[i].uid))
                .max(),
        )
    });
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_modified_utf7_folder_names() {
        for (encoded, expected) in [
            ("Gel&APY-schte Elemente", "Gelöschte Elemente"),
            ("&AOQA9gD8-", "äöü"),
            ("&AMQA1gDc-", "ÄÖÜ"),
            ("Projekte/&AOQ-/&APY-/&APw-", "Projekte/ä/ö/ü"),
            ("A &- B", "A & B"),
            ("&2D3eAA-", "😀"),
            ("&U,BTFw-", "台北"),
            ("INBOX", "INBOX"),
            ("Gelöschte Elemente", "Gelöschte Elemente"),
            ("", ""),
        ] {
            assert_eq!(folder_display_name(encoded), expected, "{encoded}");
        }
    }

    #[test]
    fn preserves_malformed_folder_names_and_sanitizes_decoded_controls() {
        for malformed in [
            "A & B", "&", "&APY", "&!!!-", "&AA-", "&2AA-", "&APY=-", "&APZ-",
        ] {
            assert_eq!(folder_display_name(malformed), malformed);
        }
        assert_eq!(folder_display_name("&!!!-/&APY-"), "&!!!-/ö");
        assert_eq!(folder_display_name("A&ABsABwCF-B\x1b"), "AB");
    }

    #[test]
    fn preserves_html_alternative_and_reads_legacy_cache_payloads() {
        let raw = b"MIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=x\r\n\r\n--x\r\nContent-Type: text/plain\r\n\r\nPlain alternative\r\n--x\r\nContent-Type: text/html\r\n\r\n<p>HTML alternative</p>\r\n--x--\r\n";
        let message = parse_message(raw, 1, "INBOX", 1);
        assert!(
            message
                .body
                .as_deref()
                .unwrap()
                .contains("Plain alternative")
        );
        assert!(
            message
                .html_body
                .as_deref()
                .unwrap()
                .contains("<p>HTML alternative</p>")
        );
        assert!(message.html_loaded);
        let mut payload = serde_json::to_value(&message).unwrap();
        payload.as_object_mut().unwrap().remove("html_body");
        payload.as_object_mut().unwrap().remove("html_loaded");
        let legacy: Message = serde_json::from_value(payload).unwrap();
        assert!(legacy.body.is_some());
        assert!(legacy.html_body.is_none());
        assert!(!legacy.html_loaded);
        let plain = parse_message(b"Content-Type: text/plain\r\n\r\nHello", 2, "INBOX", 1);
        assert!(plain.html_loaded);
        assert!(plain.html_body.is_none());
    }
    #[test]
    fn parses_html_multipart_and_references_without_active_content() {
        let raw = b"From: \"Last, First\" <first@example.org>\r\nSubject: HTML test\r\nMessage-ID: <child@test>\r\nIn-Reply-To: <parent@test>\r\nReferences: <root@test> <parent@test>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>Hello <b>world</b></p><script>secret_script()</script><img src=\"https://example.org/tracker\"></body></html>\r\n--x\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=\"test.txt\"\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--x--\r\n";
        let message = parse_message(raw, 2, "INBOX", 1);
        assert!(message.html_loaded);
        assert!(
            message
                .html_body
                .as_deref()
                .unwrap()
                .contains("secret_script()")
        );
        assert!(message.from.starts_with("\"Last, First\""));
        let body = message.body.unwrap();
        assert!(body.contains("Hello"));
        assert!(!body.contains("secret_script"));
        assert!(message.references.contains(&"root@test".to_string()));
        assert!(message.references.contains(&"parent@test".to_string()));
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(message.attachments[0].data, b"hello");
    }

    #[test]
    fn parses_unicode_mime_and_strips_controls() {
        let message = parse_message(b"From: Alice <alice@example.org>\r\nSubject: Hello\r\nMessage-ID: <a@test>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello\x1b[31m\x07 world", 1, "INBOX", 3);
        assert_eq!(message.message_id, "a@test");
        assert!(!message.body.unwrap().contains('\x1b'));
        assert_eq!(message.from, "Alice <alice@example.org>");
    }

    #[test]
    fn groups_replies_and_shared_missing_parent() {
        let messages = vec![
            Message {
                message_id: "a".into(),
                ..Default::default()
            },
            Message {
                references: vec!["a".into()],
                ..Default::default()
            },
            Message {
                references: vec!["missing".into()],
                ..Default::default()
            },
            Message {
                references: vec!["missing".into()],
                ..Default::default()
            },
            Message::default(),
        ];
        let groups = threads(&messages);
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().any(|g| g == &[0, 1]));
        assert!(groups.iter().any(|g| g == &[2, 3]));
    }
}
