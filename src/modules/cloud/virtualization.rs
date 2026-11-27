// cloud module - virtualization grouping (view only).
//
// This endpoint groups the virtualization connectors (Proxmox VE,
// KVM via Proxmox, XenServer / XCP-ng, Virtualizor) and their cached
// inventory counts. It is a grouping VIEW over the connector
// adapters - it does NOT provision VMs, manage templates or images,
// or talk to a hypervisor directly. Unified is a cloud connector:
// it connects to external platforms, it does not build or reimplement
// the VPS/VM panel (that is a separate external project). QEMU is
// covered here through the Proxmox `KvmAdapter` (QEMU guests as
// `node:qemu:vmid`); a direct libvirt/QMP driver is a future
// external-module candidate, not core.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::json;
use actix_web::{web, HttpRequest, HttpResponse};
use serde::Serialize;
use std::sync::Arc;

use super::helpers::{require_cloud_entitlement, PERM_CLOUD_VIEW};
use super::providers::handlers::{caller, require_permission, require_tenant};
use super::providers::types::ProviderKind;
use super::providers::{connections_store, inventory_store};

/// One virtualization connector with its cached inventory count.
#[derive(Debug, Clone, Serialize)]
pub struct VirtualizationConnector {
    pub connection_id: String,
    pub kind: String,
    pub display_name: String,
    /// Servers in the cached inventory (0 when never synced).
    pub server_count: usize,
}

pub(crate) async fn list_virtualization(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<VirtualizationConnector>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CLOUD_VIEW)?;
        require_cloud_entitlement(&ctx, &req)?;
        let mut connectors: Vec<VirtualizationConnector> = connections_store()
            .iter()
            .filter(|entry| {
                matches!(
                    entry.value().kind,
                    ProviderKind::Proxmox
                        | ProviderKind::Kvm
                        | ProviderKind::Xen
                        | ProviderKind::Virtualizor
                )
            })
            .map(|entry| {
                let connection = entry.value();
                let server_count = inventory_store()
                    .get(&connection.id)
                    .map(|inventory| inventory.servers.len())
                    .unwrap_or(0);
                VirtualizationConnector {
                    connection_id: connection.id.to_string(),
                    kind: connection.kind.as_str().to_string(),
                    display_name: connection.name.clone(),
                    server_count,
                }
            })
            .collect();
        connectors.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        Ok(connectors)
    })();
    json::respond(result, debug)
}
