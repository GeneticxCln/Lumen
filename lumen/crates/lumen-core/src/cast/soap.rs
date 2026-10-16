//! The UPnP `AVTransport` calls: the XML Lumen sends and the parts of the answers it reads.

use super::description::{escape, unescape};

/// A SOAP request body for `action` with the given arguments (values are escaped here).
pub fn envelope(service_type: &str, action: &str, arguments: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (name, value) in arguments {
        body.push_str(&format!("<{name}>{}</{name}>", escape(value)));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{service_type}\">{body}</u:{action}></s:Body></s:Envelope>"
    )
}

/// The `SOAPACTION` header value.
pub fn action_header(service_type: &str, action: &str) -> String {
    format!("\"{service_type}#{action}\"")
}

/// The DIDL-Lite description of the item (title, address, type) that goes with a new address.
pub fn didl(title: &str, url: &str, mime: &str) -> String {
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\"><item id=\"0\" parentID=\"-1\" restricted=\"1\"><dc:title>{}</dc:title><upnp:class>object.item.videoItem</upnp:class><res protocolInfo=\"http-get:*:{}:*\">{}</res></item></DIDL-Lite>",
        escape(title),
        escape(mime),
        escape(url)
    )
}

/// `H:MM:SS` as the transport wants positions.
pub fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
}

/// Seconds from `H:MM:SS` or `H:MM:SS.mmm`; renderers answer `NOT_IMPLEMENTED` for unknown values.
pub fn parse_clock(text: &str) -> Option<f64> {
    let mut parts = text.trim().split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    parts
        .next()
        .is_none()
        .then_some(hours * 3600.0 + minutes * 60.0 + seconds)
}

/// The text of the first `<tag>` in an answer, unescaped.
pub fn field(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{tag}>"))? + start;
    Some(unescape(xml[start..end].trim()))
}

/// The reason in a SOAP fault, if the answer is one.
pub fn fault(xml: &str) -> Option<String> {
    if !xml.contains("Fault>") && !xml.contains("<errorCode>") {
        return None;
    }
    let code = field(xml, "errorCode").unwrap_or_default();
    let text =
        field(xml, "errorDescription").unwrap_or_else(|| "the device refused the request".into());
    Some(if code.is_empty() {
        text
    } else {
        format!("{text} ({code})")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVICE: &str = "urn:schemas-upnp-org:service:AVTransport:1";

    #[test]
    fn a_request_carries_escaped_arguments() {
        let body = envelope(
            SERVICE,
            "SetAVTransportURI",
            &[
                ("InstanceID", "0"),
                ("CurrentURI", "http://h/a?x=1&y=2"),
                ("CurrentURIMetaData", "<a>"),
            ],
        );
        assert!(body.contains(
            "<u:SetAVTransportURI xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\">"
        ));
        assert!(body.contains("<CurrentURI>http://h/a?x=1&amp;y=2</CurrentURI>"));
        assert!(body.contains("<CurrentURIMetaData>&lt;a&gt;</CurrentURIMetaData>"));
        assert_eq!(
            action_header(SERVICE, "Play"),
            "\"urn:schemas-upnp-org:service:AVTransport:1#Play\""
        );
    }

    #[test]
    fn metadata_cannot_be_broken_by_a_hostile_title() {
        let didl = didl("</dc:title><evil/>", "http://h/v.mp4", "video/mp4");
        assert!(!didl.contains("<evil/>"));
        assert!(didl.contains("&lt;/dc:title&gt;"));
    }

    #[test]
    fn positions_round_trip() {
        assert_eq!(clock(0.0), "0:00:00");
        assert_eq!(clock(3725.9), "1:02:05");
        assert_eq!(clock(-5.0), "0:00:00");
        assert_eq!(parse_clock("1:02:05"), Some(3725.0));
        assert_eq!(parse_clock("00:00:07.500"), Some(7.5));
        assert_eq!(parse_clock("NOT_IMPLEMENTED"), None);
    }

    #[test]
    fn answers_and_faults_are_read() {
        let answer = "<s:Envelope><s:Body><u:GetTransportInfoResponse><CurrentTransportState>PLAYING</CurrentTransportState></u:GetTransportInfoResponse></s:Body></s:Envelope>";
        assert_eq!(
            field(answer, "CurrentTransportState").as_deref(),
            Some("PLAYING")
        );
        assert_eq!(fault(answer), None);
        let refused = "<s:Fault><detail><UPnPError><errorCode>701</errorCode><errorDescription>Transition not available</errorDescription></UPnPError></detail></s:Fault>";
        assert_eq!(
            fault(refused).as_deref(),
            Some("Transition not available (701)")
        );
    }
}
