///|
pub(all) suberror QrError {
  EmptyInput
  DataTooLong(
    needed~ : Int32,
    capacity~ : Int32,
    version~ : Int32,
    ec_level~ : EcLevel
  )
  InvalidVersion(Int32)
  InvalidCharacter(char~ : Char, mode~ : Mode)
  InvalidColor(String)
  Internal(String)
}

///|
/// Human readable diagnostic; messages name the violated constraint.
///
/// 可读的错误诊断信息，描述被违反的约束。
pub fn QrError::message(self : QrError) -> String {
  match self {
    EmptyInput => "input is empty"
    DataTooLong(needed~, capacity~, version~, ec_level~) =>
      "data needs \{needed} bits but V\{version}-\{ec_level} holds \{capacity} bits; use a higher version, a lower EC level, or shorten the input"
    InvalidVersion(v) => "invalid QR version \{v}; expected 1..40"
    InvalidCharacter(char~, mode~) =>
      "character '\{char}' cannot be encoded in \{mode} mode"
    InvalidColor(color) => "invalid CSS color: \{color}"
    Internal(reason) => "internal invariant violated: \{reason}"
  }
}
