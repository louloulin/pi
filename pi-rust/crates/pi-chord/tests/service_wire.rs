//! Service wire-protocol tests, mirroring `packages/chord/test/service-wire.test.ts`.
//!
//! The upstream suite exercises the boundary a host (Stage 19 `pi-server`/`pi-client`) uses: the
//! control calls, the strict parsers, the per-subscription state codecs, and the endpoint that turns
//! provider subscriptions into published wire updates. Everything here is synchronous because
//! pi-chord has no runtime.

use std::sync::{Arc, Mutex};

use pi_chord::context::{background_context, Context};
use pi_chord::delta::{NonEmptyPath, Op};
use pi_chord::{
    create_remote_service_endpoint, create_service_catalogue_call, create_service_subscribe_call,
    create_service_unsubscribe_call, decode_service_control_call, define_service, parse_service_call,
    parse_service_catalogue, parse_service_provider_update, parse_service_subscription_snapshot,
    parse_wire_service_provider_update, parse_wire_service_subscription_snapshot, replicated_state,
    wire_ops_to_json_value, InstanceSnapshot, MemberSnapshot, ProviderUpdate, RemoteServiceProvider,
    ServiceControlCall, ServiceImplementation, ServiceInstanceAddress, ServiceMode,
    ServiceProviderEntry, ServiceProviderUpdate, ServiceStateDecoder, ServiceStateEncoder,
    ServiceSubscriptionSnapshot, ServiceUpdatePublisher, SubscriptionSnapshot,
    WireServiceProviderUpdate,
};
use serde_json::{json, Value};

type Json = Value;

/// The `ops` array of a wire state update, as strict JSON.
fn wire_ops(update: &WireServiceProviderUpdate) -> Json {
    match update {
        ProviderUpdate::State { ops, .. } => wire_ops_to_json_value(ops),
        other => panic!("expected a state update, got {other:?}"),
    }
}

fn set(member: &str, value: Json) -> ServiceProviderUpdate {
    ProviderUpdate::State {
        instance: None,
        member: member.to_owned(),
        sequence: 1,
        ops: vec![Op::Set(NonEmptyPath::from_keys(&["revision"]), value)],
    }
}

#[test]
fn encodes_control_calls_and_validates_service_values() {
    assert_eq!(
        decode_service_control_call(&create_service_catalogue_call()),
        Some(ServiceControlCall::Catalogue)
    );
    assert_eq!(
        parse_service_catalogue(&json!([
            { "serviceId": "pi.models", "mode": "singleton" },
            { "serviceId": "pi.dialogs", "mode": "keyed" },
        ]))
        .unwrap()
        .len(),
        2
    );
    assert_eq!(
        decode_service_control_call(&create_service_subscribe_call(
            "subscription-1",
            "pi.models",
            ServiceMode::Singleton,
        )),
        Some(ServiceControlCall::Subscribe {
            subscription_id: "subscription-1".into(),
            service_id: "pi.models".into(),
            mode: ServiceMode::Singleton,
        })
    );
    assert_eq!(
        decode_service_control_call(&create_service_unsubscribe_call("subscription-1")),
        Some(ServiceControlCall::Unsubscribe {
            subscription_id: "subscription-1".into(),
        })
    );
    assert_eq!(
        parse_service_call(&json!({
            "serviceId": "pi.question-dialog",
            "instance": { "key": "invocation-1", "generation": 2 },
            "member": "submit",
            "args": [{ "outcome": "selected", "index": 0 }],
        }))
        .unwrap()
        .member,
        "submit"
    );
}

#[test]
fn rejects_malformed_service_values() {
    let call = parse_service_call(&json!({
        "serviceId": "pi.models", "member": "list", "args": [], "extra": true,
    }))
    .unwrap_err();
    assert_eq!(call.to_string(), "Invalid service call");

    let catalogue = parse_service_catalogue(&json!([
        { "serviceId": "pi.models", "mode": "unknown" }
    ]))
    .unwrap_err();
    assert_eq!(catalogue.to_string(), "Invalid service catalogue");

    let update = parse_service_provider_update(&json!({
        "type": "state", "member": "state", "sequence": 0, "ops": [],
    }))
    .unwrap_err();
    assert_eq!(update.to_string(), "Invalid service state update");

    assert!(parse_wire_service_provider_update(&json!({
        "type": "state", "member": "state", "sequence": 1, "ops": [["?", 0]],
    }))
    .is_err());
}

#[test]
fn validates_decoded_and_wire_snapshots_and_updates() {
    let snapshot: ServiceSubscriptionSnapshot = SubscriptionSnapshot {
        service_id: "pi.models".into(),
        mode: ServiceMode::Singleton,
        instances: vec![InstanceSnapshot {
            instance: None,
            members: vec![MemberSnapshot::State {
                name: "state".into(),
                sequence: 0,
                ops: vec![Op::Replace(json!({ "revision": 1 }))],
            }],
        }],
    };
    let decoded = parse_service_subscription_snapshot(&snapshot.to_json()).unwrap();
    assert_eq!(decoded, snapshot);

    let mut encoder = ServiceStateEncoder::new();
    let wire_snapshot = encoder.encode_snapshot(snapshot).unwrap();
    assert_eq!(
        parse_wire_service_subscription_snapshot(&wire_snapshot.to_json()).unwrap(),
        wire_snapshot
    );

    let update = set("state", json!(2));
    let decoded = parse_service_provider_update(&update.to_json()).unwrap();
    assert_eq!(decoded, update);
    let wire_update = encoder.encode_update(update.clone()).unwrap();
    assert_eq!(
        parse_wire_service_provider_update(&wire_update.to_json()).unwrap(),
        wire_update
    );
}

#[test]
fn keeps_one_operation_codec_pair_for_one_subscription_state() {
    let mut enc = ServiceStateEncoder::new();
    let mut dec = ServiceStateDecoder::new();
    let snapshot: ServiceSubscriptionSnapshot = SubscriptionSnapshot {
        service_id: "pi.models".into(),
        mode: ServiceMode::Singleton,
        instances: vec![InstanceSnapshot {
            instance: None,
            members: vec![MemberSnapshot::State {
                name: "state".into(),
                sequence: 0,
                ops: vec![Op::Replace(json!({ "revision": 0 }))],
            }],
        }],
    };
    let wire = enc.encode_snapshot(snapshot.clone()).unwrap();
    assert_eq!(dec.decode_snapshot(&wire).unwrap(), snapshot);

    let first = ServiceProviderUpdate::State {
        instance: None,
        member: "state".into(),
        sequence: 1,
        ops: vec![Op::Set(NonEmptyPath::from_keys(&["revision"]), json!(1))],
    };
    let second = ServiceProviderUpdate::State {
        instance: None,
        member: "state".into(),
        sequence: 2,
        ops: vec![Op::Set(NonEmptyPath::from_keys(&["revision"]), json!(2))],
    };
    let first_wire = enc.encode_update(first.clone()).unwrap();
    let second_wire = enc.encode_update(second.clone()).unwrap();
    assert_eq!(wire_ops(&first_wire), json!([["s", ["revision"], 1]]));
    assert_eq!(
        wire_ops(&second_wire),
        json!([["#", 0, ["revision"]], ["s", 0, 2]])
    );
    assert_eq!(dec.decode_update(&first_wire).unwrap(), first);
    assert_eq!(dec.decode_update(&second_wire).unwrap(), second);
}

#[test]
fn isolates_operation_dictionaries_between_states_and_subscriptions() {
    let snapshot: ServiceSubscriptionSnapshot = SubscriptionSnapshot {
        service_id: "pi.states".into(),
        mode: ServiceMode::Singleton,
        instances: vec![InstanceSnapshot {
            instance: None,
            members: vec![
                MemberSnapshot::State {
                    name: "left".into(),
                    sequence: 0,
                    ops: vec![Op::Replace(json!({ "revision": 0 }))],
                },
                MemberSnapshot::State {
                    name: "right".into(),
                    sequence: 0,
                    ops: vec![Op::Replace(json!({ "revision": 0 }))],
                },
            ],
        }],
    };
    let mut first_encoder = ServiceStateEncoder::new();
    let mut first_decoder = ServiceStateDecoder::new();
    let mut second_encoder = ServiceStateEncoder::new();
    let mut second_decoder = ServiceStateDecoder::new();
    let first_wire = first_encoder.encode_snapshot(snapshot.clone()).unwrap();
    first_decoder.decode_snapshot(&first_wire).unwrap();
    let second_wire = second_encoder.encode_snapshot(snapshot).unwrap();
    second_decoder.decode_snapshot(&second_wire).unwrap();

    let update = |member: &str, sequence: u64, revision: i64| ServiceProviderUpdate::State {
        instance: None,
        member: member.to_owned(),
        sequence,
        ops: vec![Op::Set(
            NonEmptyPath::from_keys(&["revision"]),
            json!(revision),
        )],
    };
    let first_left = first_encoder.encode_update(update("left", 1, 1)).unwrap();
    let first_right = first_encoder.encode_update(update("right", 1, 1)).unwrap();
    let second_left = first_encoder.encode_update(update("left", 2, 2)).unwrap();
    let second_right = first_encoder.encode_update(update("right", 2, 2)).unwrap();
    assert_eq!(wire_ops(&first_left), json!([["s", ["revision"], 1]]));
    assert_eq!(wire_ops(&first_right), json!([["s", ["revision"], 1]]));
    assert_eq!(
        wire_ops(&second_left),
        json!([["#", 0, ["revision"]], ["s", 0, 2]])
    );
    assert_eq!(
        wire_ops(&second_right),
        json!([["#", 0, ["revision"]], ["s", 0, 2]])
    );
    assert_eq!(
        first_decoder.decode_update(&first_left).unwrap(),
        update("left", 1, 1)
    );
    assert_eq!(
        first_decoder.decode_update(&first_right).unwrap(),
        update("right", 1, 1)
    );
    assert_eq!(
        first_decoder.decode_update(&second_left).unwrap(),
        update("left", 2, 2)
    );
    assert_eq!(
        first_decoder.decode_update(&second_right).unwrap(),
        update("right", 2, 2)
    );

    let independent_left = second_encoder.encode_update(update("left", 1, 1)).unwrap();
    assert_eq!(wire_ops(&independent_left), json!([["s", ["revision"], 1]]));
    assert_eq!(
        second_decoder.decode_update(&independent_left).unwrap(),
        update("left", 1, 1)
    );

    let left_base = ServiceProviderUpdate::State {
        instance: None,
        member: "left".into(),
        sequence: 3,
        ops: vec![Op::Replace(json!({ "revision": 3 }))],
    };
    assert_eq!(
        first_decoder
            .decode_update(&first_encoder.encode_update(left_base.clone()).unwrap())
            .unwrap(),
        left_base
    );
    let third_right = first_encoder.encode_update(update("right", 3, 3)).unwrap();
    assert_eq!(wire_ops(&third_right), json!([["s", 0, 3]]));
    assert_eq!(
        first_decoder.decode_update(&third_right).unwrap(),
        update("right", 3, 3)
    );
}

#[test]
fn creates_and_removes_keyed_instance_codecs_with_their_lifecycle() {
    let mut enc = ServiceStateEncoder::new();
    let mut dec = ServiceStateDecoder::new();
    let snapshot: ServiceSubscriptionSnapshot = SubscriptionSnapshot {
        service_id: "pi.dialogs".into(),
        mode: ServiceMode::Keyed,
        instances: Vec::new(),
    };
    let wire = enc.encode_snapshot(snapshot.clone()).unwrap();
    assert_eq!(dec.decode_snapshot(&wire).unwrap(), snapshot);

    let address = ServiceInstanceAddress::new("dialog-1", 1);
    let spawned = ServiceProviderUpdate::Spawned {
        instance: InstanceSnapshot {
            instance: Some(address.clone()),
            members: vec![MemberSnapshot::State {
                name: "request".into(),
                sequence: 0,
                ops: vec![Op::Replace(json!({ "value": 0 }))],
            }],
        },
    };
    assert_eq!(
        dec.decode_update(&enc.encode_update(spawned.clone()).unwrap())
            .unwrap(),
        spawned
    );

    let update = ServiceProviderUpdate::State {
        instance: Some(address.clone()),
        member: "request".into(),
        sequence: 1,
        ops: vec![Op::Set(NonEmptyPath::from_keys(&["value"]), json!(1))],
    };
    assert_eq!(
        dec.decode_update(&enc.encode_update(update.clone()).unwrap())
            .unwrap(),
        update
    );

    let closed = ServiceProviderUpdate::Closed {
        instance: address,
    };
    assert_eq!(
        dec.decode_update(&enc.encode_update(closed.clone()).unwrap())
            .unwrap(),
        closed
    );

    let stale = ServiceProviderUpdate::State {
        instance: Some(ServiceInstanceAddress::new("dialog-1", 1)),
        member: "request".into(),
        sequence: 2,
        ops: vec![Op::Set(NonEmptyPath::from_keys(&["value"]), json!(2))],
    };
    let error = enc.encode_update(stale).unwrap_err();
    assert_eq!(error.to_string(), "Unknown service state dialog-1@1.request");
}

#[test]
fn remote_service_endpoints_publish_and_clean_up_provider_subscriptions() {
    let counter: pi_chord::Service<Json> = define_service("test.counter").expect("valid id");
    let provider = Arc::new(
        RemoteServiceProvider::new([ServiceProviderEntry::singleton(&counter)]).unwrap(),
    );
    let state = Arc::new(replicated_state(json!({ "value": 0 })).unwrap());
    provider
        .provide(
            &counter,
            ServiceImplementation::new().state("state", Arc::clone(&state)),
        )
        .unwrap();
    let endpoint = create_remote_service_endpoint(Arc::clone(&provider));

    let updates: Arc<Mutex<Vec<ServiceProviderUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let publish: ServiceUpdatePublisher = {
        let updates = Arc::clone(&updates);
        Arc::new(move |_subscription_id: &str, update: &ServiceProviderUpdate, _context: &Context| {
            updates.lock().expect("lock").push(update.clone());
        })
    };

    let catalogue = endpoint
        .invoke(&create_service_catalogue_call(), &publish, background_context())
        .unwrap();
    assert_eq!(
        catalogue,
        json!([{ "serviceId": counter.id(), "mode": "singleton" }])
    );

    let snapshot = endpoint
        .invoke(
            &create_service_subscribe_call("subscription-1", counter.id(), ServiceMode::Singleton),
            &publish,
            background_context(),
        )
        .unwrap();
    assert_eq!(snapshot["serviceId"], counter.id());
    assert_eq!(snapshot["mode"], "singleton");

    state.state_mut()["value"] = json!(1);
    state.publish(background_context()).unwrap();
    assert_eq!(
        *updates.lock().expect("lock"),
        vec![ProviderUpdate::State {
            instance: None,
            member: "state".into(),
            sequence: 1,
            ops: vec![Op::Set(NonEmptyPath::from_keys(&["value"]), json!(1))],
        }]
    );

    endpoint.dispose();
    state.state_mut()["value"] = json!(2);
    state.publish(background_context()).unwrap();
    assert_eq!(updates.lock().expect("lock").len(), 1);

    let error = endpoint
        .invoke(&create_service_catalogue_call(), &publish, background_context())
        .unwrap_err();
    assert_eq!(error.to_string(), "Remote service endpoint is disposed");
    provider.dispose().unwrap();
}
