use std::time::Duration;

use sqlx::{PgPool, Postgres, Transaction};

use crate::db::error::{DatabaseFailure, OperationStage, classify};

use super::{event::VisitEvent, user_agent::Classification};

#[derive(Debug, Clone, Copy)]
pub(crate) enum PersistOutcome {
    Inserted,
    Duplicate,
    LinkDeleted,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PersistFailure {
    pub category: &'static str,
    pub retryable: bool,
}

pub(crate) async fn persist_once(
    pool: &PgPool,
    event: &VisitEvent,
    classification: &Classification,
    rollback_timeout: Duration,
) -> Result<PersistOutcome, PersistFailure> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| failure_from_database(classify(&error, OperationStage::BeforeCommit)))?;

    let live_link =
        sqlx::query_scalar::<_, String>("SELECT short_code FROM links WHERE id = $1 FOR UPDATE")
            .bind(event.link_id().as_uuid())
            .fetch_optional(&mut *transaction)
            .await;
    match live_link {
        Ok(Some(code)) if code == event.short_code().as_str() => {}
        Ok(Some(_)) => {
            return rollback_with(
                transaction,
                rollback_timeout,
                PersistFailure {
                    category: "analytics_link_identity_mismatch",
                    retryable: false,
                },
            )
            .await;
        }
        Ok(None) => {
            rollback_bounded(transaction, rollback_timeout).await?;
            return Ok(PersistOutcome::LinkDeleted);
        }
        Err(error) => {
            let failure = failure_from_database(classify(&error, OperationStage::BeforeCommit));
            return rollback_with(transaction, rollback_timeout, failure).await;
        }
    }

    let inserted = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO visits \
         (id, link_id, redirect_url, occurred_at, method, client_ip, referrer, user_agent, \
          device, browser, os) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (id) DO NOTHING RETURNING id",
    )
    .bind(event.id().as_uuid())
    .bind(event.link_id().as_uuid())
    .bind(event.redirect_url().as_str())
    .bind(event.occurred_at().as_utc())
    .bind(event.method().as_str())
    .bind(event.client_ip().host_network())
    .bind(event.referrer())
    .bind(event.user_agent())
    .bind(classification.device)
    .bind(classification.browser.as_str())
    .bind(classification.os)
    .fetch_optional(&mut *transaction)
    .await;
    let inserted = match inserted {
        Ok(inserted) => inserted,
        Err(error) => {
            let failure = failure_from_database(classify(&error, OperationStage::BeforeCommit));
            return rollback_with(transaction, rollback_timeout, failure).await;
        }
    };

    let outcome = if inserted.is_some() {
        // The link is already locked FOR UPDATE. The counter ceiling is
        // explicit; overflow rolls the insert and increment back together.
        let incremented = sqlx::query_scalar::<_, i64>(
            "UPDATE links SET visit_count = visit_count + 1 \
             WHERE id = $1 AND visit_count < 9223372036854775807 \
             RETURNING visit_count",
        )
        .bind(event.link_id().as_uuid())
        .fetch_optional(&mut *transaction)
        .await;
        match incremented {
            Ok(Some(_)) => PersistOutcome::Inserted,
            Ok(None) => {
                return rollback_with(
                    transaction,
                    rollback_timeout,
                    PersistFailure {
                        category: "analytics_counter_overflow",
                        retryable: false,
                    },
                )
                .await;
            }
            Err(error) => {
                let failure = failure_from_database(classify(&error, OperationStage::BeforeCommit));
                return rollback_with(transaction, rollback_timeout, failure).await;
            }
        }
    } else {
        PersistOutcome::Duplicate
    };

    transaction.commit().await.map_err(|error| {
        // A lost commit acknowledgement is safe to retry because the event
        // UUID is stable and a committed duplicate cannot increment twice.
        failure_from_database(classify(&error, OperationStage::Commit))
    })?;
    Ok(outcome)
}

async fn rollback_with<T>(
    transaction: Transaction<'static, Postgres>,
    timeout: Duration,
    failure: PersistFailure,
) -> Result<T, PersistFailure> {
    match rollback_bounded(transaction, timeout).await {
        Ok(()) => Err(failure),
        Err(rollback_failure) => Err(rollback_failure),
    }
}

async fn rollback_bounded(
    transaction: Transaction<'static, Postgres>,
    deadline: Duration,
) -> Result<(), PersistFailure> {
    match tokio::time::timeout(deadline, transaction.rollback()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(failure_from_database(classify(
            &error,
            OperationStage::BeforeCommit,
        ))),
        Err(_) => Err(PersistFailure {
            category: "analytics_rollback_deadline",
            retryable: false,
        }),
    }
}

fn failure_from_database(failure: DatabaseFailure) -> PersistFailure {
    match failure {
        DatabaseFailure::Unavailable(kind) => PersistFailure {
            category: DatabaseFailure::Unavailable(kind).safe_category(),
            retryable: true,
        },
        DatabaseFailure::CommitOutcomeUnknown => PersistFailure {
            category: failure.safe_category(),
            retryable: true,
        },
        other => PersistFailure {
            category: other.safe_category(),
            retryable: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, sync::Arc, time::Duration};

    use http::HeaderMap;
    use sqlx::PgPool;
    use tokio::{sync::Barrier, task::JoinSet, time::timeout};

    use crate::{client_ip::CanonicalIp, domain::ids::LinkId, links::resolve_active_link};

    use super::{PersistOutcome, persist_once};
    use crate::analytics::{
        event::{VisitEvent, VisitMethod},
        user_agent,
    };

    #[tokio::test]
    async fn concurrent_duplicate_event_ids_insert_once_and_overflow_rolls_back() {
        let database = crate::account_support::migrated_database().await;
        database
            .verify(|database| {
                Box::pin(async move {
                    let (link_id, _) = crate::account_support::seed_anonymous_link(
                        database.pool(),
                        "duplicate-event",
                    )
                    .await;
                    let snapshot = resolve_active_link(
                        database.pool(),
                        "duplicate-event",
                        Duration::from_secs(2),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    let event = Arc::new(VisitEvent::for_redirect(
                        &snapshot,
                        VisitMethod::Get,
                        CanonicalIp::new(Ipv4Addr::new(203, 0, 113, 1).into()),
                        &HeaderMap::new(),
                    ));
                    let classification = Arc::new(user_agent::unknown());
                    let barrier = Arc::new(Barrier::new(3));
                    let mut writers = JoinSet::new();
                    for _ in 0..2 {
                        let event = Arc::clone(&event);
                        let classification = Arc::clone(&classification);
                        let pool = database.pool().clone();
                        let barrier = Arc::clone(&barrier);
                        writers.spawn(async move {
                            barrier.wait().await;
                            persist_once(&pool, &event, &classification, Duration::from_secs(2))
                                .await
                        });
                    }
                    timeout(Duration::from_secs(3), barrier.wait())
                        .await
                        .expect("both duplicate event writers reach the barrier");
                    let mut inserted = 0;
                    let mut duplicate = 0;
                    for _ in 0..2 {
                        let result = timeout(Duration::from_secs(8), writers.join_next())
                            .await
                            .expect("duplicate event persistence finishes before deadline")
                            .unwrap()
                            .unwrap()
                            .unwrap();
                        match result {
                            PersistOutcome::Inserted => inserted += 1,
                            PersistOutcome::Duplicate => duplicate += 1,
                            PersistOutcome::LinkDeleted => panic!("fixture link remains live"),
                        }
                    }
                    assert_eq!((inserted, duplicate), (1, 1));
                    assert_eq!(visit_rows(database.pool(), link_id).await, 1);
                    assert_eq!(link_count(database.pool(), link_id).await, 1);

                    // A replay after a potentially unknown COMMIT observes the
                    // same UUID and snapshot and cannot increment a second time.
                    assert!(matches!(
                        persist_once(
                            database.pool(),
                            &event,
                            &classification,
                            Duration::from_secs(2),
                        )
                        .await,
                        Ok(PersistOutcome::Duplicate)
                    ));
                    assert_eq!(visit_rows(database.pool(), link_id).await, 1);
                    assert_eq!(link_count(database.pool(), link_id).await, 1);

                    sqlx::query("UPDATE links SET visit_count = 9223372036854775807 WHERE id = $1")
                        .bind(link_id.as_uuid())
                        .execute(database.pool())
                        .await
                        .unwrap();
                    let overflow_event = VisitEvent::for_redirect(
                        &snapshot,
                        VisitMethod::Head,
                        CanonicalIp::new(Ipv4Addr::new(203, 0, 113, 2).into()),
                        &HeaderMap::new(),
                    );
                    let failure = persist_once(
                        database.pool(),
                        &overflow_event,
                        &classification,
                        Duration::from_secs(2),
                    )
                    .await
                    .unwrap_err();
                    assert_eq!(failure.category, "analytics_counter_overflow");
                    assert_eq!(visit_rows(database.pool(), link_id).await, 1);
                    assert_eq!(link_count(database.pool(), link_id).await, i64::MAX);
                })
            })
            .await;
    }

    #[tokio::test]
    async fn cancellation_while_waiting_for_live_link_lock_leaves_no_partial_write() {
        let database = crate::account_support::migrated_database().await;
        database
            .verify(|database| {
                Box::pin(async move {
                    let (link_id, _) = crate::account_support::seed_anonymous_link(
                        database.pool(),
                        "cancel-event",
                    )
                    .await;
                    let snapshot = resolve_active_link(
                        database.pool(),
                        "cancel-event",
                        Duration::from_secs(2),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    let event = Arc::new(VisitEvent::for_redirect(
                        &snapshot,
                        VisitMethod::Get,
                        CanonicalIp::new(Ipv4Addr::new(203, 0, 113, 3).into()),
                        &HeaderMap::new(),
                    ));
                    let classification = Arc::new(user_agent::unknown());

                    let mut blocker = database.pool().begin().await.unwrap();
                    sqlx::query("SELECT id FROM links WHERE id = $1 FOR UPDATE")
                        .bind(link_id.as_uuid())
                        .fetch_one(&mut *blocker)
                        .await
                        .unwrap();
                    let mut work = JoinSet::new();
                    let pool = database.pool().clone();
                    let event_for_work = Arc::clone(&event);
                    let classification_for_work = Arc::clone(&classification);
                    work.spawn(async move {
                        persist_once(
                            &pool,
                            &event_for_work,
                            &classification_for_work,
                            Duration::from_secs(2),
                        )
                        .await
                    });
                    wait_for_worker_lock_wait(database.pool()).await;
                    work.abort_all();
                    timeout(Duration::from_secs(3), work.join_next())
                        .await
                        .expect("cancelled worker task is joined")
                        .unwrap()
                        .unwrap_err();
                    blocker.commit().await.unwrap();

                    assert_eq!(visit_rows(database.pool(), link_id).await, 0);
                    assert_eq!(link_count(database.pool(), link_id).await, 0);
                    assert!(matches!(
                        persist_once(
                            database.pool(),
                            &event,
                            &classification,
                            Duration::from_secs(2),
                        )
                        .await,
                        Ok(PersistOutcome::Inserted)
                    ));
                    assert_eq!(visit_rows(database.pool(), link_id).await, 1);
                    assert_eq!(link_count(database.pool(), link_id).await, 1);
                })
            })
            .await;
    }

    async fn visit_rows(pool: &PgPool, link_id: LinkId) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM visits WHERE link_id = $1")
            .bind(link_id.as_uuid())
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn link_count(pool: &PgPool, link_id: LinkId) -> i64 {
        sqlx::query_scalar("SELECT visit_count FROM links WHERE id = $1")
            .bind(link_id.as_uuid())
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn wait_for_worker_lock_wait(pool: &PgPool) {
        timeout(Duration::from_secs(5), async {
            loop {
                let count = sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() \
                     AND pid <> pg_backend_pid() AND wait_event_type = 'Lock' \
                     AND query LIKE '%SELECT short_code FROM links WHERE id = $1 FOR UPDATE%'",
                )
                .fetch_one(pool)
                .await
                .unwrap();
                if count > 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("worker lock wait is observed before cancellation");
    }
}
