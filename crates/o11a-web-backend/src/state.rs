use o11a_core::state::AppState;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Cached static parts of a topic view HTML (AST-derived, never invalidated).
#[derive(Debug, Clone)]
pub struct CachedTopicView {
  pub topic_panel_html: String,
  pub expanded_references_panel_html: String,
  pub breadcrumb_html: String,
  pub highlight_css: String,
}

/// Frontend-scoped HTML rendering caches, keyed by audit id then topic id.
type SourceTextCache = HashMap<String, HashMap<String, String>>;
type TopicViewCache = HashMap<String, HashMap<String, CachedTopicView>>;

/// State available to HTML-returning routes. Wraps `AppState` so frontend
/// handlers can access the same data context and database pool the core
/// JSON routes use, while owning rendering-only caches separately.
#[derive(Clone)]
pub struct FrontendState {
  pub app: AppState,
  pub source_text_cache: Arc<Mutex<SourceTextCache>>,
  pub topic_view_cache: Arc<Mutex<TopicViewCache>>,
}

impl FrontendState {
  pub fn new(app: AppState) -> Self {
    Self {
      app,
      source_text_cache: Arc::new(Mutex::new(HashMap::new())),
      topic_view_cache: Arc::new(Mutex::new(HashMap::new())),
    }
  }
}
