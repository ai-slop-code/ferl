use std::{collections::BTreeMap, fmt, net::IpAddr};

use serde::Serialize;
use serde_json::Value;

use crate::{
    config::PublicOrigin,
    db::records::{AccountRecord, LinkRecord, VisitRecord},
    domain::{
        ValidationError,
        count::{Count, PositiveCount},
        ids::{AccountId, LinkId, VisitId},
        short_code::ShortCode,
        text::{ComparisonText, Tag, TagSet, Title, Username},
        time::Timestamp,
        url::StoredUrl,
    },
};

use super::body::RawObject;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountRole {
    User,
    Admin,
}

pub enum Presence<T> {
    Omitted,
    Null,
    Value(T),
}

pub enum CreatorKind {
    Account,
    Anonymous,
}

pub struct RawCreateLink(BTreeMap<String, Value>);
pub struct CreateLinkRequest {
    destination_url: crate::domain::url::DestinationUrl,
    short_code: Option<ShortCode>,
    title: Option<Title>,
    tags: TagSet,
}
pub struct PatchLinkRequest {
    destination_url: Presence<crate::domain::url::DestinationUrl>,
    title: Presence<Title>,
    tags: Presence<TagSet>,
    enabled: Presence<bool>,
}
pub struct CreateAccountRequest {
    username: Username,
    role: AccountRole,
}
pub struct PatchAccountRequest {
    role: Presence<AccountRole>,
    enabled: Presence<bool>,
}

#[derive(Serialize)]
pub struct AccountDto {
    id: AccountId,
    username: String,
    role: AccountRole,
    enabled: bool,
    created_at: Timestamp,
    updated_at: Timestamp,
}

#[derive(Serialize)]
pub struct LinkDto {
    id: LinkId,
    short_code: String,
    short_url: String,
    destination_url: String,
    title: Option<String>,
    tags: TagSet,
    owner_id: Option<AccountId>,
    enabled: bool,
    active: bool,
    visit_count: Count,
    created_at: Timestamp,
    updated_at: Timestamp,
}

#[derive(Serialize)]
pub struct AccountCreationDto {
    account: AccountDto,
    api_key: crate::domain::secret::AccountApiKey,
}
#[derive(Serialize)]
pub struct ApiKeyReplacementDto {
    api_key: crate::domain::secret::AccountApiKey,
}
#[derive(Serialize)]
pub struct AnonymousLinkCreationDto {
    link: LinkDto,
    management_token: crate::domain::secret::ManagementTokenSecret,
}
#[derive(Serialize)]
pub struct OwnedLinkCreationDto {
    link: LinkDto,
}
#[derive(Serialize)]
pub struct ManagementTokenReplacementDto {
    management_token: crate::domain::secret::ManagementTokenSecret,
}

#[derive(Serialize)]
pub struct VisitDto {
    id: VisitId,
    link_id: LinkId,
    short_code: String,
    redirect_url: String,
    occurred_at: Timestamp,
    method: VisitMethod,
    client_ip: String,
    referrer: Option<String>,
    user_agent: Option<String>,
    device: DeviceClass,
    browser: String,
    os: String,
}

#[derive(Serialize)]
pub struct VisitStatsDto {
    lifetime_visits: Count,
    matching_visits: Count,
}

#[derive(Serialize)]
pub struct BreakdownItemDto {
    value: Option<String>,
    visits: Count,
}

#[derive(Serialize)]
pub struct TagInfoDto {
    name: String,
    link_count: PositiveCount,
}

#[derive(Serialize)]
pub struct CollectionDto<T> {
    items: Vec<T>,
    next_cursor: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
enum VisitMethod {
    #[serde(rename = "GET")]
    Get,
    #[serde(rename = "HEAD")]
    Head,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum DeviceClass {
    Desktop,
    Mobile,
    Tablet,
    Other,
    Unknown,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RequestValidationError {
    InvalidRequest,
    ForbiddenFeature,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RecordConversionError(&'static str);

impl RawCreateLink {
    pub fn from_object(object: RawObject) -> Self {
        Self(object.into_fields())
    }

    /// Permission is checked against raw member presence before validating its
    /// JSON type, so null/wrong-type short_code cannot bypass anonymous 403.
    pub fn has_short_code(&self) -> bool {
        self.0.contains_key("short_code")
    }

    pub fn validate(
        self,
        creator: CreatorKind,
    ) -> Result<CreateLinkRequest, RequestValidationError> {
        if matches!(creator, CreatorKind::Anonymous) && self.has_short_code() {
            return Err(RequestValidationError::ForbiddenFeature);
        }
        ensure_fields(&self.0, &["destination_url", "short_code", "title", "tags"])?;
        let mut fields = self.0;
        let destination_url = required_string(&mut fields, "destination_url")?;
        let destination_url = crate::domain::url::DestinationUrl::parse(&destination_url)
            .map_err(|_| RequestValidationError::InvalidRequest)?;
        let short_code = match fields.remove("short_code") {
            None => None,
            Some(Value::String(value)) => Some(
                ShortCode::parse_custom(&value)
                    .map_err(|_| RequestValidationError::InvalidRequest)?,
            ),
            Some(_) => return Err(RequestValidationError::InvalidRequest),
        };
        let title = parse_title(fields.remove("title"))?;
        let tags = match fields.remove("tags") {
            None => TagSet::parse(std::iter::empty::<&str>()),
            Some(Value::Array(values)) => {
                let mut inputs = Vec::with_capacity(values.len());
                for value in values {
                    match value {
                        Value::String(value) => inputs.push(value),
                        _ => return Err(RequestValidationError::InvalidRequest),
                    }
                }
                TagSet::parse(inputs)
            }
            Some(_) => return Err(RequestValidationError::InvalidRequest),
        }
        .map_err(|_| RequestValidationError::InvalidRequest)?;
        Ok(CreateLinkRequest {
            destination_url,
            short_code,
            title,
            tags,
        })
    }
}

impl CreateLinkRequest {
    pub fn destination_url(&self) -> &crate::domain::url::DestinationUrl {
        &self.destination_url
    }

    pub fn short_code(&self) -> Option<&ShortCode> {
        self.short_code.as_ref()
    }

    pub fn title(&self) -> Option<&Title> {
        self.title.as_ref()
    }

    pub fn tags(&self) -> &TagSet {
        &self.tags
    }
}

impl PatchLinkRequest {
    pub fn parse(object: RawObject) -> Result<Self, RequestValidationError> {
        let fields = object.into_fields();
        if fields.is_empty() {
            return Err(RequestValidationError::InvalidRequest);
        }
        ensure_fields(&fields, &["destination_url", "title", "tags", "enabled"])?;
        let mut fields = fields;
        let destination_url = match fields.remove("destination_url") {
            None => Presence::Omitted,
            Some(Value::String(value)) => Presence::Value(
                crate::domain::url::DestinationUrl::parse(&value)
                    .map_err(|_| RequestValidationError::InvalidRequest)?,
            ),
            Some(_) => return Err(RequestValidationError::InvalidRequest),
        };
        let title = parse_title_presence(fields.remove("title"))?;
        let tags = match fields.remove("tags") {
            None => Presence::Omitted,
            Some(Value::Array(values)) => {
                let mut inputs = Vec::with_capacity(values.len());
                for value in values {
                    match value {
                        Value::String(value) => inputs.push(value),
                        _ => return Err(RequestValidationError::InvalidRequest),
                    }
                }
                Presence::Value(
                    TagSet::parse(inputs).map_err(|_| RequestValidationError::InvalidRequest)?,
                )
            }
            Some(_) => return Err(RequestValidationError::InvalidRequest),
        };
        let enabled = match fields.remove("enabled") {
            None => Presence::Omitted,
            Some(Value::Bool(value)) => Presence::Value(value),
            Some(_) => return Err(RequestValidationError::InvalidRequest),
        };
        Ok(Self {
            destination_url,
            title,
            tags,
            enabled,
        })
    }

    pub const fn destination_url(&self) -> &Presence<crate::domain::url::DestinationUrl> {
        &self.destination_url
    }

    pub const fn title(&self) -> &Presence<Title> {
        &self.title
    }

    pub const fn tags(&self) -> &Presence<TagSet> {
        &self.tags
    }

    pub const fn enabled(&self) -> &Presence<bool> {
        &self.enabled
    }
}

impl CreateAccountRequest {
    pub fn parse(object: RawObject) -> Result<Self, RequestValidationError> {
        let mut fields = object.into_fields();
        ensure_fields(&fields, &["username", "role"])?;
        let username = required_string(&mut fields, "username")?;
        let username =
            Username::parse(&username).map_err(|_| RequestValidationError::InvalidRequest)?;
        let role = parse_role(fields.remove("role"), AccountRole::User)?;
        Ok(Self { username, role })
    }

    pub fn username(&self) -> &Username {
        &self.username
    }

    pub const fn role(&self) -> AccountRole {
        self.role
    }
}

impl PatchAccountRequest {
    pub fn parse(object: RawObject) -> Result<Self, RequestValidationError> {
        let mut fields = object.into_fields();
        if fields.is_empty() {
            return Err(RequestValidationError::InvalidRequest);
        }
        ensure_fields(&fields, &["role", "enabled"])?;
        let role = match fields.remove("role") {
            None => Presence::Omitted,
            value => Presence::Value(parse_role(value, AccountRole::User)?),
        };
        let enabled = match fields.remove("enabled") {
            None => Presence::Omitted,
            Some(Value::Bool(value)) => Presence::Value(value),
            Some(_) => return Err(RequestValidationError::InvalidRequest),
        };
        Ok(Self { role, enabled })
    }

    pub const fn role(&self) -> &Presence<AccountRole> {
        &self.role
    }

    pub const fn enabled(&self) -> &Presence<bool> {
        &self.enabled
    }
}

impl AccountRole {
    pub const fn as_database_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Admin => "admin",
        }
    }

    fn parse(value: &Value) -> Result<Self, RequestValidationError> {
        match value {
            Value::String(role) if role == "user" => Ok(Self::User),
            Value::String(role) if role == "admin" => Ok(Self::Admin),
            _ => Err(RequestValidationError::InvalidRequest),
        }
    }
}

fn parse_role(
    value: Option<Value>,
    default: AccountRole,
) -> Result<AccountRole, RequestValidationError> {
    match value {
        None => Ok(default),
        Some(value) => AccountRole::parse(&value),
    }
}

fn parse_title(value: Option<Value>) -> Result<Option<Title>, RequestValidationError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Title::parse(&value)
            .map(Some)
            .map_err(|_| RequestValidationError::InvalidRequest),
        Some(_) => Err(RequestValidationError::InvalidRequest),
    }
}

fn parse_title_presence(value: Option<Value>) -> Result<Presence<Title>, RequestValidationError> {
    match value {
        None => Ok(Presence::Omitted),
        Some(Value::Null) => Ok(Presence::Null),
        Some(Value::String(value)) => Title::parse(&value)
            .map(Presence::Value)
            .map_err(|_| RequestValidationError::InvalidRequest),
        Some(_) => Err(RequestValidationError::InvalidRequest),
    }
}

fn required_string(
    fields: &mut BTreeMap<String, Value>,
    field: &str,
) -> Result<String, RequestValidationError> {
    match fields.remove(field) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(RequestValidationError::InvalidRequest),
    }
}

fn ensure_fields(
    fields: &BTreeMap<String, Value>,
    allowed: &[&str],
) -> Result<(), RequestValidationError> {
    if fields.keys().all(|field| allowed.contains(&field.as_str())) {
        Ok(())
    } else {
        Err(RequestValidationError::InvalidRequest)
    }
}

impl AccountDto {
    pub fn from_record(record: &AccountRecord) -> Result<Self, RecordConversionError> {
        if record.api_key_digest.len() != 32 {
            return Err(RecordConversionError("account digest length is invalid"));
        }
        let username = Username::parse(&record.username)
            .map_err(|_| RecordConversionError("stored username is invalid"))?;
        let role = match record.role.as_str() {
            "user" => AccountRole::User,
            "admin" => AccountRole::Admin,
            _ => return Err(RecordConversionError("stored account role is invalid")),
        };
        Ok(Self {
            id: AccountId::new(record.id),
            username: username.as_str().to_owned(),
            role,
            enabled: record.enabled,
            created_at: Timestamp::from_database(record.created_at)
                .map_err(|_| RecordConversionError("account creation time is invalid"))?,
            updated_at: Timestamp::from_database(record.updated_at)
                .map_err(|_| RecordConversionError("account update time is invalid"))?,
        })
    }
}

impl LinkDto {
    pub fn from_record(
        record: &LinkRecord,
        tag_names: Vec<String>,
        origin: &PublicOrigin,
        owner_enabled: Option<bool>,
    ) -> Result<Self, RecordConversionError> {
        if record
            .management_token_digest
            .as_ref()
            .is_some_and(|digest| digest.len() != 32)
        {
            return Err(RecordConversionError(
                "management-token digest length is invalid",
            ));
        }
        match (
            record.owner_id,
            record.management_token_digest.is_some(),
            owner_enabled,
        ) {
            (None, true, None) | (Some(_), false, Some(_)) => {}
            _ => {
                return Err(RecordConversionError(
                    "stored ownership state is inconsistent",
                ));
            }
        }
        let short_code = ShortCode::parse_custom(&record.short_code)
            .map_err(|_| RecordConversionError("stored short code is invalid"))?;
        let destination = StoredUrl::from_database(record.destination_url.clone())
            .map_err(|_| RecordConversionError("stored destination URL is invalid"))?;
        let destination_search = ComparisonText::from_persisted(destination.as_str());
        if destination_search.as_str() != record.destination_search {
            return Err(RecordConversionError(
                "stored destination search value is inconsistent",
            ));
        }
        let title = record
            .title
            .clone()
            .map(Title::from_database)
            .transpose()
            .map_err(|_| RecordConversionError("stored title is invalid"))?;
        match (&title, &record.title_search) {
            (Some(title), Some(search))
                if ComparisonText::from_persisted(title.as_str()).as_str() == search => {}
            (None, None) => {}
            _ => {
                return Err(RecordConversionError(
                    "stored title search value is inconsistent",
                ));
            }
        }
        let tag_set = TagSet::from_database(tag_names.clone())
            .map_err(|_| RecordConversionError("stored tags are invalid"))?;
        if tag_set.as_slice().len() != tag_names.len() {
            return Err(RecordConversionError("stored tags contain duplicates"));
        }
        let count = Count::from_database(record.visit_count)
            .map_err(|_| RecordConversionError("stored visit count is invalid"))?;
        let owner_active = match (record.owner_id, owner_enabled) {
            (None, None) => true,
            (Some(_), Some(enabled)) => enabled,
            _ => {
                return Err(RecordConversionError(
                    "owner state is missing or unexpected",
                ));
            }
        };
        Ok(Self {
            id: LinkId::new(record.id),
            short_code: short_code.as_str().to_owned(),
            short_url: origin.short_url(&short_code),
            destination_url: destination.as_str().to_owned(),
            title: title.map(|value| value.as_str().to_owned()),
            tags: tag_set,
            owner_id: record.owner_id.map(AccountId::new),
            enabled: record.enabled,
            active: record.enabled && owner_active,
            visit_count: count,
            created_at: Timestamp::from_unix_micros(
                record
                    .created_at
                    .postgres_micros()
                    .checked_add(946_684_800_000_000)
                    .ok_or(RecordConversionError("link creation time is invalid"))?,
            )
            .map_err(|_| RecordConversionError("link creation time is invalid"))?,
            updated_at: Timestamp::from_unix_micros(
                record
                    .updated_at
                    .postgres_micros()
                    .checked_add(946_684_800_000_000)
                    .ok_or(RecordConversionError("link update time is invalid"))?,
            )
            .map_err(|_| RecordConversionError("link update time is invalid"))?,
        })
    }
}

impl VisitDto {
    pub fn from_record(
        record: &VisitRecord,
        short_code: &str,
    ) -> Result<Self, RecordConversionError> {
        let short_code = ShortCode::parse_custom(short_code)
            .map_err(|_| RecordConversionError("stored visit short code is invalid"))?;
        let redirect_url = StoredUrl::from_database(record.redirect_url.clone())
            .map_err(|_| RecordConversionError("stored redirect snapshot is invalid"))?;
        let method = match record.method.as_str() {
            "GET" => VisitMethod::Get,
            "HEAD" => VisitMethod::Head,
            _ => return Err(RecordConversionError("stored visit method is invalid")),
        };
        let device = match record.device.as_str() {
            "desktop" => DeviceClass::Desktop,
            "mobile" => DeviceClass::Mobile,
            "tablet" => DeviceClass::Tablet,
            "other" => DeviceClass::Other,
            "unknown" => DeviceClass::Unknown,
            _ => return Err(RecordConversionError("stored device class is invalid")),
        };
        if record.browser.is_empty() || record.os.is_empty() {
            return Err(RecordConversionError(
                "stored browser or OS family is empty",
            ));
        }
        for header in [&record.referrer, &record.user_agent].into_iter().flatten() {
            if header.len() > 8_192 {
                return Err(RecordConversionError(
                    "stored visit header exceeds its byte limit",
                ));
            }
        }
        Ok(Self {
            id: VisitId::new(record.id),
            link_id: LinkId::new(record.link_id),
            short_code: short_code.as_str().to_owned(),
            redirect_url: redirect_url.as_str().to_owned(),
            occurred_at: Timestamp::from_database(record.occurred_at)
                .map_err(|_| RecordConversionError("stored visit time is invalid"))?,
            method,
            client_ip: canonical_host_ip(&record.client_ip)
                .map_err(|_| RecordConversionError("stored client IP is not a host address"))?,
            referrer: record.referrer.clone(),
            user_agent: record.user_agent.clone(),
            device,
            browser: record.browser.clone(),
            os: record.os.clone(),
        })
    }
}

fn canonical_host_ip(net: &ipnet::IpNet) -> Result<String, ValidationError> {
    let address = match net {
        ipnet::IpNet::V4(network) if network.prefix_len() == 32 => IpAddr::V4(network.addr()),
        ipnet::IpNet::V6(network) if network.prefix_len() == 128 => IpAddr::V6(network.addr()),
        _ => return Err(crate::domain::invalid("inet value is not a host address")),
    };
    Ok(crate::client_ip::CanonicalIp::new(address)
        .address()
        .to_string())
}

impl VisitStatsDto {
    pub fn new(lifetime_visits: i64, matching_visits: i64) -> Result<Self, RecordConversionError> {
        Ok(Self {
            lifetime_visits: Count::from_database(lifetime_visits)
                .map_err(|_| RecordConversionError("lifetime visit count is invalid"))?,
            matching_visits: Count::from_database(matching_visits)
                .map_err(|_| RecordConversionError("matching visit count is invalid"))?,
        })
    }
}

impl BreakdownItemDto {
    pub fn new(value: Option<String>, visits: i64) -> Result<Self, RecordConversionError> {
        if value.as_ref().is_some_and(|value| value.len() > 8_192) {
            return Err(RecordConversionError(
                "breakdown grouping value is too long",
            ));
        }
        Ok(Self {
            value,
            visits: Count::from_database(visits)
                .map_err(|_| RecordConversionError("breakdown visit count is invalid"))?,
        })
    }
}

impl TagInfoDto {
    pub fn new(name: String, link_count: i64) -> Result<Self, RecordConversionError> {
        let name = Tag::from_database(name)
            .map_err(|_| RecordConversionError("stored tag name is invalid"))?;
        Ok(Self {
            name: name.as_str().to_owned(),
            link_count: PositiveCount::from_database(link_count)
                .map_err(|_| RecordConversionError("tag link count must be positive"))?,
        })
    }
}

impl<T> CollectionDto<T> {
    pub fn new(items: Vec<T>, next_cursor: Option<String>) -> Self {
        Self { items, next_cursor }
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn next_cursor(&self) -> Option<&str> {
        self.next_cursor.as_deref()
    }
}

impl AccountCreationDto {
    pub fn new(account: AccountDto, api_key: crate::domain::secret::AccountApiKey) -> Self {
        Self { account, api_key }
    }
}

impl ApiKeyReplacementDto {
    pub fn new(api_key: crate::domain::secret::AccountApiKey) -> Self {
        Self { api_key }
    }
}

impl AnonymousLinkCreationDto {
    pub fn new(
        link: LinkDto,
        management_token: crate::domain::secret::ManagementTokenSecret,
    ) -> Self {
        Self {
            link,
            management_token,
        }
    }
}

impl OwnedLinkCreationDto {
    pub fn new(link: LinkDto) -> Self {
        Self { link }
    }
}

impl ManagementTokenReplacementDto {
    pub fn new(management_token: crate::domain::secret::ManagementTokenSecret) -> Self {
        Self { management_token }
    }
}

impl fmt::Debug for RequestValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "RequestValidationError::InvalidRequest",
            Self::ForbiddenFeature => "RequestValidationError::ForbiddenFeature",
        })
    }
}

impl fmt::Display for RequestValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "request fields are invalid",
            Self::ForbiddenFeature => "the requested feature is not permitted",
        })
    }
}

impl std::error::Error for RequestValidationError {}

impl fmt::Debug for RecordConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RecordConversionError")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for RecordConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for RecordConversionError {}

impl From<ValidationError> for RecordConversionError {
    fn from(_: ValidationError) -> Self {
        Self("stored value is invalid")
    }
}

impl fmt::Debug for AccountRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::User => "User",
            Self::Admin => "Admin",
        })
    }
}

impl fmt::Debug for CreatorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Account => "Account",
            Self::Anonymous => "Anonymous",
        })
    }
}

impl fmt::Debug for VisitMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
        })
    }
}

impl fmt::Debug for DeviceClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceClass")
    }
}

#[cfg(test)]
mod tests {
    use std::{net::IpAddr, str::FromStr};

    use axum::body::Body;
    use http::{HeaderMap, HeaderValue, header};
    use serde_json::json;

    use super::{
        AccountRole, CreateAccountRequest, CreatorKind, PatchAccountRequest, PatchLinkRequest,
        Presence, RawCreateLink, RequestValidationError,
    };
    use crate::{
        domain::{secret::AccountApiKey, text::TagSet},
        http::body::{RawObject, read_json_object_with_deadline},
    };

    async fn raw(body: &str) -> RawObject {
        let headers = HeaderMap::from_iter([(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )]);
        read_json_object_with_deadline(
            &headers,
            Body::from(body.to_owned()),
            std::time::Duration::from_secs(1),
        )
        .await
        .expect("fixture JSON object parses")
    }

    #[tokio::test]
    async fn anonymous_short_code_presence_precedes_type_and_general_validation() {
        for body in [
            r#"{"destination_url":null,"short_code":null}"#,
            r#"{"short_code":false,"unexpected":1}"#,
        ] {
            let result =
                RawCreateLink::from_object(raw(body).await).validate(CreatorKind::Anonymous);
            assert_eq!(result.err(), Some(RequestValidationError::ForbiddenFeature));
        }
        let missing =
            RawCreateLink::from_object(raw(r#"{"destination_url":"https://example.test/"}"#).await);
        assert!(missing.validate(CreatorKind::Anonymous).is_ok());
    }

    #[tokio::test]
    async fn create_and_patch_presence_are_distinct_and_all_or_nothing() {
        let create = RawCreateLink::from_object(
            raw(r#"{"destination_url":"https://example.test/","title":null,"tags":[]}"#).await,
        )
        .validate(CreatorKind::Account)
        .unwrap();
        assert!(create.title().is_none());
        assert!(create.tags().as_slice().is_empty());

        let patch = PatchLinkRequest::parse(raw(r#"{"title":null,"tags":[]}"#).await).unwrap();
        assert!(matches!(patch.destination_url(), Presence::Omitted));
        assert!(matches!(patch.title(), Presence::Null));
        assert!(matches!(patch.tags(), Presence::Value(tags) if tags.as_slice().is_empty()));
        assert!(PatchLinkRequest::parse(raw("{}").await).is_err());
        for body in [
            r#"{"tags":null}"#,
            r#"{"destination_url":null}"#,
            r#"{"enabled":null}"#,
            r#"{"short_code":"immutable"}"#,
            r#"{"owner_id":null}"#,
            r#"{"tags":["valid",2]}"#,
            r#"{"title":"bad\u0000title"}"#,
        ] {
            assert!(PatchLinkRequest::parse(raw(body).await).is_err(), "{body}");
        }
        let valid = RawCreateLink::from_object(
            raw(r#"{"destination_url":"https://example.test/","title":"ok","tags":["ok"]}"#).await,
        )
        .validate(CreatorKind::Account);
        assert!(valid.is_ok());
        let bad = RawCreateLink::from_object(
            raw(r#"{"destination_url":"https://example.test/","title":"ok","tags":["", "ok"]}"#)
                .await,
        )
        .validate(CreatorKind::Account);
        assert!(bad.is_err());
    }

    #[tokio::test]
    async fn account_role_defaults_and_null_or_unknown_members_are_rejected() {
        let create = CreateAccountRequest::parse(raw(r#"{"username":"user_1"}"#).await).unwrap();
        assert_eq!(create.role(), AccountRole::User);
        for body in [
            r#"{"username":"user_1","role":null}"#,
            r#"{"username":"user_1","role":"root"}"#,
            r#"{"username":"user_1","id":"immutable"}"#,
        ] {
            assert!(CreateAccountRequest::parse(raw(body).await).is_err());
        }
        let patch = PatchAccountRequest::parse(raw(r#"{"enabled":false}"#).await).unwrap();
        assert!(matches!(patch.role(), Presence::Omitted));
        assert!(matches!(patch.enabled(), Presence::Value(false)));
        assert!(PatchAccountRequest::parse(raw(r#"{"role":null}"#).await).is_err());
    }

    #[test]
    fn ipv4_mapped_ipv6_is_normalized_and_non_host_masks_fail() {
        let mapped = ipnet::IpNet::from_str("::ffff:192.0.2.1/128").unwrap();
        assert_eq!(super::canonical_host_ip(&mapped).unwrap(), "192.0.2.1");
        let network = ipnet::IpNet::from_str("192.0.2.0/24").unwrap();
        assert!(super::canonical_host_ip(&network).is_err());
        assert_eq!(
            "2001:db8::1".parse::<IpAddr>().unwrap().to_string(),
            "2001:db8::1"
        );
    }

    #[test]
    fn one_time_secret_types_are_the_only_secret_response_fields() {
        let secret = AccountApiKey::generate().expect("OS randomness is available");
        let json = serde_json::to_value(super::ApiKeyReplacementDto::new(secret)).unwrap();
        assert_eq!(
            json.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["api_key"]
        );
        let _empty: TagSet = TagSet::parse(std::iter::empty::<&str>()).unwrap();
        let _ = json!({"body": "not emitted"});
    }
}
