use vibex_core::{
    RemoteAuditListRequest, RemoteAuditRecord, RemoteCancelPairingOfferRequest,
    RemoteCreatePairingCodeRequest, RemoteCreatePairingCodeResponse,
    RemoteCreatePairingOfferRequest, RemoteCreatePairingOfferResponse, RemoteDeviceDetail,
    RemotePairingOfferSummary, RemoteRevokeDeviceRequest,
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

    fn audit_records(
        &self,
        request: RemoteAuditListRequest,
    ) -> BackendFuture<'_, Vec<RemoteAuditRecord>>;
}
