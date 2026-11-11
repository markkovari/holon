// agent-checkpoint: Container-free agent state, Merkle state trees, and capability hash chains

fn rotr(x : UInt, n : Int) -> UInt {
  (x >> n) | (x << (32 - n))
}

fn ch(x : UInt, y : UInt, z : UInt) -> UInt {
  (x & y) ^ ((x ^ 0xFFFFFFFFU) & z)
}

fn maj(x : UInt, y : UInt, z : UInt) -> UInt {
  (x & y) ^ (x & z) ^ (y & z)
}

fn sigma0(x : UInt) -> UInt {
  rotr(x, 2) ^ rotr(x, 13) ^ rotr(x, 22)
}

fn sigma1(x : UInt) -> UInt {
  rotr(x, 6) ^ rotr(x, 11) ^ rotr(x, 25)
}

fn gamma0(x : UInt) -> UInt {
  rotr(x, 7) ^ rotr(x, 18) ^ (x >> 3)
}

fn gamma1(x : UInt) -> UInt {
  rotr(x, 17) ^ rotr(x, 19) ^ (x >> 10)
}

let k_table : FixedArray[UInt] = [
  0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U,
  0x3956c25bU, 0x59f111f1U, 0x923f82a4U, 0xab1c5ed5U,
  0xd807aa98U, 0x12835b01U, 0x243185beU, 0x550c7dc3U,
  0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U, 0xc19bf174U,
  0xe49b69c1U, 0xefbe4786U, 0x0fc19dc6U, 0x240ca1ccU,
  0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU,
  0x983e5152U, 0xa831c66dU, 0xb00327c8U, 0xbf597fc7U,
  0xc6e00bf3U, 0xd5a79147U, 0x06ca6351U, 0x14292967U,
  0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU, 0x53380d13U,
  0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U,
  0xa2bfe8a1U, 0xa81a664bU, 0xc24b8b70U, 0xc76c51a3U,
  0xd192e819U, 0xd6990624U, 0xf40e3585U, 0x106aa070U,
  0x19a4c116U, 0x1e376c08U, 0x2748774cU, 0x34b0bcb5U,
  0x391c0cb3U, 0x4ed8aa4aU, 0x5b9cca4fU, 0x682e6ff3U,
  0x748f82eeU, 0x78a5636fU, 0x84c87814U, 0x8cc70208U,
  0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U,
]

pub fn sha256_bytes(data : FixedArray[Byte]) -> FixedArray[Byte] {
  let len = data.length()
  let bit_len = (len.to_int64() * 8L).to_uint64()
  let rem = (len + 1) % 64
  let pad_zeros = if rem <= 56 { 56 - rem } else { 120 - rem }
  let total_len = len + 1 + pad_zeros + 8
  let padded = FixedArray::make(total_len, b'\x00')
  for i = 0; i < len; i = i + 1 {
    padded[i] = data[i]
  }
  padded[len] = b'\x80'
  for i = 0; i < 8; i = i + 1 {
    let shift = (7 - i) * 8
    padded[total_len - 8 + i] = ((bit_len >> shift) & 0xFFUL).to_byte()
  }

  let mut h0 = 0x6a09e667U
  let mut h1 = 0xbb67ae85U
  let mut h2 = 0x3c6ef372U
  let mut h3 = 0xa54ff53aU
  let mut h4 = 0x510e527fU
  let mut h5 = 0x9b05688cU
  let mut h6 = 0x1f83d9abU
  let mut h7 = 0x5be0cd19U

  let w = FixedArray::make(64, 0U)
  let blocks = total_len / 64

  for b_idx = 0; b_idx < blocks; b_idx = b_idx + 1 {
    let offset = b_idx * 64
    for i = 0; i < 16; i = i + 1 {
      let b0 = padded[offset + i * 4].to_int().to_uint() & 0xFFU
      let b1 = padded[offset + i * 4 + 1].to_int().to_uint() & 0xFFU
      let b2 = padded[offset + i * 4 + 2].to_int().to_uint() & 0xFFU
      let b3 = padded[offset + i * 4 + 3].to_int().to_uint() & 0xFFU
      w[i] = (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
    }
    for i = 16; i < 64; i = i + 1 {
      let s0 = gamma0(w[i - 15])
      let s1 = gamma1(w[i - 2])
      w[i] = w[i - 16] + s0 + w[i - 7] + s1
    }

    let mut a = h0
    let mut b = h1
    let mut c = h2
    let mut d = h3
    let mut e = h4
    let mut f = h5
    let mut g = h6
    let mut h = h7

    for i = 0; i < 64; i = i + 1 {
      let s1 = sigma1(e)
      let ch_val = ch(e, f, g)
      let t1 = h + s1 + ch_val + k_table[i] + w[i]
      let s0 = sigma0(a)
      let maj_val = maj(a, b, c)
      let t2 = s0 + maj_val

      h = g
      g = f
      f = e
      e = d + t1
      d = c
      c = b
      b = a
      a = t1 + t2
    }

    h0 = h0 + a
    h1 = h1 + b
    h2 = h2 + c
    h3 = h3 + d
    h4 = h4 + e
    h5 = h5 + f
    h6 = h6 + g
    h7 = h7 + h
  }

  let out = FixedArray::make(32, b'\x00')
  let state = [h0, h1, h2, h3, h4, h5, h6, h7]
  for i = 0; i < 8; i = i + 1 {
    let val = state[i]
    out[i * 4] = ((val >> 24) & 0xFFU).to_byte()
    out[i * 4 + 1] = ((val >> 16) & 0xFFU).to_byte()
    out[i * 4 + 2] = ((val >> 8) & 0xFFU).to_byte()
    out[i * 4 + 3] = (val & 0xFFU).to_byte()
  }
  out
}

pub fn to_hex(bytes : FixedArray[Byte]) -> String {
  let digits = "0123456789abcdef".to_array()
  let out = StringBuilder::new()
  for i = 0; i < bytes.length(); i = i + 1 {
    let b = bytes[i].to_int() & 0xFF
    out.write_char(digits[(b >> 4) & 0x0F])
    out.write_char(digits[b & 0x0F])
  }
  out.to_string()
}

pub fn sha256_str(s : String) -> String {
  let len = s.length()
  let arr = FixedArray::make(len, b'\x00')
  for i = 0; i < len; i = i + 1 {
    arr[i] = (s[i].to_int() & 0xFF).to_byte()
  }
  to_hex(sha256_bytes(arr))
}

// Capability invocation record
pub(all) struct CapabilityInvocation {
  index : UInt64
  prev_log_hash : String
  timestamp_ms : UInt64
  interface_name : String
  function_name : String
  input_digest : String
  output_digest : String
  fuel_consumed : UInt64
}

pub fn CapabilityInvocation::hash(self : CapabilityInvocation) -> String {
  let payload = self.index.to_string() + ":" + self.prev_log_hash + ":" + self.timestamp_ms.to_string() + ":" + self.interface_name + ":" + self.function_name + ":" + self.input_digest + ":" + self.output_digest + ":" + self.fuel_consumed.to_string()
  sha256_str(payload)
}

pub struct CapabilityChain {
  mut head_hash : String
  mut count : UInt64
}

pub fn CapabilityChain::new() -> CapabilityChain {
  { head_hash: "0000000000000000000000000000000000000000000000000000000000000000", count: 0UL }
}

pub fn CapabilityChain::append(self : CapabilityChain, timestamp_ms : UInt64, interface_name : String, function_name : String, input_digest : String, output_digest : String, fuel_consumed : UInt64) -> CapabilityInvocation {
  let invocation : CapabilityInvocation = {
    index: self.count,
    prev_log_hash: self.head_hash,
    timestamp_ms,
    interface_name,
    function_name,
    input_digest,
    output_digest,
    fuel_consumed,
  }
  self.head_hash = invocation.hash()
  self.count = self.count + 1UL
  invocation
}

pub fn verify_chain(invocations : Array[CapabilityInvocation], initial_prev : String) -> Bool {
  let mut expected_prev = initial_prev
  for i = 0; i < invocations.length(); i = i + 1 {
    let inv = invocations[i]
    if inv.prev_log_hash != expected_prev {
      return false
    }
    expected_prev = inv.hash()
  }
  true
}

// Merkle tree for content-addressed state chunking
pub fn compute_merkle_root(chunk_hashes : Array[String]) -> String {
  if chunk_hashes.length() == 0 {
    return sha256_str("")
  }
  let mut current = chunk_hashes
  while current.length() > 1 {
    let next = []
    let mut i = 0
    while i < current.length() {
      if i + 1 < current.length() {
        let combined = current[i] + ":" + current[i + 1]
        next.push(sha256_str(combined))
      } else {
        // Odd node duplicated to keep balanced binary tree
        let combined = current[i] + ":" + current[i]
        next.push(sha256_str(combined))
      }
      i = i + 2
    }
    current = next
  }
  current[0]
}

// Branch checkpoint model
pub(all) struct AgentCheckpoint {
  parent_oid : String?
  repo : String
  branch : String
  agent_id : String
  generation : UInt64
  state_merkle_root : String
  capability_log_head : String
  task_status : String
  summary : String
}

pub fn AgentCheckpoint::to_json(self : AgentCheckpoint) -> String {
  let parent_val = match self.parent_oid {
    Some(p) => "\"" + p + "\""
    None => "null"
  }
  "{\"parent_oid\":" + parent_val + ",\"repo\":\"" + self.repo + "\",\"branch\":\"" + self.branch + "\",\"agent_id\":\"" + self.agent_id + "\",\"generation\":" + self.generation.to_string() + ",\"state_merkle_root\":\"" + self.state_merkle_root + "\",\"capability_log_head\":\"" + self.capability_log_head + "\",\"task_status\":\"" + self.task_status + "\",\"summary\":\"" + self.summary + "\"}"
}
