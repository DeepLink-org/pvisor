//! Shared HTTP header filtering for proxy forwarding.

use crate::api::ForwardingHeaders;
use axum::http::HeaderMap;

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

pub(crate) fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.iter().any(|h| name.eq_ignore_ascii_case(h))
}

pub(crate) fn skip_transparent_forward_header(name: &str) -> bool {
    is_hop_by_hop(name) || name.eq_ignore_ascii_case("host")
}

/// RFC 9110 allows `Connection` to nominate additional hop-by-hop headers.
/// Those fields must be removed alongside the fixed proxy header set.
pub(crate) fn skip_transparent_forward_header_for(headers: &HeaderMap, name: &str) -> bool {
    skip_transparent_forward_header(name) || connection_nominates_header(headers, name)
}

pub(crate) fn skip_upstream_forward_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "x-api-key" | "x-goog-api-key" | "authorization" | "expect"
    ) || is_hop_by_hop(name)
}

fn connection_nominates_header(headers: &HeaderMap, name: &str) -> bool {
    headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|token| token.eq_ignore_ascii_case(name))
}

impl ForwardingHeaders for HeaderMap {
    fn skip_upstream_forward_header(&self, name: &str) -> bool {
        skip_upstream_forward_header(name) || connection_nominates_header(self, name)
    }
    fn skip_response_header_after_reframing(&self, name: &str, body_changed: bool) -> bool {
        is_hop_by_hop(name)
            || connection_nominates_header(self, name)
            || name.eq_ignore_ascii_case("content-length")
            || (body_changed
                && matches!(
                    name.to_ascii_lowercase().as_str(),
                    "content-encoding" | "content-type"
                ))
    }
    fn is_websocket_upgrade(&self) -> bool {
        self.get_all("upgrade")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .any(|protocol| protocol.eq_ignore_ascii_case("websocket"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::lowercase(false)]
    #[case::uppercase(true)]
    fn every_fixed_hop_header_is_filtered_case_insensitively(#[case] uppercase: bool) {
        for name in HOP_BY_HOP {
            let spelling = if uppercase {
                name.to_ascii_uppercase()
            } else {
                (*name).into()
            };
            assert!(is_hop_by_hop(&spelling));
            assert!(skip_transparent_forward_header(&spelling));
        }
        assert!(!is_hop_by_hop("x-request-id"));
    }

    #[rstest::rstest]
    #[case::host("host", true)]
    #[case::credentials("authorization", false)]
    #[case::body_length("content-length", false)]
    #[case::end_to_end("x-request-id", false)]
    fn transparent_forwarding_preserves_end_to_end_fields(
        #[case] name: &str,
        #[case] removed: bool,
        #[values(false, true)] uppercase: bool,
    ) {
        let name = if uppercase {
            name.to_ascii_uppercase()
        } else {
            name.into()
        };
        assert_eq!(skip_transparent_forward_header(&name), removed);
    }

    #[rstest::rstest]
    #[case::single(&["keep-alive, x-private-hop"], "x-private-hop", true)]
    #[case::unrelated(&["keep-alive, x-private-hop"], "x-end-to-end", false)]
    #[case::multiple(&["keep-alive", " x-first , X-Second "], "x-second", true)]
    #[case::first(&["keep-alive", " x-first , X-Second "], "x-first", true)]
    #[case::not_a_substring(&["x-first-other"], "x-first", false)]
    #[case::empty_tokens(&[", , X-First, "], "x-first", true)]
    #[case::absent(&[], "x-first", false)]
    fn connection_values_nominate_whole_trimmed_tokens(
        #[case] values: &[&str],
        #[case] name: &str,
        #[case] removed: bool,
    ) {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append("connection", value.parse().unwrap());
        }
        assert_eq!(skip_transparent_forward_header_for(&headers, name), removed);
    }

    #[rstest::rstest]
    fn invalid_connection_bytes_do_not_hide_other_values() {
        let mut headers = HeaderMap::new();
        headers.append(
            "connection",
            axum::http::HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        headers.append("connection", "x-private-hop".parse().unwrap());
        assert!(skip_transparent_forward_header_for(
            &headers,
            "x-private-hop"
        ));
        assert!(!skip_transparent_forward_header_for(
            &headers,
            "x-end-to-end"
        ));
    }
}
