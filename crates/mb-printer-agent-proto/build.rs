// SPDX-License-Identifier: AGPL-3.0-or-later
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost = prost_build::Config::new();
    prost.protoc_executable(protoc);
    // Generated protobuf oneofs may legitimately differ greatly in size; this
    // is wire-contract code rather than a hand-designed in-memory enum.
    prost.type_attribute(".", "#[allow(clippy::large_enum_variant)]");
    tonic_prost_build::configure().compile_with_config(
        prost,
        &["proto/agent_session.proto"],
        &["proto"],
    )?;
    println!("cargo:rerun-if-changed=proto/agent_session.proto");
    Ok(())
}
