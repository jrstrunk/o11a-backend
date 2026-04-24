use axum::{
  extract::{
    Path, State,
    ws::{Message, WebSocket, WebSocketUpgrade},
  },
  response::Response,
};
use futures::{SinkExt, StreamExt};

use crate::api::AppState;

/// WebSocket handler for comment streaming
pub async fn comment_websocket(
  ws: WebSocketUpgrade,
  Path(audit_id): Path<String>,
  State(state): State<AppState>,
) -> Response {
  ws.on_upgrade(move |socket| handle_socket(socket, audit_id, state))
}

async fn handle_socket(socket: WebSocket, audit_id: String, state: AppState) {
  let (mut sender, mut receiver) = socket.split();

  // Subscribe to comment broadcast channel
  let mut rx = state.comment_broadcast.subscribe();

  // Spawn task to forward broadcasts to this client
  let audit_id_clone = audit_id.clone();
  let send_task = tokio::spawn(async move {
    while let Ok(event) = rx.recv().await {
      // Filter events for this audit
      if event.audit_id() == audit_id_clone {
        if let Ok(msg) = serde_json::to_string(&event) {
          if sender.send(Message::Text(msg.into())).await.is_err() {
            break;
          }
        }
      }
    }
  });

  // Handle incoming messages (ping/pong, close)
  let recv_task = tokio::spawn(async move {
    while let Some(msg) = receiver.next().await {
      match msg {
        Ok(Message::Close(_)) => break,
        Ok(Message::Ping(data)) => {
          // Pong is usually handled automatically, but we can be explicit
          let _ = data; // Acknowledge ping
        }
        Err(_) => break,
        _ => {}
      }
    }
  });

  // Wait for either task to complete
  tokio::select! {
      _ = send_task => {},
      _ = recv_task => {},
  }
}
