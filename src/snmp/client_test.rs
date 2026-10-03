//! SNMP client: OID parsing and reads.

use super::*;

#[test]
fn parse_dotted_oid_handles_sysuptime() {
    // Arrange + Act
    let oid = parse_dotted_oid("1.3.6.1.2.1.1.3.0").unwrap();
    // Assert — round-trip back to dotted form
    let s = oid.to_string();
    assert_eq!(s, "1.3.6.1.2.1.1.3.0");
}

#[test]
fn load_usm_passphrases_pulls_from_env_keyed_by_security_name() {
    // Arrange
    unsafe {
        std::env::set_var("SNMP_USM_GW_TEST_AUTH_PASSPHRASE", "authsecret");
        std::env::set_var("SNMP_USM_GW_TEST_PRIV_PASSPHRASE", "privsecret");
    }
    // Act
    let (auth, priv_) = load_usm_passphrases("gw-test").unwrap();
    // Assert — uppercase + hyphens-to-underscores normalization works
    assert_eq!(auth, "authsecret");
    assert_eq!(priv_, "privsecret");
}

#[test]
fn scale_multiplies_the_raw_integer() {
    // Sentry4-MIB reports st4LineCurrent in 0.01 A: raw 1234 = 12.34 A.
    let b: SnmpBinding = serde_json::from_value(serde_json::json!({
        "host": "pdu", "port": 161, "oid": "1.3.6.1.4.1.1718.4.1.4.3.1.3.1.1.1",
        "scale": 0.01,
    }))
    .unwrap();
    assert!((scaled(1234.0, &b) - 12.34).abs() < 1e-9);
}

#[test]
fn absent_scale_leaves_the_value_unchanged() {
    // Specs from before the field existed must read exactly as before.
    let b: SnmpBinding = serde_json::from_value(serde_json::json!({
        "host": "pdu", "port": 161, "oid": "1.3.6.1.2.1.1.3.0",
    }))
    .unwrap();
    assert_eq!(scaled(1234.0, &b), 1234.0);
}

#[tokio::test]
async fn a_get_the_agent_never_answers_fails_instead_of_hanging() {
    // Arrange — a UDP endpoint that swallows requests, like an agent that
    // was restarting when the request landed
    let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let endpoint = silent.local_addr().unwrap().to_string();
    let oid = parse_dotted_oid("1.3.6.1.2.1.1.3.0").unwrap();
    // Act
    let got = tokio::time::timeout(Duration::from_secs(10), try_get_v2c(&endpoint, &oid)).await;
    // Assert — the attempt gives up on its own, so the read can retry
    assert!(matches!(got, Ok(Err(_))), "attempt never returned");
}
