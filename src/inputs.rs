//! Every MQTT topic the gateway must be subscribed to for a given spec:
//! synthetic inputs, distribute children's cache-backed readings, and
//! der_dispatch's site-level channels. Computed at boot and again on every
//! topology change, so subscriptions follow the topology.

use crate::asyncapi::types::{AsyncApiSpec, ProtocolBinding};
use crate::dispatch::rack_limits;
use crate::envelope;
use std::collections::BTreeSet;

/// Every input topic `spec` needs subscribed (`{site_id}` substituted).
pub fn input_topics(spec: &AsyncApiSpec, site_id: &str) -> Vec<String> {
    let mut topics = collect_synthetic_input_topics(spec, site_id);
    topics.extend(collect_distribute_input_topics(spec, site_id));
    topics.extend(collect_der_dispatch_active_power_topics(spec, site_id));
    topics.extend(collect_der_dispatch_state_of_charge_topics(spec, site_id));
    // der_dispatch's own target-side channels (published by ems-der-control-
    // api) — the site-distribution task's reactive trigger. Not templated
    // (der_dispatch isn't in every spec), so subscribed unconditionally;
    // holds forever on a site with no der_dispatch, same as its other inputs.
    topics.push(format!(
        "sites/{site_id}/devices/der_dispatch/measurements/target_active_power/watts"
    ));
    topics.push(format!(
        "sites/{site_id}/devices/der_dispatch/measurements/event_active/none"
    ));
    topics
}

/// Walk the spec for synthetic bindings + collect the unique set of input
/// topics (with `{site_id}` substituted). Used to subscribe up-front so cached
/// values are available by the time synthetic tasks tick.
fn collect_synthetic_input_topics(spec: &AsyncApiSpec, site_id: &str) -> Vec<String> {
    let mut topics: BTreeSet<String> = BTreeSet::new();
    for channels in spec.x_protocol_source.values() {
        for source in channels.values() {
            if let ProtocolBinding::Synthetic(b) = &source.binding {
                for raw in &b.inputs {
                    topics.insert(substitute_site_id(raw, site_id));
                }
                for pair in &b.pairs {
                    topics.insert(substitute_site_id(&pair.topic, site_id));
                }
            }
        }
    }
    topics.into_iter().collect()
}

/// Walk the spec's x-command-source for `distribute` bindings and collect
/// each child's `operating_state_topic`/`state_of_charge_topic` (with
/// `{site_id}` substituted) and its live power limits (`rack_limits`). These are read from the cache at dispatch time,
/// not polled — the gateway still needs to be subscribed for them to ever
/// land in the cache.
fn collect_distribute_input_topics(spec: &AsyncApiSpec, site_id: &str) -> Vec<String> {
    let mut topics: BTreeSet<String> = BTreeSet::new();
    for commands in spec.x_command_source.values() {
        for source in commands.values() {
            if let ProtocolBinding::Distribute(d) = &source.binding {
                for child in &d.children {
                    topics.insert(substitute_site_id(&child.operating_state_topic, site_id));
                    topics.insert(substitute_site_id(&child.state_of_charge_topic, site_id));
                    for charging in [true, false] {
                        topics.insert(rack_limits::limit_topic(
                            site_id,
                            &child.device_id,
                            charging,
                        ));
                    }
                }
                if let Some(reserve) = &d.operator_reserve_topic {
                    topics.insert(substitute_site_id(reserve, site_id));
                }
                if let Some(guard) = envelope::envelope_guard_config(d) {
                    topics.insert(substitute_site_id(&guard.import_limit_topic, site_id));
                    topics.insert(substitute_site_id(&guard.export_limit_topic, site_id));
                    topics.insert(substitute_site_id(&guard.active_power_topic, site_id));
                    if let Some(poi) = &guard.poi_active_power_topic {
                        topics.insert(substitute_site_id(poi, site_id));
                    }
                }
            }
            // A guarded power cap (compute shed) reads the same envelope.
            if let ProtocolBinding::PowerCap(p) = &source.binding {
                let guard = [
                    &p.import_limit_topic,
                    &p.export_limit_topic,
                    &p.poi_active_power_topic,
                ];
                topics.extend(
                    guard
                        .into_iter()
                        .flatten()
                        .map(|t| substitute_site_id(t, site_id)),
                );
            }
        }
    }
    topics.into_iter().collect()
}

/// Every distribute-parent device_id — today, every `bess_module` instance.
/// A device qualifies by having at least one `Distribute`-bound command, not
/// by template name — stays correct at any module count and at any
/// distribution tier (module→rack today, site→module built on the same
/// detection).
fn distribute_parent_device_ids(spec: &AsyncApiSpec) -> Vec<String> {
    spec.x_command_source
        .iter()
        .filter(|(_, commands)| {
            commands
                .values()
                .any(|source| matches!(source.binding, ProtocolBinding::Distribute(_)))
        })
        .map(|(device_id, _)| device_id.clone())
        .collect()
}

/// Each distribute-parent's own `active_power` topic (with `{site_id}`
/// substituted) — feeds `der_dispatch::actual_active_power`'s site-total
/// sum. Deliberately does NOT include state_of_charge: that's a different
/// task's (`site_distribution`) concern, and mixing it into this list would
/// make the sum wait on an unrelated measurement that may never publish.
pub(crate) fn collect_der_dispatch_active_power_topics(
    spec: &AsyncApiSpec,
    site_id: &str,
) -> Vec<String> {
    distribute_parent_device_ids(spec)
        .iter()
        .map(|device_id| {
            substitute_site_id(
                &format!("sites/{{site_id}}/devices/{device_id}/measurements/active_power/watts"),
                site_id,
            )
        })
        .collect()
}

/// Each distribute-parent's own `state_of_charge` topic (with `{site_id}`
/// substituted) — not summed by anything, just needs to be subscribed so
/// `site_distribution`'s SoC-weighted split has it cached.
fn collect_der_dispatch_state_of_charge_topics(spec: &AsyncApiSpec, site_id: &str) -> Vec<String> {
    distribute_parent_device_ids(spec)
        .iter()
        .map(|device_id| {
            substitute_site_id(
                &format!(
                    "sites/{{site_id}}/devices/{device_id}/measurements/state_of_charge/percent"
                ),
                site_id,
            )
        })
        .collect()
}

/// Substitute `{site_id}` in an input topic template. `{device_id}` is
/// already resolved by ems-device-api at AsyncAPI generation time.
pub(crate) fn substitute_site_id(template: &str, site_id: &str) -> String {
    template.replace("{site_id}", site_id)
}
