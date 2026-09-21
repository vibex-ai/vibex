use vibex_core::{
    RemoteAuditListRequest, RemoteAuditRecord, RemoteCancelPairingOfferRequest,
    RemoteCreatePairingCodeRequest, RemoteCreatePairingCodeResponse,
    RemoteCreatePairingOfferRequest, RemoteCreatePairingOfferResponse, RemoteDeviceDetail,
    RemotePairingOfferSummary, RemoteRenameDeviceRequest, RemoteRevokeDeviceRequest,
};

use crate::{BackendBound, BackendFuture, MutationRequest};

pub trait DeviceBackend: BackendBound {
    fn create_pairing_offer(
        &self,
        request: MutationRequest<RemoteCreatePairingCodeRequest>,
    ) -> BackendFuture<'_, RemoteCreatePairingCodeResponse>;

    fn create_pairing_offer_v2(
        &self,
        request: MutationRequest<RemoteCreatePairingOfferRequest>,
    ) -> BackendFuture<'_, RemoteCreatePairingOfferResponse>;

    fn cancel_pairing_offer(
        &self,
        request: MutationRequest<RemoteCancelPairingOfferRequest>,
    ) -> BackendFuture<'_, RemotePairingOfferSummary>;

    fn list_devices(&self) -> BackendFuture<'_, Vec<RemoteDeviceDetail>>;

    fn revoke_device(
        &self,
        request: MutationRequest<RemoteRevokeDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail>;

    /// Renames a paired device in the runtime's trust store. The runtime owns
    /// the name, so the renamed device reads it back from its next handshake.
    fn rename_device(
        &self,
        request: MutationRequest<RemoteRenameDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail>;

    /// Renames the runtime itself. Every client of that runtime renders the
    /// published name, so this is the rename that reaches all of them.
    fn rename_runtime(&self, request: MutationRequest<String>) -> BackendFuture<'_, String>;

    fn audit_records(
        &self,
        request: RemoteAuditListRequest,
    ) -> BackendFuture<'_, Vec<RemoteAuditRecord>>;
}
