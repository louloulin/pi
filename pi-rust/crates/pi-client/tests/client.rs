//! Client behaviour tests driven by a scripted in-memory server.
//!
//! Each test states the protocol exchange it expects; the scripted peer encodes
//! nothing a real server would not send.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{
    connected, fail_next_request, next_cancel, next_message, pairing, server_target,
    OTHER_SERVER_ID, SERVER_ID,
};
use pi_chord::services::ServiceCall;
use pi_client::{
    service_listener, Client, ClientError, ClientOptions, ConnectionState, RequestCancel,
    ServiceMode, PROTOCOL_VERSION,
};
use pi_protocol::rpc::{ClientMessage, SessionTarget};
use serde_json::json;
use tokio::sync::{mpsc, Notify};

fn demo_call() -> ServiceCall {
    ServiceCall::new("pi.demo", "echo", vec![json!({ "n": 1 })])
}

#[tokio::test]
async fn connect_completes_the_handshake_and_reports_states() {
    let pairing = pairing();
    let client = Arc::new(pairing.client());
    let states = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&states);
    let _subscription = client
        .on_connection_state_change(Arc::new(move |change| {
            sink.lock().expect("states").push(change.state);
        }))
        .expect("listener");

    let connecting = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.connect().await }
    });
    let hello = pairing.server.accept_handshake(SERVER_ID).await;
    assert_eq!(hello, ClientMessage::hello(PROTOCOL_VERSION));

    let hello = connecting.await.expect("join").expect("handshake");
    assert_eq!(hello.version, PROTOCOL_VERSION);
    assert_eq!(hello.server_id, SERVER_ID);
    assert_eq!(
        *states.lock().expect("states"),
        vec![ConnectionState::Connecting, ConnectionState::Connected]
    );
    assert_eq!(client.connection_state(), ConnectionState::Connected);
    assert!(client.connected());
    assert_eq!(client.server_id(), SERVER_ID);
}

#[tokio::test]
async fn a_foreign_server_id_fails_the_handshake() {
    let error = pairing()
        .handshake(OTHER_SERVER_ID)
        .await
        .expect_err("mismatched server id");
    match error {
        ClientError::Protocol(message) => assert!(message.contains("does not match"), "{message}"),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn a_hello_error_fails_the_handshake() {
    let pairing = pairing();
    let factory = Arc::clone(&pairing.factory);
    let connecting =
        tokio::spawn(
            async move { Client::connect_with(ClientOptions::new(factory, SERVER_ID)).await },
        );
    pairing
        .server
        .reject_handshake("version", "unsupported")
        .await;
    let error = connecting.await.expect("join").expect_err("hello error");
    match error {
        ClientError::Server(error) => {
            assert_eq!(error.code, "version");
            assert_eq!(error.message, "unsupported");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn request_correlates_the_response() {
    let (client, server) = connected().await;
    let expected = server_target();
    let (result, ()) = tokio::join!(client.request(expected.clone(), demo_call(), None), async {
        match next_message(&server).await {
            ClientMessage::Request { id, target, call } => {
                assert_eq!(id, "request-1");
                assert_eq!(target, expected);
                assert_eq!(call["serviceId"], "pi.demo");
                assert_eq!(call["member"], "echo");
                assert_eq!(call["args"], json!([{ "n": 1 }]));
                server.respond_ok(&id, Some(json!({ "pong": true })));
            }
            other => panic!("expected a request, received {other:?}"),
        }
    });
    assert_eq!(result.expect("request"), Some(json!({ "pong": true })));
}

#[tokio::test]
async fn a_failed_response_surfaces_the_server_error() {
    let (client, server) = connected().await;
    let (result, ()) = tokio::join!(
        client.request(server_target(), demo_call(), None),
        fail_next_request(&server, "unavailable", "provider restarting")
    );
    match result.expect_err("failed response") {
        ClientError::Server(error) => {
            assert_eq!(error.code, "unavailable");
            assert_eq!(error.message, "provider restarting");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    // A failed call is not a failed connection.
    assert!(client.connected());
}

#[tokio::test]
async fn a_response_without_a_request_disconnects_the_client() {
    let (client, server) = connected().await;
    let disconnected = Arc::new(Notify::new());
    let signal = Arc::clone(&disconnected);
    let _subscription = client
        .on_connection_state_change(Arc::new(move |change| {
            if change.state == ConnectionState::Disconnected {
                signal.notify_one();
            }
        }))
        .expect("listener");

    server.respond_ok("request-404", None);
    tokio::time::timeout(Duration::from_secs(1), disconnected.notified())
        .await
        .expect("the client should report a disconnect");
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
    assert!(!client.connected());
}

#[tokio::test]
async fn connecting_twice_is_rejected() {
    let pairing = pairing();
    let client = pairing.connect().await;
    let error = client.connect().await.expect_err("already connected");
    match error {
        ClientError::Disconnected { message } => assert!(message.contains("already"), "{message}"),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn cancelling_sends_a_cancel_frame_and_fails_the_request() {
    let (client, server) = connected().await;
    let cancel = RequestCancel::new();
    let request = client.request(server_target(), demo_call(), Some(cancel.clone()));
    let (result, ()) = tokio::join!(request, async {
        match next_message(&server).await {
            ClientMessage::Request { id, .. } => {
                cancel.cancel();
                let cancelled = next_cancel(&server).await;
                assert_eq!(cancelled, id);
            }
            other => panic!("expected a request, received {other:?}"),
        }
    });
    assert!(matches!(result, Err(ClientError::Cancelled)));
    assert!(client.connected());
}

#[tokio::test]
async fn a_pre_cancelled_request_never_reaches_the_wire() {
    let (client, server) = connected().await;
    let cancel = RequestCancel::new();
    cancel.cancel();
    let error = client
        .request(server_target(), demo_call(), Some(cancel))
        .await
        .expect_err("pre-cancelled");
    assert!(matches!(error, ClientError::Cancelled));
    drop(server);
}

#[tokio::test]
async fn a_subscription_hydrates_then_streams_updates() {
    let (client, server) = connected().await;
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let listener = service_listener(move |update| {
        let updates = updates_tx.clone();
        async move {
            let _ = updates.send(format!("{update:?}"));
        }
    });

    let (subscription, ()) = tokio::join!(
        client.subscribe_service(
            server_target(),
            "pi.demo",
            ServiceMode::Singleton,
            listener,
            None
        ),
        async {
            match next_message(&server).await {
                ClientMessage::Request { id, call, .. } => {
                    assert_eq!(call["serviceId"], "$chord.service");
                    assert_eq!(call["member"], "subscribe");
                    assert_eq!(call["args"][0], "service-1");
                    assert_eq!(call["args"][1], "pi.demo");
                    assert_eq!(call["args"][2], "singleton");
                    server.respond_ok(
                        &id,
                        Some(json!({
                            "serviceId": "pi.demo",
                            "mode": "singleton",
                            "instances": [],
                        })),
                    );
                }
                other => panic!("expected a request, received {other:?}"),
            }
        }
    );
    let subscription = subscription.expect("subscribe");
    assert_eq!(subscription.id(), "service-1");
    assert_eq!(subscription.snapshot().service_id, "pi.demo");
    assert_eq!(subscription.snapshot().instances.len(), 0);

    subscription.start();
    server.service_update("service-1", json!({ "type": "unavailable" }));
    let update = tokio::time::timeout(Duration::from_secs(1), updates_rx.recv())
        .await
        .expect("update")
        .expect("channel");
    assert!(update.contains("Unavailable"), "{update}");

    let ((), ()) = tokio::join!(subscription.dispose(), async {
        match next_message(&server).await {
            ClientMessage::Request { id, call, .. } => {
                assert_eq!(call["member"], "unsubscribe");
                assert_eq!(call["args"][0], "service-1");
                server.respond_ok(&id, None);
            }
            other => panic!("expected an unsubscribe, received {other:?}"),
        }
    });
    assert!(subscription.disposed());
}

#[tokio::test]
async fn updates_that_race_the_snapshot_keep_wire_order() {
    let (client, server) = connected().await;
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let listener = service_listener(move |update| {
        let updates = updates_tx.clone();
        async move {
            let _ = updates.send(format!("{update:?}"));
        }
    });

    let (subscription, ()) = tokio::join!(
        client.subscribe_service(
            server_target(),
            "pi.demo",
            ServiceMode::Singleton,
            listener,
            None
        ),
        async {
            match next_message(&server).await {
                ClientMessage::Request { id, .. } => {
                    // The update arrives before the snapshot is answered.
                    server.service_update("service-1", json!({ "type": "unavailable" }));
                    server.respond_ok(
                        &id,
                        Some(json!({
                            "serviceId": "pi.demo",
                            "mode": "singleton",
                            "instances": [],
                        })),
                    );
                }
                other => panic!("expected a request, received {other:?}"),
            }
        }
    );
    let subscription = subscription.expect("subscribe");
    // Updates are buffered until the caller starts consuming.
    assert!(updates_rx.try_recv().is_err());
    subscription.start();
    let update = tokio::time::timeout(Duration::from_secs(1), updates_rx.recv())
        .await
        .expect("update")
        .expect("channel");
    assert!(update.contains("Unavailable"), "{update}");
}

#[tokio::test]
async fn attachment_updates_are_reported_and_validated() {
    let (client, server) = connected().await;
    let (changes_tx, mut changes_rx) = mpsc::unbounded_channel::<Option<SessionTarget>>();
    let _subscription = client
        .on_attachment_change(Arc::new(move |attachment| {
            let _ = changes_tx.send(attachment.cloned());
        }))
        .expect("listener");

    let session = SessionTarget {
        server_id: SERVER_ID.to_owned(),
        session_id: "session-1".to_owned(),
        attachment_id: "attachment-1".to_owned(),
    };
    server.attachment(Some(session.clone()));
    let attachment = tokio::time::timeout(Duration::from_secs(1), changes_rx.recv())
        .await
        .expect("attachment change")
        .expect("channel");
    assert_eq!(attachment, Some(session.clone()));
    assert_eq!(client.attachment(), Some(session));

    server.attachment(None);
    let attachment = tokio::time::timeout(Duration::from_secs(1), changes_rx.recv())
        .await
        .expect("detach")
        .expect("channel");
    assert_eq!(attachment, None);
    assert_eq!(client.attachment(), None);
}

#[tokio::test]
async fn an_attachment_for_another_server_fails_the_connection() {
    let (client, server) = connected().await;
    let disconnected = Arc::new(Notify::new());
    let signal = Arc::clone(&disconnected);
    let _subscription = client
        .on_connection_state_change(Arc::new(move |change| {
            if change.state == ConnectionState::Disconnected {
                signal.notify_one();
            }
        }))
        .expect("listener");

    server.attachment(Some(SessionTarget {
        server_id: OTHER_SERVER_ID.to_owned(),
        session_id: "session-1".to_owned(),
        attachment_id: "attachment-1".to_owned(),
    }));
    tokio::time::timeout(Duration::from_secs(1), disconnected.notified())
        .await
        .expect("the client should reject a foreign attachment");
    assert_eq!(client.attachment(), None);
}

#[tokio::test]
async fn the_service_catalogue_is_parsed() {
    let (client, server) = connected().await;
    let (catalogue, ()) = tokio::join!(client.service_catalogue(server_target(), None), async {
        match next_message(&server).await {
            ClientMessage::Request { id, call, .. } => {
                assert_eq!(call["member"], "catalogue");
                server.respond_ok(
                    &id,
                    Some(json!([
                        { "serviceId": "pi.demo", "mode": "singleton" },
                        { "serviceId": "pi.keyed", "mode": "keyed" },
                    ])),
                );
            }
            other => panic!("expected a request, received {other:?}"),
        }
    });
    let catalogue = catalogue.expect("catalogue");
    assert_eq!(catalogue.len(), 2);
    assert_eq!(catalogue[0].service_id, "pi.demo");
}

#[tokio::test]
async fn a_malformed_catalogue_fails_the_connection() {
    let (client, server) = connected().await;
    let (result, ()) = tokio::join!(client.service_catalogue(server_target(), None), async {
        match next_message(&server).await {
            ClientMessage::Request { id, .. } => {
                server.respond_ok(&id, Some(json!({ "not": "a catalogue" })));
            }
            other => panic!("expected a request, received {other:?}"),
        }
    });
    result.expect_err("malformed catalogue");
    // The response body is unreadable, so the client cannot trust later frames.
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
    drop(server);
}

#[tokio::test]
async fn dispose_rejects_pending_requests_and_is_idempotent() {
    let (client, server) = connected().await;
    let request = client.request(server_target(), demo_call(), None);
    let (result, ()) = tokio::join!(request, async {
        let _ = next_message(&server).await;
        client.dispose().await;
    });
    assert!(matches!(result, Err(ClientError::Disposed)));
    assert!(client.disposed());
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);

    // Idempotent, and later requests are rejected without touching the wire.
    client.dispose().await;
    let error = client
        .request(server_target(), demo_call(), None)
        .await
        .expect_err("disposed");
    assert!(matches!(error, ClientError::Disposed));
    assert!(client.on_connection_state_change(Arc::new(|_| {})).is_err());
}

#[tokio::test]
async fn a_panicking_listener_is_reported_without_killing_the_client() {
    let hits = Arc::new(AtomicUsize::new(0));
    let reported = Arc::clone(&hits);
    let (client, server) = connected().await;
    let _subscription = client
        .on_connection_state_change(Arc::new(move |_change| {
            reported.fetch_add(1, Ordering::SeqCst);
            panic!("listener exploded");
        }))
        .expect("listener");

    // A disconnect notification runs the panicking listener; the client must
    // survive it and still report the state.
    client.disconnect("boom");
    assert_eq!(client.connection_state(), ConnectionState::Disconnected);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    drop(server);
}

#[tokio::test]
async fn listener_panics_reach_the_configured_handler() {
    let errors = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    let pairing = pairing();
    let options = ClientOptions::new(Arc::clone(&pairing.factory), SERVER_ID)
        .with_listener_error_handler(Arc::new(move |error| {
            sink.lock().expect("errors").push(error.to_string());
        }));
    let client = Client::new(options).expect("client");
    let _subscription = client
        .on_connection_state_change(Arc::new(|change| {
            assert_ne!(
                change.state,
                ConnectionState::Disconnected,
                "listener exploded"
            );
        }))
        .expect("listener");

    let (hello, ()) = tokio::join!(client.connect(), async {
        pairing.server.accept_handshake(SERVER_ID).await;
    });
    hello.expect("handshake");
    assert!(errors.lock().expect("errors").is_empty());

    client.disconnect("bye");
    let errors = errors.lock().expect("errors");
    assert_eq!(errors.len(), 1, "{errors:?}");
    // The handler receives the client's bounded message, not the listener's
    // arbitrary panic payload.
    assert!(
        errors[0].contains("Connection state listener panicked"),
        "{}",
        errors[0]
    );
}
