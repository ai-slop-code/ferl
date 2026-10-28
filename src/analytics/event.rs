use chrono::Utc;
use http::{HeaderMap, header};
use uuid::Uuid;

use crate::{
    client_ip::CanonicalIp,
    domain::{
        ids::{LinkId, VisitId},
        short_code::ShortCode,
        time::Timestamp,
        url::StoredUrl,
    },
    links::ActiveLinkSnapshot,
};

const MAX_ANALYTICS_HEADER_BYTES: usize = 8_192;

#[derive(Clone, Copy)]
pub enum VisitMethod {
    Get,
    Head,
}

impl VisitMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
        }
    }
}

/// A private, non-serializable event. It deliberately has no `Debug` impl:
/// URLs, IPs, user agents, and referrers must not be logged accidentally.
pub struct VisitEvent {
    id: VisitId,
    link_id: LinkId,
    short_code: ShortCode,
    redirect_url: StoredUrl,
    occurred_at: Timestamp,
    method: VisitMethod,
    client_ip: CanonicalIp,
    referrer: Option<String>,
    user_agent: Option<String>,
}

impl VisitEvent {
    pub(crate) fn for_redirect(
        snapshot: &ActiveLinkSnapshot,
        method: VisitMethod,
        client_ip: CanonicalIp,
        headers: &HeaderMap,
    ) -> Self {
        let now = Utc::now();
        // `Timestamp` applies PostgreSQL's microsecond resolution at the
        // response-issuance boundary, never later in the background worker.
        let occurred_at = Timestamp::from_database(now)
            .expect("the current UTC time is inside the supported database range");
        Self {
            id: VisitId::new(Uuid::new_v4()),
            link_id: snapshot.link_id(),
            short_code: snapshot.short_code().clone(),
            redirect_url: snapshot.destination_url().clone(),
            occurred_at,
            method,
            client_ip,
            referrer: optional_header(headers, header::REFERER),
            user_agent: optional_header(headers, header::USER_AGENT),
        }
    }

    pub(crate) const fn id(&self) -> VisitId {
        self.id
    }

    pub(crate) const fn link_id(&self) -> LinkId {
        self.link_id
    }

    pub(crate) fn short_code(&self) -> &ShortCode {
        &self.short_code
    }

    pub(crate) fn redirect_url(&self) -> &StoredUrl {
        &self.redirect_url
    }

    pub(crate) const fn occurred_at(&self) -> Timestamp {
        self.occurred_at
    }

    pub(crate) const fn method(&self) -> VisitMethod {
        self.method
    }

    pub(crate) const fn client_ip(&self) -> CanonicalIp {
        self.client_ip
    }

    pub(crate) fn referrer(&self) -> Option<&str> {
        self.referrer.as_deref()
    }

    pub(crate) fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }
}

fn optional_header(headers: &HeaderMap, name: http::HeaderName) -> Option<String> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() || value.as_bytes().len() > MAX_ANALYTICS_HEADER_BYTES {
        return None;
    }
    std::str::from_utf8(value.as_bytes())
        .ok()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header};

    use super::{MAX_ANALYTICS_HEADER_BYTES, optional_header};

    #[test]
    fn optional_header_capture_requires_one_complete_utf8_value_with_byte_bound() {
        let mut headers = HeaderMap::new();
        assert_eq!(optional_header(&headers, header::REFERER), None);

        headers.insert(
            header::REFERER,
            HeaderValue::from_bytes("https://例.example/".as_bytes()).unwrap(),
        );
        assert_eq!(
            optional_header(&headers, header::REFERER).as_deref(),
            Some("https://例.example/")
        );

        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_bytes(&[b'a'; MAX_ANALYTICS_HEADER_BYTES]).unwrap(),
        );
        assert_eq!(
            optional_header(&headers, header::USER_AGENT).unwrap().len(),
            MAX_ANALYTICS_HEADER_BYTES
        );
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_bytes(&[b'a'; MAX_ANALYTICS_HEADER_BYTES + 1]).unwrap(),
        );
        assert_eq!(optional_header(&headers, header::USER_AGENT), None);

        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        assert_eq!(optional_header(&headers, header::USER_AGENT), None);

        headers.append(
            header::REFERER,
            HeaderValue::from_static("https://other.example/"),
        );
        assert_eq!(optional_header(&headers, header::REFERER), None);
    }
}
