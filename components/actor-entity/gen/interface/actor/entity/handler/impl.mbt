// Reference implementation of actor:entity/handler

///|
pub fn on_spawn(_id : String, _state : FixedArray[Byte]) -> Result[Unit, String] {
  Ok(())
}

///|
pub fn on_message(_message : FixedArray[Byte]) -> Result[Unit, String] {
  Ok(())
}

///|
pub fn on_snapshot() -> FixedArray[Byte] {
  FixedArray::make(0, b'\x00')
}
