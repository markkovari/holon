// Implementation of experiment:assign/assigner

let bucket_name = "default"

fn open_bucket() -> Result[@kv.Bucket, AssignError] {
  match @kv.open(bucket_name) {
    Ok(b) => Ok(b)
    Err(_) => Err(BackendUnavailable("open failed"))
  }
}

fn read_set(bucket : @kv.Bucket, key : String) -> Result[Array[Arm]?, AssignError] {
  match bucket.get(key) {
    Ok(Some(bytes)) => match @core.variants_from_bytes(bytes) {
      Some(pairs) => {
        let arms = []
        for p in pairs {
          arms.push({ name: p.0, weight: p.1 })
        }
        Ok(Some(arms))
      }
      None => Ok(None)
    }
    Ok(None) => Ok(None)
    Err(_) => Err(BackendUnavailable("get failed"))
  }
}

fn effective(bucket : @kv.Bucket, name : String, tenant : String) -> Result[Array[Arm], AssignError] {
  if tenant.length() > 0 {
    let tenant_key = @core.exp_key(tenant, name)
    match read_set(bucket, tenant_key) {
      Ok(Some(vs)) => return Ok(vs)
      Ok(None) => ()
      Err(e) => return Err(e)
    }
  }
  let global_key = @core.exp_key("", name)
  match read_set(bucket, global_key) {
    Ok(Some(vs)) => Ok(vs)
    Ok(None) => Err(NotFound)
    Err(e) => Err(e)
  }
}

///|
pub fn set_experiment(name : String, tenant : String, variants : Array[Arm]) -> Result[Unit, AssignError] {
  if variants.length() == 0 {
    return Err(InvalidVariants("no variants"))
  }
  let mut all_zero = true
  for v in variants {
    if v.weight > 0U {
      all_zero = false
      break
    }
  }
  if all_zero {
    return Err(InvalidVariants("all weights zero"))
  }
  let bucket = match open_bucket() {
    Ok(b) => b
    Err(e) => return Err(e)
  }
  let pairs = []
  for v in variants {
    pairs.push((v.name, v.weight))
  }
  let bytes = @core.variants_to_bytes(pairs)
  let key = @core.exp_key(tenant, name)
  match bucket.set(key, bytes) {
    Ok(_) => Ok(())
    Err(_) => Err(BackendUnavailable("set failed"))
  }
}

///|
pub fn clear_experiment(name : String, tenant : String) -> Result[Unit, AssignError] {
  let bucket = match open_bucket() {
    Ok(b) => b
    Err(e) => return Err(e)
  }
  let key = @core.exp_key(tenant, name)
  match bucket.delete(key) {
    Ok(_) => Ok(())
    Err(_) => Err(BackendUnavailable("delete failed"))
  }
}

///|
pub fn assign(name : String, ctx : Context) -> Result[String, AssignError] {
  let bucket = match open_bucket() {
    Ok(b) => b
    Err(e) => return Err(e)
  }
  let variants = match effective(bucket, name, ctx.tenant) {
    Ok(vs) => vs
    Err(e) => return Err(e)
  }
  let pairs = []
  for v in variants {
    pairs.push((v.name, v.weight))
  }
  match @core.pick(pairs, ctx.subject) {
    Some(arm) => Ok(arm)
    None => Err(InvalidVariants("total weight zero"))
  }
}

///|
pub fn describe(name : String, tenant : String) -> Result[Array[Arm], AssignError] {
  let bucket = match open_bucket() {
    Ok(b) => b
    Err(e) => return Err(e)
  }
  effective(bucket, name, tenant)
}

///|
pub fn cohort(name : String, tenant : String, n : UInt) -> Result[Array[Assignment], AssignError] {
  let bucket = match open_bucket() {
    Ok(b) => b
    Err(e) => return Err(e)
  }
  let variants = match effective(bucket, name, tenant) {
    Ok(vs) => vs
    Err(e) => return Err(e)
  }
  let pairs = []
  for v in variants {
    pairs.push((v.name, v.weight))
  }
  let mut count = n
  if count < 1U {
    count = 1U
  } else if count > 1000U {
    count = 1000U
  }
  let out = []
  for i = 0U; i < count; i = i + 1U {
    let subject = "subject-\{i}"
    let arm = match @core.pick(pairs, subject) {
      Some(a) => a
      None => return Err(InvalidVariants("total weight zero"))
    }
    out.push({ subject, arm })
  }
  Ok(out)
}
