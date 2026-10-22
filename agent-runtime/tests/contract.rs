//! The runtime's contract (ADR-0120) compiles, with agent.proto beside it, before anything serves it.

#[test]
fn the_runtime_contract_compiles_and_names_its_rpcs() {
    let api = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../api");
    let fds = protox::compile(["holon/runtime/v1/runtime.proto"], [&api])
        .unwrap_or_else(|e| panic!("api/holon/runtime/v1/runtime.proto: {e:?}"));
    let file = fds.file.iter().find(|f| f.name() == "holon/runtime/v1/runtime.proto").unwrap();
    let service = &file.service[0];
    assert_eq!(service.name(), "AgentRuntimeService");
    let rpcs: Vec<&str> = service.method.iter().map(|m| m.name()).collect();
    for want in ["StartRun", "ListRuns", "StreamEvents", "GetState", "SubmitApproval", "PutKey"] {
        assert!(rpcs.contains(&want), "no {want} in {rpcs:?}");
    }
    // One envelope kind per agent_wire::Event variant.
    let event = file.message_type.iter().find(|m| m.name() == "RuntimeEvent").unwrap();
    let kinds: Vec<&str> =
        event.field.iter().filter(|f| f.oneof_index.is_some()).map(|f| f.name()).collect();
    assert_eq!(
        kinds,
        [
            "run_started",
            "tool_started",
            "step_done",
            "run_finished",
            "approval_requested",
            "approval_resolved",
            "session_changed"
        ]
    );
}
