///|
fn ec_of(name : String) -> @core.EcLevel {
  match name {
    "L" => @core.EcLevel::L
    "Q" => @core.EcLevel::Q
    "H" => @core.EcLevel::H
    _ => @core.EcLevel::M
  }
}

///|
fn mask_number(mask : @core.Mask) -> Int32 {
  match mask {
    @core.Mask::M0 => 0
    @core.Mask::M1 => 1
    @core.Mask::M2 => 2
    @core.Mask::M3 => 3
    @core.Mask::M4 => 4
    @core.Mask::M5 => 5
    @core.Mask::M6 => 6
    @core.Mask::M7 => 7
  }
}

///|
/// Stable 31-bit digest used to compare whole matrices against reference
/// implementations without storing thousands of characters.
fn digest(qr : @core.QrCode) -> Int32 {
  let mut h : Int64 = 0L
  for b in qr.to_bytes() {
    h = (h * 131L + b.to_int64()) % 1000000007L
  }
  h.to_int()
}

///|
fn rows(qr : @core.QrCode) -> String {
  let sb = StringBuilder()
  for y in 0..<qr.size() {
    if y > 0 {
      sb.write_char('\n')
    }
    for x in 0..<qr.size() {
      sb.write_char(if qr.get(y, x) { '1' } else { '0' })
    }
  }
  sb.to_string()
}

///|
test "reference digests: byte mode across versions, EC levels and masks" {
  let vectors : Array[(String, Int32, String, Int32, Int32)] = [
    ("hi", 1, "L", 0, 110705609),
    ("hi", 1, "L", 3, 560624034),
    ("hi", 1, "L", 7, 340524679),
    ("hi", 1, "M", 0, 825343),
    ("hi", 1, "M", 3, 107903583),
    ("hi", 1, "M", 7, 18134898),
    ("hi", 1, "Q", 0, 687328611),
    ("hi", 1, "Q", 3, 436734512),
    ("hi", 1, "Q", 7, 716605594),
    ("hi", 1, "H", 0, 759742611),
    ("hi", 1, "H", 3, 152012261),
    ("hi", 1, "H", 7, 540699983),
    ("hello world", 2, "L", 0, 745004677),
    ("hello world", 2, "L", 3, 372064010),
    ("hello world", 2, "L", 7, 77925778),
    ("hello world", 2, "M", 0, 196488081),
    ("hello world", 2, "M", 3, 452041267),
    ("hello world", 2, "M", 7, 498930723),
    ("hello world", 2, "Q", 0, 993329642),
    ("hello world", 2, "Q", 3, 597252963),
    ("hello world", 2, "Q", 7, 223265893),
    ("hello world", 2, "H", 0, 304146280),
    ("hello world", 2, "H", 3, 257657739),
    ("hello world", 2, "H", 7, 425066721),
    ("hello world", 7, "L", 0, 189397324),
    ("hello world", 7, "L", 3, 470444553),
    ("hello world", 7, "L", 7, 481411899),
    ("hello world", 7, "M", 0, 565184544),
    ("hello world", 7, "M", 3, 548388681),
    ("hello world", 7, "M", 7, 422306095),
    ("hello world", 7, "Q", 0, 885984125),
    ("hello world", 7, "Q", 3, 552302734),
    ("hello world", 7, "Q", 7, 701222674),
    ("hello world", 7, "H", 0, 858975431),
    ("hello world", 7, "H", 3, 483780385),
    ("hello world", 7, "H", 7, 259827878),
    ("hello world", 10, "L", 0, 105751423),
    ("hello world", 10, "L", 3, 316138248),
    ("hello world", 10, "L", 7, 214152362),
    ("hello world", 10, "M", 0, 190580715),
    ("hello world", 10, "M", 3, 783389545),
    ("hello world", 10, "M", 7, 926296510),
    ("hello world", 10, "Q", 0, 421084798),
    ("hello world", 10, "Q", 3, 727270321),
    ("hello world", 10, "Q", 7, 960808904),
    ("hello world", 10, "H", 0, 853085695),
    ("hello world", 10, "H", 3, 793298105),
    ("hello world", 10, "H", 7, 636171713),
    ("hello world", 27, "L", 0, 456732711),
    ("hello world", 27, "L", 3, 338130767),
    ("hello world", 27, "L", 7, 472180517),
    ("hello world", 27, "M", 0, 113731610),
    ("hello world", 27, "M", 3, 585847569),
    ("hello world", 27, "M", 7, 424925047),
    ("hello world", 27, "Q", 0, 821738958),
    ("hello world", 27, "Q", 3, 131708530),
    ("hello world", 27, "Q", 7, 531228923),
    ("hello world", 27, "H", 0, 475490266),
    ("hello world", 27, "H", 3, 515972026),
    ("hello world", 27, "H", 7, 230443911),
    ("hello world", 40, "L", 0, 756811982),
    ("hello world", 40, "L", 3, 972757172),
    ("hello world", 40, "L", 7, 749774970),
    ("hello world", 40, "M", 0, 595633835),
    ("hello world", 40, "M", 3, 396631104),
    ("hello world", 40, "M", 7, 686295088),
    ("hello world", 40, "Q", 0, 374524712),
    ("hello world", 40, "Q", 3, 338334635),
    ("hello world", 40, "Q", 7, 285286150),
    ("hello world", 40, "H", 0, 170994389),
    ("hello world", 40, "H", 3, 274136486),
    ("hello world", 40, "H", 7, 758520090),
  ]
  for (text, version, ec, mask_num, expected) in vectors {
    let mask = mask_of(mask_num)
    let qr = @core.encode(
      text,
      ec_level=ec_of(ec),
      min_version=version,
      max_version=version,
      mode=@core.Mode::Byte,
      mask~,
    )
    assert_eq(qr.version().value(), version)
    assert_eq(qr.mask(), mask)
    assert_eq(digest(qr), expected)
  }
}

///|
test "reference digests: alphanumeric mode with every mask" {
  let vectors : Array[(String, Int32, Int32)] = [
    ("L", 0, 737694076),
    ("L", 1, 686400586),
    ("L", 2, 423643674),
    ("L", 3, 363198851),
    ("L", 4, 830998824),
    ("L", 5, 171532450),
    ("L", 6, 273432339),
    ("L", 7, 120230773),
    ("M", 0, 784288095),
    ("M", 1, 988596480),
    ("M", 2, 819055506),
    ("M", 3, 110410999),
    ("M", 4, 412660150),
    ("M", 5, 172180198),
    ("M", 6, 584968249),
    ("M", 7, 151046356),
    ("Q", 0, 824365753),
    ("Q", 1, 105078418),
    ("Q", 2, 421367409),
    ("Q", 3, 375848196),
    ("Q", 4, 513980551),
    ("Q", 5, 419324028),
    ("Q", 6, 633642450),
    ("Q", 7, 29621437),
    ("H", 0, 242904502),
    ("H", 1, 414720472),
    ("H", 2, 774319807),
    ("H", 3, 672016193),
    ("H", 4, 653261289),
    ("H", 5, 699843068),
    ("H", 6, 168808154),
    ("H", 7, 151972278),
  ]
  for (ec, mask_num, expected) in vectors {
    let qr = @core.encode(
      "HELLO WORLD",
      ec_level=ec_of(ec),
      min_version=2,
      max_version=2,
      mode=@core.Mode::Alphanumeric,
      mask=mask_of(mask_num),
    )
    assert_eq(digest(qr), expected)
  }
}

///|
test "reference digests: numeric mode with every mask" {
  let expected : Array[Int32] = [
    523244762, 326935681, 541547735, 493211965, 682933700, 70487092, 488447974, 75755295,
  ]
  for mask_num in 0..<8 {
    let qr = @core.encode(
      "01234567",
      ec_level=@core.EcLevel::H,
      min_version=1,
      max_version=1,
      mode=@core.Mode::Numeric,
      mask=mask_of(mask_num),
    )
    assert_eq(digest(qr), expected[mask_num])
  }
}

///|
test "snapshot: HELLO WORLD V1-M mask 0" {
  let qr = @core.encode("HELLO WORLD", mask=@core.Mask::M0)
  assert_eq(qr.version().value(), 1)
  assert_eq(qr.ec_level(), @core.EcLevel::M)
  assert_eq(
    rows(qr),
    "111111100010101111111\n100000101110001000001\n101110100010101011101\n101110100010101011101\n101110101011101011101\n100000100111001000001\n111111101010101111111\n000000000000000000000\n101010100100100010010\n011110001001000010001\n000111111101001011000\n111101011001110101110\n010011110101001110101\n000000001010001000101\n111111100000100101100\n100000100110001101000\n101110101100101111111\n101110100011010100010\n101110101111011101001\n100000100001110001011\n111111101101011100001",
  )
}

///|
test "snapshot: UTF-8 byte payload matches the reference encoder" {
  let qr = @core.encode("中文测试")
  assert_eq(qr.version().value(), 1)
  assert_eq(qr.mask(), @core.Mask::M7)
  assert_eq(
    rows(qr),
    "111111100001001111111\n100000100110001000001\n101110100001101011101\n101110100101101011101\n101110100111101011101\n100000101110001000001\n111111101010101111111\n000000000010000000000\n100101101011110100000\n001100011111000011101\n010110111001001010110\n111011001110110110111\n101010101011001110011\n000000001011001100101\n111111100001111100111\n100000101000001010110\n101110100111110100000\n101110101111010101011\n101110100010001000001\n100000100000101101100\n111111101000001110110",
  )
}

///|
test "finder patterns, separators and dark module are present in every symbol" {
  for version in 1..<=40 {
    for
      ec in [
        @core.EcLevel::L,
        @core.EcLevel::M,
        @core.EcLevel::Q,
        @core.EcLevel::H,
      ] {
      let qr = @core.encode(
        "hi",
        ec_level=ec,
        min_version=version,
        max_version=version,
        mode=@core.Mode::Byte,
      )
      let size = qr.size()
      assert_eq(size, 4 * version + 17)
      // Three finder patterns: dark outer ring, light ring, dark 3x3 core.
      for (oy, ox) in [(0, 0), (0, size - 7), (size - 7, 0)] {
        for
          (dy, dx) in [
            (0, 0),
            (0, 3),
            (0, 6),
            (3, 0),
            (3, 6),
            (6, 0),
            (6, 3),
            (6, 6),
          ] {
          assert_true(qr.get(oy + dy, ox + dx))
        }
        assert_false(qr.get(oy + 1, ox + 1))
        assert_true(qr.get(oy + 2, ox + 2))
      }
      // Dark module immediately above the bottom-left format area.
      assert_true(qr.get(4 * version + 9, 8))
      // Separators around the top-left finder are light.
      for i in 0..<=7 {
        assert_false(qr.get(7, i))
        assert_false(qr.get(i, 7))
      }
    }
  }
}

///|
test "timing patterns alternate in version 1" {
  let qr = @core.encode(
    "hi",
    min_version=1,
    max_version=1,
    mode=@core.Mode::Byte,
  )
  let size = qr.size()
  for x in 8..<(size - 8) {
    assert_eq(qr.get(6, x), x % 2 == 0)
  }
  for y in 8..<(size - 8) {
    assert_eq(qr.get(y, 6), y % 2 == 0)
  }
}

///|
test "forced masks are recorded and produce distinct symbols" {
  let qr = @core.encode("forced mask check", mask=@core.Mask::M0)
  assert_eq(mask_number(qr.mask()), 0)
  let other = @core.encode("forced mask check", mask=@core.Mask::M1)
  assert_eq(mask_number(other.mask()), 1)
  assert_true(rows(qr) != rows(other))
}
