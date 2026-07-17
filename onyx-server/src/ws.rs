//! Live push: one WebSocket per (device, vault) that receives the vault's
//! new head sequence whenever ops are appended. Deliberately tiny frames —
//! clients pull the actual ops over the existing HTTP lane, so there is
//! exactly one op-delivery code path.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use data_encoding::HEXLOWER;

use crate::ServerState;
use crate::auth::authenticate;

pub async fn live(
    State(state): State<Arc<ServerState>>,
    Path(vault): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let device = match authenticate(&state, &headers) {
        Ok(device) => device,
        Err(error) => return error.into_response(),
    };
    let vault: [u8; 16] = match HEXLOWER
        .decode(vault.as_bytes())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
    {
        Some(vault) => vault,
        None => return (StatusCode::BAD_REQUEST, "invalid vault id").into_response(),
    };
    match state.db.is_member(vault, device) {
        Ok(true) => {}
        Ok(false) => return (StatusCode::FORBIDDEN, "not a member").into_response(),
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    }

    let receiver = state.hub(vault).subscribe();
    upgrade.on_upgrade(move |socket| pump(socket, receiver))
}

async fn pump(mut socket: WebSocket, mut receiver: tokio::sync::broadcast::Receiver<u64>) {
    loop {
        tokio::select! {
            head = receiver.recv() => match head {
                Ok(head_seq) => {
                    if socket
                        .send(Message::Text(head_seq.to_string().into()))
                        .await
                        .is_err()
                    {
                        return; // client gone
                    }
                }
                // Missed some notifications under load: the exact head
                // doesn't matter, any nudge triggers a pull.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if socket.send(Message::Text("0".into())).await.is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
            message = socket.recv() => match message {
                // Pings/pongs are handled by axum; any other traffic is
                // ignored, close/error ends the session.
                Some(Ok(_)) => continue,
                _ => return,
            },
        }
    }
}
