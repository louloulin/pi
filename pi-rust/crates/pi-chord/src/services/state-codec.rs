//! Per-subscription state codecs, ported from `packages/chord/src/services/state-codec.ts`.
//!
//! A provider and its consumer share one [`Encoder`]/[`Decoder`] per replicated-state member: the
//! encoder interns paths on the way out, the decoder resolves them on the way in, and both reset
//! when a new snapshot arrives. The registry is a private detail, but it is load-bearing: without
//! it every wire batch would have to be self-contained and the interning payoff would vanish.
//!
//! The ports keep the exact upstream bookkeeping:
//!
//! * [`ServiceStateEncoder::encode_snapshot`] / [`ServiceStateDecoder::decode_snapshot`] reset the
//!   registry, then add one codec per state member;
//! * an update either reuses the codec for `(instance, member)`, resets for `replaced`/`unavailable`,
//!   adds a codec for `spawned`, or drops the instance codecs for `closed`;
//! * addressing the same `(instance, member)` twice is a duplicate error, addressing an unknown one
//!   is an unknown-state error.
//!
//! Upstream's `encodeSnapshot` is infallible only because a provider cannot build a snapshot with
//! duplicate member names. This port returns `Result` from every encoding entry point so a
//! hand-built snapshot fails with an error instead of a panic.

use std::collections::HashMap;

use crate::delta::{Decoder, Encoder, WireOp};
use crate::types::ServiceInstanceAddress;

use super::errors::ServiceError;
use super::wire::{
    InstanceSnapshot, MemberSnapshot, ProviderUpdate, ServiceInstanceSnapshot,
    ServiceProviderUpdate, ServiceSubscriptionSnapshot, SubscriptionSnapshot,
    WireServiceInstanceSnapshot, WireServiceProviderUpdate, WireServiceSubscriptionSnapshot,
};

/// Encodes every state member of one service subscription.
#[derive(Debug)]
pub struct ServiceStateEncoder {
    codecs: StateCodecRegistry<Encoder>,
}

impl Default for ServiceStateEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceStateEncoder {
    /// Creates an encoder with an empty registry.
    pub fn new() -> Self {
        Self {
            codecs: StateCodecRegistry::new(Encoder::new),
        }
    }

    /// Encodes a complete snapshot, resetting the registry.
    pub fn encode_snapshot(
        &mut self,
        snapshot: ServiceSubscriptionSnapshot,
    ) -> Result<WireServiceSubscriptionSnapshot, ServiceError> {
        self.codecs.reset();
        let instances = snapshot
            .instances
            .into_iter()
            .map(|instance| self.codecs.encode_instance(instance))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SubscriptionSnapshot {
            service_id: snapshot.service_id,
            mode: snapshot.mode,
            instances,
        })
    }

    /// Encodes one lifecycle update.
    pub fn encode_update(
        &mut self,
        update: ServiceProviderUpdate,
    ) -> Result<WireServiceProviderUpdate, ServiceError> {
        match update {
            ProviderUpdate::State {
                instance,
                member,
                sequence,
                ops,
            } => {
                let codec = self.codecs.get(instance.as_ref(), &member)?;
                Ok(ProviderUpdate::State {
                    instance,
                    member,
                    sequence,
                    ops: codec.encode(&ops),
                })
            }
            ProviderUpdate::Replaced { snapshot } => {
                self.codecs.reset();
                Ok(ProviderUpdate::Replaced {
                    snapshot: self.codecs.encode_instance(snapshot)?,
                })
            }
            ProviderUpdate::Spawned { instance } => Ok(ProviderUpdate::Spawned {
                instance: self.codecs.encode_instance(instance)?,
            }),
            ProviderUpdate::Unavailable => {
                self.codecs.reset();
                Ok(ProviderUpdate::Unavailable)
            }
            ProviderUpdate::Closed { instance } => {
                self.codecs.remove_instance(&instance);
                Ok(ProviderUpdate::Closed { instance })
            }
        }
    }
}

/// Decodes every state member of one service subscription.
#[derive(Debug)]
pub struct ServiceStateDecoder {
    codecs: StateCodecRegistry<Decoder>,
}

impl Default for ServiceStateDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceStateDecoder {
    /// Creates a decoder with an empty registry.
    pub fn new() -> Self {
        Self {
            codecs: StateCodecRegistry::new(Decoder::new),
        }
    }

    /// Decodes a complete snapshot, resetting the registry.
    pub fn decode_snapshot(
        &mut self,
        snapshot: &WireServiceSubscriptionSnapshot,
    ) -> Result<ServiceSubscriptionSnapshot, ServiceError> {
        self.codecs.reset();
        let instances = snapshot
            .instances
            .iter()
            .map(|instance| self.codecs.decode_instance(instance))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SubscriptionSnapshot {
            service_id: snapshot.service_id.clone(),
            mode: snapshot.mode,
            instances,
        })
    }

    /// Decodes one lifecycle update.
    pub fn decode_update(
        &mut self,
        update: &WireServiceProviderUpdate,
    ) -> Result<ServiceProviderUpdate, ServiceError> {
        match update {
            ProviderUpdate::State {
                instance,
                member,
                sequence,
                ops,
            } => {
                let codec = self.codecs.get(instance.as_ref(), member)?;
                Ok(ProviderUpdate::State {
                    instance: instance.clone(),
                    member: member.clone(),
                    sequence: *sequence,
                    ops: codec.decode(ops).map_err(ServiceError::from)?,
                })
            }
            ProviderUpdate::Replaced { snapshot } => {
                self.codecs.reset();
                Ok(ProviderUpdate::Replaced {
                    snapshot: self.codecs.decode_instance(snapshot)?,
                })
            }
            ProviderUpdate::Spawned { instance } => Ok(ProviderUpdate::Spawned {
                instance: self.codecs.decode_instance(instance)?,
            }),
            ProviderUpdate::Unavailable => {
                self.codecs.reset();
                Ok(ProviderUpdate::Unavailable)
            }
            ProviderUpdate::Closed { instance } => {
                self.codecs.remove_instance(instance);
                Ok(ProviderUpdate::Closed {
                    instance: instance.clone(),
                })
            }
        }
    }
}

/// Decodes a wire snapshot in one shot.
pub fn decode_wire_subscription_snapshot(
    snapshot: &WireServiceSubscriptionSnapshot,
) -> Result<ServiceSubscriptionSnapshot, ServiceError> {
    ServiceStateDecoder::new().decode_snapshot(snapshot)
}

/// Decodes a wire provider update in one shot.
pub fn decode_wire_provider_update(
    update: &WireServiceProviderUpdate,
) -> Result<ServiceProviderUpdate, ServiceError> {
    ServiceStateDecoder::new().decode_update(update)
}

/// Encodes a snapshot in one shot.
pub fn encode_subscription_snapshot(
    snapshot: ServiceSubscriptionSnapshot,
) -> Result<WireServiceSubscriptionSnapshot, ServiceError> {
    ServiceStateEncoder::new().encode_snapshot(snapshot)
}

struct CodecEntry<C> {
    instance: Option<ServiceInstanceAddress>,
    codec: C,
}

/// A `(instance, member)`-keyed codec table.
struct StateCodecRegistry<C> {
    create: fn() -> C,
    entries: HashMap<String, CodecEntry<C>>,
}

impl<C> std::fmt::Debug for StateCodecRegistry<C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StateCodecRegistry")
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl<C> StateCodecRegistry<C> {
    fn new(create: fn() -> C) -> Self {
        Self {
            create,
            entries: HashMap::new(),
        }
    }

    fn reset(&mut self) {
        self.entries.clear();
    }

    fn add(
        &mut self,
        instance: Option<ServiceInstanceAddress>,
        member: &str,
    ) -> Result<&mut C, ServiceError> {
        let key = state_key(instance.as_ref(), member);
        if self.entries.contains_key(&key) {
            return Err(ServiceError::message(format!(
                "Duplicate service state {}",
                describe_state(instance.as_ref(), member)
            )));
        }
        let codec = (self.create)();
        self.entries.insert(key.clone(), CodecEntry { instance, codec });
        Ok(&mut self.entries.get_mut(&key).expect("just inserted").codec)
    }

    fn get(
        &mut self,
        instance: Option<&ServiceInstanceAddress>,
        member: &str,
    ) -> Result<&mut C, ServiceError> {
        let key = state_key(instance, member);
        self.entries
            .get_mut(&key)
            .map(|entry| &mut entry.codec)
            .ok_or_else(|| {
                ServiceError::message(format!(
                    "Unknown service state {}",
                    describe_state(instance, member)
                ))
            })
    }

    fn remove_instance(&mut self, instance: &ServiceInstanceAddress) {
        self.entries
            .retain(|_, entry| !same_address(entry.instance.as_ref(), instance));
    }
}

impl StateCodecRegistry<Encoder> {
    fn encode_instance(
        &mut self,
        instance: ServiceInstanceSnapshot,
    ) -> Result<WireServiceInstanceSnapshot, ServiceError> {
        let address = instance.instance;
        let members = instance
            .members
            .into_iter()
            .map(|member| match member {
                MemberSnapshot::Method { name } => Ok(MemberSnapshot::Method { name }),
                MemberSnapshot::State {
                    name,
                    sequence,
                    ops,
                } => {
                    let codec = self.add(address.clone(), &name)?;
                    Ok(MemberSnapshot::State {
                        name,
                        sequence,
                        ops: codec.encode(&ops),
                    })
                }
            })
            .collect::<Result<Vec<_>, ServiceError>>()?;
        Ok(InstanceSnapshot {
            instance: address,
            members,
        })
    }
}

impl StateCodecRegistry<Decoder> {
    fn decode_instance(
        &mut self,
        instance: &WireServiceInstanceSnapshot,
    ) -> Result<ServiceInstanceSnapshot, ServiceError> {
        let address = instance.instance.clone();
        let members = instance
            .members
            .iter()
            .map(|member| match member {
                MemberSnapshot::Method { name } => Ok(MemberSnapshot::Method { name: name.clone() }),
                MemberSnapshot::State {
                    name,
                    sequence,
                    ops,
                } => {
                    let codec = self.add(address.clone(), name)?;
                    let ops: Vec<WireOp> = ops.clone();
                    Ok(MemberSnapshot::State {
                        name: name.clone(),
                        sequence: *sequence,
                        ops: codec.decode(&ops).map_err(ServiceError::from)?,
                    })
                }
            })
            .collect::<Result<Vec<_>, ServiceError>>()?;
        Ok(InstanceSnapshot {
            instance: address,
            members,
        })
    }
}

fn state_key(instance: Option<&ServiceInstanceAddress>, member: &str) -> String {
    let key = instance.map(|address| address.key.as_str());
    let generation = instance.map(|address| address.generation);
    serde_json::json!([key, generation, member]).to_string()
}

fn same_address(left: Option<&ServiceInstanceAddress>, right: &ServiceInstanceAddress) -> bool {
    left.map(|address| (address.key.as_str(), address.generation))
        == Some((right.key.as_str(), right.generation))
}

fn describe_state(instance: Option<&ServiceInstanceAddress>, member: &str) -> String {
    match instance {
        Some(address) => format!("{}@{}.{}", address.key, address.generation, member),
        None => member.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::{NonEmptyPath, Op};
    use crate::services::wire::ServiceMemberSnapshot;
    use crate::types::ServiceMode;

    fn state_snapshot() -> ServiceSubscriptionSnapshot {
        SubscriptionSnapshot {
            service_id: "pi.models".into(),
            mode: ServiceMode::Singleton,
            instances: vec![InstanceSnapshot {
                instance: None,
                members: vec![MemberSnapshot::State {
                    name: "state".into(),
                    sequence: 0,
                    ops: vec![Op::Replace(serde_json::json!({ "revision": 1 }))],
                }],
            }],
        }
    }

    #[test]
    fn a_snapshot_and_its_updates_round_trip_through_the_codec_pair() {
        let snapshot = state_snapshot();
        let mut encoder = ServiceStateEncoder::new();
        let mut decoder = ServiceStateDecoder::new();
        let wire = encoder.encode_snapshot(snapshot.clone()).unwrap();
        let decoded = decoder.decode_snapshot(&wire).unwrap();
        assert_eq!(decoded, snapshot);

        let update = ProviderUpdate::State {
            instance: None,
            member: "state".into(),
            sequence: 1,
            ops: vec![Op::Set(
                NonEmptyPath::from_keys(&["revision"]),
                serde_json::json!(2),
            )],
        };
        let wire = encoder.encode_update(update.clone()).unwrap();
        let decoded = decoder.decode_update(&wire).unwrap();
        assert_eq!(decoded, update);
    }

    #[test]
    fn unknown_states_and_duplicates_are_refused() {
        let mut encoder = ServiceStateEncoder::new();
        let unknown = encoder
            .encode_update(ProviderUpdate::State {
                instance: None,
                member: "state".into(),
                sequence: 1,
                ops: Vec::new(),
            })
            .unwrap_err();
        assert_eq!(unknown.to_string(), "Unknown service state state");

        let duplicate = ServiceSubscriptionSnapshot {
            service_id: "pi.models".into(),
            mode: ServiceMode::Singleton,
            instances: vec![InstanceSnapshot {
                instance: None,
                members: vec![
                    MemberSnapshot::State {
                        name: "state".into(),
                        sequence: 0,
                        ops: vec![Op::Replace(serde_json::json!({}))],
                    },
                    MemberSnapshot::State {
                        name: "state".into(),
                        sequence: 0,
                        ops: vec![Op::Replace(serde_json::json!({}))],
                    },
                ],
            }],
        };
        assert_eq!(
            encoder.encode_snapshot(duplicate).unwrap_err().to_string(),
            "Duplicate service state state"
        );
    }

    #[test]
    fn closed_drops_only_that_instance() {
        let mut decoder = ServiceStateDecoder::new();
        let wire = ServiceStateEncoder::new()
            .encode_snapshot(SubscriptionSnapshot {
                service_id: "pi.dialogs".into(),
                mode: ServiceMode::Keyed,
                instances: vec![
                    InstanceSnapshot {
                        instance: Some(ServiceInstanceAddress::new("a", 1)),
                        members: vec![ServiceMemberSnapshot::State {
                            name: "state".into(),
                            sequence: 0,
                            ops: vec![Op::Replace(serde_json::json!({ "a": 1 }))],
                        }],
                    },
                    InstanceSnapshot {
                        instance: Some(ServiceInstanceAddress::new("b", 1)),
                        members: vec![ServiceMemberSnapshot::State {
                            name: "state".into(),
                            sequence: 0,
                            ops: vec![Op::Replace(serde_json::json!({ "b": 1 }))],
                        }],
                    },
                ],
            })
            .unwrap();
        decoder.decode_snapshot(&wire).unwrap();
        let closed = ProviderUpdate::Closed {
            instance: ServiceInstanceAddress::new("a", 1),
        };
        assert!(decoder.decode_update(&closed).is_ok());
        let still_known = ProviderUpdate::State {
            instance: Some(ServiceInstanceAddress::new("b", 1)),
            member: "state".into(),
            sequence: 1,
            ops: vec![WireOp::Replace(serde_json::json!({ "b": 2 }))],
        };
        assert!(decoder.decode_update(&still_known).is_ok());
        let gone = ProviderUpdate::State {
            instance: Some(ServiceInstanceAddress::new("a", 1)),
            member: "state".into(),
            sequence: 1,
            ops: vec![WireOp::Replace(serde_json::json!({}))],
        };
        assert!(decoder.decode_update(&gone).is_err());
    }
}
