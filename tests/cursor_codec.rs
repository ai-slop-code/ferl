use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ferl_server::{
    domain::{
        ids::{AccountId, LinkId, VisitId},
        time::Timestamp,
    },
    http::query::{
        AccountListQuery, BreakdownQuery, LinkListQuery, TagDiscoveryQuery, VisitListQuery,
    },
    pagination::{
        CursorActor, CursorBoundary, CursorCodec, CursorContext, MAX_CURSOR_ENCODED_BYTES,
        MAX_CURSOR_GROUPING_VALUE_BYTES,
    },
};
use uuid::Uuid;

fn timestamp() -> Timestamp {
    Timestamp::parse_rfc3339("2026-10-28T12:34:56.123456789+02:00").unwrap()
}

#[test]
fn instances_with_the_same_key_share_stateless_authenticated_boundaries() {
    let account = AccountId::new(Uuid::from_u128(101));
    let query = AccountListQuery::parse(None).unwrap();
    let context = CursorContext::accounts(account, &query);
    let producer = CursorCodec::new([0x42; 32]);
    let other_instance = CursorCodec::new([0x42; 32]);
    let boundary = CursorBoundary::AccountCreatedAt {
        at: timestamp(),
        id: AccountId::new(Uuid::from_u128(202)),
    };
    let cursor = producer.encode(&context, &boundary).unwrap();
    assert!(cursor.len() <= MAX_CURSOR_ENCODED_BYTES);
    assert!(matches!(
        other_instance.decode(&context, &cursor).unwrap(),
        CursorBoundary::AccountCreatedAt { at, id }
            if at == timestamp() && id.as_uuid() == Uuid::from_u128(202)
    ));
}

#[test]
fn version_nonce_ciphertext_tag_key_resource_actor_and_context_are_authenticated() {
    let owner = AccountId::new(Uuid::from_u128(10));
    let query =
        LinkListQuery::parse(Some("scope=all&sort=title&tag=Work&tag=rust&limit=2")).unwrap();
    let context = CursorContext::links(owner, &query);
    let codec = CursorCodec::new([0x61; 32]);
    let cursor = codec
        .encode(
            &context,
            &CursorBoundary::LinkTitle {
                normalized_title: None,
                id: LinkId::new(Uuid::from_u128(33)),
            },
        )
        .unwrap();
    assert!(matches!(
        codec.decode(&context, &cursor).unwrap(),
        CursorBoundary::LinkTitle {
            normalized_title: None,
            ..
        }
    ));

    let prefix = &cursor[..5];
    let envelope = URL_SAFE_NO_PAD.decode(&cursor[5..]).unwrap();
    for offset in [0, 24, envelope.len() - 1] {
        let mut tampered = envelope.clone();
        tampered[offset] ^= 0x80;
        let encoded = format!("{prefix}{}", URL_SAFE_NO_PAD.encode(tampered));
        assert!(
            codec.decode(&context, &encoded).is_err(),
            "envelope byte {offset}"
        );
    }
    assert!(
        codec
            .decode(&context, &format!("fc2{}", &cursor[3..]))
            .is_err()
    );
    let other_resource = format!("fc1a.{}", &cursor[5..]);
    assert!(codec.decode(&context, &other_resource).is_err());
    assert!(
        CursorCodec::new([0x62; 32])
            .decode(&context, &cursor)
            .is_err()
    );
    assert!(codec.decode(&context, &cursor[..cursor.len() - 2]).is_err());
    assert!(
        codec
            .decode(
                &context,
                &format!("{}x", "x".repeat(MAX_CURSOR_ENCODED_BYTES))
            )
            .is_err()
    );

    let changed_actor = CursorContext::links(AccountId::new(Uuid::from_u128(11)), &query);
    assert!(codec.decode(&changed_actor, &cursor).is_err());
    let changed_filter =
        LinkListQuery::parse(Some("scope=all&sort=title&order=asc&tag=rust&tag=work")).unwrap();
    assert!(
        codec
            .decode(&CursorContext::links(owner, &changed_filter), &cursor)
            .is_err()
    );
}

#[test]
fn typed_resource_boundaries_and_report_actor_binding_are_not_interchangeable() {
    let codec = CursorCodec::new([0x71; 32]);
    let account = AccountId::new(Uuid::from_u128(1));
    let link = LinkId::new(Uuid::from_u128(2));
    let visit_query = VisitListQuery::parse(None).unwrap();
    let visit_context = CursorContext::visits(CursorActor::Account(account), link, &visit_query);
    let visit = CursorBoundary::VisitOccurredAt {
        at: timestamp(),
        id: VisitId::new(Uuid::from_u128(3)),
    };
    let cursor = codec.encode(&visit_context, &visit).unwrap();
    assert!(codec.decode(&visit_context, &cursor).is_ok());
    assert!(
        codec
            .decode(
                &CursorContext::visits(CursorActor::AnonymousLink(link), link, &visit_query),
                &cursor,
            )
            .is_err()
    );
    assert!(
        codec
            .encode(
                &visit_context,
                &CursorBoundary::TagName {
                    name: "rust".to_owned()
                }
            )
            .is_err()
    );

    let breakdown = BreakdownQuery::parse(Some("group_by=referrer")).unwrap();
    let breakdown_context =
        CursorContext::breakdown(CursorActor::Account(account), link, &breakdown);
    let null_bucket = CursorBoundary::BreakdownValue { value: None };
    let null_cursor = codec.encode(&breakdown_context, &null_bucket).unwrap();
    assert!(matches!(
        codec.decode(&breakdown_context, &null_cursor).unwrap(),
        CursorBoundary::BreakdownValue { value: None }
    ));
    assert!(codec.decode(&visit_context, &null_cursor).is_err());
}

#[test]
fn canonical_contexts_bind_filters_but_not_page_size() {
    let account = AccountId::new(Uuid::from_u128(22));
    let first = LinkListQuery::parse(Some("tag=Work&tag=rust&sort=title&limit=1")).unwrap();
    let equivalent = LinkListQuery::parse(Some("sort=title&tag=rust&tag=work&limit=200")).unwrap();
    let changed = LinkListQuery::parse(Some("sort=title&tag=rust&tag=work&enabled=true")).unwrap();
    let context = CursorContext::links(account, &first);
    let codec = CursorCodec::new([0x22; 32]);
    let cursor = codec
        .encode(
            &context,
            &CursorBoundary::LinkTitle {
                normalized_title: Some("rust docs".into()),
                id: LinkId::new(Uuid::from_u128(23)),
            },
        )
        .unwrap();
    assert!(
        codec
            .decode(&CursorContext::links(account, &equivalent), &cursor)
            .is_ok()
    );
    assert!(
        codec
            .decode(&CursorContext::links(account, &changed), &cursor)
            .is_err()
    );

    let tags = TagDiscoveryQuery::parse(Some("scope=own&q=Rust&limit=1")).unwrap();
    let tag_context = CursorContext::tags(account, &tags);
    let tag_cursor = codec
        .encode(
            &tag_context,
            &CursorBoundary::TagName {
                name: "rust".into(),
            },
        )
        .unwrap();
    assert!(
        matches!(codec.decode(&tag_context, &tag_cursor).unwrap(), CursorBoundary::TagName { name } if name == "rust")
    );
}

#[test]
fn maximum_group_values_include_json_escaping_and_stay_under_codec_capacity() {
    let link = LinkId::new(Uuid::from_u128(44));
    let account = AccountId::new(Uuid::from_u128(45));
    let query = BreakdownQuery::parse(Some("group_by=user_agent")).unwrap();
    let context = CursorContext::breakdown(CursorActor::Account(account), link, &query);
    let mut group = "\0\"\\\u{0001}".repeat(1_170);
    while group.len() < MAX_CURSOR_GROUPING_VALUE_BYTES {
        group.push('\u{0002}');
    }
    group.truncate(MAX_CURSOR_GROUPING_VALUE_BYTES);
    assert_eq!(group.len(), MAX_CURSOR_GROUPING_VALUE_BYTES);
    let codec = CursorCodec::new([0x99; 32]);
    let cursor = codec
        .encode(
            &context,
            &CursorBoundary::BreakdownValue {
                value: Some(group.clone()),
            },
        )
        .unwrap();
    assert!(cursor.len() <= MAX_CURSOR_ENCODED_BYTES);
    assert!(
        matches!(codec.decode(&context, &cursor).unwrap(), CursorBoundary::BreakdownValue { value: Some(decoded) } if decoded == group)
    );
}

#[test]
fn grouping_boundary_values_are_checked_against_the_selected_group() {
    let codec = CursorCodec::new([0x55; 32]);
    let link = LinkId::new(Uuid::from_u128(51));
    let actor = CursorActor::Account(AccountId::new(Uuid::from_u128(52)));

    let method = BreakdownQuery::parse(Some("group_by=method")).unwrap();
    let method_context = CursorContext::breakdown(actor, link, &method);
    assert!(
        codec
            .encode(
                &method_context,
                &CursorBoundary::BreakdownValue {
                    value: Some("DELETE".into())
                },
            )
            .is_err()
    );
    assert!(
        codec
            .encode(
                &method_context,
                &CursorBoundary::BreakdownValue { value: None }
            )
            .is_err()
    );

    let day = BreakdownQuery::parse(Some("group_by=day")).unwrap();
    let day_context = CursorContext::breakdown(actor, link, &day);
    assert!(
        codec
            .encode(
                &day_context,
                &CursorBoundary::BreakdownValue {
                    value: Some("2026-02-30".into())
                },
            )
            .is_err()
    );
    assert!(
        codec
            .encode(
                &day_context,
                &CursorBoundary::BreakdownValue {
                    value: Some("2026-10-28".into())
                },
            )
            .is_ok()
    );

    let referrer = BreakdownQuery::parse(Some("group_by=referrer")).unwrap();
    let referrer_context = CursorContext::breakdown(actor, link, &referrer);
    assert!(
        codec
            .encode(
                &referrer_context,
                &CursorBoundary::BreakdownValue { value: None }
            )
            .is_ok()
    );

    let ip = BreakdownQuery::parse(Some("group_by=client_ip")).unwrap();
    let ip_context = CursorContext::breakdown(actor, link, &ip);
    assert!(
        codec
            .encode(
                &ip_context,
                &CursorBoundary::BreakdownValue {
                    value: Some("192.0.2.1".into())
                },
            )
            .is_ok()
    );
    assert!(
        codec
            .encode(
                &ip_context,
                &CursorBoundary::BreakdownValue {
                    value: Some("192.0.2.1/32".into())
                },
            )
            .is_err()
    );
}
