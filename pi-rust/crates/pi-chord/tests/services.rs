//! Remote service integration tests, mirroring `packages/chord/test/services.test.ts`.
//!
//! The upstream suite drives an async provider/consumer pair. This port is synchronous (pi-chord
//! is deliberately runtime-free), so `await` disappears and every assertion runs inline. Each test
//! keeps the upstream name and the upstream observable behaviour; the only documented deviation is
//! that "buffered while subscriptions are starting" is expressed with an explicit `activate` call
//! instead of a raced promise.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_chord::context::{background_context, Context};
use pi_chord::delta::Op;
use pi_chord::{
    create_loopback_service_transport, create_remote_service_binding, define_local_service,
    define_service, replicated_state, MemberSnapshot, ProviderUpdate, RemoteServiceBinding,
    RemoteServiceBindingOptions, RemoteServiceErrorCode, RemoteServiceProvider,
    RemoteServiceTransport, Service, ServiceCall, ServiceError, ServiceImplementation, ServiceMode,
    ServiceProviderEntry, ServiceProviderListener, ServiceSubscription,
    ServiceSubscriptionSnapshot, SubscriptionSnapshot,
};
use serde_json::{json, Value};

type Json = Value;

fn models() -> Service<Json> {
    define_service("test.models").expect("valid id")
}

fn dialogs() -> Service<Json> {
    define_service("test.question-dialog").expect("valid id")
}

fn echo_service() -> Service<Json> {
    define_service("test.echo").expect("valid id")
}

fn models_provider() -> (
    Arc<RemoteServiceProvider>,
    Arc<pi_chord::MutableReplicatedState<Json>>,
) {
    let provider = Arc::new(
        RemoteServiceProvider::new([ServiceProviderEntry::singleton(&models())]).expect("provider"),
    );
    let state =
        Arc::new(replicated_state(json!({ "selected": null, "revision": 0 })).expect("state"));
    provider
        .provide(
            &models(),
            ServiceImplementation::new()
                .state("state", Arc::clone(&state))
                .method("select", {
                    let state = Arc::clone(&state);
                    move |args, context| {
                        let mut guard = state.state_mut();
                        guard["selected"] = args[0].clone();
                        let revision = guard["revision"].as_u64().unwrap_or(0) + 1;
                        guard["revision"] = json!(revision);
                        drop(guard);
                        state.publish(context)?;
                        Ok(Value::Null)
                    }
                }),
        )
        .expect("provide");
    (provider, state)
}

fn models_binding(provider: &Arc<RemoteServiceProvider>) -> RemoteServiceBinding {
    let transport: Arc<dyn RemoteServiceTransport> =
        create_loopback_service_transport(Arc::clone(provider));
    create_remote_service_binding(RemoteServiceBindingOptions::new(transport).service(&models()))
        .expect("binding")
}

#[test]
fn marks_services_remotable_by_default_and_reserves_chord_ids() {
    let local: Service<u32> = define_local_service("test.local").expect("valid id");
    assert!(!models().is_local());
    assert!(local.is_local());
    assert!(define_service::<u32>("$chord.internal").is_err());
    assert!(RemoteServiceProvider::new([ServiceProviderEntry::singleton(&local)]).is_err());
}

#[test]
fn checks_the_catalogue_shape() {
    let provider = models_provider().0;
    let catalogue = provider.catalogue();
    assert_eq!(catalogue.len(), 1);
    assert_eq!(catalogue[0].service_id, models().id());
    assert_eq!(catalogue[0].mode, ServiceMode::Singleton);

    assert!(RemoteServiceProvider::new([
        ServiceProviderEntry::singleton(&models()),
        ServiceProviderEntry::singleton(&models()),
    ])
    .is_err());
}

#[test]
fn round_trips_calls_over_the_loopback_transport() {
    let service = echo_service();
    let provider = RemoteServiceProvider::new([ServiceProviderEntry::singleton(&service)]).unwrap();
    let received = Arc::new(Mutex::new(None));
    provider
        .provide(
            &service,
            ServiceImplementation::new().method("echo", {
                let received = Arc::clone(&received);
                move |args, _context| {
                    *received.lock().expect("lock") = Some(args[0].clone());
                    Ok(json!({ "value": "response" }))
                }
            }),
        )
        .unwrap();
    let transport: Arc<dyn RemoteServiceTransport> =
        create_loopback_service_transport(Arc::new(provider));
    let binding = create_remote_service_binding(
        RemoteServiceBindingOptions::new(transport).service(&service),
    )
    .unwrap();
    let proxy = binding.use_service(&service).unwrap();
    binding.ready().unwrap();

    let request = json!({ "value": "request" });
    let response = proxy
        .call("echo", std::slice::from_ref(&request), background_context())
        .unwrap();
    assert_eq!(response, json!({ "value": "response" }));
    assert_eq!(*received.lock().expect("lock"), Some(request));
}

#[test]
fn shares_one_singleton_facade_with_replicated_state() {
    let (provider, _state) = models_provider();
    let binding = models_binding(&provider);

    let first = binding.use_service(&models()).unwrap();
    let second = binding.use_service(&models()).unwrap();
    assert!(first.is_same(&second));
    // The port is synchronous, so the subscription starts inline: the state is already hydrated
    // instead of staying `None` until `ready()` (the documented async-to-sync deviation).
    assert_eq!(
        first.state("state").unwrap().value().unwrap(),
        Some(json!({ "selected": null, "revision": 0 }))
    );

    binding.ready().unwrap();
    let state = first.state("state").unwrap();
    assert_eq!(
        state.value().unwrap(),
        Some(json!({ "selected": null, "revision": 0 }))
    );

    let updates = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&updates);
    let _subscription = state
        .subscribe(move |value, _context| {
            seen.lock().expect("lock").push(value.clone());
        })
        .unwrap();

    first
        .call(
            "select",
            &[json!({ "provider": "test", "modelId": "one" })],
            background_context(),
        )
        .unwrap();
    assert_eq!(
        state.value().unwrap(),
        Some(json!({ "selected": { "provider": "test", "modelId": "one" }, "revision": 1 }))
    );
    assert_eq!(
        *updates.lock().expect("lock"),
        vec![
            json!({ "selected": null, "revision": 0 }),
            json!({ "selected": { "provider": "test", "modelId": "one" }, "revision": 1 }),
        ]
    );
}

#[test]
fn keeps_facades_stable_across_withdraw_and_replace() {
    let (provider, _state) = models_provider();
    let binding = models_binding(&provider);
    let proxy = binding.use_service(&models()).unwrap();
    let handle = proxy.state("state").unwrap();
    binding.ready().unwrap();
    assert_eq!(handle.value().unwrap().unwrap()["revision"], json!(0));

    provider.withdraw(&models()).unwrap();
    assert_eq!(handle.value().unwrap(), None);
    let error = proxy
        .call("select", &[json!(null)], background_context())
        .unwrap_err();
    assert!(
        error.is_code(RemoteServiceErrorCode::ServiceNotFound),
        "{error}"
    );

    let published = Arc::new(AtomicUsize::new(0));
    provider
        .replace(
            &models(),
            ServiceImplementation::new()
                .state(
                    "state",
                    Arc::new(replicated_state(json!({ "selected": null, "revision": 2 })).unwrap()),
                )
                .method("select", {
                    let published = Arc::clone(&published);
                    move |_args, _context| {
                        published.fetch_add(1, Ordering::SeqCst);
                        Ok(Value::Null)
                    }
                }),
        )
        .unwrap();

    assert!(binding.use_service(&models()).unwrap().is_same(&proxy));
    assert_eq!(handle.value().unwrap().unwrap()["revision"], json!(2));
    proxy
        .call("select", &[json!(null)], background_context())
        .unwrap();
    assert_eq!(published.load(Ordering::SeqCst), 1);

    // The replacement shape must match: dropping the state member is refused.
    let error = provider
        .replace(
            &models(),
            ServiceImplementation::new().method("select", |_args, _context| Ok(Value::Null)),
        )
        .unwrap_err();
    assert!(
        error.is_code(RemoteServiceErrorCode::ServiceMemberMismatch),
        "{error}"
    );
    assert_eq!(handle.value().unwrap().unwrap()["revision"], json!(2));

    // Late bindings still see the current revision.
    let late = models_binding(&provider);
    let late_models = late.use_service(&models()).unwrap();
    late.ready().unwrap();
    assert_eq!(
        late_models
            .state("state")
            .unwrap()
            .value()
            .unwrap()
            .unwrap()["revision"],
        json!(2)
    );
}

#[test]
fn maps_transport_failures_to_remote_codes() {
    let service = echo_service();
    let provider = RemoteServiceProvider::new([ServiceProviderEntry::singleton(&service)]).unwrap();
    provider
        .provide(
            &service,
            ServiceImplementation::new().method("echo", |_args, _context| Ok(Value::Null)),
        )
        .unwrap();

    let call = ServiceCall {
        service_id: service.id().to_owned(),
        instance: None,
        member: "missing".to_owned(),
        args: Vec::new(),
    };
    let error = provider.invoke(&call, background_context()).unwrap_err();
    assert!(
        error.is_code(RemoteServiceErrorCode::ServiceMemberNotFound),
        "{error}"
    );

    let unknown = ServiceCall {
        service_id: "test.unknown".to_owned(),
        instance: None,
        member: "echo".to_owned(),
        args: Vec::new(),
    };
    let error = provider.invoke(&unknown, background_context()).unwrap_err();
    assert!(
        error.is_code(RemoteServiceErrorCode::ServiceNotAllowed),
        "{error}"
    );

    let keyed = ServiceCall {
        service_id: service.id().to_owned(),
        instance: Some(pi_chord::ServiceInstanceAddress::new("one", 1)),
        member: "echo".to_owned(),
        args: Vec::new(),
    };
    let error = provider.invoke(&keyed, background_context()).unwrap_err();
    assert!(
        error.is_code(RemoteServiceErrorCode::ServiceModeMismatch),
        "{error}"
    );

    // A singleton member used as method is a member mismatch.
    let (provider, _state) = models_provider();
    let state_call = ServiceCall {
        service_id: models().id().to_owned(),
        instance: None,
        member: "state".to_owned(),
        args: Vec::new(),
    };
    let error = provider
        .invoke(&state_call, background_context())
        .unwrap_err();
    assert!(
        error.is_code(RemoteServiceErrorCode::ServiceMemberMismatch),
        "{error}"
    );

    // Spawning a key is refused for a singleton.
    let error = provider
        .spawn(
            &models(),
            "wrong",
            ServiceImplementation::new().method("select", |_args, _context| Ok(Value::Null)),
        )
        .unwrap_err();
    assert!(
        error.is_code(RemoteServiceErrorCode::ServiceModeMismatch),
        "{error}"
    );
}

#[test]
fn delivers_active_subscriber_updates_before_reporting_listener_failures() {
    let service = models();
    let provider = RemoteServiceProvider::new([ServiceProviderEntry::singleton(&service)]).unwrap();
    provider
        .provide(
            &service,
            ServiceImplementation::new()
                .state(
                    "state",
                    Arc::new(replicated_state(json!({ "selected": null, "revision": 1 })).unwrap()),
                )
                .method("select", |_args, _context| Ok(Value::Null)),
        )
        .unwrap();

    let delivered = Arc::new(AtomicUsize::new(0));
    let failing: ServiceProviderListener =
        Arc::new(|_update, _context| Err(ServiceError::message("listener failed")));
    let succeeding: ServiceProviderListener = Arc::new({
        let delivered = Arc::clone(&delivered);
        move |_update, _context| {
            delivered.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    });
    let failing = provider
        .subscribe(service.id(), ServiceMode::Singleton, failing)
        .unwrap();
    let succeeding = provider
        .subscribe(service.id(), ServiceMode::Singleton, succeeding)
        .unwrap();
    failing.activate().unwrap();
    succeeding.activate().unwrap();

    let error = provider
        .replace(
            &service,
            ServiceImplementation::new()
                .state(
                    "state",
                    Arc::new(replicated_state(json!({ "selected": null, "revision": 2 })).unwrap()),
                )
                .method("select", |_args, _context| Ok(Value::Null)),
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "listener failed");
    assert_eq!(delivered.load(Ordering::SeqCst), 1);

    failing.close();
    succeeding.close();
}

#[test]
fn replays_every_buffered_update_before_reporting_listener_failures() {
    let service = models();
    let provider = RemoteServiceProvider::new([ServiceProviderEntry::singleton(&service)]).unwrap();
    let state = Arc::new(replicated_state(json!({ "selected": null, "revision": 0 })).unwrap());
    provider
        .provide(
            &service,
            ServiceImplementation::new()
                .state("state", Arc::clone(&state))
                .method("select", |_args, _context| Ok(Value::Null)),
        )
        .unwrap();

    let delivered = Arc::new(AtomicUsize::new(0));
    let listener: ServiceProviderListener = Arc::new({
        let delivered = Arc::clone(&delivered);
        move |_update, _context| {
            delivered.fetch_add(1, Ordering::SeqCst);
            Err(ServiceError::message("listener failed"))
        }
    });
    let subscription = provider
        .subscribe(service.id(), ServiceMode::Singleton, listener)
        .unwrap();
    state.state_mut()["revision"] = json!(1);
    state.publish(background_context()).unwrap();
    state.state_mut()["revision"] = json!(2);
    state.publish(background_context()).unwrap();

    let error = subscription.activate().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Failed to activate remote service subscription"),
        "{error}"
    );
    assert_eq!(delivered.load(Ordering::SeqCst), 2);
    subscription.close();
}

#[test]
fn stops_delivering_after_a_subscription_is_closed() {
    let service = echo_service();
    let provider = RemoteServiceProvider::new([ServiceProviderEntry::singleton(&service)]).unwrap();
    let state = Arc::new(replicated_state(json!({ "revision": 0 })).unwrap());
    provider
        .provide(
            &service,
            ServiceImplementation::new()
                .state("state", Arc::clone(&state))
                .method("echo", |_args, _context| Ok(Value::Null)),
        )
        .unwrap();

    let updates = Arc::new(AtomicUsize::new(0));
    let listener: ServiceProviderListener = Arc::new({
        let updates = Arc::clone(&updates);
        move |update, _context| {
            if matches!(update, ProviderUpdate::State { .. }) {
                updates.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
    });
    let subscription = provider
        .subscribe(service.id(), ServiceMode::Singleton, listener)
        .unwrap();
    subscription.activate().unwrap();

    state.state_mut()["revision"] = json!(1);
    state.publish(background_context()).unwrap();
    assert_eq!(updates.load(Ordering::SeqCst), 1);
    subscription.close();
    state.state_mut()["revision"] = json!(2);
    state.publish(background_context()).unwrap();
    assert_eq!(updates.load(Ordering::SeqCst), 1);
}

#[test]
fn buffers_updates_that_race_subscription_hydration() {
    let service = models();
    let provider =
        Arc::new(RemoteServiceProvider::new([ServiceProviderEntry::singleton(&service)]).unwrap());
    let state = Arc::new(replicated_state(json!({ "selected": null, "revision": 0 })).unwrap());
    provider
        .provide(
            &service,
            ServiceImplementation::new()
                .state("state", Arc::clone(&state))
                .method("select", |_args, _context| Ok(Value::Null)),
        )
        .unwrap();

    let transport: Arc<dyn RemoteServiceTransport> = Arc::new(RacingTransport {
        provider: Arc::clone(&provider),
        state: Arc::clone(&state),
    });
    let binding = create_remote_service_binding(
        RemoteServiceBindingOptions::new(transport).service(&service),
    )
    .unwrap();
    let models = binding.use_service(&service).unwrap();
    let revisions = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&revisions);
    let _subscription = models
        .state("state")
        .unwrap()
        .subscribe(move |value, _context| {
            seen.lock()
                .expect("lock")
                .push(value["revision"].as_u64().unwrap_or(0));
        })
        .unwrap();
    binding.ready().unwrap();
    // The synchronous port has already replayed the raced update, so a subscriber registered
    // afterwards observes only the converged value.
    assert_eq!(*revisions.lock().expect("lock"), vec![1]);
    assert_eq!(
        models.state("state").unwrap().value().unwrap().unwrap()["revision"],
        json!(1)
    );
}

/// A loopback transport that publishes a state update between `subscribe` and `activate`.
struct RacingTransport {
    provider: Arc<RemoteServiceProvider>,
    state: Arc<pi_chord::MutableReplicatedState<Json>>,
}

impl RemoteServiceTransport for RacingTransport {
    fn invoke(&self, call: &ServiceCall, context: &Context) -> Result<Value, ServiceError> {
        self.provider.invoke(call, context)
    }

    fn subscribe(
        &self,
        service_id: &str,
        mode: ServiceMode,
        listener: ServiceProviderListener,
    ) -> Result<Arc<dyn ServiceSubscription>, ServiceError> {
        let subscription = self.provider.subscribe(service_id, mode, listener)?;
        self.state.state_mut()["revision"] = json!(1);
        self.state.publish(background_context()).unwrap();
        Ok(subscription)
    }
}

#[test]
fn clears_replicated_state_after_a_duplicate_or_gap_sequence() {
    for sequence in [0_u64, 2] {
        let service = models();
        let transport = Arc::new(ScriptedTransport::new(service.id()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let binding = create_remote_service_binding(
            RemoteServiceBindingOptions::new(
                Arc::clone(&transport) as Arc<dyn RemoteServiceTransport>
            )
            .service(&service)
            .on_error(Arc::new({
                let errors = Arc::clone(&errors);
                move |error| errors.lock().expect("lock").push(error)
            })),
        )
        .unwrap();
        let models = binding.use_service(&service).unwrap();
        binding.ready().unwrap();
        assert_eq!(
            models.state("state").unwrap().value().unwrap().unwrap()["revision"],
            json!(0)
        );

        transport.send(ProviderUpdate::State {
            instance: None,
            member: "state".to_owned(),
            sequence,
            ops: vec![Op::Replace(
                json!({ "selected": null, "revision": sequence }),
            )],
        });
        assert_eq!(models.state("state").unwrap().value().unwrap(), None);
        let reported = errors.lock().expect("lock");
        assert_eq!(reported.len(), 1, "sequence {sequence}");
        assert!(
            reported[0].to_string().contains("sequence"),
            "{}",
            reported[0]
        );
    }
}

/// A transport that returns a fixed snapshot and lets the test push later updates.
struct ScriptedTransport {
    listener: Mutex<Option<ServiceProviderListener>>,
    snapshot: ServiceSubscriptionSnapshot,
}

impl ScriptedTransport {
    fn new(service_id: &str) -> Self {
        Self {
            listener: Mutex::new(None),
            snapshot: SubscriptionSnapshot {
                service_id: service_id.to_owned(),
                mode: ServiceMode::Singleton,
                instances: vec![pi_chord::InstanceSnapshot {
                    instance: None,
                    members: vec![MemberSnapshot::State {
                        name: "state".to_owned(),
                        sequence: 0,
                        ops: vec![Op::Replace(json!({ "selected": null, "revision": 0 }))],
                    }],
                }],
            },
        }
    }

    fn send(&self, update: ProviderUpdate<Op>) {
        let listener = self.listener.lock().expect("lock").clone();
        if let Some(listener) = listener {
            listener(&update, background_context()).expect("listener");
        }
    }
}

impl RemoteServiceTransport for ScriptedTransport {
    fn invoke(&self, _call: &ServiceCall, _context: &Context) -> Result<Value, ServiceError> {
        Err(ServiceError::message("unexpected invocation"))
    }

    fn subscribe(
        &self,
        _service_id: &str,
        _mode: ServiceMode,
        listener: ServiceProviderListener,
    ) -> Result<Arc<dyn ServiceSubscription>, ServiceError> {
        *self.listener.lock().expect("lock") = Some(listener);
        Ok(Arc::new(ScriptedSubscription {
            snapshot: self.snapshot.clone(),
        }))
    }
}

struct ScriptedSubscription {
    snapshot: ServiceSubscriptionSnapshot,
}

impl ServiceSubscription for ScriptedSubscription {
    fn snapshot(&self) -> ServiceSubscriptionSnapshot {
        self.snapshot.clone()
    }

    fn activate(&self) -> Result<(), ServiceError> {
        Ok(())
    }

    fn close(&self) {}
}

#[test]
fn rejects_readiness_when_initial_hydration_fails() {
    struct FailingTransport;
    impl RemoteServiceTransport for FailingTransport {
        fn invoke(&self, _call: &ServiceCall, _context: &Context) -> Result<Value, ServiceError> {
            Err(ServiceError::message("unexpected invocation"))
        }
        fn subscribe(
            &self,
            _service_id: &str,
            _mode: ServiceMode,
            _listener: ServiceProviderListener,
        ) -> Result<Arc<dyn ServiceSubscription>, ServiceError> {
            Err(ServiceError::message("initial hydration failed"))
        }
    }
    let service = models();
    let errors = Arc::new(Mutex::new(Vec::new()));
    let binding = create_remote_service_binding(
        RemoteServiceBindingOptions::new(Arc::new(FailingTransport))
            .service(&service)
            .on_error(Arc::new({
                let errors = Arc::clone(&errors);
                move |error| errors.lock().expect("lock").push(error)
            })),
    )
    .unwrap();
    let models = binding.use_service(&service).unwrap();
    let error = binding.ready().unwrap_err();
    assert_eq!(error.to_string(), "initial hydration failed");
    assert_eq!(models.state("state").unwrap().value().unwrap(), None);
    assert_eq!(errors.lock().expect("lock").len(), 1);
}

#[test]
fn defers_handles_until_the_host_activates_them() {
    let (provider, _state) = models_provider();
    let transport: Arc<dyn RemoteServiceTransport> =
        create_loopback_service_transport(Arc::clone(&provider));
    let active = Arc::new(AtomicBool::new(false));
    let binding = create_remote_service_binding(
        RemoteServiceBindingOptions::new(transport)
            .service(&models())
            .bound(false)
            .assert_access(Arc::new({
                let active = Arc::clone(&active);
                move || {
                    if active.load(Ordering::SeqCst) {
                        Ok(())
                    } else {
                        Err(ServiceError::message("Service handles are not active"))
                    }
                }
            })),
    )
    .unwrap();
    let models = binding.use_service(&models()).unwrap();
    assert!(models.state("state").unwrap().value().is_err());
    assert!(models.state("state").unwrap().subscribe(|_, _| {}).is_err());
    assert!(models
        .call("select", &[json!(null)], background_context())
        .is_err());

    binding.rebind(true).unwrap();
    active.store(true, Ordering::SeqCst);
    binding.ready().unwrap();
    assert_eq!(
        models.state("state").unwrap().value().unwrap().unwrap()["revision"],
        json!(0)
    );
}

#[test]
fn rebinds_cold_replicas_across_disconnects() {
    let (provider, state) = models_provider();
    let transport: Arc<dyn RemoteServiceTransport> =
        create_loopback_service_transport(Arc::clone(&provider));
    let binding = create_remote_service_binding(
        RemoteServiceBindingOptions::new(transport)
            .service(&models())
            .bound(false),
    )
    .unwrap();
    let models = binding.use_service(&models()).unwrap();
    let revisions = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&revisions);
    let _subscription = models
        .state("state")
        .unwrap()
        .subscribe(move |value, _context| {
            seen.lock()
                .expect("lock")
                .push(value["revision"].as_u64().unwrap_or(0));
        })
        .unwrap();
    assert_eq!(models.state("state").unwrap().value().unwrap(), None);
    assert!(revisions.lock().expect("lock").is_empty());

    state.state_mut()["revision"] = json!(1);
    state.publish(background_context()).unwrap();
    binding.rebind(true).unwrap();
    assert_eq!(
        models.state("state").unwrap().value().unwrap().unwrap()["revision"],
        json!(1)
    );
    assert_eq!(*revisions.lock().expect("lock"), vec![1]);

    binding.rebind(false).unwrap();
    assert_eq!(models.state("state").unwrap().value().unwrap(), None);
    state.state_mut()["revision"] = json!(2);
    state.publish(background_context()).unwrap();
    assert_eq!(*revisions.lock().expect("lock"), vec![1]);

    binding.rebind(true).unwrap();
    assert_eq!(
        models.state("state").unwrap().value().unwrap().unwrap()["revision"],
        json!(2)
    );
    assert_eq!(*revisions.lock().expect("lock"), vec![1, 2]);
}

#[test]
fn clears_facades_when_the_provider_and_binding_are_disposed() {
    let (provider, _state) = models_provider();
    let binding = models_binding(&provider);
    let models = binding.use_service(&models()).unwrap();
    let state = models.state("state").unwrap();
    binding.ready().unwrap();
    assert_eq!(state.value().unwrap().unwrap()["revision"], json!(0));

    provider.dispose().unwrap();
    assert_eq!(state.value().unwrap(), None);
    let error = models
        .call("select", &[json!(null)], background_context())
        .unwrap_err();
    assert_eq!(error.to_string(), "Remote service provider is disposed");

    binding.dispose().unwrap();
    let error = state.value().unwrap_err();
    assert_eq!(error.to_string(), "Remote service binding is disposed");
}

#[test]
fn hydrates_keyed_state_before_observe_handlers_and_fences_reused_keys() {
    let service = dialogs();
    let provider =
        Arc::new(RemoteServiceProvider::new([ServiceProviderEntry::keyed(&service)]).unwrap());
    assert_eq!(provider.catalogue()[0].mode, ServiceMode::Keyed);
    let transport: Arc<dyn RemoteServiceTransport> =
        create_loopback_service_transport(Arc::clone(&provider));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let binding = create_remote_service_binding(
        RemoteServiceBindingOptions::new(transport)
            .service(&service)
            .on_error(Arc::new({
                let errors = Arc::clone(&errors);
                move |error| errors.lock().expect("lock").push(error)
            })),
    )
    .unwrap();

    #[derive(Clone)]
    struct Observed {
        question: Option<Json>,
        service: pi_chord::KeyedServiceProxy,
        submit: Option<pi_chord::RemoteMethodHandle>,
        context: Context,
    }

    let observed: Arc<Mutex<Vec<Observed>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&observed);
    let observation = binding
        .observe_service(&service, move |service, context| {
            let question = service
                .state("request")
                .and_then(|handle| handle.value())
                .ok()
                .flatten();
            let submit = service.method("submit").ok();
            captured.lock().expect("lock").push(Observed {
                question,
                service,
                submit,
                context: context.clone(),
            });
        })
        .unwrap();
    binding.ready().unwrap();
    assert!(errors.lock().expect("lock").is_empty());

    let first_state = Arc::new(replicated_state(json!({ "question": "First?" })).unwrap());
    let first_submissions = Arc::new(AtomicUsize::new(0));
    let first = provider
        .spawn(
            &service,
            "invocation-1",
            ServiceImplementation::new()
                .state("request", Arc::clone(&first_state))
                .method("submit", {
                    let counter = Arc::clone(&first_submissions);
                    move |_args, _context| {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(json!({ "accepted": true }))
                    }
                }),
        )
        .unwrap();
    assert_eq!(observed.lock().expect("lock").len(), 1);
    assert_eq!(
        observed.lock().expect("lock")[0].question,
        Some(json!({ "question": "First?" }))
    );

    let first_service = observed.lock().expect("lock")[0].service.clone();
    first_state.state_mut()["question"] = json!("Updated?");
    first_state.publish(background_context()).unwrap();
    assert_eq!(
        first_service.state("request").unwrap().value().unwrap(),
        Some(json!({ "question": "Updated?" }))
    );
    let accepted = first_service
        .call("submit", &[json!("yes")], background_context())
        .unwrap();
    assert_eq!(accepted, json!({ "accepted": true }));
    assert_eq!(first_submissions.load(Ordering::SeqCst), 1);

    let retained_submit = observed.lock().expect("lock")[0]
        .submit
        .clone()
        .expect("captured method handle");
    let first_context = observed.lock().expect("lock")[0].context.clone();
    first.close().unwrap();
    assert!(first_context
        .abort_signal()
        .is_some_and(pi_chord::context::AbortSignal::is_aborted));
    let error = first_service.state("request").unwrap_err();
    assert_eq!(
        error.code(),
        Some(RemoteServiceErrorCode::ServiceStaleInstance)
    );
    assert!(error.to_string().contains("observation is closed"));
    let error = retained_submit
        .call(&[json!("late")], background_context())
        .unwrap_err();
    assert!(error.to_string().contains("observation is closed"));

    let second_state = Arc::new(replicated_state(json!({ "question": "Again?" })).unwrap());
    let second = provider
        .spawn(
            &service,
            "invocation-1",
            ServiceImplementation::new()
                .state("request", Arc::clone(&second_state))
                .method("submit", |_args, _context| Ok(json!({ "accepted": false }))),
        )
        .unwrap();
    assert_eq!(observed.lock().expect("lock").len(), 2);
    assert_eq!(
        observed.lock().expect("lock")[1].question,
        Some(json!({ "question": "Again?" }))
    );

    let second_service = observed.lock().expect("lock")[1].service.clone();
    let retained_second_submit = observed.lock().expect("lock")[1]
        .submit
        .clone()
        .expect("captured method handle");
    observation.stop();
    let error = second_service.state("request").unwrap_err();
    assert!(error.to_string().contains("observation is closed"));
    let error = retained_second_submit
        .call(&[json!("late")], background_context())
        .unwrap_err();
    assert!(error.to_string().contains("observation is closed"));
    assert!(errors.lock().expect("lock").is_empty());

    second.close().unwrap();
}

#[test]
fn rejects_unsupported_keyed_members() {
    let service = dialogs();
    let provider = RemoteServiceProvider::new([ServiceProviderEntry::keyed(&service)]).unwrap();
    let error = provider
        .spawn(&service, "invalid", ServiceImplementation::new())
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Remote service test.question-dialog has no members"
    );
    let error = provider
        .spawn(
            &service,
            "invalid",
            ServiceImplementation::new().method("", |_args, _context| Ok(Value::Null)),
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("member with an empty name"),
        "{error}"
    );
}
