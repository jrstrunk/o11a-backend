use crate::collaborator::CommentEvent;
use crate::core::DataContext;
use sqlx::SqlitePool;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AppState {
  pub db: SqlitePool,
  pub data_context: Arc<Mutex<DataContext>>,
  pub comment_broadcast: broadcast::Sender<CommentEvent>,
}

impl AppState {
  pub fn new(db: SqlitePool, data_context: DataContext) -> Self {
    let (tx, _) = broadcast::channel(100); // Buffer 100 events
    Self {
      db,
      data_context: Arc::new(Mutex::new(data_context)),
      comment_broadcast: tx,
    }
  }
}
