//! o11a-web-backend — HTML rendering and HTML-serving HTTP routes.
//!
//! This crate contains the server-side presentation layer: token/block-level
//! HTML formatting, Solidity/documentation/comment renderers, the topic view
//! HTML builders, and the handlers + routes that serve HTML (or JSON
//! responses whose payload is HTML). It pairs with a lightweight client-side
//! frontend (`o11a-web-frontend`, in Gleam, separate repository) for
//! interactive behavior — together they form the web UI on top of the
//! `o11a-server` collaboration API.
//!
//! This crate depends on `o11a-core` for types and data access only; no
//! analysis logic lives here.

pub mod comment_formatter;
pub mod documentation_formatter;
pub mod formatting;
pub mod handlers;
pub mod routes;
pub mod solidity_formatter;
pub mod state;
pub mod topic_view;
