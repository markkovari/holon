///|
/// Error correction level. Capacity order is L > M > Q > H.
///
/// 纠错等级。容量顺序为 L > M > Q > H。
pub(all) enum EcLevel {
  L
  M
  Q
  H
} derive(Eq, Debug)

///|
pub impl Show for EcLevel with fn to_string(self) {
  match self {
    L => "L"
    M => "M"
    Q => "Q"
    H => "H"
  }
}

///|
pub extend EcLevel with Debug::{to_repr}

///|
pub extend EcLevel with Eq::{not_equal, equal}

///|
pub extend EcLevel with Show::{to_string, output}

///|
/// Segment mode defined by ISO/IEC 18004 Model 2.
///
/// ISO/IEC 18004 Model 2 定义的段模式。
pub(all) enum Mode {
  Numeric
  Alphanumeric
  Byte
} derive(Eq, Debug)

///|
pub impl Show for Mode with fn to_string(self) {
  match self {
    Numeric => "Numeric"
    Alphanumeric => "Alphanumeric"
    Byte => "Byte"
  }
}

///|
pub extend Mode with Debug::{to_repr}

///|
pub extend Mode with Eq::{not_equal, equal}

///|
pub extend Mode with Show::{to_string, output}

///|
/// Data mask patterns 0..7.
///
/// 数据掩码图案 0..7。
pub(all) enum Mask {
  M0
  M1
  M2
  M3
  M4
  M5
  M6
  M7
} derive(Eq, Debug)

///|
pub impl Show for Mask with fn to_string(self) {
  match self {
    M0 => "M0"
    M1 => "M1"
    M2 => "M2"
    M3 => "M3"
    M4 => "M4"
    M5 => "M5"
    M6 => "M6"
    M7 => "M7"
  }
}

///|
pub extend Mask with Debug::{to_repr}

///|
pub extend Mask with Eq::{not_equal, equal}

///|
pub extend Mask with Show::{to_string, output}

///|
/// QR version 1..40. Values can only be built through `Version::new`.
///
/// QR 版本 1..40。只能通过 `Version::new` 构造。
pub struct Version(Int32) derive(Eq)

///|
pub extend Version with Eq::{not_equal, equal}

///|
pub fn Version::new(value : Int32) -> Version? {
  if value >= 1 && value <= 40 {
    Some(Version(value))
  } else {
    None
  }
}

///|
pub fn Version::value(self : Version) -> Int32 {
  self.0
}

///|
/// Side length in modules (4 * version + 17).
///
/// 模块边长（4 * version + 17）。
pub fn Version::size(self : Version) -> Int32 {
  self.0 * 4 + 17
}

///|
/// A finished, masked QR code symbol.
///
/// 已完成的、已应用掩码的 QR 码符号。
pub struct QrCode {
  version_ : Version
  ec_level_ : EcLevel
  mask_ : Mask
  size_ : Int32
  modules_ : Array[Bool]
}

///|
pub fn QrCode::version(self : QrCode) -> Version {
  self.version_
}

///|
pub fn QrCode::ec_level(self : QrCode) -> EcLevel {
  self.ec_level_
}

///|
pub fn QrCode::mask(self : QrCode) -> Mask {
  self.mask_
}

///|
pub fn QrCode::size(self : QrCode) -> Int32 {
  self.size_
}

///|
/// Returns whether the module at `row`, `col` is dark. Panics when out of range.
///
/// 返回指定位置 `(row, col)` 的模块是否为深色。越界时会 panic。
pub fn QrCode::get(self : QrCode, row : Int32, col : Int32) -> Bool {
  self.modules_[row * self.size_ + col]
}

///|
/// Row-major copy of the module matrix.
///
/// 行优先的模块矩阵副本。
pub fn QrCode::modules(self : QrCode) -> Array[Bool] {
  self.modules_.copy()
}

///|
/// Row-major bytes, 1 for dark and 0 for light.
///
/// 行优先字节数组，1 表示深色，0 表示浅色。
pub fn QrCode::to_bytes(self : QrCode) -> Array[Byte] {
  self.modules_.map(fn(dark) { if dark { (1 : Byte) } else { (0 : Byte) } })
}
