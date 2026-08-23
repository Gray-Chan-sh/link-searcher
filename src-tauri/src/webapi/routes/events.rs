use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
    Router,
};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::webapi::state::ApiState;

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new().route("/api/events", get(events_handler))
}

async fn events_handler(
    State(state): State<ApiState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|item| match item {
        Ok((name, payload)) => Some(Ok(Event::default().event(name).data(payload))),
        // Receiver lagged behind and missed N events; nothing sensible to
        // replay — skip the error frame so the SSE stream stays clean.
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}
