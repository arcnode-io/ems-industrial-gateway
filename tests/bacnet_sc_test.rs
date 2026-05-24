//! Gateway BACnet/SC integration — proves `crate::bacnet_sc::client::
//! read_measurement` can read a property end-to-end:
//!   gateway → (our fork's BacnetScClient) → hub (our fork's
//!   BacnetScHub) → fake device (our fork's BacnetScDataLink)
//!   → ReadPropertyResponse → back through hub → gateway returns f64.

mod fixtures;

use bacnet_rs::app::Apdu;
use bacnet_rs::datalink::sc::hub::NodeIdentity;
use bacnet_rs::datalink::sc::hub_server::BacnetScHub;
use bacnet_rs::datalink::sc::messages::ConnectAcceptPayload;
use bacnet_rs::datalink::sc::node::{BacnetScConfig, BacnetScDataLink};
use bacnet_rs::datalink::sc::tls::{PeerCredentials as ScPeerCredentials, build_server_config};
use bacnet_rs::network::Npdu;
use bacnet_rs::object::{ObjectIdentifier, ObjectType, PropertyIdentifier};
use bacnet_rs::property::PropertyValue;
use bacnet_rs::service::{ConfirmedServiceChoice, ReadPropertyResponse};
use ems_industrial_gateway::asyncapi::types::BacnetScBinding;
use ems_industrial_gateway::bacnet_sc::client::read_measurement;
use ems_industrial_gateway::config::GatewayCredentials;
use fixtures::pki::gen_test_pki;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

const DEVICE_VMAC: [u8; 6] = [0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gateway_reads_present_value_from_bacnet_sc_device() {
    // Arrange — PKI shared across hub, fake device, gateway.
    let pki = gen_test_pki("hub.test.local").unwrap();
    let server_cfg = build_server_config(ScPeerCredentials {
        ca_bundle_path: pki.ca_bundle.path(),
        cert_path: pki.server_cert.path(),
        key_path: pki.server_key.path(),
    })
    .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(server_cfg));
    let hub_identity = ConnectAcceptPayload {
        vmac: [0x00; 6],
        device_uuid: [0xCC; 16],
        max_bvlc_length: 1497,
        max_npdu_length: 1476,
    };
    let hub = BacnetScHub::bind("127.0.0.1:0".parse().unwrap(), acceptor, hub_identity)
        .await
        .unwrap();
    let addr = hub.local_addr().unwrap();
    tokio::spawn(hub.run());

    // Spawn a fake BACnet device on the same hub. Connects + waits for
    // a ReadProperty request, responds with PropertyValue::Real(42.5).
    let url = format!("wss://127.0.0.1:{}/", addr.port());
    let device_cfg = BacnetScConfig {
        hub_url: url.clone(),
        ca_bundle_path: pki.ca_bundle.path().to_path_buf(),
        cert_path: pki.gateway_cert.path().to_path_buf(),
        key_path: pki.gateway_key.path().to_path_buf(),
        identity: NodeIdentity {
            vmac: DEVICE_VMAC,
            device_uuid: [0xEE; 16],
            max_bvlc_length: 1497,
            max_npdu_length: 1476,
        },
    };
    let device_handle = tokio::spawn(run_fake_device(device_cfg));

    // Give the device a moment to register in the hub's directory
    // before the gateway connects + addresses it by VMAC.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Build the binding + creds the gateway sees.
    let binding = BacnetScBinding {
        hub_url: url,
        device_vmac: "D1:D2:D3:D4:D5:D6".to_string(),
        object_type: "analog_input".to_string(),
        object_instance: 1,
        property_id: "present_value".to_string(),
    };
    let creds = GatewayCredentials {
        ca_bundle_path: pki.ca_bundle.path().to_path_buf(),
        cert_path: pki.gateway_cert.path().to_path_buf(),
        key_path: pki.gateway_key.path().to_path_buf(),
    };

    // Act — call the gateway's read_measurement.
    let value = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        read_measurement(&binding, None, Some(&creds)),
    )
    .await
    .expect("gateway read timed out")
    .unwrap();

    // Assert — got the canned 42.5 back.
    assert!((value - 42.5).abs() < 0.01);

    device_handle.abort();
}

/// Fake BACnet device: connects to the hub, then on first inbound NPDU,
/// decodes the ReadProperty request and replies with Real(42.5).
async fn run_fake_device(cfg: BacnetScConfig) {
    let mut dl = BacnetScDataLink::connect(cfg).await.unwrap();
    let (npdu_bytes, source) = dl.recv_npdu().await.unwrap();
    let (_npdu, npdu_len) = Npdu::decode(&npdu_bytes).unwrap();
    let apdu = Apdu::decode(&npdu_bytes[npdu_len..]).unwrap();
    let invoke_id = match apdu {
        Apdu::ConfirmedRequest {
            invoke_id,
            service_choice: ConfirmedServiceChoice::ReadProperty,
            ..
        } => invoke_id,
        _ => panic!("expected ConfirmedRequest/ReadProperty, got {apdu:?}"),
    };
    // Build response.
    let response = ReadPropertyResponse {
        object_identifier: ObjectIdentifier::new(ObjectType::AnalogInput, 1),
        property_identifier: PropertyIdentifier::PresentValue,
        property_array_index: None,
        property_values: vec![PropertyValue::Real(42.5)],
    };
    let mut svc = Vec::new();
    response.encode(&mut svc).unwrap();
    let resp_apdu = Apdu::ComplexAck {
        segmented: false,
        more_follows: false,
        invoke_id,
        sequence_number: None,
        proposed_window_size: None,
        service_choice: ConfirmedServiceChoice::ReadProperty,
        service_data: svc,
    };
    let npdu = Npdu::new();
    let mut out = npdu.encode();
    out.extend_from_slice(&resp_apdu.encode());
    // Address the response back to the request's source (the gateway).
    dl.send_npdu(&out, source).await.unwrap();
    // Keep the WS alive long enough for the gateway to recv before close.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    drop(dl);
}
