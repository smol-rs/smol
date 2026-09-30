//! The TLS examples load `examples/identity.pfx` through `native_tls`.
//! OpenSSL 3 rejects the historical RC2-40-CBC PKCS#12 protection by default,
//! which made `cargo run --example hyper-server` (and the other TLS examples)
//! fail on modern Linux. Keep the bundled identity on modern algorithms.

use std::process::Command;

#[test]
fn example_identity_pfx_loads_with_native_tls() {
    native_tls::Identity::from_pkcs12(include_bytes!("../examples/identity.pfx"), "password")
        .expect(
            "examples/identity.pfx must load with native-tls without enabling the \
             OpenSSL legacy provider",
        );
}

#[test]
fn example_identity_pfx_opens_with_openssl3_without_legacy() {
    let version = Command::new("openssl")
        .arg("version")
        .output()
        .expect("openssl must be available to check the bundled PKCS#12");
    let version = String::from_utf8_lossy(&version.stdout);
    if !version.contains("OpenSSL 3") {
        eprintln!("skipping OpenSSL 3 PKCS#12 check; got {version}");
        return;
    }

    let out = tempfile::NamedTempFile::new().expect("temp file");
    let output = Command::new("openssl")
        .args([
            "pkcs12",
            "-in",
            "examples/identity.pfx",
            "-passin",
            "pass:password",
            "-nodes",
            "-out",
        ])
        .arg(out.path())
        .output()
        .expect("failed to run openssl pkcs12");

    assert!(
        output.status.success(),
        "examples/identity.pfx must open under OpenSSL 3 without `-legacy`\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
