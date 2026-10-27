CREATE TABLE system_state (
    singleton boolean NOT NULL DEFAULT true,
    bootstrap_completed boolean NOT NULL DEFAULT false,
    CONSTRAINT system_state_pkey PRIMARY KEY (singleton),
    CONSTRAINT system_state_singleton_check CHECK (singleton)
);

CREATE TABLE accounts (
    id uuid NOT NULL,
    username text COLLATE "C" NOT NULL,
    role text COLLATE "C" NOT NULL,
    enabled boolean NOT NULL,
    api_key_digest bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT accounts_pkey PRIMARY KEY (id),
    CONSTRAINT accounts_username_unique UNIQUE (username),
    CONSTRAINT accounts_api_key_digest_unique UNIQUE (api_key_digest),
    CONSTRAINT accounts_username_syntax_check CHECK (username ~ '^[a-z0-9_-]{3,64}$'),
    CONSTRAINT accounts_role_check CHECK (role IN ('user', 'admin')),
    CONSTRAINT accounts_api_key_digest_length_check CHECK (octet_length(api_key_digest) = 32)
);

CREATE TABLE code_reservations (
    short_code text COLLATE "C" NOT NULL,
    CONSTRAINT code_reservations_pkey PRIMARY KEY (short_code),
    CONSTRAINT code_reservations_short_code_syntax_check
        CHECK (short_code ~ '^[a-z0-9-]{3,64}$')
);

CREATE TABLE links (
    id uuid NOT NULL,
    short_code text COLLATE "C" NOT NULL,
    owner_id uuid,
    destination_url text COLLATE "C" NOT NULL,
    destination_search text COLLATE "C" NOT NULL,
    title text,
    title_search text COLLATE "C",
    enabled boolean NOT NULL DEFAULT true,
    visit_count bigint NOT NULL DEFAULT 0,
    management_token_digest bytea,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT links_pkey PRIMARY KEY (id),
    CONSTRAINT links_short_code_unique UNIQUE (short_code),
    CONSTRAINT links_code_reservation_fk FOREIGN KEY (short_code)
        REFERENCES code_reservations (short_code) ON DELETE RESTRICT,
    CONSTRAINT links_owner_fk FOREIGN KEY (owner_id)
        REFERENCES accounts (id) ON DELETE CASCADE,
    CONSTRAINT links_short_code_syntax_check CHECK (short_code ~ '^[a-z0-9-]{3,64}$'),
    CONSTRAINT links_destination_length_check
        CHECK (octet_length(destination_url) BETWEEN 1 AND 8192),
    CONSTRAINT links_destination_scheme_check CHECK (destination_url ~* '^https?://'),
    CONSTRAINT links_title_search_consistency_check
        CHECK ((title IS NULL) = (title_search IS NULL)),
    CONSTRAINT links_visit_count_nonnegative_check CHECK (visit_count >= 0),
    CONSTRAINT links_management_token_digest_length_check
        CHECK (management_token_digest IS NULL OR octet_length(management_token_digest) = 32),
    CONSTRAINT links_ownership_mode_check CHECK (
        (owner_id IS NULL AND management_token_digest IS NOT NULL)
        OR (owner_id IS NOT NULL AND management_token_digest IS NULL)
    )
);

CREATE TABLE link_tags (
    link_id uuid NOT NULL,
    name text COLLATE "C" NOT NULL,
    CONSTRAINT link_tags_pkey PRIMARY KEY (link_id, name),
    CONSTRAINT link_tags_link_fk FOREIGN KEY (link_id)
        REFERENCES links (id) ON DELETE CASCADE,
    CONSTRAINT link_tags_name_check CHECK (
        char_length(name) BETWEEN 1 AND 64 AND name !~ '[[:cntrl:]]'
    )
);

CREATE TABLE visits (
    id uuid NOT NULL,
    link_id uuid NOT NULL,
    redirect_url text COLLATE "C" NOT NULL,
    occurred_at timestamptz NOT NULL,
    method text COLLATE "C" NOT NULL,
    client_ip inet NOT NULL,
    referrer text,
    user_agent text,
    device text COLLATE "C" NOT NULL,
    browser text COLLATE "C" NOT NULL,
    os text COLLATE "C" NOT NULL,
    CONSTRAINT visits_pkey PRIMARY KEY (id),
    CONSTRAINT visits_link_fk FOREIGN KEY (link_id)
        REFERENCES links (id) ON DELETE CASCADE,
    CONSTRAINT visits_redirect_url_length_check
        CHECK (octet_length(redirect_url) BETWEEN 1 AND 8192),
    CONSTRAINT visits_redirect_url_scheme_check CHECK (redirect_url ~* '^https?://'),
    CONSTRAINT visits_method_check CHECK (method IN ('GET', 'HEAD')),
    CONSTRAINT visits_client_ip_host_check CHECK (
        family(client_ip) IN (4, 6)
        AND masklen(client_ip) = CASE family(client_ip) WHEN 4 THEN 32 ELSE 128 END
    ),
    CONSTRAINT visits_referrer_length_check
        CHECK (referrer IS NULL OR octet_length(referrer) <= 8192),
    CONSTRAINT visits_user_agent_length_check
        CHECK (user_agent IS NULL OR octet_length(user_agent) <= 8192),
    CONSTRAINT visits_device_check CHECK (
        device IN ('desktop', 'mobile', 'tablet', 'other', 'unknown')
    ),
    CONSTRAINT visits_browser_nonempty_check CHECK (char_length(browser) > 0),
    CONSTRAINT visits_os_nonempty_check CHECK (char_length(os) > 0)
);

CREATE TABLE rate_buckets (
    id uuid NOT NULL,
    bucket_kind text COLLATE "C" NOT NULL,
    account_id uuid,
    client_ip inet,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT rate_buckets_pkey PRIMARY KEY (id),
    CONSTRAINT rate_buckets_kind_check CHECK (bucket_kind IN ('account', 'anonymous_ip')),
    CONSTRAINT rate_buckets_identity_check CHECK (
        (bucket_kind = 'account' AND account_id IS NOT NULL AND client_ip IS NULL)
        OR (
            bucket_kind = 'anonymous_ip'
            AND account_id IS NULL
            AND client_ip IS NOT NULL
            AND family(client_ip) IN (4, 6)
            AND masklen(client_ip) = CASE family(client_ip) WHEN 4 THEN 32 ELSE 128 END
        )
    )
);

CREATE TABLE rate_charges (
    id uuid NOT NULL,
    bucket_id uuid NOT NULL,
    charged_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT rate_charges_pkey PRIMARY KEY (id),
    CONSTRAINT rate_charges_bucket_fk FOREIGN KEY (bucket_id)
        REFERENCES rate_buckets (id) ON DELETE CASCADE
);

CREATE INDEX links_owner_id_idx ON links (owner_id);
CREATE UNIQUE INDEX links_management_token_digest_unique
    ON links (management_token_digest)
    WHERE management_token_digest IS NOT NULL;
CREATE INDEX visits_link_occurred_id_idx
    ON visits (link_id, occurred_at DESC, id DESC);
CREATE INDEX link_tags_name_link_idx ON link_tags (name, link_id);
CREATE UNIQUE INDEX rate_buckets_account_identity_unique
    ON rate_buckets (account_id)
    WHERE bucket_kind = 'account';
CREATE UNIQUE INDEX rate_buckets_client_ip_identity_unique
    ON rate_buckets (client_ip)
    WHERE bucket_kind = 'anonymous_ip';
CREATE INDEX rate_charges_bucket_time_idx ON rate_charges (bucket_id, charged_at);

INSERT INTO system_state (singleton, bootstrap_completed) VALUES (true, false);
INSERT INTO code_reservations (short_code) VALUES ('api');

CREATE FUNCTION ferl_guard_system_state() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'system state cannot be deleted'
            USING ERRCODE = '23514', CONSTRAINT = 'system_state_singleton_immutable';
    END IF;

    IF NEW.singleton IS DISTINCT FROM OLD.singleton
       OR (OLD.bootstrap_completed AND NOT NEW.bootstrap_completed) THEN
        RAISE EXCEPTION 'system state transition is not permitted'
            USING ERRCODE = '23514', CONSTRAINT = 'system_state_transition_check';
    END IF;

    IF NOT OLD.bootstrap_completed AND NEW.bootstrap_completed
       AND NOT EXISTS (
           SELECT 1 FROM accounts WHERE role = 'admin' AND enabled
       ) THEN
        RAISE EXCEPTION 'bootstrap completion requires an enabled administrator'
            USING ERRCODE = '23514', CONSTRAINT = 'system_state_admin_required';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER system_state_immutable_trigger
    BEFORE UPDATE OR DELETE ON system_state
    FOR EACH ROW EXECUTE FUNCTION ferl_guard_system_state();

CREATE FUNCTION ferl_guard_account_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.username IS DISTINCT FROM OLD.username THEN
        RAISE EXCEPTION 'account identity is immutable'
            USING ERRCODE = '23514', CONSTRAINT = 'accounts_identity_immutable';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER accounts_identity_immutable_trigger
    BEFORE UPDATE ON accounts
    FOR EACH ROW EXECUTE FUNCTION ferl_guard_account_identity();

CREATE FUNCTION ferl_guard_reservation_permanence() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'code reservations are permanent'
        USING ERRCODE = '23514', CONSTRAINT = 'code_reservations_permanent';
END;
$$;

CREATE TRIGGER code_reservations_permanent_trigger
    BEFORE UPDATE OR DELETE ON code_reservations
    FOR EACH ROW EXECUTE FUNCTION ferl_guard_reservation_permanence();

CREATE FUNCTION ferl_guard_link_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
       OR NEW.short_code IS DISTINCT FROM OLD.short_code
       OR NEW.owner_id IS DISTINCT FROM OLD.owner_id THEN
        RAISE EXCEPTION 'link identity is immutable'
            USING ERRCODE = '23514', CONSTRAINT = 'links_identity_immutable';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER links_identity_immutable_trigger
    BEFORE UPDATE ON links
    FOR EACH ROW EXECUTE FUNCTION ferl_guard_link_identity();

CREATE FUNCTION ferl_guard_visit_immutability() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'visits are immutable'
        USING ERRCODE = '23514', CONSTRAINT = 'visits_immutable';
END;
$$;

CREATE TRIGGER visits_immutable_trigger
    BEFORE UPDATE ON visits
    FOR EACH ROW EXECUTE FUNCTION ferl_guard_visit_immutability();
