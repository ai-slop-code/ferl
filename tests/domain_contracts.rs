use std::{collections::BTreeSet, str::FromStr};

use chrono::{DateTime, TimeZone, Utc};
use ferl_server::{
    db::records::{AccountRecord, DatabaseTimestamp, LinkRecord, VisitRecord},
    domain::{
        count::{Count, PositiveCount},
        secret::{AccountApiKey, ManagementTokenSecret},
        short_code::ShortCode,
        text::{ComparisonText, SearchQuery, Tag, TagSet, Title, Username},
        time::{TimeInterval, Timestamp},
        url::{DestinationUrl, PublicOrigin, StoredUrl},
    },
    http::dto::{
        AccountCreationDto, AccountDto, AnonymousLinkCreationDto, ApiKeyReplacementDto,
        BreakdownItemDto, CollectionDto, LinkDto, ManagementTokenReplacementDto,
        OwnedLinkCreationDto, TagInfoDto, VisitDto, VisitStatsDto,
    },
};
use uuid::Uuid;

fn utc(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("fixture has an RFC 3339 offset")
        .with_timezone(&Utc)
}

fn sample_link(owner_id: Option<Uuid>) -> LinkRecord {
    LinkRecord {
        id: Uuid::from_u128(10),
        short_code: "rust-docs".to_owned(),
        owner_id,
        destination_url: "https://example.test/docs?edition=2#intro".to_owned(),
        destination_search: ComparisonText::from_persisted(
            "https://example.test/docs?edition=2#intro",
        )
        .as_str()
        .to_owned(),
        title: Some("Rust Docs".to_owned()),
        title_search: Some(
            ComparisonText::from_persisted("Rust Docs")
                .as_str()
                .to_owned(),
        ),
        enabled: true,
        visit_count: i64::MAX,
        management_token_digest: owner_id.is_none().then_some(vec![0xa5; 32]),
        created_at: DatabaseTimestamp::from_utc(utc("2026-10-28T10:00:00.123456789Z"))
            .expect("finite fixture timestamp is representable"),
        updated_at: DatabaseTimestamp::from_utc(utc("2026-10-28T10:00:01.987654321Z"))
            .expect("finite fixture timestamp is representable"),
    }
}

fn json_keys(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("DTO serializes as an object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn validated_domain_values_keep_their_scalar_and_canonical_invariants() {
    assert_eq!(
        Username::parse("user_name-1").unwrap().as_str(),
        "user_name-1"
    );
    assert!(Username::parse("User").is_err());
    assert_eq!(ShortCode::parse_custom("-api-").unwrap().as_str(), "-api-");
    assert_eq!(
        Title::parse("\u{2003}Straße\u{a0}").unwrap().as_str(),
        "Straße"
    );
    assert_eq!(Tag::parse(" Work ").unwrap().as_str(), "work");
    let tags = TagSet::parse(["É", "E\u{301}", "Work", "work"]).unwrap();
    assert_eq!(
        tags.as_slice().iter().map(Tag::as_str).collect::<Vec<_>>(),
        ["work", "é"]
    );
    assert_eq!(SearchQuery::parse("  Σςß  ").unwrap().as_str(), "σσss");
    assert_eq!(
        ComparisonText::from_persisted("E\u{301}").as_str(),
        ComparisonText::from_persisted("É").as_str()
    );
    assert!(TagSet::from_database(["Work".to_owned()]).is_err());
    assert!(Count::from_database(i64::MAX).unwrap().increment().is_err());
    assert!(PositiveCount::from_database(0).is_err());
}

#[test]
fn url_normalization_and_persisted_snapshot_checks_are_precise() {
    let url = DestinationUrl::parse("HTTPS://bücher.example/a/é?q=2&q=1#section")
        .expect("IDN and UTF-8 path normalize safely");
    assert_eq!(
        url.as_str(),
        "https://xn--bcher-kva.example/a/%C3%A9?q=2&q=1#section"
    );
    assert_eq!(url.location_header().to_str().unwrap(), url.as_str());
    assert!(DestinationUrl::parse("http://@internal").is_err());
    assert!(DestinationUrl::parse("http://internal:").is_err());
    assert!(DestinationUrl::parse("http://internal\\path").is_err());
    assert_eq!(
        PublicOrigin::parse("https://short.example/")
            .unwrap()
            .as_str(),
        "https://short.example"
    );
    assert_eq!(
        StoredUrl::from_database(url.as_str().to_owned())
            .unwrap()
            .as_str(),
        url.as_str()
    );
    assert!(StoredUrl::from_database("https://example.test/a/../b".to_owned()).is_err());
}

#[test]
fn account_and_link_dtos_expose_exact_fields_and_required_nulls() {
    let account = AccountRecord {
        id: Uuid::from_u128(1),
        username: "alice_1".to_owned(),
        role: "user".to_owned(),
        enabled: true,
        api_key_digest: vec![0x44; 32],
        created_at: utc("2026-10-28T10:00:00.123456789+02:00"),
        updated_at: utc("2026-10-28T10:00:01Z"),
    };
    let account_json = serde_json::to_value(AccountDto::from_record(&account).unwrap()).unwrap();
    assert_eq!(
        json_keys(&account_json),
        [
            "id",
            "username",
            "role",
            "enabled",
            "created_at",
            "updated_at"
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    assert_eq!(account_json["created_at"], "2026-10-28T08:00:00.123456Z");
    assert!(
        AccountDto::from_record(&AccountRecord {
            api_key_digest: vec![0; 31],
            ..account
        })
        .is_err()
    );

    let account_for_creation = AccountDto::from_record(&AccountRecord {
        id: Uuid::from_u128(1),
        username: "alice_1".to_owned(),
        role: "user".to_owned(),
        enabled: true,
        api_key_digest: vec![0x44; 32],
        created_at: utc("2026-10-28T10:00:00Z"),
        updated_at: utc("2026-10-28T10:00:00Z"),
    })
    .unwrap();
    let account_creation = serde_json::to_value(AccountCreationDto::new(
        account_for_creation,
        AccountApiKey::generate().unwrap(),
    ))
    .unwrap();
    assert_eq!(
        json_keys(&account_creation),
        BTreeSet::from(["account".to_owned(), "api_key".to_owned()])
    );
    assert_eq!(
        json_keys(
            &serde_json::to_value(ApiKeyReplacementDto::new(
                AccountApiKey::generate().unwrap(),
            ))
            .unwrap()
        ),
        BTreeSet::from(["api_key".to_owned()])
    );
    assert_eq!(
        json_keys(
            &serde_json::to_value(ManagementTokenReplacementDto::new(
                ManagementTokenSecret::generate().unwrap(),
            ))
            .unwrap()
        ),
        BTreeSet::from(["management_token".to_owned()])
    );

    let origin = PublicOrigin::parse("https://short.example/").unwrap();
    let anonymous_record = sample_link(None);
    let anonymous_link = LinkDto::from_record(
        &anonymous_record,
        vec!["work".into(), "rust".into()],
        &origin,
        None,
    )
    .unwrap();
    let anonymous_json = serde_json::to_value(&anonymous_link).unwrap();
    assert_eq!(
        anonymous_json["short_url"],
        "https://short.example/rust-docs"
    );
    assert_eq!(anonymous_json["owner_id"], serde_json::Value::Null);
    assert_eq!(anonymous_json["title"], "Rust Docs");
    assert_eq!(anonymous_json["tags"], serde_json::json!(["rust", "work"]));
    assert_eq!(anonymous_json["active"], true);
    assert_eq!(anonymous_json["visit_count"], i64::MAX);
    assert_eq!(anonymous_json["created_at"], "2026-10-28T10:00:00.123456Z");
    assert_eq!(
        json_keys(&anonymous_json),
        [
            "id",
            "short_code",
            "short_url",
            "destination_url",
            "title",
            "tags",
            "owner_id",
            "enabled",
            "active",
            "visit_count",
            "created_at",
            "updated_at",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );

    let token = ManagementTokenSecret::generate().unwrap();
    let anonymous_creation =
        serde_json::to_value(AnonymousLinkCreationDto::new(anonymous_link, token)).unwrap();
    assert!(anonymous_creation.get("management_token").is_some());
    let owned_record = sample_link(Some(Uuid::from_u128(2)));
    assert!(LinkDto::from_record(&owned_record, vec![], &origin, None).is_err());
    let owned_link = LinkDto::from_record(&owned_record, vec![], &origin, Some(false)).unwrap();
    let owned_creation = serde_json::to_value(OwnedLinkCreationDto::new(owned_link)).unwrap();
    assert!(owned_creation.get("management_token").is_none());
    assert_eq!(owned_creation["link"]["active"], false);

    let mut invalid_count = sample_link(None);
    invalid_count.visit_count = -1;
    assert!(LinkDto::from_record(&invalid_count, vec![], &origin, None).is_err());
}

#[test]
fn visit_and_report_conversions_keep_snapshot_nulls_counts_and_bare_ip() {
    let make_record = |occurred_at, client_ip| VisitRecord {
        id: Uuid::from_u128(20),
        link_id: Uuid::from_u128(10),
        redirect_url: "https://example.test/old?x=1#top".to_owned(),
        occurred_at,
        method: "HEAD".to_owned(),
        client_ip,
        referrer: None,
        user_agent: None,
        device: "unknown".to_owned(),
        browser: "unknown".to_owned(),
        os: "unknown".to_owned(),
    };
    let occurred_at = DatabaseTimestamp::from_utc(utc("2026-10-28T12:00:00.999999999+02:00"))
        .expect("finite test timestamp is in PostgreSQL range");
    let record = make_record(
        occurred_at,
        ipnet::IpNet::from_str("192.0.2.10/32").unwrap(),
    );
    let visit = serde_json::to_value(VisitDto::from_record(&record, "rust-docs").unwrap()).unwrap();
    assert_eq!(visit["redirect_url"], "https://example.test/old?x=1#top");
    assert_eq!(visit["client_ip"], "192.0.2.10");
    assert_eq!(visit["referrer"], serde_json::Value::Null);
    assert_eq!(visit["user_agent"], serde_json::Value::Null);
    assert_eq!(visit["occurred_at"], "2026-10-28T10:00:00.999999Z");
    assert_eq!(
        json_keys(&visit),
        [
            "id",
            "link_id",
            "short_code",
            "redirect_url",
            "occurred_at",
            "method",
            "client_ip",
            "referrer",
            "user_agent",
            "device",
            "browser",
            "os",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    let wire_out_of_range = DatabaseTimestamp::from_utc(
        Utc.with_ymd_and_hms(10_000, 1, 1, 0, 0, 0)
            .single()
            .expect("Chrono can represent a finite PostgreSQL-range timestamp"),
    )
    .expect("year 10000 is still inside PostgreSQL's finite range");
    assert!(
        VisitDto::from_record(
            &make_record(
                wire_out_of_range,
                ipnet::IpNet::from_str("192.0.2.10/32").unwrap()
            ),
            "rust-docs"
        )
        .is_err()
    );
    assert!(
        VisitDto::from_record(
            &make_record(occurred_at, ipnet::IpNet::from_str("192.0.2.0/24").unwrap()),
            "rust-docs"
        )
        .is_err()
    );

    let stats = serde_json::to_value(VisitStatsDto::new(12, 4).unwrap()).unwrap();
    assert_eq!(
        stats,
        serde_json::json!({"lifetime_visits":12,"matching_visits":4})
    );
    assert!(VisitStatsDto::new(-1, 0).is_err());
    assert_eq!(
        serde_json::to_value(BreakdownItemDto::new(None, 0).unwrap()).unwrap(),
        serde_json::json!({"value":null,"visits":0})
    );
    assert_eq!(
        serde_json::to_value(TagInfoDto::new("rust".into(), 1).unwrap()).unwrap(),
        serde_json::json!({"name":"rust","link_count":1})
    );
    assert!(TagInfoDto::new("rust".into(), 0).is_err());
    let collection = serde_json::to_value(CollectionDto::new(vec!["item"], None)).unwrap();
    assert_eq!(
        collection,
        serde_json::json!({"items":["item"],"next_cursor":null})
    );
}

#[test]
fn time_intervals_are_utc_microsecond_normalized_and_checked() {
    let lower = Timestamp::parse_rfc3339("2026-10-28T12:00:00.123456789+02:00").unwrap();
    let upper = Timestamp::parse_rfc3339("2026-10-28T10:00:01Z").unwrap();
    assert_eq!(lower.to_wire(), "2026-10-28T10:00:00.123456Z");
    assert_eq!(
        Timestamp::parse_rfc3339("0000-01-01T00:00:00Z")
            .unwrap()
            .to_wire(),
        "0000-01-01T00:00:00.000000Z",
        "four-digit year zero is valid under the locked RFC 3339 format checker"
    );
    assert!(TimeInterval::new(Some(lower), Some(upper)).is_ok());
    assert!(TimeInterval::new(Some(upper), Some(lower)).is_err());
    let outside_rfc3339 = Utc
        .with_ymd_and_hms(10_000, 1, 1, 0, 0, 0)
        .single()
        .expect("Chrono supports this timestamp");
    assert!(Timestamp::from_unix_micros(outside_rfc3339.timestamp_micros()).is_err());
    assert!(DestinationUrl::parse("https://internal.example/").is_ok());
}
