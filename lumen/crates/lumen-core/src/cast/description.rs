//! What a renderer says about itself: its name and where its `AVTransport` service takes commands.

use url::Url;

use crate::error::{PlayerError, Result};

/// The part of a device description Lumen needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    pub udn: String,
    pub name: String,
    /// The full service type the device announced (`…:AVTransport:1`), used as the SOAP namespace.
    pub service_type: String,
    pub control_url: Url,
}

fn tag_text(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&format!("</{tag}>"))? + start;
    Some(unescape(block[start..end].trim()))
}

/// The five XML entities and numeric references; descriptions use nothing more.
pub fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let replacement = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match replacement {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Escapes text for use inside an XML element or attribute.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Reads a device description. `location` is where it was fetched from: relative control
/// addresses are resolved against it, and the control address must stay on the same host, so a
/// device cannot point Lumen's commands somewhere else.
pub fn parse(xml: &str, location: &Url) -> Result<Description> {
    let bad = |reason: &str| PlayerError::Parse(format!("The device description {reason}"));
    let device_type = tag_text(xml, "deviceType").unwrap_or_default();
    if !device_type.contains("MediaRenderer") {
        return Err(bad("is not for a media renderer"));
    }
    let udn = tag_text(xml, "UDN").ok_or_else(|| bad("has no unique id"))?;
    let name = tag_text(xml, "friendlyName").unwrap_or_else(|| "Media renderer".to_string());
    let mut services = xml.split("<service>").skip(1);
    let (service_type, control) = loop {
        let Some(block) = services.next() else {
            return Err(bad("has no AVTransport service"));
        };
        let block = block.split("</service>").next().unwrap_or(block);
        if let Some(kind) = tag_text(block, "serviceType")
            && kind.starts_with("urn:schemas-upnp-org:service:AVTransport:")
            && let Some(control) = tag_text(block, "controlURL")
        {
            break (kind, control);
        }
    };
    let control_url = location
        .join(&control)
        .map_err(|_| bad("has an unusable control address"))?;
    if control_url.host() != location.host()
        || control_url.port_or_known_default() != location.port_or_known_default()
    {
        return Err(bad("sends commands to a different host"));
    }
    Ok(Description {
        udn,
        name,
        service_type,
        control_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESCRIPTION: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0"><device>
<deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
<friendlyName>Living room &amp; Kitchen TV</friendlyName>
<UDN>uuid:1234-abcd</UDN>
<serviceList>
<service><serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType><controlURL>/rc/control</controlURL></service>
<service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>/AVT/control</controlURL></service>
</serviceList></device></root>"#;

    fn location() -> Url {
        Url::parse("http://192.168.1.50:49152/desc.xml").unwrap()
    }

    #[test]
    fn a_renderer_is_described_by_name_and_control_address() {
        let description = parse(DESCRIPTION, &location()).unwrap();
        assert_eq!(description.name, "Living room & Kitchen TV");
        assert_eq!(description.udn, "uuid:1234-abcd");
        assert_eq!(
            description.service_type,
            "urn:schemas-upnp-org:service:AVTransport:1"
        );
        assert_eq!(
            description.control_url.as_str(),
            "http://192.168.1.50:49152/AVT/control"
        );
    }

    #[test]
    fn devices_that_cannot_play_video_or_point_elsewhere_are_refused() {
        let printer = DESCRIPTION.replace("MediaRenderer", "Printer");
        assert!(parse(&printer, &location()).is_err());
        let no_transport = DESCRIPTION.replace("AVTransport", "Other");
        assert!(parse(&no_transport, &location()).is_err());
        let elsewhere = DESCRIPTION.replace("/AVT/control", "http://10.9.9.9/control");
        assert!(
            parse(&elsewhere, &location()).is_err(),
            "a control address on another host"
        );
        let other_port = DESCRIPTION.replace("/AVT/control", "http://192.168.1.50:22/control");
        assert!(
            parse(&other_port, &location()).is_err(),
            "a control address on another port"
        );
    }

    #[test]
    fn escaping_round_trips() {
        let text = "Tom & Jerry <\"1\"> it's";
        assert_eq!(unescape(&escape(text)), text);
        assert_eq!(
            unescape("&#65;&#x42; &unknown; & done"),
            "AB &unknown; & done"
        );
    }
}
