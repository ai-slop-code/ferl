use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    domain::{
        ids::{AccountId, LinkId},
        time::TimeInterval,
    },
    http::query::{
        AccountListQuery, BreakdownQuery, GroupBy, LinkListQuery, LinkSort, OwnerFilter, Scope,
        SortOrder, TagDiscoveryQuery, TagMode, VisitListQuery,
    },
};

pub const CURSOR_CONTEXT_VERSION: u8 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorResource {
    Accounts,
    Links,
    Tags,
    Visits,
    Breakdown,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CursorActor {
    Account(AccountId),
    AnonymousLink(LinkId),
}

pub struct CursorContext {
    resource: CursorResource,
    digest: [u8; 32],
    link_sort: Option<LinkSort>,
    group_by: Option<GroupBy>,
}

#[derive(Serialize)]
struct CanonicalContext {
    version: u8,
    resource: CursorResource,
    actor: ActorContext,
    target_link_id: Option<Uuid>,
    scope: Option<ScopeContext>,
    owner: Option<OwnerContext>,
    filters: FilterContext,
}

#[derive(Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
enum ActorContext {
    Account(Uuid),
    AnonymousLink(Uuid),
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ScopeContext {
    Own,
    All,
}

#[derive(Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
enum OwnerContext {
    Account(Uuid),
    Anonymous,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum FilterContext {
    Accounts {
        order: &'static str,
        tie_break: &'static str,
    },
    Links {
        q: Option<String>,
        enabled: Option<bool>,
        created_from_micros: Option<i64>,
        created_to_micros: Option<i64>,
        tags: Vec<String>,
        tag_mode: Option<&'static str>,
        sort: &'static str,
        order: &'static str,
    },
    Tags {
        q: Option<String>,
        order: &'static str,
    },
    Visits {
        from_micros: Option<i64>,
        to_micros: Option<i64>,
        order: &'static str,
        tie_break: &'static str,
    },
    Breakdown {
        group_by: &'static str,
        from_micros: Option<i64>,
        to_micros: Option<i64>,
        order: &'static str,
    },
}

impl CursorContext {
    pub fn accounts(actor: AccountId, _query: &AccountListQuery) -> Self {
        Self::build(
            CanonicalContext {
                version: CURSOR_CONTEXT_VERSION,
                resource: CursorResource::Accounts,
                actor: ActorContext::Account(actor.as_uuid()),
                target_link_id: None,
                scope: None,
                owner: None,
                filters: FilterContext::Accounts {
                    order: "created_at_desc",
                    tie_break: "uuid_desc",
                },
            },
            None,
            None,
        )
    }

    pub fn links(actor: AccountId, query: &LinkListQuery) -> Self {
        let tags = query
            .tags()
            .as_slice()
            .iter()
            .map(|tag| tag.as_str().to_owned())
            .collect();
        let tag_mode = query.tag_mode().map(|mode| match mode {
            TagMode::Any => "any",
            TagMode::All => "all",
        });
        let interval = query.created();
        Self::build(
            CanonicalContext {
                version: CURSOR_CONTEXT_VERSION,
                resource: CursorResource::Links,
                actor: ActorContext::Account(actor.as_uuid()),
                target_link_id: None,
                scope: Some(scope_context(query.scope())),
                owner: query.owner().map(owner_context),
                filters: FilterContext::Links {
                    q: query.search().map(|search| search.as_str().to_owned()),
                    enabled: query.enabled(),
                    created_from_micros: interval.from().map(|time| time.unix_micros()),
                    created_to_micros: interval.to().map(|time| time.unix_micros()),
                    tags,
                    tag_mode,
                    sort: match query.sort() {
                        LinkSort::CreatedAt => "created_at",
                        LinkSort::Title => "title",
                        LinkSort::VisitCount => "visit_count",
                    },
                    order: match query.order() {
                        SortOrder::Asc => "asc",
                        SortOrder::Desc => "desc",
                    },
                },
            },
            Some(query.sort()),
            None,
        )
    }

    pub fn tags(actor: AccountId, query: &TagDiscoveryQuery) -> Self {
        Self::build(
            CanonicalContext {
                version: CURSOR_CONTEXT_VERSION,
                resource: CursorResource::Tags,
                actor: ActorContext::Account(actor.as_uuid()),
                target_link_id: None,
                scope: Some(scope_context(query.scope())),
                owner: query.owner().map(owner_context),
                filters: FilterContext::Tags {
                    q: query.search().map(|search| search.as_str().to_owned()),
                    order: "name_asc_utf8",
                },
            },
            None,
            None,
        )
    }

    pub fn visits(actor: CursorActor, link: LinkId, query: &VisitListQuery) -> Self {
        Self::link_report_context(CursorResource::Visits, actor, link, query.interval(), None)
    }

    pub fn breakdown(actor: CursorActor, link: LinkId, query: &BreakdownQuery) -> Self {
        Self::link_report_context(
            CursorResource::Breakdown,
            actor,
            link,
            query.interval(),
            Some(query.group_by()),
        )
    }

    fn link_report_context(
        resource: CursorResource,
        actor: CursorActor,
        link: LinkId,
        interval: TimeInterval,
        group_by: Option<GroupBy>,
    ) -> Self {
        let actor = match actor {
            CursorActor::Account(id) => ActorContext::Account(id.as_uuid()),
            CursorActor::AnonymousLink(id) => ActorContext::AnonymousLink(id.as_uuid()),
        };
        let (filters, scope, owner) = match resource {
            CursorResource::Visits => (
                FilterContext::Visits {
                    from_micros: interval.from().map(|time| time.unix_micros()),
                    to_micros: interval.to().map(|time| time.unix_micros()),
                    order: "occurred_at_desc",
                    tie_break: "uuid_desc",
                },
                None,
                None,
            ),
            CursorResource::Breakdown => (
                FilterContext::Breakdown {
                    group_by: group_context(group_by.expect("breakdown context has a group")),
                    from_micros: interval.from().map(|time| time.unix_micros()),
                    to_micros: interval.to().map(|time| time.unix_micros()),
                    order: "value_asc_null_first",
                },
                None,
                None,
            ),
            _ => unreachable!("link report contexts have only report resources"),
        };
        Self::build(
            CanonicalContext {
                version: CURSOR_CONTEXT_VERSION,
                resource,
                actor,
                target_link_id: Some(link.as_uuid()),
                scope,
                owner,
                filters,
            },
            None,
            group_by,
        )
    }

    fn build(
        context: CanonicalContext,
        link_sort: Option<LinkSort>,
        group_by: Option<GroupBy>,
    ) -> Self {
        let resource = context.resource;
        let bytes = serde_json::to_vec(&context)
            .expect("canonical cursor context contains only serializable validated values");
        let digest = Sha256::digest(bytes).into();
        Self {
            resource,
            digest,
            link_sort,
            group_by,
        }
    }

    pub const fn resource(&self) -> CursorResource {
        self.resource
    }

    pub(crate) const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    pub(crate) const fn link_sort(&self) -> Option<LinkSort> {
        self.link_sort
    }

    pub(crate) const fn group_by(&self) -> Option<GroupBy> {
        self.group_by
    }
}

fn scope_context(scope: Scope) -> ScopeContext {
    match scope {
        Scope::Own => ScopeContext::Own,
        Scope::All => ScopeContext::All,
    }
}

fn owner_context(owner: OwnerFilter) -> OwnerContext {
    match owner {
        OwnerFilter::Account(id) => OwnerContext::Account(id.as_uuid()),
        OwnerFilter::Anonymous => OwnerContext::Anonymous,
    }
}

fn group_context(group: GroupBy) -> &'static str {
    match group {
        GroupBy::Day => "day",
        GroupBy::Method => "method",
        GroupBy::Referrer => "referrer",
        GroupBy::ClientIp => "client_ip",
        GroupBy::UserAgent => "user_agent",
        GroupBy::Device => "device",
        GroupBy::Browser => "browser",
        GroupBy::Os => "os",
    }
}

impl fmt::Debug for CursorResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Accounts => "Accounts",
            Self::Links => "Links",
            Self::Tags => "Tags",
            Self::Visits => "Visits",
            Self::Breakdown => "Breakdown",
        })
    }
}

impl fmt::Debug for CursorActor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Account(_) => "CursorActor::Account([ID])",
            Self::AnonymousLink(_) => "CursorActor::AnonymousLink([ID])",
        })
    }
}

impl fmt::Debug for CursorContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CursorContext([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::{
        domain::{
            ids::{AccountId, LinkId},
            time::Timestamp,
        },
        http::query::{BreakdownQuery, LinkListQuery, VisitListQuery},
    };

    use super::{CursorActor, CursorContext, CursorResource};

    fn account() -> AccountId {
        AccountId::new(Uuid::from_u128(10))
    }

    #[test]
    fn contexts_are_canonical_and_limit_independent() {
        let short = LinkListQuery::parse(Some("tag=Work&tag=rust&sort=title&limit=1")).unwrap();
        let long = LinkListQuery::parse(Some("sort=title&tag=rust&tag=work&limit=200")).unwrap();
        assert_eq!(
            CursorContext::links(account(), &short).digest(),
            CursorContext::links(account(), &long).digest()
        );
        let changed = LinkListQuery::parse(Some("sort=title&tag=rust&tag=work&order=asc")).unwrap();
        assert_ne!(
            CursorContext::links(account(), &short).digest(),
            CursorContext::links(account(), &changed).digest()
        );
        assert_eq!(
            CursorContext::links(account(), &short).resource(),
            CursorResource::Links
        );
    }

    #[test]
    fn context_binds_actor_target_time_and_grouping() {
        let link = LinkId::new(Uuid::from_u128(20));
        let visits = VisitListQuery::parse(None).unwrap();
        let context = CursorContext::visits(CursorActor::Account(account()), link, &visits);
        assert_ne!(
            context.digest(),
            CursorContext::visits(
                CursorActor::Account(AccountId::new(Uuid::from_u128(11))),
                link,
                &visits,
            )
            .digest()
        );
        assert_ne!(
            context.digest(),
            CursorContext::visits(
                CursorActor::Account(account()),
                LinkId::new(Uuid::from_u128(21)),
                &visits,
            )
            .digest()
        );

        let dates = "from=2026-01-01T00%3A00%3A00Z";
        let day = BreakdownQuery::parse(Some(&format!("group_by=day&{dates}"))).unwrap();
        let os = BreakdownQuery::parse(Some(&format!("group_by=os&{dates}"))).unwrap();
        assert_ne!(
            CursorContext::breakdown(CursorActor::Account(account()), link, &day).digest(),
            CursorContext::breakdown(CursorActor::Account(account()), link, &os).digest()
        );
        let _ = Timestamp::parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
    }
}
