//! The distributed owner's side of the review-owned port [ORB-17939].

use orbit_store::contracts::AdmissionShipContract;

use crate::OrbitRuntime;
use crate::application::review::ReviewDistributedPort;

impl ReviewDistributedPort for OrbitRuntime {
    fn owner_delivery_ship(&self) -> AdmissionShipContract {
        self.owner_ship_contract()
    }

    fn drain_owner_machine(&self) -> Option<String> {
        self.distributed_owner_machine_id()
    }
}
