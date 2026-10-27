use std::{collections::BTreeMap, fmt};

use uuid::Uuid;

use crate::domain::{
    ValidationError,
    ids::AccountId,
    text::{SearchQuery, TagSet},
    time::{TimeInterval, Timestamp},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Own,
    All,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OwnerFilter {
    Account(AccountId),
    Anonymous,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TagMode {
    Any,
    All,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LinkSort {
    CreatedAt,
    Title,
    VisitCount,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Asc,
    Desc,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Day,
    Method,
    Referrer,
    ClientIp,
    UserAgent,
    Device,
    Browser,
    Os,
}

pub struct PageRequest {
    limit: u16,
    cursor: Option<String>,
}

pub struct AccountListQuery(PageRequest);
pub struct LinkListQuery {
    scope: Scope,
    owner: Option<OwnerFilter>,
    search: Option<SearchQuery>,
    enabled: Option<bool>,
    created: TimeInterval,
    tags: TagSet,
    tag_mode: Option<TagMode>,
    sort: LinkSort,
    order: SortOrder,
    page: PageRequest,
}
pub struct TagDiscoveryQuery {
    scope: Scope,
    owner: Option<OwnerFilter>,
    search: Option<SearchQuery>,
    page: PageRequest,
}
pub struct VisitListQuery {
    interval: TimeInterval,
    page: PageRequest,
}
pub struct VisitStatsQuery(TimeInterval);
pub struct BreakdownQuery {
    group_by: GroupBy,
    interval: TimeInterval,
    page: PageRequest,
}

#[derive(PartialEq, Eq)]
pub enum QueryError {
    InvalidRequest,
    Forbidden,
}

impl PageRequest {
    pub const fn limit(&self) -> u16 {
        self.limit
    }

    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }
}

impl AccountListQuery {
    pub fn parse(raw_query: Option<&str>) -> Result<Self, QueryError> {
        let mut query = RawQuery::parse(raw_query)?;
        query.ensure_allowed(&["limit", "cursor"])?;
        Ok(Self(query.page()?))
    }

    pub fn page(&self) -> &PageRequest {
        &self.0
    }
}

impl LinkListQuery {
    pub fn parse(raw_query: Option<&str>) -> Result<Self, QueryError> {
        let mut query = RawQuery::parse(raw_query)?;
        query.ensure_allowed(&[
            "scope",
            "owner_id",
            "q",
            "enabled",
            "created_from",
            "created_to",
            "tag",
            "tag_mode",
            "sort",
            "order",
            "limit",
            "cursor",
        ])?;
        let scope = parse_scope(query.scalar("scope")?.as_deref())?;
        let owner_raw = query.scalar("owner_id")?;
        let owner = owner_raw.as_deref().map(parse_owner).transpose()?;
        let search_raw = query.scalar("q")?;
        let search = search_raw
            .as_deref()
            .map(SearchQuery::parse)
            .transpose()
            .map_err(|_| QueryError::InvalidRequest)?;
        let enabled_raw = query.scalar("enabled")?;
        let enabled = enabled_raw.as_deref().map(parse_bool).transpose()?;
        let created_from = query.scalar("created_from")?;
        let created_to = query.scalar("created_to")?;
        let created = parse_interval(created_from.as_deref(), created_to.as_deref())?;
        let tag_inputs = query.repeated("tag")?;
        if tag_inputs.len() > 20 {
            return Err(QueryError::InvalidRequest);
        }
        let tags =
            TagSet::parse(tag_inputs).map_err(|_: ValidationError| QueryError::InvalidRequest)?;
        let supplied_tag_mode = query.scalar("tag_mode")?;
        if supplied_tag_mode.is_some() && tags.as_slice().is_empty() {
            return Err(QueryError::InvalidRequest);
        }
        let tag_mode = if tags.as_slice().is_empty() {
            None
        } else {
            Some(match supplied_tag_mode.as_deref() {
                None | Some("any") => TagMode::Any,
                Some("all") => TagMode::All,
                Some(_) => return Err(QueryError::InvalidRequest),
            })
        };
        let sort_raw = query.scalar("sort")?;
        let sort = match sort_raw.as_deref() {
            None | Some("created_at") => LinkSort::CreatedAt,
            Some("title") => LinkSort::Title,
            Some("visit_count") => LinkSort::VisitCount,
            Some(_) => return Err(QueryError::InvalidRequest),
        };
        let order_raw = query.scalar("order")?;
        let order = match order_raw.as_deref() {
            None | Some("desc") => SortOrder::Desc,
            Some("asc") => SortOrder::Asc,
            Some(_) => return Err(QueryError::InvalidRequest),
        };
        let page = query.page()?;
        Ok(Self {
            scope,
            owner,
            search,
            enabled,
            created,
            tags,
            tag_mode,
            sort,
            order,
            page,
        })
    }

    /// Syntax parsing never authenticates callers. This separate check preserves
    /// the specified 403/400 distinction for services after principal lookup.
    pub fn authorize_scope(&self, is_admin: bool) -> Result<(), QueryError> {
        if (self.owner.is_some() || self.scope == Scope::All) && !is_admin {
            return Err(QueryError::Forbidden);
        }
        if self.owner.is_some() && self.scope != Scope::All {
            return Err(QueryError::InvalidRequest);
        }
        Ok(())
    }

    pub const fn scope(&self) -> Scope {
        self.scope
    }

    pub const fn owner(&self) -> Option<OwnerFilter> {
        self.owner
    }

    pub fn search(&self) -> Option<&SearchQuery> {
        self.search.as_ref()
    }

    pub const fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    pub const fn created(&self) -> TimeInterval {
        self.created
    }

    pub fn tags(&self) -> &TagSet {
        &self.tags
    }

    pub const fn tag_mode(&self) -> Option<TagMode> {
        self.tag_mode
    }

    pub const fn sort(&self) -> LinkSort {
        self.sort
    }

    pub const fn order(&self) -> SortOrder {
        self.order
    }

    pub fn page(&self) -> &PageRequest {
        &self.page
    }
}

impl TagDiscoveryQuery {
    pub fn parse(raw_query: Option<&str>) -> Result<Self, QueryError> {
        let mut query = RawQuery::parse(raw_query)?;
        query.ensure_allowed(&["scope", "owner_id", "q", "limit", "cursor"])?;
        let scope = parse_scope(query.scalar("scope")?.as_deref())?;
        let owner_raw = query.scalar("owner_id")?;
        let owner = owner_raw.as_deref().map(parse_owner).transpose()?;
        let search_raw = query.scalar("q")?;
        let search = search_raw
            .as_deref()
            .map(SearchQuery::parse)
            .transpose()
            .map_err(|_| QueryError::InvalidRequest)?;
        Ok(Self {
            scope,
            owner,
            search,
            page: query.page()?,
        })
    }

    pub fn authorize_scope(&self, is_admin: bool) -> Result<(), QueryError> {
        if (self.owner.is_some() || self.scope == Scope::All) && !is_admin {
            return Err(QueryError::Forbidden);
        }
        if self.owner.is_some() && self.scope != Scope::All {
            return Err(QueryError::InvalidRequest);
        }
        Ok(())
    }

    pub const fn scope(&self) -> Scope {
        self.scope
    }

    pub const fn owner(&self) -> Option<OwnerFilter> {
        self.owner
    }

    pub fn search(&self) -> Option<&SearchQuery> {
        self.search.as_ref()
    }

    pub fn page(&self) -> &PageRequest {
        &self.page
    }
}

impl VisitListQuery {
    pub fn parse(raw_query: Option<&str>) -> Result<Self, QueryError> {
        let mut query = RawQuery::parse(raw_query)?;
        query.ensure_allowed(&["from", "to", "limit", "cursor"])?;
        let from = query.scalar("from")?;
        let to = query.scalar("to")?;
        Ok(Self {
            interval: parse_interval(from.as_deref(), to.as_deref())?,
            page: query.page()?,
        })
    }

    pub const fn interval(&self) -> TimeInterval {
        self.interval
    }

    pub fn page(&self) -> &PageRequest {
        &self.page
    }
}

impl VisitStatsQuery {
    pub fn parse(raw_query: Option<&str>) -> Result<Self, QueryError> {
        let mut query = RawQuery::parse(raw_query)?;
        query.ensure_allowed(&["from", "to"])?;
        let from = query.scalar("from")?;
        let to = query.scalar("to")?;
        Ok(Self(parse_interval(from.as_deref(), to.as_deref())?))
    }

    pub const fn interval(&self) -> TimeInterval {
        self.0
    }
}

impl BreakdownQuery {
    pub fn parse(raw_query: Option<&str>) -> Result<Self, QueryError> {
        let mut query = RawQuery::parse(raw_query)?;
        query.ensure_allowed(&["group_by", "from", "to", "limit", "cursor"])?;
        let group_by_raw = query.scalar("group_by")?;
        let group_by = match group_by_raw.as_deref() {
            Some("day") => GroupBy::Day,
            Some("method") => GroupBy::Method,
            Some("referrer") => GroupBy::Referrer,
            Some("client_ip") => GroupBy::ClientIp,
            Some("user_agent") => GroupBy::UserAgent,
            Some("device") => GroupBy::Device,
            Some("browser") => GroupBy::Browser,
            Some("os") => GroupBy::Os,
            _ => return Err(QueryError::InvalidRequest),
        };
        let from = query.scalar("from")?;
        let to = query.scalar("to")?;
        let interval = parse_interval(from.as_deref(), to.as_deref())?;
        let page = query.page()?;
        Ok(Self {
            group_by,
            interval,
            page,
        })
    }

    pub const fn group_by(&self) -> GroupBy {
        self.group_by
    }

    pub const fn interval(&self) -> TimeInterval {
        self.interval
    }

    pub fn page(&self) -> &PageRequest {
        &self.page
    }
}

struct RawQuery(BTreeMap<String, Vec<String>>);

impl RawQuery {
    fn parse(raw_query: Option<&str>) -> Result<Self, QueryError> {
        let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let Some(raw_query) = raw_query.filter(|query| !query.is_empty()) else {
            return Ok(Self(values));
        };
        for pair in raw_query.split('&') {
            if pair.is_empty() {
                return Err(QueryError::InvalidRequest);
            }
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            let name = decode_component(name)?;
            let value = decode_component(value)?;
            values.entry(name).or_default().push(value);
        }
        Ok(Self(values))
    }

    fn ensure_allowed(&self, allowed: &[&str]) -> Result<(), QueryError> {
        if self.0.keys().all(|key| allowed.contains(&key.as_str())) {
            Ok(())
        } else {
            Err(QueryError::InvalidRequest)
        }
    }

    fn scalar(&mut self, name: &str) -> Result<Option<String>, QueryError> {
        let Some(values) = self.0.remove(name) else {
            return Ok(None);
        };
        if values.len() != 1 {
            return Err(QueryError::InvalidRequest);
        }
        Ok(values.into_iter().next())
    }

    fn repeated(&mut self, name: &str) -> Result<Vec<String>, QueryError> {
        Ok(self.0.remove(name).unwrap_or_default())
    }

    fn page(&mut self) -> Result<PageRequest, QueryError> {
        let limit = match self.scalar("limit")? {
            None => 50,
            Some(value) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                value
                    .parse::<u16>()
                    .ok()
                    .filter(|value| (1..=200).contains(value))
                    .ok_or(QueryError::InvalidRequest)?
            }
            Some(_) => return Err(QueryError::InvalidRequest),
        };
        let cursor = self.scalar("cursor")?;
        if cursor.as_ref().is_some_and(String::is_empty) {
            return Err(QueryError::InvalidRequest);
        }
        Ok(PageRequest { limit, cursor })
    }
}

fn decode_component(raw: &str) -> Result<String, QueryError> {
    let replaced_plus = raw.replace('+', " ");
    let bytes = replaced_plus.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(QueryError::InvalidRequest);
            }
            let high = hex(bytes[index + 1]).ok_or(QueryError::InvalidRequest)?;
            let low = hex(bytes[index + 2]).ok_or(QueryError::InvalidRequest)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let value = String::from_utf8(decoded).map_err(|_| QueryError::InvalidRequest)?;
    Ok(value)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_scope(value: Option<&str>) -> Result<Scope, QueryError> {
    match value {
        None | Some("own") => Ok(Scope::Own),
        Some("all") => Ok(Scope::All),
        Some(_) => Err(QueryError::InvalidRequest),
    }
}

fn parse_owner(value: &str) -> Result<OwnerFilter, QueryError> {
    if value == "anonymous" {
        return Ok(OwnerFilter::Anonymous);
    }
    let id = Uuid::parse_str(value).map_err(|_| QueryError::InvalidRequest)?;
    if id.to_string().eq_ignore_ascii_case(value) {
        Ok(OwnerFilter::Account(AccountId::new(id)))
    } else {
        Err(QueryError::InvalidRequest)
    }
}

fn parse_bool(value: &str) -> Result<bool, QueryError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(QueryError::InvalidRequest),
    }
}

fn parse_interval(from: Option<&str>, to: Option<&str>) -> Result<TimeInterval, QueryError> {
    let from = from
        .map(Timestamp::parse_rfc3339)
        .transpose()
        .map_err(|_| QueryError::InvalidRequest)?;
    let to = to
        .map(Timestamp::parse_rfc3339)
        .transpose()
        .map_err(|_| QueryError::InvalidRequest)?;
    TimeInterval::new(from, to).map_err(|_| QueryError::InvalidRequest)
}

impl fmt::Debug for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "QueryError::InvalidRequest",
            Self::Forbidden => "QueryError::Forbidden",
        })
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "query parameters are invalid",
            Self::Forbidden => "query scope is not permitted",
        })
    }
}

impl std::error::Error for QueryError {}

macro_rules! safe_debug {
    ($($name:ty),+ $(,)?) => {$ (
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(stringify!($name))
            }
        }
    )+ };
}

safe_debug!(
    Scope,
    OwnerFilter,
    TagMode,
    LinkSort,
    SortOrder,
    GroupBy,
    PageRequest,
    AccountListQuery,
    LinkListQuery,
    TagDiscoveryQuery,
    VisitListQuery,
    VisitStatsQuery,
    BreakdownQuery,
);

#[cfg(test)]
mod tests {
    use super::{
        AccountListQuery, BreakdownQuery, GroupBy, LinkListQuery, LinkSort, OwnerFilter,
        QueryError, Scope, SortOrder, TagDiscoveryQuery, TagMode, VisitListQuery, VisitStatsQuery,
    };
    use crate::domain::text::Tag;
    use uuid::Uuid;

    #[test]
    fn raw_query_preserves_multiplicity_and_only_tags_are_repeated() {
        assert!(matches!(
            LinkListQuery::parse(Some("enabled=true&enabled=false")),
            Err(QueryError::InvalidRequest)
        ));
        let parsed = LinkListQuery::parse(Some("tag=work%2Crust&tag=Work&tag_mode=all"))
            .expect("only tag permits repeated values");
        assert_eq!(parsed.tag_mode(), Some(TagMode::All));
        assert_eq!(parsed.tags().as_slice().len(), 2);
        assert!(
            parsed
                .tags()
                .as_slice()
                .iter()
                .any(|tag| tag.as_str() == "work,rust")
        );
        assert!(matches!(
            AccountListQuery::parse(Some("limit=1&limit=2")),
            Err(QueryError::InvalidRequest)
        ));
        let repeated = std::iter::repeat_n("tag=work", 21)
            .collect::<Vec<_>>()
            .join("&");
        assert!(matches!(
            LinkListQuery::parse(Some(&repeated)),
            Err(QueryError::InvalidRequest)
        ));
    }

    #[test]
    fn strict_percent_and_utf8_decoding_never_repairs_invalid_values() {
        for raw in ["q=%", "q=%0", "q=%GG", "q=%ff", "q=%C0%AF", "q=a&&q=b"] {
            assert!(matches!(
                LinkListQuery::parse(Some(raw)),
                Err(QueryError::InvalidRequest)
            ));
        }
        assert_eq!(
            LinkListQuery::parse(Some("q=%25_%5C"))
                .unwrap()
                .search()
                .unwrap()
                .as_str(),
            "%_\\"
        );
        assert_eq!(
            LinkListQuery::parse(Some("q=a+b"))
                .unwrap()
                .search()
                .unwrap()
                .as_str(),
            "a b"
        );
    }

    #[test]
    fn all_operations_have_concrete_allowlists_and_limits() {
        assert_eq!(AccountListQuery::parse(None).unwrap().page().limit(), 50);
        assert_eq!(
            AccountListQuery::parse(Some("limit=200"))
                .unwrap()
                .page()
                .limit(),
            200
        );
        for invalid in ["0", "201", "+2", "1.0", "abc", ""] {
            assert!(AccountListQuery::parse(Some(&format!("limit={invalid}"))).is_err());
        }
        assert!(AccountListQuery::parse(Some("q=x")).is_err());
        assert!(TagDiscoveryQuery::parse(Some("tag=work")).is_err());
        assert!(VisitStatsQuery::parse(Some("limit=1")).is_err());
        assert!(VisitListQuery::parse(Some("sort=title")).is_err());
        assert!(BreakdownQuery::parse(None).is_err());
        assert_eq!(
            BreakdownQuery::parse(Some("group_by=client_ip"))
                .unwrap()
                .group_by(),
            GroupBy::ClientIp
        );
        assert!(BreakdownQuery::parse(Some("group_by=day&group_by=os")).is_err());
    }

    #[test]
    fn link_defaults_cross_fields_and_authorization_statuses_are_explicit() {
        let defaults = LinkListQuery::parse(None).unwrap();
        assert_eq!(defaults.scope(), Scope::Own);
        assert_eq!(defaults.sort(), LinkSort::CreatedAt);
        assert_eq!(defaults.order(), SortOrder::Desc);
        assert_eq!(defaults.page().limit(), 50);
        assert!(defaults.tags().as_slice().is_empty());
        assert!(LinkListQuery::parse(Some("tag_mode=all")).is_err());
        assert!(
            LinkListQuery::parse(Some(
                "created_from=2026-01-01T00:00:00Z&created_to=2026-01-01T00:00:00+00:00"
            ))
            .is_err()
        );
        assert!(LinkListQuery::parse(Some("enabled=True")).is_err());
        assert!(LinkListQuery::parse(Some("sort=updated_at")).is_err());

        let owner = Uuid::from_u128(1).to_string();
        let owner_query = LinkListQuery::parse(Some(&format!("owner_id={owner}"))).unwrap();
        assert_eq!(
            owner_query.authorize_scope(false),
            Err(QueryError::Forbidden)
        );
        assert_eq!(
            owner_query.authorize_scope(true),
            Err(QueryError::InvalidRequest)
        );
        let all_query = LinkListQuery::parse(Some("scope=all")).unwrap();
        assert_eq!(all_query.authorize_scope(false), Err(QueryError::Forbidden));
        assert_eq!(all_query.authorize_scope(true), Ok(()));
        assert!(matches!(owner_query.owner(), Some(OwnerFilter::Account(_))));
        assert!(matches!(
            LinkListQuery::parse(Some("scope=all&owner_id=anonymous"))
                .unwrap()
                .owner(),
            Some(OwnerFilter::Anonymous)
        ));
    }

    #[test]
    fn time_filters_keep_offset_normalization_and_nonexistent_owner_narrowing() {
        let query = VisitStatsQuery::parse(Some(
            "from=2026-01-01T02%3A00%3A00%2B02%3A00&to=2026-01-02T00%3A00%3A00Z",
        ))
        .unwrap();
        assert!(query.interval().from().is_some());
        let id = Uuid::from_u128(999).to_string();
        let query = LinkListQuery::parse(Some(&format!("scope=all&owner_id={id}"))).unwrap();
        assert_eq!(query.authorize_scope(true), Ok(()));
        assert!(Tag::parse("a,b").is_ok());
    }
}
