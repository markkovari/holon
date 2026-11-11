// Tests for capability hash-chaining and Merkle tree roots

test "sha256 empty and test vector" {
  // SHA-256 of empty string is e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
  let empty_hash = sha256_str("")
  assert_eq(empty_hash, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")

  // SHA-256 of "abc" is ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
  let abc_hash = sha256_str("abc")
  assert_eq(abc_hash, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
}

test "capability hash chain integrity and verification" {
  let chain = CapabilityChain::new()
  let log = []

  let inv0 = chain.append(1000UL, "wasi:keyvalue/store", "get", "input_digest_1", "output_digest_1", 100UL)
  log.push(inv0)

  let inv1 = chain.append(1050UL, "card:identify/identifier", "identify", "input_digest_2", "output_digest_2", 250UL)
  log.push(inv1)

  let inv2 = chain.append(1100UL, "wasi:keyvalue/store", "set", "input_digest_3", "output_digest_3", 120UL)
  log.push(inv2)

  assert_true(verify_chain(log, "0000000000000000000000000000000000000000000000000000000000000000"))
  assert_eq(chain.count, 3UL)
  assert_eq(chain.head_hash, inv2.hash())
}

test "tampered capability invocation is detected" {
  let chain = CapabilityChain::new()
  let log = []

  let inv0 = chain.append(1000UL, "wasi:keyvalue/store", "get", "digest_a", "digest_b", 10UL)
  log.push(inv0)

  let inv1 = chain.append(1050UL, "wasi:keyvalue/store", "set", "digest_c", "digest_d", 20UL)
  // Tamper with inv1's prev_log_hash
  let tampered : CapabilityInvocation = {
    index: inv1.index,
    prev_log_hash: "deadbeef00000000000000000000000000000000000000000000000000000000",
    timestamp_ms: inv1.timestamp_ms,
    interface_name: inv1.interface_name,
    function_name: inv1.function_name,
    input_digest: inv1.input_digest,
    output_digest: inv1.output_digest,
    fuel_consumed: inv1.fuel_consumed,
  }
  log.push(tampered)

  assert_false(verify_chain(log, "0000000000000000000000000000000000000000000000000000000000000000"))
}

test "merkle tree root computation" {
  let chunks = [
    sha256_str("chunk0"),
    sha256_str("chunk1"),
    sha256_str("chunk2"),
    sha256_str("chunk3"),
  ]
  let root = compute_merkle_root(chunks)
  assert_eq(root.length(), 64)

  // Determinism check
  let root2 = compute_merkle_root(chunks)
  assert_eq(root, root2)

  // Single chunk root is the chunk hash itself
  let single = [sha256_str("single_chunk")]
  assert_eq(compute_merkle_root(single), sha256_str("single_chunk"))
}

test "agent checkpoint serialization" {
  let chk : AgentCheckpoint = {
    parent_oid: Some("sha256:parent123"),
    repo: "holon-core",
    branch: "agent/explore-1",
    agent_id: "agent-007",
    generation: 42UL,
    state_merkle_root: "sha256:root456",
    capability_log_head: "sha256:log789",
    task_status: "passed_gate",
    summary: "Successfully ported module and verified tests",
  }
  let json = chk.to_json()
  assert_true(json.contains("\"parent_oid\":\"sha256:parent123\""))
  assert_true(json.contains("\"repo\":\"holon-core\""))
  assert_true(json.contains("\"generation\":42"))
  assert_true(json.contains("\"task_status\":\"passed_gate\""))
}
