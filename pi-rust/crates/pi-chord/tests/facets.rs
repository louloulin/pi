//! Facet host contract tests: generation ordering, failure isolation, service resolution,
//! reload and disposal.
//!
//! Upstream `packages/chord/test/facets.test.ts` and `facet-loader.test.ts` cover the same ground.
//! These tests drive the public API only, and use the crate's minimal executor
//! ([`pi_chord::context::block_on`]) instead of a runtime dependency.

use std::sync::{Arc, Mutex};

use pi_chord::context::block_on;
use pi_chord::facets::{create_facet_host, HostPhase};
use pi_chord::types::{
    define_facet, define_local_service, define_service, FacetError, FacetOptions, Service,
};

type Events = Arc<Mutex<Vec<String>>>;

fn events() -> Events {
    Arc::new(Mutex::new(Vec::new()))
}

fn record(events: &Events, event: impl Into<String>) {
    events.lock().expect("lock").push(event.into());
}

fn seen(events: &Events) -> Vec<String> {
    events.lock().expect("lock").clone()
}

#[test]
fn activation_follows_dependencies_not_declaration_order() {
    let log = events();
    let count: Service<u32> = define_local_service("test.count").expect("valid id");
    let doubled: Service<u32> = define_local_service("test.doubled").expect("valid id");

    let provider = {
        let count = count.clone();
        let log = Arc::clone(&log);
        define_facet("provider", move |env| {
            record(&log, "provider:setup");
            env.provide(&count, 2u32)?;
            env.on_activate({
                let log = Arc::clone(&log);
                move || {
                    let log = Arc::clone(&log);
                    async move {
                        record(&log, "provider:active");
                        Ok(())
                    }
                }
            })?;
            Ok(())
        })
    };
    let consumer = {
        let count = count.clone();
        let doubled = doubled.clone();
        let log = Arc::clone(&log);
        define_facet("consumer", move |env| {
            record(&log, "consumer:setup");
            let handle = env.use_service(&count)?;
            // A handle exists from setup, but resolving through it is refused until the facet is
            // active.
            let early = handle
                .get()
                .expect_err("handles are gated during setup")
                .to_string();
            record(&log, format!("consumer:early={early}"));
            env.provide(&doubled, 0u32)?;
            env.on_activate({
                let log = Arc::clone(&log);
                move || {
                    let log = Arc::clone(&log);
                    async move {
                        let value = *handle.get().expect("active");
                        record(&log, format!("consumer:active={value}"));
                        Ok(())
                    }
                }
            })?;
            Ok(())
        })
    };

    // Declared in reverse dependency order on purpose.
    let host = block_on(create_facet_host(FacetOptions::new(vec![
        consumer, provider,
    ])))
    .expect("starts");
    assert_eq!(host.phase(), HostPhase::Active);
    assert_eq!(host.activation_order(), ["provider", "consumer"]);

    let log = seen(&log);
    // Setup runs in declaration order; activation runs in dependency order.
    assert_eq!(
        &log[..4],
        [
            "consumer:setup",
            "consumer:early=Facet consumer service handles cannot be used while setting_up",
            "provider:setup",
            "provider:active",
        ]
    );
    assert_eq!(log[4], "consumer:active=2");
}

#[test]
fn a_missing_dependency_is_reported_and_partial_setup_is_cleaned_up() {
    let log = events();
    let missing: Service<u32> = define_local_service("test.missing").expect("valid id");

    let earlier = {
        let log = Arc::clone(&log);
        define_facet("earlier", move |env| {
            env.own({
                let log = Arc::clone(&log);
                move || record(&log, "earlier:disposed")
            })?;
            Ok(())
        })
    };
    let dependent = {
        let missing = missing.clone();
        define_facet("dependent", move |env| {
            env.use_service(&missing)?;
            Ok(())
        })
    };

    let error = block_on(create_facet_host(FacetOptions::new(vec![
        earlier, dependent,
    ])))
    .expect_err("no provider");
    assert_eq!(
        error.to_string(),
        "Facet dependent requires local/test.missing/singleton, but no facet provides it"
    );
    assert_eq!(seen(&log), ["earlier:disposed"]);
}

#[test]
fn a_failing_setup_facet_aborts_the_generation() {
    let failing = define_facet("failing", |_env| Err(FacetError::new("setup exploded")));
    let error = block_on(create_facet_host(FacetOptions::new(vec![failing])))
        .expect_err("setup failed");
    assert_eq!(error.to_string(), "setup exploded");
}

#[test]
fn duplicate_providers_and_mode_mismatches_are_rejected() {
    let count: Service<u32> = define_local_service("test.count").expect("valid id");
    let first = {
        let count = count.clone();
        define_facet("first", move |env| {
            env.provide(&count, 1u32)?;
            Ok(())
        })
    };
    let second = {
        let count = count.clone();
        define_facet("second", move |env| {
            env.provide(&count, 2u32)?;
            Ok(())
        })
    };
    let error = block_on(create_facet_host(FacetOptions::new(vec![first, second])))
        .expect_err("duplicate provider");
    assert_eq!(
        error.to_string(),
        "Service test.count is provided by both first and second"
    );

    let singleton = {
        let count = count.clone();
        define_facet("singleton", move |env| {
            env.provide(&count, 1u32)?;
            Ok(())
        })
    };
    let keyed = {
        let count = count.clone();
        define_facet("keyed", move |env| {
            env.provide_many(&count)?;
            Ok(())
        })
    };
    let error = block_on(create_facet_host(FacetOptions::new(vec![singleton, keyed])))
        .expect_err("mode mismatch");
    assert_eq!(
        error.to_string(),
        "Service test.count is provided as both singleton and keyed"
    );
}

#[test]
fn a_dependency_cycle_is_rejected() {
    let left: Service<u32> = define_local_service("test.left").expect("valid id");
    let right: Service<u32> = define_local_service("test.right").expect("valid id");

    let first = {
        let left = left.clone();
        let right = right.clone();
        define_facet("first", move |env| {
            env.use_service(&right)?;
            env.provide(&left, 1u32)?;
            Ok(())
        })
    };
    let second = {
        let left = left.clone();
        let right = right.clone();
        define_facet("second", move |env| {
            env.use_service(&left)?;
            env.provide(&right, 2u32)?;
            Ok(())
        })
    };

    let error = block_on(create_facet_host(FacetOptions::new(vec![first, second])))
        .expect_err("cycle");
    assert_eq!(error.to_string(), "Facet dependency cycle: first, second");
}

#[test]
fn a_requirement_nothing_provides_is_not_satisfiable_by_a_host_singleton() {
    // A requirement is only satisfied by a facet provision in the same generation; the host
    // registry is for the host's own reads.
    let orphan: Service<u32> = define_service("test.orphan").expect("valid id");
    let consumer = {
        let orphan = orphan.clone();
        define_facet("consumer", move |env| {
            env.use_service(&orphan)?;
            Ok(())
        })
    };
    let error = block_on(create_facet_host(FacetOptions::new(vec![consumer])))
        .expect_err("unsatisfied");
    assert!(error
        .to_string()
        .starts_with("Facet consumer requires local/test.orphan/singleton"));
}

#[test]
fn disposal_runs_in_reverse_activation_order_and_aggregates_failures() {
    let log = events();
    let count: Service<u32> = define_local_service("test.count").expect("valid id");

    let provider = {
        let count = count.clone();
        let log = Arc::clone(&log);
        define_facet("provider", move |env| {
            env.provide(&count, 1u32)?;
            env.own({
                let log = Arc::clone(&log);
                move || record(&log, "provider:disposed")
            })?;
            Ok(())
        })
    };
    let consumer = {
        let count = count.clone();
        let log = Arc::clone(&log);
        define_facet("consumer", move |env| {
            env.use_service(&count)?;
            // Registered first, disposed last.
            env.own({
                let log = Arc::clone(&log);
                move || record(&log, "consumer:first")
            })?;
            env.own({
                let log = Arc::clone(&log);
                move || record(&log, "consumer:second")
            })?;
            env.own_async(|| async { Err(FacetError::new("cleanup failed")) })?;
            Ok(())
        })
    };

    let mut host = block_on(create_facet_host(FacetOptions::new(vec![
        provider, consumer,
    ])))
    .expect("starts");
    assert_eq!(host.facet_ids(), ["consumer", "provider"]);
    assert_eq!(*host.use_service(&count).expect("active"), 1);

    let error = block_on(host.dispose()).expect_err("one disposal failed");
    assert_eq!(error.to_string(), "Failed to dispose facet consumer: cleanup failed");
    assert_eq!(host.phase(), HostPhase::Dead);
    assert_eq!(
        seen(&log),
        ["consumer:second", "consumer:first", "provider:disposed"]
    );

    // Disposal is idempotent, and the service directory is emptied.
    assert!(block_on(host.dispose()).is_ok());
    let error = host.use_service(&count).expect_err("host is dead");
    assert_eq!(
        error.to_string(),
        "Facet host service handles cannot be used while dead"
    );
}

#[test]
fn reload_replaces_the_provider_for_existing_handles() {
    let count: Service<u32> = define_local_service("test.count").expect("valid id");

    let make = |value: u32| {
        let count = count.clone();
        define_facet("provider", move |env| {
            env.provide(&count, value)?;
            Ok(())
        })
    };

    let mut host = block_on(create_facet_host(FacetOptions::new(vec![make(1)]))).expect("starts");
    assert_eq!(*host.use_service(&count).expect("bound"), 1);

    block_on(host.reload(vec![make(2)])).expect("reloads");
    assert_eq!(host.phase(), HostPhase::Active);
    assert_eq!(*host.use_service(&count).expect("rebound"), 2);

    // A reload must preserve the facet's shape.
    let changed = define_facet("provider", |_env| Ok(()));
    let error = block_on(host.reload(vec![changed])).expect_err("shape changed");
    assert_eq!(
        error.to_string(),
        "Reloaded facet provider must preserve its service requirements and provisions"
    );
    assert_eq!(host.phase(), HostPhase::Active);
    assert_eq!(*host.use_service(&count).expect("still bound"), 2);
}

#[test]
fn reload_rejects_unknown_and_duplicate_facet_ids() {
    let count: Service<u32> = define_local_service("test.count").expect("valid id");
    let make = |id: &'static str| {
        let count = count.clone();
        define_facet(id, move |env| {
            env.provide(&count, 1u32)?;
            Ok(())
        })
    };
    let mut host = block_on(create_facet_host(FacetOptions::new(vec![make("provider")])))
        .expect("starts");

    let error = block_on(host.reload(vec![make("other")])).expect_err("unknown id");
    assert_eq!(error.to_string(), "Facet other is not active");

    // Duplicate ids are refused before the generation is touched.
    let duplicate: Service<u32> = define_local_service("test.other").expect("valid id");
    let first = {
        let duplicate = duplicate.clone();
        define_facet("provider", move |env| {
            env.provide(&duplicate, 1u32)?;
            Ok(())
        })
    };
    let second = {
        let duplicate = duplicate.clone();
        define_facet("provider", move |env| {
            env.provide(&duplicate, 2u32)?;
            Ok(())
        })
    };
    let error = block_on(host.reload(vec![first, second])).expect_err("duplicate ids");
    assert_eq!(error.to_string(), "Reloaded facet IDs must be unique");
}

#[test]
fn keyed_instances_reach_observers() {
    let log = events();
    let feed: Service<u32> = define_service("test.feed").expect("valid id");
    let spawner: Arc<Mutex<Option<Arc<pi_chord::facets::KeyedServiceSpawner<u32>>>>> =
        Arc::new(Mutex::new(None));

    let provider = {
        let feed = feed.clone();
        let spawner = Arc::clone(&spawner);
        let log = Arc::clone(&log);
        define_facet("provider", move |env| {
            let handle = env.provide_many(&feed)?;
            *spawner.lock().expect("lock") = Some(handle);
            env.own({
                let log = Arc::clone(&log);
                move || record(&log, "provider:disposed")
            })?;
            Ok(())
        })
    };
    let observer = {
        let feed = feed.clone();
        let log = Arc::clone(&log);
        define_facet("observer", move |env| {
            let log = Arc::clone(&log);
            env.observe(&feed, move |value: Arc<u32>, _context| {
                record(&log, format!("observed={value}"));
            })?;
            Ok(())
        })
    };

    let mut host = block_on(create_facet_host(FacetOptions::new(vec![
        provider, observer,
    ])))
    .expect("starts");
    assert_eq!(host.activation_order(), ["provider", "observer"]);
    assert_eq!(seen(&log), Vec::<String>::new(), "no instances yet");

    let spawner = spawner
        .lock()
        .expect("lock")
        .clone()
        .expect("the provider registered a spawner");
    spawner.spawn("one", 1u32).expect("spawns");
    spawner.spawn("two", 2u32).expect("spawns");
    assert_eq!(seen(&log), ["observed=1", "observed=2"]);

    // Spawning the same key twice is a conflict.
    let error = spawner.spawn("one", 3u32).expect_err("duplicate key");
    assert_eq!(
        error.to_string(),
        "Facet service test.feed already has a live instance with key one"
    );

    // Disposing the generation retires the instances and stops observation.
    block_on(host.dispose()).expect("disposes");
    spawner
        .spawn("three", 3u32)
        .expect_err("the facet is gone");
    assert_eq!(seen(&log), ["observed=1", "observed=2", "provider:disposed"]);
}

#[test]
fn a_panicking_observer_is_reported_without_breaking_the_provider() {
    let feed: Service<u32> = define_service("test.feed").expect("valid id");
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let provider = {
        let feed = feed.clone();
        define_facet("provider", move |env| {
            env.provide_many(&feed)?;
            Ok(())
        })
    };
    let observer = {
        let feed = feed.clone();
        define_facet("observer", move |env| {
            env.observe(&feed, |_value: Arc<u32>, _context| panic!("observer boom"))?;
            Ok(())
        })
    };
    let on_error = {
        let errors = Arc::clone(&errors);
        Arc::new(move |error: FacetError| {
            errors.lock().expect("lock").push(error.to_string());
        }) as Arc<dyn Fn(FacetError) + Send + Sync>
    };

    let mut host = block_on(create_facet_host(
        FacetOptions::new(vec![provider, observer]).with_on_error(on_error),
    ))
    .expect("starts");

    // The provider's spawner is not exposed here, so drive the slot through the host directory.
    let slot = host.services().keyed_slot("test.feed");
    slot.insert("one", Arc::new(1u32)).expect("inserts");
    let instance = slot.get::<u32>("one").expect("stored");
    let erased: pi_chord::facets::Erased = instance.clone();
    slot.notify("one", &erased, pi_chord::context::background_context());

    let errors = errors.lock().expect("lock");
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0],
        "Facet observer observer panicked: observer boom"
    );
    drop(errors);
    block_on(host.dispose()).expect("disposes");
}
