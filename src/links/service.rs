use std::{fmt, sync::Arc, time::Duration};

use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    auth::{self, AccountMutationGuard, AccountRole, GuardFailure, LinkMutationGuard, Principal},
    config::{Config, PublicOrigin},
    db::{
        coordination,
        error::{DatabaseFailure, OperationStage, classify},
        records::LinkRecord,
    },
    domain::{
        ids::LinkId,
        secret::ManagementTokenSecret,
        short_code::ShortCode,
        text::{ComparisonText, TagSet},
        url::StoredUrl,
    },
    http::dto::{
        AnonymousLinkCreationDto, CollectionDto, CreateLinkRequest, LinkDto,
        ManagementTokenReplacementDto, OwnedLinkCreationDto, PatchLinkRequest, Presence,
        RecordConversionError,
    },
    http::query::{LinkListQuery, LinkSort},
    pagination::{CursorBoundary, CursorCodec, CursorContext},
    rate_limit::{ChargedAttempt, Creator},
};

use super::{allocation, list_repository, repository};

const ROLLBACK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct LinkService {
    pool: PgPool,
    origin: PublicOrigin,
    operation_timeout: Duration,
    statement_timeout: Duration,
    lock_timeout: Duration,
    candidates: Arc<dyn allocation::CandidateSource>,
}

pub enum CreatedLink {
    Anonymous(AnonymousLinkCreationDto),
    Owned(OwnedLinkCreationDto),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LinkFailure {
    AuthenticationRequired,
    InvalidCredentials,
    Forbidden,
    NotFound,
    InvalidRequest,
    ShortCodeUnavailable,
    CodeGenerationUnavailable,
    NotAnonymousLink,
    StorageUnavailable,
    CommitOutcomeUnknown,
    Internal,
}

/// Minimal immutable destination snapshot for the Part 07 redirect boundary.
/// It contains no account or credential data and is never cached.
pub struct ActiveLinkSnapshot {
    link_id: LinkId,
    short_code: ShortCode,
    destination_url: StoredUrl,
}

impl ActiveLinkSnapshot {
    pub const fn link_id(&self) -> LinkId {
        self.link_id
    }

    pub fn short_code(&self) -> &ShortCode {
        &self.short_code
    }

    pub fn destination_url(&self) -> &StoredUrl {
        &self.destination_url
    }
}

impl LinkService {
    pub(crate) fn from_config(pool: PgPool, config: &Config) -> Self {
        Self {
            pool,
            origin: config.public_origin.clone(),
            operation_timeout: config.link_operation_timeout,
            statement_timeout: config.database.statement_timeout,
            lock_timeout: config.database.lock_timeout,
            candidates: Arc::new(allocation::OsCandidates),
        }
    }

    /// Takes the successful Part 05 charge capability by ownership. No identity
    /// or previously observed role can be supplied independently.
    pub async fn create(
        &self,
        attempt: ChargedAttempt,
        input: CreateLinkRequest,
    ) -> Result<CreatedLink, LinkFailure> {
        self.with_deadline(self.create_inner(attempt.into_creator(), input))
            .await
    }

    /// List a bounded page using one PostgreSQL statement snapshot for the
    /// candidate page, owner state, canonical tags and cursor ordering values.
    pub async fn list(
        &self,
        codec: &CursorCodec,
        actor_id: crate::domain::ids::AccountId,
        query: &LinkListQuery,
    ) -> Result<CollectionDto<LinkDto>, LinkFailure> {
        self.with_deadline(async {
            let context = CursorContext::links(actor_id, query);
            let boundary = query
                .page()
                .cursor()
                .map(|cursor| codec.decode(&context, cursor))
                .transpose()
                .map_err(|_| LinkFailure::InvalidRequest)?;
            let mut rows =
                list_repository::list(&self.pool, actor_id.as_uuid(), query, boundary.as_ref())
                    .await
                    .map_err(map_sql)?;
            let limit = usize::from(query.page().limit());
            let has_more = rows.len() > limit;
            rows.truncate(limit);

            let mut items = Vec::with_capacity(rows.len());
            let mut next_cursor = None;
            let page_len = rows.len();
            for (index, snapshot) in rows.into_iter().enumerate() {
                let (record, tags, owner_enabled) = snapshot.into_parts();
                let item = super::representation::build(&record, tags, &self.origin, owner_enabled)
                    .map_err(map_conversion)?;
                if has_more && index + 1 == page_len {
                    let boundary = link_boundary(query.sort(), &record)?;
                    next_cursor = Some(
                        codec
                            .encode(&context, &boundary)
                            .map_err(|_| LinkFailure::Internal)?,
                    );
                }
                items.push(item);
            }
            Ok(CollectionDto::new(items, next_cursor))
        })
        .await
    }

    async fn create_inner(
        &self,
        creator: Creator,
        input: CreateLinkRequest,
    ) -> Result<CreatedLink, LinkFailure> {
        let principal = creator.principal();
        let anonymous = principal.is_none();
        let token = if anonymous {
            Some(ManagementTokenSecret::generate().map_err(|_| LinkFailure::Internal)?)
        } else {
            None
        };

        if let Some(principal) = principal {
            let mut guard = auth::begin_account_mutation(&self.pool, principal, self.lock_timeout)
                .await
                .map_err(map_guard)?;
            if let Err(failure) = restore_timeouts(
                guard.connection(),
                self.statement_timeout,
                self.lock_timeout,
            )
            .await
            {
                return rollback_error(WriteTransaction::Account(guard), failure).await;
            }
            let owner_id = guard.caller().id().as_uuid();
            let mut transaction = WriteTransaction::Account(guard);
            let result = self
                .create_in_transaction(
                    transaction.connection(),
                    Some(owner_id),
                    &input,
                    token.as_ref(),
                )
                .await;
            return match result {
                Ok(dto) => {
                    transaction.commit().await?;
                    Ok(CreatedLink::Owned(OwnedLinkCreationDto::new(dto)))
                }
                Err(failure) => rollback_error(transaction, failure).await,
            };
        }

        let mut transaction = coordination::begin_read_committed(&self.pool)
            .await
            .map_err(map_coordination)?;
        if let Err(failure) =
            restore_timeouts(&mut transaction, self.statement_timeout, self.lock_timeout).await
        {
            return rollback_error(WriteTransaction::Anonymous(transaction), failure).await;
        }
        let mut transaction = WriteTransaction::Anonymous(transaction);
        let result = self
            .create_in_transaction(transaction.connection(), None, &input, token.as_ref())
            .await;
        match result {
            Ok(dto) => {
                transaction.commit().await?;
                let secret = token.ok_or(LinkFailure::Internal)?;
                Ok(CreatedLink::Anonymous(AnonymousLinkCreationDto::new(
                    dto, secret,
                )))
            }
            Err(failure) => rollback_error(transaction, failure).await,
        }
    }

    async fn create_in_transaction(
        &self,
        connection: &mut PgConnection,
        owner_id: Option<Uuid>,
        input: &CreateLinkRequest,
        token: Option<&ManagementTokenSecret>,
    ) -> Result<LinkDto, LinkFailure> {
        let code = match input.short_code() {
            Some(code) => {
                if !allocation::reserve_custom(connection, code)
                    .await
                    .map_err(map_sql)?
                {
                    return Err(LinkFailure::ShortCodeUnavailable);
                }
                ShortCode::parse_custom(code.as_str()).map_err(|_| LinkFailure::Internal)?
            }
            None => allocation::reserve_generated(connection, self.candidates.as_ref())
                .await
                .map_err(|failure| match failure {
                    allocation::AllocationFailure::CodeGenerationUnavailable => {
                        LinkFailure::CodeGenerationUnavailable
                    }
                    allocation::AllocationFailure::Database(error) => map_sql(error),
                })?,
        };
        let destination = input.destination_url();
        let destination_search = ComparisonText::from_persisted(destination.as_str());
        let title = input.title().map(crate::domain::text::Title::as_str);
        let title_search = title.map(ComparisonText::from_persisted);
        let digest = token.map(ManagementTokenSecret::digest);
        let link_id = new_link_id()?;
        repository::insert_link(
            connection,
            repository::NewLink {
                id: link_id.as_uuid(),
                code: code.as_str(),
                owner_id,
                destination_url: destination.as_str(),
                destination_search: destination_search.as_str(),
                title,
                title_search: title_search.as_ref().map(ComparisonText::as_str),
                token_digest: digest.as_ref().map(|value| value.as_bytes()),
            },
        )
        .await
        .map_err(map_sql)?;
        repository::replace_tags(connection, link_id, &tag_names(input.tags()))
            .await
            .map_err(map_sql)?;
        let snapshot = repository::find_snapshot_by_id(connection, link_id)
            .await
            .map_err(map_sql)?
            .ok_or(LinkFailure::Internal)?;
        snapshot_dto(snapshot, &self.origin)
    }

    /// Metadata, owner state and tag names are read from one PostgreSQL statement
    /// snapshot, so a response cannot combine separate committed tag versions.
    pub async fn get(&self, principal: &Principal, code: &str) -> Result<LinkDto, LinkFailure> {
        self.with_deadline(async {
            let snapshot = repository::find_snapshot_by_code(&self.pool, code)
                .await
                .map_err(map_sql)?
                .ok_or(LinkFailure::NotFound)?;
            let (record, tags, owner_enabled) = snapshot.into_parts();
            authorize_snapshot(&record, owner_enabled, principal)?;
            LinkDto::from_record(&record, tags, &self.origin, owner_enabled).map_err(map_conversion)
        })
        .await
    }

    /// Establish current object authority before the HTTP layer reads an
    /// operation body. The guard is released before body I/O; PATCH/DELETE
    /// repeat the full check under their mutation transaction afterward.
    pub(crate) async fn authorize_management(
        &self,
        principal: &Principal,
        code: &str,
    ) -> Result<(), LinkFailure> {
        self.with_deadline(async {
            let (transaction, _, _, _) = self.lock_authorized(principal, code).await?;
            transaction.commit().await
        })
        .await
    }

    /// Check rotation scope, current credential and target kind before the
    /// documented bodyless request is inspected. The actual replacement still
    /// repeats all checks and retains its row lock through commit.
    pub(crate) async fn authorize_token_replacement(
        &self,
        principal: &Principal,
        code: &str,
    ) -> Result<(), LinkFailure> {
        self.with_deadline(async {
            match principal {
                Principal::Account(_) => {
                    let mut guard =
                        auth::begin_account_mutation(&self.pool, principal, self.lock_timeout)
                            .await
                            .map_err(map_guard)?;
                    if let Err(failure) = restore_timeouts(
                        guard.connection(),
                        self.statement_timeout,
                        self.lock_timeout,
                    )
                    .await
                    {
                        return rollback_error(WriteTransaction::Account(guard), failure).await;
                    }
                    if guard.require_admin().is_err() {
                        return rollback_error(
                            WriteTransaction::Account(guard),
                            LinkFailure::Forbidden,
                        )
                        .await;
                    }
                    let record = match repository::lock_by_code(guard.connection(), code).await {
                        Ok(Some(record)) => record,
                        Ok(None) => {
                            return rollback_error(
                                WriteTransaction::Account(guard),
                                LinkFailure::NotFound,
                            )
                            .await;
                        }
                        Err(error) => {
                            return rollback_error(
                                WriteTransaction::Account(guard),
                                map_sql(error),
                            )
                            .await;
                        }
                    };
                    let snapshot = match repository::find_snapshot_by_id(
                        guard.connection(),
                        LinkId::new(record.id),
                    )
                    .await
                    {
                        Ok(Some(snapshot)) => snapshot,
                        Ok(None) => {
                            return rollback_error(
                                WriteTransaction::Account(guard),
                                LinkFailure::Internal,
                            )
                            .await;
                        }
                        Err(error) => {
                            return rollback_error(
                                WriteTransaction::Account(guard),
                                map_sql(error),
                            )
                            .await;
                        }
                    };
                    let (record, tags, owner_enabled) = snapshot.into_parts();
                    if record.owner_id.is_some() {
                        return rollback_error(
                            WriteTransaction::Account(guard),
                            LinkFailure::NotAnonymousLink,
                        )
                        .await;
                    }
                    if let Err(failure) =
                        LinkDto::from_record(&record, tags, &self.origin, owner_enabled)
                            .map_err(map_conversion)
                    {
                        return rollback_error(WriteTransaction::Account(guard), failure).await;
                    }
                    guard.commit().await.map_err(map_guard)
                }
                Principal::ManagementToken(token) => {
                    let guard = auth::begin_anonymous_link_mutation(
                        &self.pool,
                        principal,
                        token.link_id(),
                        self.lock_timeout,
                    )
                    .await
                    .map_err(map_guard)?;
                    let mut transaction = WriteTransaction::Token(guard);
                    if let Err(failure) = restore_timeouts(
                        transaction.connection(),
                        self.statement_timeout,
                        self.lock_timeout,
                    )
                    .await
                    {
                        return rollback_error(transaction, failure).await;
                    }
                    let snapshot = match repository::find_snapshot_by_id(
                        transaction.connection(),
                        token.link_id(),
                    )
                    .await
                    {
                        Ok(Some(snapshot)) => snapshot,
                        Ok(None) => {
                            return rollback_error(transaction, LinkFailure::NotFound).await;
                        }
                        Err(error) => return rollback_error(transaction, map_sql(error)).await,
                    };
                    let (record, tags, owner_enabled) = snapshot.into_parts();
                    if record.short_code != code {
                        return rollback_error(transaction, LinkFailure::NotFound).await;
                    }
                    if let Err(failure) =
                        LinkDto::from_record(&record, tags, &self.origin, owner_enabled)
                            .map_err(map_conversion)
                    {
                        return rollback_error(transaction, failure).await;
                    }
                    transaction.commit().await
                }
            }
        })
        .await
    }

    pub async fn patch(
        &self,
        principal: &Principal,
        code: &str,
        patch: PatchLinkRequest,
    ) -> Result<LinkDto, LinkFailure> {
        self.with_deadline(async {
            let (mut transaction, record, tag_set, owner_enabled) =
                self.lock_authorized(principal, code).await?;
            let current_dto =
                LinkDto::from_record(&record, tag_names(&tag_set), &self.origin, owner_enabled)
                    .map_err(map_conversion)?;
            let destination = match patch.destination_url() {
                Presence::Omitted => record.destination_url.as_str(),
                Presence::Value(value) => value.as_str(),
                Presence::Null => {
                    return rollback_error(transaction, LinkFailure::InvalidRequest).await;
                }
            };
            let destination_search = ComparisonText::from_persisted(destination);
            let title = match patch.title() {
                Presence::Omitted => record.title.as_deref(),
                Presence::Null => None,
                Presence::Value(value) => Some(value.as_str()),
            };
            let title_search = title.map(ComparisonText::from_persisted);
            let existing_names = tag_names(&tag_set);
            let names = match patch.tags() {
                Presence::Omitted => existing_names.clone(),
                Presence::Value(value) => tag_names(value),
                Presence::Null => {
                    return rollback_error(transaction, LinkFailure::InvalidRequest).await;
                }
            };
            let enabled = match patch.enabled() {
                Presence::Omitted => record.enabled,
                Presence::Value(value) => *value,
                Presence::Null => {
                    return rollback_error(transaction, LinkFailure::InvalidRequest).await;
                }
            };
            let changed = record.destination_url != destination
                || record.title.as_deref() != title
                || record.enabled != enabled
                || existing_names != names;
            if !changed {
                transaction.commit().await?;
                return Ok(current_dto);
            }

            let updated = match repository::update_metadata(
                transaction.connection(),
                &record,
                destination,
                destination_search.as_str(),
                title,
                title_search.as_ref().map(ComparisonText::as_str),
                enabled,
            )
            .await
            {
                Ok(updated) => updated,
                Err(error) => return rollback_error(transaction, map_sql(error)).await,
            };
            if let Err(error) =
                repository::replace_tags(transaction.connection(), LinkId::new(updated.id), &names)
                    .await
            {
                return rollback_error(transaction, map_sql(error)).await;
            }
            let snapshot = match repository::find_snapshot_by_id(
                transaction.connection(),
                LinkId::new(updated.id),
            )
            .await
            {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) => return rollback_error(transaction, LinkFailure::Internal).await,
                Err(error) => return rollback_error(transaction, map_sql(error)).await,
            };
            let dto = match snapshot_dto(snapshot, &self.origin) {
                Ok(dto) => dto,
                Err(failure) => return rollback_error(transaction, failure).await,
            };
            transaction.commit().await?;
            Ok(dto)
        })
        .await
    }

    pub async fn delete(&self, principal: &Principal, code: &str) -> Result<(), LinkFailure> {
        self.with_deadline(async {
            let (mut transaction, record, tags, owner_enabled) =
                self.lock_authorized(principal, code).await?;
            if let Err(failure) =
                LinkDto::from_record(&record, tag_names(&tags), &self.origin, owner_enabled)
                    .map_err(map_conversion)
            {
                return rollback_error(transaction, failure).await;
            }
            let deleted =
                match repository::delete(transaction.connection(), LinkId::new(record.id)).await {
                    Ok(deleted) => deleted,
                    Err(error) => return rollback_error(transaction, map_sql(error)).await,
                };
            if deleted != 1 {
                return rollback_error(transaction, LinkFailure::NotFound).await;
            }
            transaction.commit().await
        })
        .await
    }

    pub async fn rotate_token(
        &self,
        principal: &Principal,
        code: &str,
    ) -> Result<ManagementTokenReplacementDto, LinkFailure> {
        self.with_deadline(async {
            match principal {
                Principal::Account(_) => {
                    let mut guard =
                        auth::begin_account_mutation(&self.pool, principal, self.lock_timeout)
                            .await
                            .map_err(map_guard)?;
                    if let Err(failure) = restore_timeouts(
                        guard.connection(),
                        self.statement_timeout,
                        self.lock_timeout,
                    )
                    .await
                    {
                        return rollback_error(WriteTransaction::Account(guard), failure).await;
                    }
                    if guard.require_admin().is_err() {
                        return rollback_error(
                            WriteTransaction::Account(guard),
                            LinkFailure::Forbidden,
                        )
                        .await;
                    }
                    let record = match repository::lock_by_code(guard.connection(), code).await {
                        Ok(Some(record)) => record,
                        Ok(None) => {
                            return rollback_error(
                                WriteTransaction::Account(guard),
                                LinkFailure::NotFound,
                            )
                            .await;
                        }
                        Err(error) => {
                            return rollback_error(
                                WriteTransaction::Account(guard),
                                map_sql(error),
                            )
                            .await;
                        }
                    };
                    let snapshot = match repository::find_snapshot_by_id(
                        guard.connection(),
                        LinkId::new(record.id),
                    )
                    .await
                    {
                        Ok(Some(snapshot)) => snapshot,
                        Ok(None) => {
                            return rollback_error(
                                WriteTransaction::Account(guard),
                                LinkFailure::Internal,
                            )
                            .await;
                        }
                        Err(error) => {
                            return rollback_error(
                                WriteTransaction::Account(guard),
                                map_sql(error),
                            )
                            .await;
                        }
                    };
                    let (checked, tags, owner_enabled) = snapshot.into_parts();
                    if checked.owner_id.is_some() {
                        return rollback_error(
                            WriteTransaction::Account(guard),
                            LinkFailure::NotAnonymousLink,
                        )
                        .await;
                    }
                    if let Err(failure) =
                        LinkDto::from_record(&checked, tags, &self.origin, owner_enabled)
                            .map_err(map_conversion)
                    {
                        return rollback_error(WriteTransaction::Account(guard), failure).await;
                    }
                    let secret = match ManagementTokenSecret::generate() {
                        Ok(secret) => secret,
                        Err(_) => {
                            return rollback_error(
                                WriteTransaction::Account(guard),
                                LinkFailure::Internal,
                            )
                            .await;
                        }
                    };
                    match repository::replace_token_digest(
                        guard.connection(),
                        LinkId::new(record.id),
                        secret.digest().as_bytes(),
                    )
                    .await
                    {
                        Ok(1) => {}
                        Ok(_) => {
                            return rollback_error(
                                WriteTransaction::Account(guard),
                                LinkFailure::NotFound,
                            )
                            .await;
                        }
                        Err(error) => {
                            return rollback_error(
                                WriteTransaction::Account(guard),
                                map_sql(error),
                            )
                            .await;
                        }
                    }
                    guard.commit().await.map_err(map_guard)?;
                    Ok(ManagementTokenReplacementDto::new(secret))
                }
                Principal::ManagementToken(token) => {
                    let guard = auth::begin_anonymous_link_mutation(
                        &self.pool,
                        principal,
                        token.link_id(),
                        self.lock_timeout,
                    )
                    .await
                    .map_err(map_guard)?;
                    let mut transaction = WriteTransaction::Token(guard);
                    if let Err(failure) = restore_timeouts(
                        transaction.connection(),
                        self.statement_timeout,
                        self.lock_timeout,
                    )
                    .await
                    {
                        return rollback_error(transaction, failure).await;
                    }
                    let snapshot = match repository::find_snapshot_by_id(
                        transaction.connection(),
                        token.link_id(),
                    )
                    .await
                    {
                        Ok(Some(snapshot)) => snapshot,
                        Ok(None) => {
                            return rollback_error(transaction, LinkFailure::NotFound).await;
                        }
                        Err(error) => return rollback_error(transaction, map_sql(error)).await,
                    };
                    let (record, tags, owner_enabled) = snapshot.into_parts();
                    if record.short_code != code {
                        return rollback_error(transaction, LinkFailure::NotFound).await;
                    }
                    if let Err(failure) =
                        LinkDto::from_record(&record, tags, &self.origin, owner_enabled)
                            .map_err(map_conversion)
                    {
                        return rollback_error(transaction, failure).await;
                    }
                    let secret = match ManagementTokenSecret::generate() {
                        Ok(secret) => secret,
                        Err(_) => return rollback_error(transaction, LinkFailure::Internal).await,
                    };
                    match repository::replace_token_digest(
                        transaction.connection(),
                        token.link_id(),
                        secret.digest().as_bytes(),
                    )
                    .await
                    {
                        Ok(1) => {}
                        Ok(_) => return rollback_error(transaction, LinkFailure::NotFound).await,
                        Err(error) => return rollback_error(transaction, map_sql(error)).await,
                    }
                    transaction.commit().await?;
                    Ok(ManagementTokenReplacementDto::new(secret))
                }
            }
        })
        .await
    }

    async fn lock_authorized(
        &self,
        principal: &Principal,
        code: &str,
    ) -> Result<(WriteTransaction, LinkRecord, TagSet, Option<bool>), LinkFailure> {
        let mut transaction = match principal {
            Principal::Account(_) => {
                let mut guard =
                    auth::begin_account_mutation(&self.pool, principal, self.lock_timeout)
                        .await
                        .map_err(map_guard)?;
                if let Err(failure) = restore_timeouts(
                    guard.connection(),
                    self.statement_timeout,
                    self.lock_timeout,
                )
                .await
                {
                    return rollback_error(WriteTransaction::Account(guard), failure).await;
                }
                WriteTransaction::Account(guard)
            }
            Principal::ManagementToken(token) => {
                let guard = auth::begin_anonymous_link_mutation(
                    &self.pool,
                    principal,
                    token.link_id(),
                    self.lock_timeout,
                )
                .await
                .map_err(map_guard)?;
                let mut transaction = WriteTransaction::Token(guard);
                if let Err(failure) = restore_timeouts(
                    transaction.connection(),
                    self.statement_timeout,
                    self.lock_timeout,
                )
                .await
                {
                    return rollback_error(transaction, failure).await;
                }
                let snapshot = match repository::find_snapshot_by_id(
                    transaction.connection(),
                    token.link_id(),
                )
                .await
                {
                    Ok(Some(snapshot)) => snapshot,
                    Ok(None) => return rollback_error(transaction, LinkFailure::NotFound).await,
                    Err(error) => return rollback_error(transaction, map_sql(error)).await,
                };
                let (record, tags, owner_enabled) = snapshot.into_parts();
                if record.short_code != code {
                    return rollback_error(transaction, LinkFailure::NotFound).await;
                }
                if let Err(failure) = authorize_current(&record, transaction.caller(), principal) {
                    return rollback_error(transaction, failure).await;
                }
                let tag_set = match TagSet::from_database(tags) {
                    Ok(tags) => tags,
                    Err(_) => return rollback_error(transaction, LinkFailure::Internal).await,
                };
                if let Err(failure) =
                    LinkDto::from_record(&record, tag_names(&tag_set), &self.origin, owner_enabled)
                        .map_err(map_conversion)
                {
                    return rollback_error(transaction, failure).await;
                }
                return Ok((transaction, record, tag_set, owner_enabled));
            }
        };

        let record = match &mut transaction {
            WriteTransaction::Account(guard) => {
                match repository::lock_by_code(guard.connection(), code).await {
                    Ok(Some(record)) => record,
                    Ok(None) => return rollback_error(transaction, LinkFailure::NotFound).await,
                    Err(error) => return rollback_error(transaction, map_sql(error)).await,
                }
            }
            WriteTransaction::Token(_) => unreachable!("token branch returns its locked object"),
            WriteTransaction::Anonymous(_) => {
                unreachable!("link object mutations have an authority guard")
            }
        };

        let snapshot = match &mut transaction {
            WriteTransaction::Account(_) => {
                repository::find_snapshot_by_id(transaction.connection(), LinkId::new(record.id))
                    .await
            }
            WriteTransaction::Token(_) | WriteTransaction::Anonymous(_) => unreachable!(),
        };
        let snapshot = match snapshot {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return rollback_error(transaction, LinkFailure::Internal).await,
            Err(error) => return rollback_error(transaction, map_sql(error)).await,
        };
        let (record, tags, owner_enabled) = snapshot.into_parts();
        if let Err(failure) = authorize_current(&record, transaction.caller(), principal) {
            return rollback_error(transaction, failure).await;
        }
        let tag_set = match TagSet::from_database(tags) {
            Ok(tags) => tags,
            Err(_) => return rollback_error(transaction, LinkFailure::Internal).await,
        };
        if let Err(failure) =
            LinkDto::from_record(&record, tag_names(&tag_set), &self.origin, owner_enabled)
                .map_err(map_conversion)
        {
            return rollback_error(transaction, failure).await;
        }
        Ok((transaction, record, tag_set, owner_enabled))
    }

    async fn with_deadline<T>(
        &self,
        operation: impl std::future::Future<Output = Result<T, LinkFailure>>,
    ) -> Result<T, LinkFailure> {
        match tokio::time::timeout(self.operation_timeout, operation).await {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    category = "link_operation_deadline",
                    "link operation exceeded its deadline; transaction ownership was dropped"
                );
                Err(LinkFailure::StorageUnavailable)
            }
        }
    }
}

/// Fresh, one-statement primary-database active resolution for Part 07.
pub async fn resolve_active_link(
    pool: &PgPool,
    code: &str,
    deadline: Duration,
) -> Result<Option<ActiveLinkSnapshot>, LinkFailure> {
    let result = tokio::time::timeout(deadline, repository::active_resolution(pool, code))
        .await
        .map_err(|_| LinkFailure::StorageUnavailable)?
        .map_err(map_sql)?;
    let Some(row) = result else {
        return Ok(None);
    };
    use sqlx::Row;
    let owner_id: Option<Uuid> = row.try_get("owner_id").map_err(|_| LinkFailure::Internal)?;
    let owner_row_id: Option<Uuid> = row
        .try_get("owner_row_id")
        .map_err(|_| LinkFailure::Internal)?;
    let owner_enabled: Option<bool> = row
        .try_get("owner_enabled")
        .map_err(|_| LinkFailure::Internal)?;
    let enabled: bool = row.try_get("enabled").map_err(|_| LinkFailure::Internal)?;
    let token_digest_absent: bool = row
        .try_get("token_digest_absent")
        .map_err(|_| LinkFailure::Internal)?;
    let token_digest_valid: bool = row
        .try_get("token_digest_valid")
        .map_err(|_| LinkFailure::Internal)?;
    match (
        owner_id,
        owner_row_id,
        owner_enabled,
        token_digest_absent,
        token_digest_valid,
    ) {
        (None, None, None, false, true) => {}
        (Some(id), Some(owner_row), Some(_), true, false) if id == owner_row => {}
        _ => return Err(LinkFailure::Internal),
    }
    let id: Uuid = row.try_get("id").map_err(|_| LinkFailure::Internal)?;
    let code: String = row
        .try_get("short_code")
        .map_err(|_| LinkFailure::Internal)?;
    let destination: String = row
        .try_get("destination_url")
        .map_err(|_| LinkFailure::Internal)?;
    let short_code = ShortCode::parse_custom(&code).map_err(|_| LinkFailure::Internal)?;
    let destination_url =
        StoredUrl::from_database(destination).map_err(|_| LinkFailure::Internal)?;
    if !enabled || owner_enabled == Some(false) {
        return Ok(None);
    }
    Ok(Some(ActiveLinkSnapshot {
        link_id: LinkId::new(id),
        short_code,
        destination_url,
    }))
}

fn snapshot_dto(
    snapshot: repository::LinkSnapshotRecord,
    origin: &PublicOrigin,
) -> Result<LinkDto, LinkFailure> {
    let (record, tags, owner_enabled) = snapshot.into_parts();
    LinkDto::from_record(&record, tags, origin, owner_enabled).map_err(map_conversion)
}

fn link_boundary(sort: LinkSort, record: &LinkRecord) -> Result<CursorBoundary, LinkFailure> {
    let id = LinkId::new(record.id);
    match sort {
        LinkSort::CreatedAt => Ok(CursorBoundary::LinkCreatedAt {
            at: list_repository::boundary_timestamp(record.created_at)
                .map_err(|_| LinkFailure::Internal)?,
            id,
        }),
        LinkSort::Title => Ok(CursorBoundary::LinkTitle {
            normalized_title: record.title_search.clone(),
            id,
        }),
        LinkSort::VisitCount => Ok(CursorBoundary::LinkVisitCount {
            count: record.visit_count,
            id,
        }),
    }
}

fn authorize_snapshot(
    record: &LinkRecord,
    owner_enabled: Option<bool>,
    principal: &Principal,
) -> Result<(), LinkFailure> {
    match principal {
        Principal::Account(account) if account.role() == AccountRole::Admin => Ok(()),
        Principal::Account(account)
            if record.owner_id == Some(account.id().as_uuid()) && owner_enabled == Some(true) =>
        {
            Ok(())
        }
        Principal::ManagementToken(token)
            if record.owner_id.is_none()
                && token.link_id() == LinkId::new(record.id)
                && record.management_token_digest.as_deref()
                    == Some(token.digest().as_bytes().as_slice()) =>
        {
            Ok(())
        }
        _ => Err(LinkFailure::NotFound),
    }
}

fn authorize_current(
    record: &LinkRecord,
    caller: Option<&crate::auth::AccountPrincipal>,
    principal: &Principal,
) -> Result<(), LinkFailure> {
    match principal {
        Principal::Account(_) => {
            let caller = caller.ok_or(LinkFailure::Internal)?;
            if caller.role() == AccountRole::Admin || record.owner_id == Some(caller.id().as_uuid())
            {
                Ok(())
            } else {
                Err(LinkFailure::NotFound)
            }
        }
        Principal::ManagementToken(token)
            if token.link_id() == LinkId::new(record.id)
                && record.owner_id.is_none()
                && record.management_token_digest.as_deref()
                    == Some(token.digest().as_bytes().as_slice()) =>
        {
            Ok(())
        }
        Principal::ManagementToken(_) => Err(LinkFailure::NotFound),
    }
}

fn tag_names(tags: &TagSet) -> Vec<String> {
    tags.as_slice()
        .iter()
        .map(|tag| tag.as_str().to_owned())
        .collect()
}

fn new_link_id() -> Result<LinkId, LinkFailure> {
    new_link_id_with(|bytes| getrandom::fill(bytes).map_err(|_| ()))
}

fn new_link_id_with(
    fill_random: impl FnOnce(&mut [u8]) -> Result<(), ()>,
) -> Result<LinkId, LinkFailure> {
    let mut random = [0_u8; 16];
    fill_random(&mut random).map_err(|_| LinkFailure::Internal)?;
    Ok(LinkId::new(
        uuid::Builder::from_random_bytes(random).into_uuid(),
    ))
}

async fn restore_timeouts(
    connection: &mut PgConnection,
    statement_timeout: Duration,
    lock_timeout: Duration,
) -> Result<(), LinkFailure> {
    let statement_ms = statement_timeout.as_millis().max(1).to_string();
    let lock_ms = lock_timeout.as_millis().max(1).to_string();
    sqlx::query(
        "SELECT set_config('statement_timeout', $1, true), \
                set_config('lock_timeout', $2, true)",
    )
    .bind(statement_ms)
    .bind(lock_ms)
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(map_sql)
}

enum WriteTransaction {
    Account(AccountMutationGuard),
    Token(LinkMutationGuard),
    Anonymous(Transaction<'static, Postgres>),
}

impl WriteTransaction {
    fn connection(&mut self) -> &mut PgConnection {
        match self {
            Self::Account(guard) => guard.connection(),
            Self::Token(guard) => guard.connection(),
            Self::Anonymous(transaction) => transaction,
        }
    }

    fn caller(&self) -> Option<&crate::auth::AccountPrincipal> {
        match self {
            Self::Account(guard) => Some(guard.caller()),
            Self::Token(_) | Self::Anonymous(_) => None,
        }
    }

    async fn commit(self) -> Result<(), LinkFailure> {
        match self {
            Self::Account(guard) => guard.commit().await.map_err(map_guard),
            Self::Token(guard) => guard.commit().await.map_err(map_guard),
            Self::Anonymous(transaction) => transaction
                .commit()
                .await
                .map_err(|error| map_database(classify(&error, OperationStage::Commit))),
        }
    }

    async fn rollback(self) -> Result<(), LinkFailure> {
        match self {
            Self::Account(guard) => guard.rollback().await.map_err(map_guard),
            Self::Token(guard) => guard.rollback().await.map_err(map_guard),
            Self::Anonymous(transaction) => transaction.rollback().await.map_err(map_sql),
        }
    }
}

async fn rollback_error<T>(
    transaction: WriteTransaction,
    failure: LinkFailure,
) -> Result<T, LinkFailure> {
    if !matches!(
        tokio::time::timeout(ROLLBACK_TIMEOUT, transaction.rollback()).await,
        Ok(Ok(()))
    ) {
        tracing::warn!(
            category = "link_transaction_rollback_failed",
            "link rollback exceeded its bound or failed"
        );
        return Err(LinkFailure::StorageUnavailable);
    }
    Err(failure)
}

fn map_guard(failure: GuardFailure) -> LinkFailure {
    match failure {
        GuardFailure::InvalidCredentials => LinkFailure::InvalidCredentials,
        GuardFailure::Forbidden => LinkFailure::Forbidden,
        GuardFailure::NotFound => LinkFailure::NotFound,
        GuardFailure::DeadlineExceeded => LinkFailure::StorageUnavailable,
        GuardFailure::Database(failure) => map_database(failure),
        GuardFailure::Internal => LinkFailure::Internal,
    }
}

fn map_coordination(failure: coordination::CoordinationError) -> LinkFailure {
    match failure {
        coordination::CoordinationError::DeadlineExceeded => LinkFailure::StorageUnavailable,
        coordination::CoordinationError::Database(failure) => map_database(failure),
    }
}

fn map_sql(error: sqlx::Error) -> LinkFailure {
    map_database(classify(&error, OperationStage::BeforeCommit))
}

fn map_database(failure: DatabaseFailure) -> LinkFailure {
    match failure {
        DatabaseFailure::Unavailable(_) => LinkFailure::StorageUnavailable,
        DatabaseFailure::CommitOutcomeUnknown => LinkFailure::CommitOutcomeUnknown,
        DatabaseFailure::Conflict(_) => LinkFailure::Internal,
        DatabaseFailure::NotFound | DatabaseFailure::Internal => LinkFailure::Internal,
    }
}

fn map_conversion(_: RecordConversionError) -> LinkFailure {
    LinkFailure::Internal
}

impl LinkFailure {
    pub const fn safe_category(self) -> &'static str {
        match self {
            Self::AuthenticationRequired => "authentication_required",
            Self::InvalidCredentials => "invalid_credentials",
            Self::Forbidden => "forbidden",
            Self::NotFound => "record_absent",
            Self::InvalidRequest => "invalid_request",
            Self::ShortCodeUnavailable => "short_code_conflict",
            Self::CodeGenerationUnavailable => "code_generation_unavailable",
            Self::NotAnonymousLink => "not_anonymous_link",
            Self::StorageUnavailable => "link_storage_unavailable",
            Self::CommitOutcomeUnknown => "link_commit_outcome_unknown",
            Self::Internal => "link_persistence_failure",
        }
    }
}

impl fmt::Debug for LinkFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LinkFailure")
    }
}

impl fmt::Debug for CreatedLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CreatedLink([REDACTED ONE-TIME SECRET])")
    }
}

impl fmt::Debug for ActiveLinkSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ActiveLinkSnapshot([DESTINATION REDACTED])")
    }
}

#[cfg(test)]
mod tests;
