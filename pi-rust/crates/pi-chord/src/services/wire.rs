//! Service wire protocol — Rust port of `packages/chord/src/services/wire.ts`.
//!
//! Two vocabularies travel across the remote-service boundary:
//!
//! * the **decoded** vocabulary (`ServiceCall`, `ServiceProviderUpdate<Op>`) that a transport
//!   carries in-process, and
//! * the **wire** vocabulary (`WireServiceProviderUpdate`, `WireOp`) whose paths are interned by a
//!   stateful [`crate::delta::Encoder`]/[`crate::delta::Decoder`] pair.
//!
//! Upstream keeps them as structural TypeScript types and validates untrusted input with a pile of
//! `assert*` functions. Rust keeps the two vocabularies as generics over the operation type and
//! ports the same validators, so a payload that fails a JavaScript assertion fails here too, with
//! the same message.
//!
//! The only shape that differs from upstream is the optional `instance` address: it is modelled
//! with `Option<ServiceInstanceAddress>` instead of a possibly-`undefined` property, which is the
//! same information.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::delta::{
    assert_valid_op, assert_valid_wire_op, ops_from_json, ops_to_json, wire_ops_from_json,
    wire_ops_to_json, DeltaError, Op, WireOp,
};
use crate::json::JsonValue;
use crate::types::{ServiceCatalogueEntry, ServiceInstanceAddress, ServiceMode};

use super::errors::ServiceError;

/// Bridges the two operation vocabularies to their JSON form.
///
/// Implemented for [`Op`] (validated with `assertValidOp`) and [`WireOp`] (validated with
/// `assertValidWireOp`), so the snapshot and update structures below can be written once.
pub trait WireOpCodec: Clone + std::fmt::Debug + PartialEq + Send + Sync + 'static {
    /// The operation as strict JSON.
    fn op_to_json(&self) -> JsonValue;
    /// Parses and validates one operation.
    fn op_from_json(value: &JsonValue) -> Result<Self, DeltaError>;
    /// Validates an untrusted operation value without building it.
    fn assert_op(value: &JsonValue) -> Result<(), DeltaError>;
}

impl WireOpCodec for Op {
    fn op_to_json(&self) -> JsonValue {
        Op::to_json(self)
    }

    fn op_from_json(value: &JsonValue) -> Result<Self, DeltaError> {
        Op::from_json(value)
    }

    fn assert_op(value: &JsonValue) -> Result<(), DeltaError> {
        assert_valid_op(value)
    }
}

impl WireOpCodec for WireOp {
    fn op_to_json(&self) -> JsonValue {
        WireOp::to_json(self)
    }

    fn op_from_json(value: &JsonValue) -> Result<Self, DeltaError> {
        WireOp::from_json(value)
    }

    fn assert_op(value: &JsonValue) -> Result<(), DeltaError> {
        assert_valid_wire_op(value)
    }
}

/// The kind of one remote-service member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServiceMemberKind {
    /// A callable method.
    Method,
    /// A replicated-state member.
    State,
}

impl ServiceMemberKind {
    /// The wire/display spelling (`"method"` or `"state"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceMemberKind::Method => "method",
            ServiceMemberKind::State => "state",
        }
    }

    /// Parses a wire spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "method" => Some(ServiceMemberKind::Method),
            "state" => Some(ServiceMemberKind::State),
            _ => None,
        }
    }
}

impl std::fmt::Display for ServiceMemberKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One member of a service instance snapshot (upstream `ServiceMemberSnapshot`).
#[derive(Clone, Debug, PartialEq)]
pub enum MemberSnapshot<O> {
    /// A method: only its name crosses the wire.
    Method {
        /// The member name.
        name: String,
    },
    /// A replicated state: name, sequence and the base batch that reproduces it.
    State {
        /// The member name.
        name: String,
        /// The published sequence.
        sequence: u64,
        /// The delta batch (a single `r` on the provider side).
        ops: Vec<O>,
    },
}

/// A decoded member snapshot.
pub type ServiceMemberSnapshot = MemberSnapshot<Op>;
/// A wire-encoded member snapshot.
pub type WireServiceMemberSnapshot = MemberSnapshot<WireOp>;

impl<O: WireOpCodec> MemberSnapshot<O> {
    /// The member name.
    pub fn name(&self) -> &str {
        match self {
            MemberSnapshot::Method { name } | MemberSnapshot::State { name, .. } => name,
        }
    }

    /// The member kind.
    pub fn kind(&self) -> ServiceMemberKind {
        match self {
            MemberSnapshot::Method { .. } => ServiceMemberKind::Method,
            MemberSnapshot::State { .. } => ServiceMemberKind::State,
        }
    }

    /// Serialises the member as strict JSON.
    pub fn to_json(&self) -> JsonValue {
        match self {
            MemberSnapshot::Method { name } => {
                let mut map = Map::new();
                map.insert("name".into(), Value::String(name.clone()));
                map.insert("kind".into(), Value::String("method".into()));
                Value::Object(map)
            }
            MemberSnapshot::State {
                name,
                sequence,
                ops,
            } => {
                let mut map = Map::new();
                map.insert("name".into(), Value::String(name.clone()));
                map.insert("kind".into(), Value::String("state".into()));
                map.insert("sequence".into(), Value::from(*sequence));
                map.insert("ops".into(), ops_to_json_generic(ops));
                Value::Object(map)
            }
        }
    }

    /// Parses and validates a member snapshot.
    pub fn from_json(value: &JsonValue) -> Result<Self, ServiceError> {
        let map = object(value, "service member snapshot")?;
        match map.get("kind").and_then(Value::as_str) {
            Some("method") => {
                assert_keys(map, &["name", "kind"], &[], "service method snapshot")?;
                let name = id_string(map, "name", "service method snapshot")?;
                Ok(MemberSnapshot::Method { name })
            }
            Some("state") => {
                assert_keys(
                    map,
                    &["name", "kind", "sequence", "ops"],
                    &[],
                    "service state snapshot",
                )?;
                let name = id_string(map, "name", "service state snapshot")?;
                let sequence = integer(map, "sequence", 0, "service state snapshot")?;
                let ops = parse_ops_generic::<O>(map.get("ops"), "service state snapshot")?;
                Ok(MemberSnapshot::State {
                    name,
                    sequence,
                    ops,
                })
            }
            _ => Err(invalid("service member snapshot")),
        }
    }
}

/// A snapshot of one service instance (upstream `ServiceInstanceSnapshot`).
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceSnapshot<O> {
    /// The instance address, absent for singletons.
    pub instance: Option<ServiceInstanceAddress>,
    /// The instance members.
    pub members: Vec<MemberSnapshot<O>>,
}

/// A decoded instance snapshot.
pub type ServiceInstanceSnapshot = InstanceSnapshot<Op>;
/// A wire-encoded instance snapshot.
pub type WireServiceInstanceSnapshot = InstanceSnapshot<WireOp>;

impl<O: WireOpCodec> InstanceSnapshot<O> {
    /// Serialises the snapshot as strict JSON.
    pub fn to_json(&self) -> JsonValue {
        let mut map = Map::new();
        if let Some(instance) = &self.instance {
            map.insert("instance".into(), address_to_json(instance));
        }
        map.insert(
            "members".into(),
            Value::Array(self.members.iter().map(MemberSnapshot::to_json).collect()),
        );
        Value::Object(map)
    }

    /// Parses and validates an instance snapshot.
    pub fn from_json(value: &JsonValue) -> Result<Self, ServiceError> {
        let map = object(value, "service instance snapshot")?;
        assert_keys(map, &["members"], &["instance"], "service instance snapshot")?;
        let instance = match map.get("instance") {
            Some(value) => Some(parse_address(value)?),
            None => None,
        };
        let members = map
            .get("members")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("service instance snapshot"))?
            .iter()
            .map(MemberSnapshot::from_json)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(InstanceSnapshot { instance, members })
    }
}

/// A snapshot of one subscription (upstream `ServiceSubscriptionSnapshot`).
#[derive(Clone, Debug, PartialEq)]
pub struct SubscriptionSnapshot<O> {
    /// The subscribed service id.
    pub service_id: String,
    /// The subscribed mode.
    pub mode: ServiceMode,
    /// The live instances, empty for an unavailable singleton.
    pub instances: Vec<InstanceSnapshot<O>>,
}

/// A decoded subscription snapshot.
pub type ServiceSubscriptionSnapshot = SubscriptionSnapshot<Op>;
/// A wire-encoded subscription snapshot.
pub type WireServiceSubscriptionSnapshot = SubscriptionSnapshot<WireOp>;

impl<O: WireOpCodec> SubscriptionSnapshot<O> {
    /// Serialises the snapshot as strict JSON.
    pub fn to_json(&self) -> JsonValue {
        let mut map = Map::new();
        map.insert("serviceId".into(), Value::String(self.service_id.clone()));
        map.insert("mode".into(), Value::String(self.mode.as_str().into()));
        map.insert(
            "instances".into(),
            Value::Array(self.instances.iter().map(InstanceSnapshot::to_json).collect()),
        );
        Value::Object(map)
    }

    /// Parses and validates a subscription snapshot.
    pub fn from_json(value: &JsonValue) -> Result<Self, ServiceError> {
        let map = object(value, "service subscription snapshot")?;
        assert_keys(
            map,
            &["serviceId", "mode", "instances"],
            &[],
            "service subscription snapshot",
        )?;
        let service_id = id_string(map, "serviceId", "service subscription snapshot")?;
        let mode = parse_mode(map.get("mode")).ok_or_else(|| invalid("service subscription snapshot"))?;
        let instances = map
            .get("instances")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("service subscription snapshot"))?
            .iter()
            .map(InstanceSnapshot::from_json)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SubscriptionSnapshot {
            service_id,
            mode,
            instances,
        })
    }
}

/// A lifecycle update from a provider (upstream `ServiceProviderUpdate`).
#[derive(Clone, Debug, PartialEq)]
pub enum ProviderUpdate<O> {
    /// A state member published a batch.
    State {
        /// The instance address, absent for singletons.
        instance: Option<ServiceInstanceAddress>,
        /// The state member name.
        member: String,
        /// The published sequence.
        sequence: u64,
        /// The delta batch.
        ops: Vec<O>,
    },
    /// A singleton lost its provider but keeps its remote facade.
    Unavailable,
    /// A singleton was replaced.
    Replaced {
        /// The replacement snapshot.
        snapshot: InstanceSnapshot<O>,
    },
    /// A keyed instance was created.
    Spawned {
        /// The new instance snapshot.
        instance: InstanceSnapshot<O>,
    },
    /// A keyed instance was closed.
    Closed {
        /// The closed instance address.
        instance: ServiceInstanceAddress,
    },
}

/// A decoded provider update.
pub type ServiceProviderUpdate = ProviderUpdate<Op>;
/// A wire-encoded provider update.
pub type WireServiceProviderUpdate = ProviderUpdate<WireOp>;

impl<O: WireOpCodec> ProviderUpdate<O> {
    /// Serialises the update as strict JSON.
    pub fn to_json(&self) -> JsonValue {
        match self {
            ProviderUpdate::State {
                instance,
                member,
                sequence,
                ops,
            } => {
                let mut map = Map::new();
                map.insert("type".into(), Value::String("state".into()));
                if let Some(instance) = instance {
                    map.insert("instance".into(), address_to_json(instance));
                }
                map.insert("member".into(), Value::String(member.clone()));
                map.insert("sequence".into(), Value::from(*sequence));
                map.insert("ops".into(), ops_to_json_generic(ops));
                Value::Object(map)
            }
            ProviderUpdate::Unavailable => {
                let mut map = Map::new();
                map.insert("type".into(), Value::String("unavailable".into()));
                Value::Object(map)
            }
            ProviderUpdate::Replaced { snapshot } => {
                let mut map = Map::new();
                map.insert("type".into(), Value::String("replaced".into()));
                map.insert("snapshot".into(), snapshot.to_json());
                Value::Object(map)
            }
            ProviderUpdate::Spawned { instance } => {
                let mut map = Map::new();
                map.insert("type".into(), Value::String("spawned".into()));
                map.insert("instance".into(), instance.to_json());
                Value::Object(map)
            }
            ProviderUpdate::Closed { instance } => {
                let mut map = Map::new();
                map.insert("type".into(), Value::String("closed".into()));
                map.insert("instance".into(), address_to_json(instance));
                Value::Object(map)
            }
        }
    }

    /// Parses and validates a provider update.
    pub fn from_json(value: &JsonValue) -> Result<Self, ServiceError> {
        let map = object(value, "service provider update")?;
        match map.get("type").and_then(Value::as_str) {
            Some("state") => {
                assert_keys(
                    map,
                    &["type", "member", "sequence", "ops"],
                    &["instance"],
                    "state update",
                )?;
                let member = id_string(map, "member", "service state update")?;
                let sequence = integer(map, "sequence", 1, "service state update")?;
                let ops = parse_ops_generic::<O>(map.get("ops"), "service state update")?;
                let instance = match map.get("instance") {
                    Some(value) => Some(parse_address(value)?),
                    None => None,
                };
                Ok(ProviderUpdate::State {
                    instance,
                    member,
                    sequence,
                    ops,
                })
            }
            Some("unavailable") => {
                assert_keys(map, &["type"], &[], "unavailable update")?;
                Ok(ProviderUpdate::Unavailable)
            }
            Some("replaced") => {
                assert_keys(map, &["type", "snapshot"], &[], "replacement update")?;
                let snapshot = InstanceSnapshot::from_json(
                    map.get("snapshot")
                        .ok_or_else(|| invalid("service instance snapshot"))?,
                )?;
                Ok(ProviderUpdate::Replaced { snapshot })
            }
            Some("spawned") => {
                assert_keys(map, &["type", "instance"], &[], "spawn update")?;
                let instance = InstanceSnapshot::from_json(
                    map.get("instance")
                        .ok_or_else(|| invalid("service instance snapshot"))?,
                )?;
                Ok(ProviderUpdate::Spawned { instance })
            }
            Some("closed") => {
                assert_keys(map, &["type", "instance"], &[], "close update")?;
                let instance = parse_address(
                    map.get("instance")
                        .ok_or_else(|| invalid("service instance address"))?,
                )?;
                Ok(ProviderUpdate::Closed { instance })
            }
            _ => Err(invalid("service provider update")),
        }
    }
}

/// A remote service call (upstream `ServiceCall`).
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceCall {
    /// The target service id.
    pub service_id: String,
    /// The keyed instance address, absent for singletons.
    pub instance: Option<ServiceInstanceAddress>,
    /// The member name.
    pub member: String,
    /// The borrowed arguments.
    pub args: Vec<JsonValue>,
}

impl ServiceCall {
    /// Creates a call.
    pub fn new(service_id: impl Into<String>, member: impl Into<String>, args: Vec<JsonValue>) -> Self {
        Self {
            service_id: service_id.into(),
            instance: None,
            member: member.into(),
            args,
        }
    }

    /// Serialises the call as strict JSON.
    pub fn to_json(&self) -> JsonValue {
        let mut map = Map::new();
        map.insert("serviceId".into(), Value::String(self.service_id.clone()));
        if let Some(instance) = &self.instance {
            map.insert("instance".into(), address_to_json(instance));
        }
        map.insert("member".into(), Value::String(self.member.clone()));
        map.insert("args".into(), Value::Array(self.args.clone()));
        Value::Object(map)
    }

    /// Parses and validates a call.
    pub fn from_json(value: &JsonValue) -> Result<Self, ServiceError> {
        let map = object(value, "service call")?;
        assert_keys(map, &["serviceId", "member", "args"], &["instance"], "service call")?;
        let service_id = id_string(map, "serviceId", "service call")?;
        let member = id_string(map, "member", "service call")?;
        let args = map
            .get("args")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("service call"))?
            .clone();
        let instance = match map.get("instance") {
            Some(value) => Some(parse_address(value)?),
            None => None,
        };
        Ok(ServiceCall {
            service_id,
            instance,
            member,
            args,
        })
    }
}

/// The reserved control service id.
pub const SERVICE_CONTROL_ID: &str = "$chord.service";
/// The catalogue control member.
pub const SERVICE_CATALOGUE_MEMBER: &str = "catalogue";
/// The subscribe control member.
pub const SERVICE_SUBSCRIBE_MEMBER: &str = "subscribe";
/// The unsubscribe control member.
pub const SERVICE_UNSUBSCRIBE_MEMBER: &str = "unsubscribe";

/// A decoded control call (upstream `ServiceControlCall`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceControlCall {
    /// List the provider catalogue.
    Catalogue,
    /// Open a subscription.
    Subscribe {
        /// The caller-chosen subscription id.
        subscription_id: String,
        /// The subscribed service id.
        service_id: String,
        /// The subscribed mode.
        mode: ServiceMode,
    },
    /// Close a subscription.
    Unsubscribe {
        /// The subscription id to close.
        subscription_id: String,
    },
}

/// Builds the catalogue control call.
pub fn create_service_catalogue_call() -> ServiceCall {
    ServiceCall::new(SERVICE_CONTROL_ID, SERVICE_CATALOGUE_MEMBER, Vec::new())
}

/// Builds a subscribe control call.
pub fn create_service_subscribe_call(
    subscription_id: &str,
    service_id: &str,
    mode: ServiceMode,
) -> ServiceCall {
    ServiceCall::new(
        SERVICE_CONTROL_ID,
        SERVICE_SUBSCRIBE_MEMBER,
        vec![
            Value::String(subscription_id.to_owned()),
            Value::String(service_id.to_owned()),
            Value::String(mode.as_str().to_owned()),
        ],
    )
}

/// Builds an unsubscribe control call.
pub fn create_service_unsubscribe_call(subscription_id: &str) -> ServiceCall {
    ServiceCall::new(
        SERVICE_CONTROL_ID,
        SERVICE_UNSUBSCRIBE_MEMBER,
        vec![Value::String(subscription_id.to_owned())],
    )
}

/// Recognises a control call, or returns `None` when it addresses a real service.
pub fn decode_service_control_call(call: &ServiceCall) -> Option<ServiceControlCall> {
    if call.service_id != SERVICE_CONTROL_ID || call.instance.is_some() {
        return None;
    }
    if call.member == SERVICE_CATALOGUE_MEMBER && call.args.is_empty() {
        return Some(ServiceControlCall::Catalogue);
    }
    if call.member == SERVICE_SUBSCRIBE_MEMBER && call.args.len() == 3 {
        let subscription_id = call.args[0].as_str().filter(|value| !value.is_empty())?;
        let service_id = call.args[1].as_str().filter(|value| !value.is_empty())?;
        let mode = parse_mode(call.args.get(2))?;
        return Some(ServiceControlCall::Subscribe {
            subscription_id: subscription_id.to_owned(),
            service_id: service_id.to_owned(),
            mode,
        });
    }
    if call.member == SERVICE_UNSUBSCRIBE_MEMBER && call.args.len() == 1 {
        let subscription_id = call.args[0].as_str().filter(|value| !value.is_empty())?;
        return Some(ServiceControlCall::Unsubscribe {
            subscription_id: subscription_id.to_owned(),
        });
    }
    None
}

/// Parses a decoded subscription snapshot from JSON (upstream `parseServiceSubscriptionSnapshot`).
pub fn parse_service_subscription_snapshot(value: &JsonValue) -> Result<ServiceSubscriptionSnapshot, ServiceError> {
    SubscriptionSnapshot::from_json(value)
}

/// Parses a wire subscription snapshot from JSON (upstream `parseWireServiceSubscriptionSnapshot`).
pub fn parse_wire_service_subscription_snapshot(
    value: &JsonValue,
) -> Result<WireServiceSubscriptionSnapshot, ServiceError> {
    SubscriptionSnapshot::from_json(value)
}

/// Parses a decoded provider update from JSON (upstream `parseServiceProviderUpdate`).
pub fn parse_service_provider_update(value: &JsonValue) -> Result<ServiceProviderUpdate, ServiceError> {
    ProviderUpdate::from_json(value)
}

/// Parses a wire provider update from JSON (upstream `parseWireServiceProviderUpdate`).
pub fn parse_wire_service_provider_update(value: &JsonValue) -> Result<WireServiceProviderUpdate, ServiceError> {
    ProviderUpdate::from_json(value)
}

/// Parses a service call from JSON (upstream `parseServiceCall`).
pub fn parse_service_call(value: &JsonValue) -> Result<ServiceCall, ServiceError> {
    ServiceCall::from_json(value)
}

/// Parses a provider catalogue from JSON (upstream `parseServiceCatalogue`).
pub fn parse_service_catalogue(value: &JsonValue) -> Result<Vec<ServiceCatalogueEntry>, ServiceError> {
    let items = value
        .as_array()
        .ok_or_else(|| invalid("service catalogue"))?;
    let mut ids: HashSet<&str> = HashSet::new();
    let mut catalogue = Vec::with_capacity(items.len());
    for item in items {
        let map = object(item, "service catalogue entry")?;
        assert_keys(map, &["serviceId", "mode"], &[], "service catalogue entry")?;
        let service_id = map
            .get("serviceId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid("service catalogue"))?;
        let mode = parse_mode(map.get("mode")).ok_or_else(|| invalid("service catalogue"))?;
        if !ids.insert(service_id) {
            return Err(invalid("service catalogue"));
        }
        catalogue.push(ServiceCatalogueEntry::new(service_id, mode));
    }
    Ok(catalogue)
}

/// Serialises a provider catalogue as strict JSON.
pub fn service_catalogue_to_json(catalogue: &[ServiceCatalogueEntry]) -> JsonValue {
    Value::Array(
        catalogue
            .iter()
            .map(|entry| {
                let mut map = Map::new();
                map.insert("serviceId".into(), Value::String(entry.service_id.clone()));
                map.insert("mode".into(), Value::String(entry.mode.as_str().into()));
                Value::Object(map)
            })
            .collect(),
    )
}

/// Serialises a decoded operation batch.
pub fn decoded_ops_to_json(ops: &[Op]) -> JsonValue {
    ops_to_json(ops)
}

/// Parses a decoded operation batch.
pub fn decoded_ops_from_json(value: &JsonValue) -> Result<Vec<Op>, ServiceError> {
    ops_from_json(value).map_err(ServiceError::from)
}

/// Serialises a wire operation batch.
pub fn wire_ops_to_json_value(ops: &[WireOp]) -> JsonValue {
    wire_ops_to_json(ops)
}

/// Parses a wire operation batch.
pub fn wire_ops_from_json_value(value: &JsonValue) -> Result<Vec<WireOp>, ServiceError> {
    wire_ops_from_json(value).map_err(ServiceError::from)
}

fn ops_to_json_generic<O: WireOpCodec>(ops: &[O]) -> JsonValue {
    Value::Array(ops.iter().map(WireOpCodec::op_to_json).collect())
}

fn parse_ops_generic<O: WireOpCodec>(
    value: Option<&JsonValue>,
    description: &str,
) -> Result<Vec<O>, ServiceError> {
    let items = value
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(description))?;
    for item in items {
        O::assert_op(item).map_err(|error| ServiceError::message(error.to_string()))?;
    }
    items
        .iter()
        .map(|item| O::op_from_json(item).map_err(ServiceError::from))
        .collect()
}

/// Parses a service instance address (upstream `assertAddress`).
pub fn parse_service_instance_address(value: &JsonValue) -> Result<ServiceInstanceAddress, ServiceError> {
    parse_address(value)
}

/// Serialises a service instance address.
pub fn service_instance_address_to_json(address: &ServiceInstanceAddress) -> JsonValue {
    address_to_json(address)
}

fn parse_address(value: &JsonValue) -> Result<ServiceInstanceAddress, ServiceError> {
    let map = object(value, "service instance address")?;
    assert_keys(map, &["key", "generation"], &[], "service instance address")?;
    let key = id_string(map, "key", "service instance address")?;
    let generation = integer(map, "generation", 1, "service instance address")?;
    Ok(ServiceInstanceAddress { key, generation })
}

fn address_to_json(address: &ServiceInstanceAddress) -> JsonValue {
    let mut map = Map::new();
    map.insert("key".into(), Value::String(address.key.clone()));
    map.insert("generation".into(), Value::from(address.generation));
    Value::Object(map)
}

/// Parses a service mode, returning `None` for an unknown spelling.
pub fn parse_service_mode(value: &str) -> Option<ServiceMode> {
    match value {
        "singleton" => Some(ServiceMode::Singleton),
        "keyed" => Some(ServiceMode::Keyed),
        _ => None,
    }
}

fn parse_mode(value: Option<&JsonValue>) -> Option<ServiceMode> {
    parse_service_mode(value?.as_str()?)
}

fn invalid(description: &str) -> ServiceError {
    ServiceError::message(format!("Invalid {description}"))
}

fn object<'a>(value: &'a JsonValue, description: &str) -> Result<&'a Map<String, JsonValue>, ServiceError> {
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(invalid(description)),
    }
}

fn assert_keys(
    map: &Map<String, JsonValue>,
    required: &[&str],
    optional: &[&str],
    description: &str,
) -> Result<(), ServiceError> {
    let mut allowed: HashSet<&str> = HashSet::with_capacity(required.len() + optional.len());
    allowed.extend(required.iter().copied());
    allowed.extend(optional.iter().copied());
    let missing = required.iter().any(|key| !map.contains_key(*key));
    let extra = map.keys().any(|key| !allowed.contains(key.as_str()));
    if missing || extra {
        return Err(invalid(description));
    }
    Ok(())
}

fn id_string(
    map: &Map<String, JsonValue>,
    key: &str,
    description: &str,
) -> Result<String, ServiceError> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(description))
}

fn integer(
    map: &Map<String, JsonValue>,
    key: &str,
    minimum: u64,
    description: &str,
) -> Result<u64, ServiceError> {
    map.get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value >= minimum)
        .ok_or_else(|| invalid(description))
}

// A convenience map of member name to kind lives in `provider` as [`MemberShape`].

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::{Op, PathRef, WireOp};

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
    fn snapshots_and_updates_round_trip_through_json() {
        let snapshot = state_snapshot();
        let json = snapshot.to_json();
        assert_eq!(
            json,
            serde_json::json!({
                "serviceId": "pi.models",
                "mode": "singleton",
                "instances": [{ "members": [{ "name": "state", "kind": "state", "sequence": 0, "ops": [["r", {"revision": 1}]] }] }],
            })
        );
        assert_eq!(ServiceSubscriptionSnapshot::from_json(&json).unwrap(), snapshot);

        let update = ProviderUpdate::State {
            instance: None,
            member: "state".into(),
            sequence: 1,
            ops: vec![Op::Set(
                crate::delta::NonEmptyPath::from_keys(&["revision"]),
                serde_json::json!(2),
            )],
        };
        assert_eq!(ServiceProviderUpdate::from_json(&update.to_json()).unwrap(), update);
    }

    #[test]
    fn control_calls_decode_only_for_the_reserved_service() {
        assert_eq!(
            decode_service_control_call(&create_service_catalogue_call()),
            Some(ServiceControlCall::Catalogue)
        );
        assert_eq!(
            decode_service_control_call(&create_service_subscribe_call(
                "subscription-1",
                "pi.models",
                ServiceMode::Singleton
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
        let mut foreign = create_service_catalogue_call();
        foreign.service_id = "pi.models".into();
        assert_eq!(decode_service_control_call(&foreign), None);
    }

    #[test]
    fn malformed_values_keep_the_upstream_messages() {
        let call = parse_service_call(&serde_json::json!({
            "serviceId": "pi.models", "member": "list", "args": [], "extra": true,
        }))
        .unwrap_err();
        assert_eq!(call.to_string(), "Invalid service call");

        let catalogue = parse_service_catalogue(&serde_json::json!([
            { "serviceId": "pi.models", "mode": "unknown" }
        ]))
        .unwrap_err();
        assert_eq!(catalogue.to_string(), "Invalid service catalogue");

        let update = parse_service_provider_update(&serde_json::json!({
            "type": "state", "member": "state", "sequence": 0, "ops": [],
        }))
        .unwrap_err();
        assert_eq!(update.to_string(), "Invalid service state update");

        // Op-level validation still runs for the wire vocabulary.
        let wire = parse_wire_service_provider_update(&serde_json::json!({
            "type": "state", "member": "state", "sequence": 1, "ops": [["?", 0]],
        }));
        assert!(wire.is_err());
    }

    #[test]
    fn duplicate_catalogue_ids_are_refused() {
        let error = parse_service_catalogue(&serde_json::json!([
            { "serviceId": "pi.models", "mode": "singleton" },
            { "serviceId": "pi.models", "mode": "singleton" }
        ]))
        .unwrap_err();
        assert_eq!(error.to_string(), "Invalid service catalogue");
    }

    #[test]
    fn address_keys_are_exactly_key_and_generation() {
        let address = parse_service_instance_address(&serde_json::json!({
            "key": "dialog-1", "generation": 2,
        }))
        .unwrap();
        assert_eq!(address.key, "dialog-1");
        assert_eq!(address.generation, 2);
        assert!(parse_service_instance_address(&serde_json::json!({
            "key": "dialog-1", "generation": 0,
        }))
        .is_err());
    }

    #[test]
    fn member_kinds_match_their_strings() {
        assert_eq!(ServiceMemberKind::parse("method"), Some(ServiceMemberKind::Method));
        assert_eq!(ServiceMemberKind::State.to_string(), "state");
        let member = MemberSnapshot::<WireOp>::Method { name: "select".into() };
        assert_eq!(member.kind(), ServiceMemberKind::Method);
        assert_eq!(
            member.to_json(),
            serde_json::json!({ "name": "select", "kind": "method" })
        );
        let wire = WireOp::Define {
            id: 0,
            path: vec![crate::delta::Seg::Key("a".into())],
        };
        let _ = PathRef::Inline(vec![]); // keep the import used in every configuration
        assert_eq!(wire.to_json(), serde_json::json!(["#", 0, ["a"]]));
    }
}
