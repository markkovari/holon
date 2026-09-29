//! Generates the gRPC server and client for `api/holon/v1/agent.proto`
//! (ADR-0102) into OUT_DIR. `protox` compiles the proto in-process, so the
//! build needs no `protoc` on the machine and CI needs no extra step.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../api/holon/v1/agent.proto");
    let fds = protox::compile(["holon/v1/agent.proto"], ["../api"])?;
    tonic_prost_build::configure().compile_fds(fds)?;
    Ok(())
}
