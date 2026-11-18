use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post, put},
    Router,
};
use sqlx::PgPool;
use std::sync::Arc;
use tower_governor::{
    governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor, GovernorLayer,
};
use tower_http::cors::CorsLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../infra/migrations");

pub fn latest_migration_version() -> i64 {
    MIGRATOR
        .migrations
        .last()
        .map(|migration| migration.version)
        .unwrap_or_default()
}

pub mod agent;
pub mod assistant_provider;
pub mod auth;
pub mod errors;
pub mod form_auto_import;
pub mod payment_provider;
mod routes;
mod services;
/// The search relevance reranker (flag, batch job, cache), for the eval harness.
pub use services::relevance;

// ── Request body size limit ──
//
// Axum enforces an implicit 2 MiB body limit by default; we make it explicit
// here so the intent is documented and reviewable (security
// checklist: explicit request body limits). Every JSON API request (profile, program admin forms,
// alerts, etc.) is comfortably small. User documents never reach the server:
// the vault is on-device only.
const DEFAULT_BODY_LIMIT_BYTES: usize = 1024 * 1024; // 1 MiB
/// Admin upload of a PDF converted from an official HWP/HWPX form.
const DERIVED_PDF_BODY_LIMIT_BYTES: usize = mazimi_core::form_templates::MAX_DERIVED_PDF_BYTES;

use payment_provider::PaymentProvider;

// ── Shared application state ──
//
// `FromRef` lets Axum extract sub-states automatically, so existing handlers
// that declare `State<PgPool>` continue to work without modification.

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt_secret: String,
    /// "local" | "staging" | "production"
    pub app_env: String,
    pub public_api_base_url: String,
    pub http_client: reqwest::Client,
    /// Payment gateway provider. Fail-closed: absent credentials → always fails.
    pub payment_provider: Arc<dyn PaymentProvider>,
    pub assistant: Arc<assistant_provider::AssistantRuntime>,
    pub agent: Arc<agent::AgentRuntime>,
    /// `ANONYMOUS_PREVIEW_ENABLED`, read once when the app is built.
    pub anonymous_preview: AnonymousPreview,
}

/// Whether `POST /recommend/preview` answers without a login (default on:
/// the web `/preview` page still calls it).
#[derive(Debug, Clone, Copy)]
pub struct AnonymousPreview(pub bool);

impl AnonymousPreview {
    pub fn from_env() -> Self {
        Self(
            std::env::var("ANONYMOUS_PREVIEW_ENABLED").map_or(true, |v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                )
            }),
        )
    }
}

impl axum::extract::FromRef<AppState> for AnonymousPreview {
    fn from_ref(state: &AppState) -> Self {
        state.anonymous_preview
    }
}

impl axum::extract::FromRef<AppState> for PgPool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}

/// Newtype wrapper to avoid a blanket `FromRef<AppState> for String` impl
/// which could collide with other string-typed state fields.
#[derive(Debug, Clone)]
pub struct JwtSecret(pub String);

impl axum::extract::FromRef<AppState> for JwtSecret {
    fn from_ref(state: &AppState) -> Self {
        JwtSecret(state.jwt_secret.clone())
    }
}

pub fn build_app(pool: PgPool, jwt_secret: String) -> Router {
    build_app_with_env(
        pool,
        jwt_secret,
        // Fail closed: if APP_ENV is not set at all, assume "production" so
        // error sanitization (and any other env-gated hardening) is on by
        // default. Local development must opt out explicitly by setting
        // APP_ENV=local (already done in compose.dev.yml / .env.dev / CI).
        std::env::var("APP_ENV").unwrap_or_else(|_| "production".to_string()),
    )
}

pub fn build_app_with_env(pool: PgPool, jwt_secret: String, app_env: String) -> Router {
    let public_api_base_url = std::env::var("PUBLIC_API_BASE_URL").unwrap_or_else(|_| {
        if matches!(app_env.as_str(), "local" | "test") {
            let host = std::env::var("CALLBACK_HOST").unwrap_or_else(|_| "localhost".to_string());
            let port = std::env::var("APP_PORT").unwrap_or_else(|_| "8080".to_string());
            format!("http://{}:{port}", host.trim())
        } else {
            std::env::var("DOMAIN")
                .map(|domain| format!("https://{}/api", domain.trim().trim_end_matches('/')))
                .unwrap_or_else(|_| "https://invalid.local/api".to_string())
        }
    });
    build_app_with_runtime(pool, jwt_secret, app_env, public_api_base_url)
}

pub fn build_app_with_runtime(
    pool: PgPool,
    jwt_secret: String,
    app_env: String,
    public_api_base_url: String,
) -> Router {
    build_app_with_assistant_runtime(
        pool,
        jwt_secret,
        app_env,
        public_api_base_url,
        Arc::new(assistant_provider::AssistantRuntime::from_env()),
    )
}

/// Dependency injection keeps provider failures testable without live model calls.
pub fn build_app_with_assistant_runtime(
    pool: PgPool,
    jwt_secret: String,
    app_env: String,
    public_api_base_url: String,
    assistant: Arc<assistant_provider::AssistantRuntime>,
) -> Router {
    let agent = Arc::new(agent::AgentRuntime::from_env(assistant.clone()));
    build_app_with_runtimes(
        pool,
        jwt_secret,
        app_env,
        public_api_base_url,
        assistant,
        agent,
    )
}

/// Both runtimes injected: tests run the agent enabled with a mock planner.
pub fn build_app_with_runtimes(
    pool: PgPool,
    jwt_secret: String,
    app_env: String,
    public_api_base_url: String,
    assistant: Arc<assistant_provider::AssistantRuntime>,
    agent: Arc<agent::AgentRuntime>,
) -> Router {
    build_app_with_options(
        pool,
        jwt_secret,
        app_env,
        public_api_base_url,
        assistant,
        agent,
        AnonymousPreview::from_env(),
    )
}

/// [`build_app_with_env`] with the anonymous-preview switch given instead
/// of read from the environment (tests).
pub fn build_app_with_preview(
    pool: PgPool,
    jwt_secret: String,
    app_env: String,
    anonymous_preview: bool,
) -> Router {
    let assistant = Arc::new(assistant_provider::AssistantRuntime::from_env());
    let agent = Arc::new(agent::AgentRuntime::from_env(assistant.clone()));
    build_app_with_options(
        pool,
        jwt_secret,
        app_env,
        "http://localhost:8080".to_owned(),
        assistant,
        agent,
        AnonymousPreview(anonymous_preview),
    )
}

fn build_app_with_options(
    pool: PgPool,
    jwt_secret: String,
    app_env: String,
    public_api_base_url: String,
    assistant: Arc<assistant_provider::AssistantRuntime>,
    agent: Arc<agent::AgentRuntime>,
    anonymous_preview: AnonymousPreview,
) -> Router {
    // Off unless ASSISTANT_SEARCH_RERANK_ENABLED (and the classifier loads).
    services::relevance::spawn_when_ready(pool.clone(), agent.clone());
    if agent.config.enabled {
        agent::run::spawn_stale_sweep(pool.clone());
    }
    let http_client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .pool_idle_timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("mazimi-api/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("static outbound HTTP client configuration must be valid");
    let state = AppState {
        anonymous_preview,
        assistant,
        agent,
        pool,
        jwt_secret,
        app_env,
        public_api_base_url: public_api_base_url.trim_end_matches('/').to_string(),
        http_client: http_client.clone(),
        payment_provider: Arc::from(payment_provider::build_payment_provider(http_client)),
    };

    // ── Rate limiter configs ──
    //
    // GovernorConfigBuilder uses a token-bucket model:
    //   per_second(N)      — replenish 1 token every N seconds
    //   per_millisecond(N) — replenish 1 token every N milliseconds
    //   burst_size(B)      — max burst allowed before throttling kicks in
    //
    // OAuth handoff:   30 req/min  → replenish every  2 s, burst 30
    // Token endpoints:  5 req/min  → replenish every 12 s, burst 5
    // Admin endpoints: 30 req/min  → replenish every  2 s, burst 30
    // General API:    100 req/min  → replenish every 600 ms, burst 100

    let auth_governor_conf = std::sync::Arc::new(
        GovernorConfigBuilder::default()
            .per_second(12)
            .burst_size(5)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("auth rate-limiter config"),
    );

    // Publisher opt-out, its own bucket: burst 5, then one request per 2 h
    // per IP, so one address stays below the global daily intake cap
    // (optout::DAILY_CAP = 20) and cannot pause the form for everyone. A
    // request changes nothing until domain control is proven.
    let publisher_governor_conf = std::sync::Arc::new(
        GovernorConfigBuilder::default()
            .per_second(2 * 60 * 60)
            .burst_size(5)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("publisher rate-limiter config"),
    );

    // OAuth handoff (start → callback → exchange): one sign-in spends three
    // calls, and testers or a campus share one NAT address, so 5/min would
    // 429 the second person. Each call is single-use and verifier-bound, so
    // a looser bucket does not help guessing; refresh/logout keep the strict
    // one above.
    let oauth_governor_conf = std::sync::Arc::new(
        GovernorConfigBuilder::default()
            .per_second(2)
            .burst_size(30)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("oauth rate-limiter config"),
    );

    let admin_governor_conf = std::sync::Arc::new(
        GovernorConfigBuilder::default()
            .per_second(2)
            .burst_size(30)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("admin rate-limiter config"),
    );

    let api_governor_conf = std::sync::Arc::new(
        GovernorConfigBuilder::default()
            .per_millisecond(600)
            .burst_size(100)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("api rate-limiter config"),
    );

    // ── OAuth handoff routes: 30 burst, then 1 per 2 s per IP ──
    let oauth_routes = Router::new()
        .route("/api/v1/auth/kakao/start", get(routes::auth::kakao_start))
        .route(
            "/api/v1/auth/kakao/exchange",
            post(routes::auth::kakao_exchange),
        )
        .route(
            "/api/v1/auth/kakao/callback",
            get(routes::auth::kakao_callback),
        )
        .route("/api/v1/auth/google/start", get(routes::auth::google_start))
        .route(
            "/api/v1/auth/google/callback",
            get(routes::auth::google_callback),
        )
        .route(
            "/api/v1/auth/google/exchange",
            post(routes::auth::google_exchange),
        )
        .layer(GovernorLayer::new(Arc::clone(&oauth_governor_conf)));

    // ── Session token routes: strict limit (5 burst, then 1 per 12 s per IP) ──
    let auth_routes = Router::new()
        .route("/api/v1/auth/refresh", post(routes::auth::refresh))
        .route("/api/v1/auth/logout", post(routes::auth::logout))
        .layer(GovernorLayer::new(Arc::clone(&auth_governor_conf)));

    // ── Publisher opt-out: unauthenticated, so its own bucket (burst 5, then
    // one request per 2 h per IP, publisher_governor_conf above), a small
    // body cap, and per-host caps inside the handler.
    let publisher_routes = Router::new()
        .route(
            "/api/v1/publisher/opt-out",
            post(routes::corporate_pipeline::opt_out),
        )
        .route_layer(DefaultBodyLimit::max(4096))
        .layer(GovernorLayer::new(Arc::clone(&publisher_governor_conf)));

    // ── Admin routes: moderate limit (30/min per IP) + JWT auth ──
    // Role enforcement (admin-only) is done inside each handler via AdminUser extractor.
    let admin_routes = Router::new()
        .route(
            "/api/v1/admin/application-routing-stats",
            get(routes::application_routing::stats),
        )
        .route(
            "/api/v1/admin/assistant/runs",
            get(routes::agent::admin_runs),
        )
        .route(
            "/api/v1/admin/ai-extraction/candidates",
            get(routes::ai_extraction::queue),
        )
        .route(
            "/api/v1/admin/ai-extraction/candidates/{id}/audit",
            post(routes::ai_extraction::audit),
        )
        .route(
            "/api/v1/admin/ai-extraction/auto-runs",
            get(routes::ai_extraction::auto_runs),
        )
        .route(
            "/api/v1/admin/ai-extraction/revert",
            post(routes::ai_extraction::revert),
        )
        .route(
            "/api/v1/admin/applicant-reviews",
            get(routes::applicant_reviews::list),
        )
        .route(
            "/api/v1/admin/programs/{id}/applicant-review",
            put(routes::applicant_reviews::put).delete(routes::applicant_reviews::delete),
        )
        .route(
            "/api/v1/admin/condition-reviews",
            get(routes::condition_reviews::list),
        )
        .route(
            "/api/v1/admin/programs/{id}/condition-review",
            put(routes::condition_reviews::put).delete(routes::condition_reviews::delete),
        )
        .route(
            "/api/v1/admin/programs/{id}/ai-extraction",
            get(routes::ai_extraction::program),
        )
        .route(
            "/api/v1/admin/programs/{id}/ai-extraction/review",
            post(routes::ai_extraction::review),
        )
        .route(
            "/api/v1/admin/programs/{id}/application-resources",
            get(routes::application_resources::list).post(routes::application_resources::queue),
        )
        .route(
            "/api/v1/admin/application-resources/{id}",
            put(routes::application_resources::review),
        )
        .route(
            "/api/v1/admin/application-resources/{id}/form-template",
            get(routes::form_templates::admin_get)
                .put(routes::form_templates::admin_put)
                .delete(routes::form_templates::admin_delete),
        )
        .route(
            "/api/v1/admin/application-resources/{id}/form-pdf",
            put(routes::form_templates::admin_put_derived_pdf)
                .layer(DefaultBodyLimit::max(DERIVED_PDF_BODY_LIMIT_BYTES)),
        )
        .route(
            "/api/v1/admin/programs",
            get(routes::admin::list_admin_programs),
        )
        .route(
            "/api/v1/admin/programs",
            post(routes::admin::create_program),
        )
        .route(
            "/api/v1/admin/programs/{id}",
            get(routes::admin::get_admin_program).put(routes::admin::update_program),
        )
        .route(
            "/api/v1/admin/programs/{id}/publish",
            post(routes::admin::toggle_publish),
        )
        .route(
            "/api/v1/admin/programs/{id}/brands",
            put(routes::admin::set_program_brands),
        )
        .route(
            "/api/v1/admin/programs/{id}/source-verification",
            put(routes::discovery::review_program_source),
        )
        .route(
            "/api/v1/admin/programs/source-verification/batch",
            put(routes::discovery::review_program_sources_batch),
        )
        .route("/api/v1/admin/brands", post(routes::brands::upsert_brand))
        .route(
            "/api/v1/admin/benefit-feeds",
            post(routes::benefit_feeds::create),
        )
        .route(
            "/api/v1/admin/benefit-feeds/{id}",
            axum::routing::delete(routes::benefit_feeds::revoke),
        )
        .route(
            "/api/v1/admin/benefit-source-domains",
            post(routes::discovery::upsert_source_domain),
        )
        .route(
            "/api/v1/admin/benefit-source-domains/{id}/approval",
            put(routes::discovery::approve_source_domain_crawl)
                .delete(routes::discovery::revoke_source_domain_crawl),
        )
        .route(
            "/api/v1/admin/benefit-source-domains/{id}/robots-preflight",
            post(routes::discovery::preflight_source_domain_robots),
        )
        .route(
            "/api/v1/admin/benefit-candidates",
            get(routes::discovery::list_candidates).post(routes::discovery::queue_candidate),
        )
        .route(
            "/api/v1/admin/benefit-candidates/{id}",
            get(routes::discovery::get_candidate),
        )
        .route(
            "/api/v1/admin/benefit-candidates/{id}/review",
            put(routes::discovery::review_candidate),
        )
        .route(
            "/api/v1/admin/brand-discovery-targets",
            get(routes::discovery::list_discovery_targets),
        )
        .route(
            "/api/v1/admin/brand-source-suggestions",
            get(routes::discovery::list_source_suggestions),
        )
        .route(
            "/api/v1/admin/brand-source-suggestions/{id}/review",
            put(routes::discovery::review_source_suggestion),
        )
        .route(
            "/api/v1/admin/operational-events",
            get(routes::operations::list_operational_events),
        )
        .route(
            "/api/v1/admin/operational-events/{id}",
            put(routes::operations::review_operational_event),
        )
        .route(
            "/api/v1/admin/data-quality-issues",
            get(routes::operations::list_data_quality_issues),
        )
        .route(
            "/api/v1/admin/data-quality-issues/{id}",
            put(routes::operations::review_data_quality_issue),
        )
        .route(
            "/api/v1/admin/program-maintenance",
            get(routes::operations::get_program_maintenance),
        )
        .route(
            "/api/v1/admin/corporate-pipeline/health",
            get(routes::corporate_pipeline::health),
        )
        .route(
            "/api/v1/admin/publisher-optouts",
            get(routes::corporate_pipeline::optout_status),
        )
        .route(
            "/api/v1/admin/publisher-optouts/{id}/lift",
            post(routes::corporate_pipeline::optout_lift),
        )
        .route("/api/v1/admin/stats", get(routes::admin::get_stats))
        .route("/api/v1/admin/sync", post(routes::admin::trigger_sync))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(GovernorLayer::new(Arc::clone(&admin_governor_conf)));

    // ── General public routes: relaxed limit (100/min per IP) ──
    let public_routes = Router::new()
        .route("/api/v1/auth/providers", get(routes::auth::providers))
        .route(
            "/api/v1/campus/universities",
            get(routes::campus::universities),
        )
        .route("/api/v1/campus/programs", get(routes::campus::programs))
        .route("/health", get(routes::health::health))
        .route("/api/v1/health", get(routes::health::health_detail))
        .route("/ready", get(routes::health::readiness))
        .route("/api/v1/ready", get(routes::health::readiness))
        .route("/api/v1/programs", get(routes::programs::list_programs))
        .route(
            "/api/v1/programs/corporate-categories",
            get(routes::programs::corporate_category_counts),
        )
        .route("/api/v1/programs/{id}", get(routes::programs::get_program))
        .route(
            "/api/v1/programs/{id}/application-resources/{resource_id}/form-template",
            get(routes::form_templates::public_get),
        )
        .route(
            "/api/v1/programs/{id}/application-resources/{resource_id}/form-pdf",
            get(routes::form_templates::public_derived_pdf),
        )
        // TODO(remove brand_id): the per-company browse is gone from the app; the
        // public brand list stays only for installed TestFlight build 6. Remove
        // with ProgramQuery::brand_id once this release's app update is live.
        .route("/api/v1/brands", get(routes::brands::list_brands))
        .route("/api/v1/brands/{id}", get(routes::brands::get_brand))
        .route(
            "/api/v1/benefit-feeds/{id}/batches",
            axum::routing::put(routes::benefit_feeds::ingest),
        )
        .route(
            "/api/v1/recommend/preview",
            post(routes::recommend::preview),
        )
        .route(
            "/api/v1/stack-check",
            post(routes::stack_check::stack_check),
        )
        .layer(GovernorLayer::new(Arc::clone(&api_governor_conf)));

    // ── Protected routes: general limit (100/min per IP) + JWT auth ──
    let protected_routes = Router::new()
        .route(
            "/api/v1/programs/{id}/assistant",
            get(routes::assistant::overview).post(routes::assistant::ask),
        )
        .route(
            "/api/v1/assistant/briefing",
            get(routes::assistant::briefing),
        )
        .route(
            "/api/v1/programs/{id}/assistant/answer-stream",
            post(routes::assistant::answer_stream),
        )
        .route("/api/v1/assistant/search", post(routes::assistant::search))
        .route("/api/v1/assistant/bundles", get(routes::assistant::bundle))
        .route(
            "/api/v1/assistant/compare",
            post(routes::assistant::compare),
        )
        .route("/api/v1/assistant/agent", get(routes::agent::status))
        .route(
            "/api/v1/assistant/agent/proactive",
            get(routes::agent::get_proactive).put(routes::agent::put_proactive),
        )
        .route(
            "/api/v1/assistant/conversations",
            get(routes::agent::list_conversations).post(routes::agent::create_conversation),
        )
        .route(
            "/api/v1/assistant/conversations/{id}",
            get(routes::agent::get_conversation).delete(routes::agent::delete_conversation),
        )
        .route(
            "/api/v1/assistant/conversations/{id}/messages",
            post(routes::agent::post_message),
        )
        .route("/api/v1/assistant/runs/{id}", get(routes::agent::get_run))
        .route(
            "/api/v1/assistant/runs/{id}/events",
            get(routes::agent::run_events),
        )
        .route(
            "/api/v1/assistant/runs/{id}/cancel",
            post(routes::agent::cancel_run),
        )
        .route(
            "/api/v1/assistant/confirmations/{id}/approve",
            post(routes::agent::approve),
        )
        .route(
            "/api/v1/assistant/confirmations/{id}/reject",
            post(routes::agent::reject),
        )
        .route_layer(DefaultBodyLimit::max(4096))
        .route_layer(middleware::from_fn(routes::assistant::private_response))
        .route(
            "/api/v1/programs/{id}/handoff",
            post(routes::journeys::start),
        )
        .route(
            "/api/v1/my/application-journeys/{id}",
            get(routes::journeys::get).put(routes::journeys::report),
        )
        .route(
            "/api/v1/programs/{id}/progress",
            get(routes::journeys::progress),
        )
        .route(
            "/api/v1/programs/{id}/preparation",
            get(routes::preparation::get).put(routes::preparation::save),
        )
        .route(
            "/api/v1/auth/me",
            get(routes::auth::me).delete(routes::auth::delete_account),
        )
        .route(
            "/api/v1/push/register",
            post(routes::push::register_push_token).delete(routes::push::unregister_push_token),
        )
        .route(
            "/api/v1/profile",
            post(routes::profile::save_profile).get(routes::profile::get_my_profile),
        )
        .route(
            "/api/v1/profile/{user_id}",
            get(routes::profile::get_profile),
        )
        .route(
            "/api/v1/programs/{program_id}/bookmark",
            post(routes::bookmark::toggle_bookmark).put(routes::bookmark::set_bookmark),
        )
        .route(
            "/api/v1/programs/{program_id}/state",
            post(routes::state::upsert_state),
        )
        .route("/api/v1/recommendations", get(routes::recommend::mine))
        .route("/api/v1/dashboard", get(routes::dashboard::get_dashboard))
        .route("/api/v1/alerts", get(routes::alerts::list_alerts))
        .route(
            "/api/v1/alerts/read-all",
            post(routes::alerts::mark_all_read),
        )
        .route(
            "/api/v1/alerts/preferences",
            post(routes::alerts::upsert_preferences),
        )
        .route(
            "/api/v1/alerts/preferences/{program_id}",
            get(routes::alerts::get_preference),
        )
        .route(
            "/api/v1/alerts/notification-preferences",
            get(routes::alerts::get_notification_preferences),
        )
        .route(
            "/api/v1/alerts/notification-preferences",
            put(routes::alerts::update_notification_preferences),
        )
        .route("/api/v1/my/saved", get(routes::my::get_saved))
        .route(
            "/api/v1/my/document-items",
            get(routes::document_items::list).post(routes::document_items::create),
        )
        .route(
            "/api/v1/my/document-items/{id}",
            put(routes::document_items::update).delete(routes::document_items::delete),
        )
        .route(
            "/api/v1/my/applications",
            get(routes::state::list_applications),
        )
        .route(
            "/api/v1/my/applications/{program_id}",
            get(routes::state::get_application),
        )
        .route(
            "/api/v1/my/applications/{program_id}",
            put(routes::state::update_application),
        )
        .route(
            "/api/v1/payments/verify",
            post(routes::payment::verify_payment),
        )
        .route(
            "/api/v1/payments/status",
            get(routes::payment::subscription_status),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(GovernorLayer::new(Arc::clone(&api_governor_conf)));

    Router::new()
        .merge(oauth_routes)
        .merge(auth_routes)
        .merge(publisher_routes)
        .merge(admin_routes)
        .merge(public_routes)
        .merge(protected_routes)
        // Apply security headers to every response
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_headers_middleware,
        ))
        // Apply error sanitization to every response
        .layer(middleware::from_fn_with_state(
            state.clone(),
            error_sanitization_middleware,
        ))
        .layer(build_cors_layer(&state.app_env))
        // Log only the path, never the query string: OAuth callbacks carry a
        // short-lived authorization code in their query parameters.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        path = request.uri().path()
                    )
                })
                .on_response(
                    DefaultOnResponse::new()
                        .level(tracing::Level::INFO)
                        .include_headers(false),
                ),
        )
        // Global request body limit.
        .layer(DefaultBodyLimit::max(DEFAULT_BODY_LIMIT_BYTES))
        .with_state(state)
}

// ── CORS ──
//
// Allows requests from localhost:3000 (dev) and the production domain
// supplied via the DOMAIN environment variable.  Only the necessary
// HTTP methods and headers are permitted; credentials (cookies/tokens)
// may be sent from these origins.

fn build_cors_layer(app_env: &str) -> CorsLayer {
    let mut origins: Vec<HeaderValue> = Vec::new();

    if matches!(app_env, "local" | "test") {
        origins.extend([
            HeaderValue::from_static("http://localhost:3000"),
            HeaderValue::from_static("http://127.0.0.1:3000"),
        ]);
    }

    if let Ok(domain) = std::env::var("DOMAIN") {
        // Only allow HTTPS for production domains
        let https = format!("https://{domain}");
        if let Ok(v) = https.parse() {
            origins.push(v);
        }
    }

    if let Ok(domain) = std::env::var("APP_DOMAIN") {
        if let Ok(v) = format!("https://{domain}").parse() {
            origins.push(v);
        }
    }
    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::HeaderName::from_static("idempotency-key"),
            header::HeaderName::from_static("x-mazimi-client-caps"),
        ])
        .allow_credentials(true)
}

// ── Auth middleware ──
//
// Validates the Bearer JWT and injects `AuthUser` into request extensions so
// that downstream handlers can optionally extract it via `Extension<AuthUser>`.

async fn auth_middleware(
    State(state): State<AppState>,
    mut req: axum::extract::Request,
    next: Next,
) -> Result<Response, (StatusCode, axum::Json<serde_json::Value>)> {
    use axum::http::StatusCode;
    use serde_json::json;

    let unauthorized = |msg: &str| {
        (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "error": msg })),
        )
    };

    let auth_header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| unauthorized("Missing Authorization header"))?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or_else(|| unauthorized("Authorization header must use Bearer scheme"))?;

    let claims = auth::verify_token(token, &state.jwt_secret)
        .map_err(|_| unauthorized("Invalid or expired token"))?;

    if claims.token_type != "access" {
        return Err(unauthorized("Token type must be 'access'"));
    }

    let user_id =
        uuid::Uuid::parse_str(&claims.sub).map_err(|_| unauthorized("Malformed token subject"))?;

    // Account deletion and role changes take effect immediately, including
    // for access tokens issued before the change.
    let role = sqlx::query_scalar::<_, String>("SELECT role FROM users WHERE id=$1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(crate::errors::internal_error)?
        .ok_or_else(|| unauthorized("Account no longer exists"))?;
    let auth_user = auth::AuthUser { id: user_id, role };

    // Each handler scopes data to this identity. A standalone SET LOCAL
    // would expire before the next pooled query and provide no RLS context.

    // Make AuthUser available to handlers via request extensions
    req.extensions_mut().insert(auth_user);

    Ok(next.run(req).await)
}

// ── Security headers middleware ──
//
// Adds defensive HTTP headers to every outbound response:
//   - X-Content-Type-Options: nosniff       — prevents MIME-type sniffing
//   - X-Frame-Options: DENY                 — blocks clickjacking via iframes
//   - Strict-Transport-Security             — HSTS for HTTPS deployments
//   - Content-Security-Policy               — this is a JSON API with no
//     HTML/JS/CSS of its own, so the policy denies every resource type by
//     default. This only matters if a response is ever coerced into an
//     HTML context (e.g. content-type sniffing bugs upstream, or a browser
//     rendering an error page) — in that case nothing is allowed to load.
//   - X-Powered-By is removed if present    — hides implementation details

async fn security_headers_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();

    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    // max-age=63072000 = 2 years; includeSubDomains covers all subdomains
    headers.insert(
        "strict-transport-security",
        HeaderValue::from_static("max-age=63072000; includeSubDomains"),
    );
    // Conservative deny-everything policy — this API never serves HTML,
    // scripts, styles, or embeddable frames.
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
    );
    // Remove x-powered-by if any upstream layer added it
    headers.remove("x-powered-by");

    response
}

// ── Error sanitization middleware ──
//
// In non-local environments (APP_ENV != "local") this middleware rewrites
// 5xx JSON error bodies so that internal DB errors and stack traces are
// never sent to clients.  In "local" the full error is preserved for
// easier debugging.
//
// It works by inspecting the response status code; when it is a 5xx the
// body is replaced with a generic message.  4xx errors (auth failures,
// validation errors, not-found) are passed through unchanged, except
// framework rejections (see `client_rejection_json`).
//
// Every handler answers errors as JSON. A 4xx `text/plain` body therefore
// comes from an extractor or layer rejection (Path, Query, Json, body limit,
// rate limit), whose text names Rust types and parser internals ("Cannot
// parse `id`... UUID parsing failed", "missing field `school`"). In every
// environment it becomes the standard `{"error": ...}` JSON with a generic
// message for its status; status and other headers are kept.
fn client_rejection_json(response: Response) -> Response {
    let status = response.status();
    let is_text = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/plain"));
    if !status.is_client_error() || !is_text {
        return response;
    }
    let message = match status {
        StatusCode::PAYLOAD_TOO_LARGE => "Request body too large",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "Unsupported content type",
        StatusCode::UNPROCESSABLE_ENTITY => "Invalid request body",
        StatusCode::TOO_MANY_REQUESTS => "Too many requests",
        StatusCode::METHOD_NOT_ALLOWED => "Method not allowed",
        StatusCode::NOT_FOUND => "Not found",
        _ => "Invalid request",
    };
    let (mut parts, _) = response.into_parts();
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Response::from_parts(
        parts,
        axum::body::Body::from(serde_json::json!({ "error": message }).to_string()),
    )
}

async fn error_sanitization_middleware(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    use axum::body::Body;

    let response = client_rejection_json(next.run(req).await);

    // Local dev: preserve full error details
    if state.app_env == "local" {
        return response;
    }

    let status = response.status();

    // Only sanitize 5xx responses
    if !status.is_server_error() {
        return response;
    }

    // Log the full error server-side before replacing it
    tracing::error!(
        status = status.as_u16(),
        "Internal server error (body redacted in response)"
    );

    // Replace body with a generic message; do not leak DB errors or traces
    let generic_body = serde_json::json!({
        "error": "An internal server error occurred. Please try again later."
    })
    .to_string();

    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(generic_body))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap()
        })
}
