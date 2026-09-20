//! # `db`
//!
//! PostgreSQL access layer: connection pool configuration, embedded migrations
//! and (from Phase 1) the query models.
//!
//! ## Connection
//!
//! The database is a managed PostgreSQL instance (Northflank). The only thing
//! this crate needs is a `DATABASE_URL`; nothing about the host is hardcoded.
//! Load it from the environment (see `.env.example`):
//!
//! ```text
//! DATABASE_URL=postgres://user:password@host:5432/ai_trading?sslmode=require
//! ```
//!
//! ## Status
//!
//! Phase 0: pool wired and health-checkable, not yet used by any feature code.

#![deny(missing_docs)]

pub mod bots;
pub mod config;
pub mod drawings;
pub mod error;
pub mod indicator_workspaces;
pub mod live;
pub mod loading;
pub mod market;
pub mod models;
pub mod paper;
pub mod pump;
pub mod repositories;
pub mod skills;
pub mod strategies;
pub mod users;

pub use bots::{create_bot, delete_bot, get_bot, list_bots, set_status, BotRow, STATUSES};
pub use config::DatabaseConfig;
pub use drawings::{
    create_drawing, delete_drawing, list_drawings, needs_second_anchor, update_drawing, DrawingRow,
    NewDrawing, KINDS as DRAWING_KINDS,
};
pub use error::DbError;
pub use indicator_workspaces::{
    create_indicator_bot_draft, create_indicator_revision, create_indicator_workspace,
    create_indicator_workspace_message, delete_indicator_workspace, get_indicator_bot_draft,
    get_indicator_revision, get_indicator_workspace, list_indicator_alert_preferences,
    list_indicator_bot_drafts, list_indicator_revisions,
    list_indicator_workspace_messages,
    list_indicator_workspaces, restore_indicator_revision, set_indicator_alert_preference,
    update_indicator_workspace_memory, IndicatorAlertPreference, IndicatorBotDraftRow,
    IndicatorRevisionRow, IndicatorWorkspaceMessageRow, IndicatorWorkspaceRow,
};
pub use live::{
    list_live_orders, open_live_orders, opted_in_venues, record_live_order, record_venue_opt_in,
    update_live_order, LiveOrderRow, TERMINAL_STATUSES,
};
pub use loading::{coverage, load_timeframe_series, warn_about_short_series, MIN_COVERAGE};
pub use market::{
    best_bid_ask, latest_orderbook, list_symbols, orderbook_snapshot_count, SymbolCoverage,
    TimeframeCoverage,
};
pub use models::Database;
pub use paper::{
    bot_summary, count_audit_events, count_executed_trades, create_owner, find_or_create_strategy,
    find_user_by_email, insert_audit_events, insert_bot, insert_executed_trades,
    list_bot_notifications, purge_bot, purge_owner, purge_owners_with_prefix, recent_decisions,
    set_bot_status, strategy_paper_record, AuditEvent, BotSummary, ExecutedTrade, Notification,
    StrategyPaperRecord,
};
pub use repositories::TradeCoverage;
pub use repositories::{dt_to_ns, ns_to_dt};
pub use skills::{
    create_skill, delete_skill, list_skills, skill_version_exists, SkillRow, MAX_SKILLS,
};
pub use strategies::{
    count_bots_for_strategy, create_backtest, create_strategy, delete_strategy, get_backtest,
    get_strategy, list_backtests, list_strategies, newest_backtest_for_symbol, BacktestRow,
    StrategyRow,
};
pub use users::{create_user, delete_user, find_by_email, find_by_id, normalize_email, UserRow};

/// Embedded migrations from `crates/db/migrations`.
///
/// `sqlx::migrate!` reads the directory at **compile time**, so a missing or
/// renamed migration file is a build error rather than a runtime surprise.
pub static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
