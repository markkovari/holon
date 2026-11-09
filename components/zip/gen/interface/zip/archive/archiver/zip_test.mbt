fn hex_to_bytes(hex : String) -> FixedArray[Byte] {
  let len = hex.length() / 2
  let arr = FixedArray::make(len, (0).to_byte())
  for i = 0; i < len; i = i + 1 {
    let c1 = hex[i * 2].to_int()
    let c2 = hex[i * 2 + 1].to_int()
    let v1 = if c1 >= '0'.to_int() && c1 <= '9'.to_int() {
      c1 - '0'.to_int()
    } else if c1 >= 'a'.to_int() && c1 <= 'f'.to_int() {
      c1 - 'a'.to_int() + 10
    } else {
      c1 - 'A'.to_int() + 10
    }
    let v2 = if c2 >= '0'.to_int() && c2 <= '9'.to_int() {
      c2 - '0'.to_int()
    } else if c2 >= 'a'.to_int() && c2 <= 'f'.to_int() {
      c2 - 'a'.to_int() + 10
    } else {
      c2 - 'A'.to_int() + 10
    }
    arr[i] = ((v1 << 4) | v2).to_byte()
  }
  arr
}

const CARDS_XLSX_HEX : String = "504b0304140000000800acb31b5ddf691e500c010000b5020000130000005b436f6e74656e745f54797065735d2e786d6cad52c94ec33010fd15cbd72a76cb012194b407962320513e60b02789556ff2b825fd7b9cb420840abdf434b2dfaad1d4abc159b6c34426f8862fc49c33f42a68e3bb86bfad1fab1bce2883d76083c786ef91f86a59aff7118915ada786f739c75b2949f5e8804488e80bd286e4209767ea6404b5810ee5d57c7e2d55f0197daef2e8c197f53db6b0b5993d0ce5fbd023a125ceee0ec431abe110a3350a72c1e5ceeb5f29d5314114e5c4a1de449a150297271346e4ef80a3eeb92c26198dec05527e02575872b0f223a4cd7b081bf1bfc98996a16d8d421dd4d61589a0981034f588d959314de1c0f8d9f9fc894c721a8b0b17f9f63fd3837a48a85f732ac742175fc60fefaf1e723abbe527504b0304140000000800acb31b5d98daeb8bae000000270100000b0000005f72656c732f2e72656c738dcfc10e82300c06e057597a978107630c838b31e16af001e6560601d6659b0a6fef8e623c786cfaf7fbd3b25ee6893dd18781ac8022cb81a155a4076b04dcdacbee082c4469b59cc8a2801503d45579c549c67412fac105960c1b04f431ba13e741f538cb9091439b361df959c6347ac39d54a334c8f7797ee0fed380adc91a2dc037ba00d6ae0effb1a9eb068567528f196dfc51f19548b2f406a38065e22ff2e39d68cc120abc2af9e6c1ea0d504b0304140000000800acb31b5dd0627117bb0000001a0100000f000000786c2f776f726b626f6f6b2e786d6c8d8fcb6ec2400c457f65e47d99c002a128098b5648eccb07b819878cc8d8913d14f87ba63cf65df9a57b7c6fb3bda6c9fd925a146e61b9a8c011f712221f5b387cef3e36e02c23079c84a9851b196cbbe6227afa1139b922676b61cc79aebdb77ea484b69099b85c06d184b98c7af4362b61b09128a7c9afaa6aed13468627a1d6ff306418624f5fd29f13717e429426ccc5bc8d7136e89ac7077b55c7988ae94fd46025c7df6a1f4a4c705ac7d2e83e2cc1778d7fabfc3b587707504b0304140000000800acb31b5dd6df7c96c8000000b50100001a000000786c2f5f72656c732f776f726b626f6f6b2e786d6c2e72656c73ad90cf6ac3300c875fc5e8be28e9619451b79732e875eb1e40d84a1c9ad846f2d6f6ed6b06fb13e861879d8424f4e9e3b7d95de6c97cb0e898a285ae69c17074c98f71b0f0767c7e5883d142d1d394225bb8b2c26ebb79e1894a3dd13066359511d54228253f21aa0b3c93362973ac9b3ec94ca5b632602677a28171d5b68f28bf19b0649a83b72007df81395e33ff859dfa7e74bc4fee7de658eebcc073929306e652a12403170bdf23c5cfd235950a785f66f59f321a48d8bf16a949eb8fd062fc25838bb8b737504b0304140000000800acb31b5d4c395c1ef20000000702000014000000786c2f736861726564537472696e67732e786d6c6dd1df4bc3301007f07f25e4dda52d3a86b41d6ce89b20ea9ee56c6f6bb0b974b98b73fef54604c1748ff7b9fb72f951af3fdda83e30b0f5d4e872516885d4f9ded2a1d1bb97fbab95562c403d8c9eb0d16764bd6e6b66512949dce84164ba3586bb011df0c24f48a9b3f7c181a4321c0c4f01a1e70151dc68aaa2581a0796b4ea7c24495b975a45b2c788db3f482b6c5b4b4be0b036d2d6e6a7fe3546c989a27bc390eb3102899573ee13d8fed559f2b344174348f79f25b60304fb05a1cf1b1b6054cff3035d9bb2a872bcdb3de5f468dfa11b62ce37ab4bf9cd082cdef2ec45aa4bd30f789293cfb52cfecf9af495ed37504b0304140000000800acb31b5d9dc29356370100002a04000018000000786c2f776f726b7368656574732f7368656574312e786d6c8593ed6e832014866fc5f07fc50fe8ba456de6b457b05d00b1ac9a29182076bbfbb1b621076aec3f398f471e5e0ff9fe671ca2992bdd4b51a06413a3888b561e7b712ad0e7c7e16987226d9838b2410a5ea05faed1becccf527deb8e7313d97ea10bd41933bd62acdb8e8f4c6fe4c485255f528dccd8a53a613d29ce8e97a671c0691c6ff1c87a81cafc52ab996165aee43952d6c356dbff87b70445a640daaee732cef15ce6b8bdb10ab2c467ef90a53eab21cb7cd640467c76808c3a86adb3134f9d780a5ede06e2903d07e290ed02f174e9a8cdad6a138d83840ef0632fcbc69933ce609c61d6d98ab2d718d8d5d95294cdb59ad23be3ecb13171c6046e1cfce58aac187b8dc108d46469689a6b95d0fb90c96365ea9429dc3948a5a22bca5e230d94e9e22c5fabc9ee5e99ae2863701db1bbe7e51f504b01021403140000000800acb31b5ddf691e500c010000b50200001300000000000000000000008001000000005b436f6e74656e745f54797065735d2e786d6c504b01021403140000000800acb31b5d98daeb8bae000000270100000b000000000000000000000080013d0100005f72656c732f2e72656c73504b01021403140000000800acb31b5dd0627117bb0000001a0100000f0000000000000000000000800114020000786c2f776f726b626f6f6b2e786d6c504b01021403140000000800acb31b5dd6df7c96c8000000b50100001a00000000000000000000008001fc020000786c2f5f72656c732f776f726b626f6f6b2e786d6c2e72656c73504b01021403140000000800acb31b5d4c395c1ef2000000070200001400000000000000000000008001fc030000786c2f736861726564537472696e67732e786d6c504b01021403140000000800acb31b5d9dc29356370100002a040000180000000000000000000000800120050000786c2f776f726b7368656574732f7368656574312e786d6c504b05060000000006000600870100008d0600000000"

test "store round trips" {
  let files : Array[File] = [
    { name: "a.txt", data: FixedArray::makei(5, fn(i) { "hello"[i].to_int().to_byte() }) },
    { name: "nested/b.bin", data: FixedArray::makei(256, fn(i) { i.to_byte() }) },
    { name: "empty.txt", data: FixedArray::make(0, (0).to_byte()) },
  ]
  let bytes = archive(files)
  match extract(bytes) {
    Ok(extracted) => {
      assert_eq(extracted.length(), 3)
      assert_eq(extracted[0].name, "a.txt")
      assert_eq(extracted[0].data.length(), 5)
      assert_eq(extracted[1].name, "nested/b.bin")
      assert_eq(extracted[1].data.length(), 256)
      assert_eq(extracted[2].name, "empty.txt")
      assert_eq(extracted[2].data.length(), 0)
    }
    Err(_) => panic()
  }
}

test "something that is not a zip" {
  let not_zip = FixedArray::makei(16, fn(i) { "not a zip at all"[i].to_int().to_byte() })
  assert_true(match extract(not_zip) { Err(NotAZip) => true; _ => false })

  let empty_bytes = FixedArray::make(0, (0).to_byte())
  assert_true(match extract(empty_bytes) { Err(NotAZip) => true; _ => false })

  let png_magic : FixedArray[Byte] = [
    (0x89).to_byte(), 'P'.to_int().to_byte(), 'N'.to_int().to_byte(), 'G'.to_int().to_byte(),
    (13).to_byte(), (10).to_byte(), (26).to_byte(), (10).to_byte(),
    (0).to_byte(), (0).to_byte(), (0).to_byte(), (13).to_byte()
  ]
  assert_true(match extract(png_magic) { Err(NotAZip) => true; _ => false })
}

test "a corrupted entry is refused rather than returned" {
  let files : Array[File] = [
    { name: "a.txt", data: FixedArray::makei(21, fn(i) { "the original contents"[i].to_int().to_byte() }) }
  ]
  let bytes = archive(files)
  let at = 30 + "a.txt".length() + 3
  bytes[at] = (bytes[at].to_int() ^ 0xFF).to_byte()
  assert_true(match extract(bytes) {
    Err(BadChecksum(name)) => name == "a.txt"
    _ => false
  })
}

test "a truncated archive says so" {
  let files : Array[File] = [
    { name: "a.txt", data: FixedArray::make(400, 'x'.to_int().to_byte()) }
  ]
  let bytes = archive(files)
  // Cut bytes from 40 to 200
  let cut = FixedArray::make(bytes.length() - 160, (0).to_byte())
  for i = 0; i < 40; i = i + 1 {
    cut[i] = bytes[i]
  }
  for i = 200; i < bytes.length(); i = i + 1 {
    cut[i - 160] = bytes[i]
  }
  assert_true(match extract(cut) {
    Err(Truncated(_)) => true
    Err(BadChecksum(_)) => true
    _ => false
  })
}

test "a real deflated xlsx reads back" {
  let xlsx_bytes = hex_to_bytes(CARDS_XLSX_HEX)
  match extract(xlsx_bytes) {
    Ok(files) => {
      let mut found_sheet1 = false
      let mut found_shared = false
      let mut found_content_types = false
      for f in files {
        if f.name == "xl/worksheets/sheet1.xml" { found_sheet1 = true }
        if f.name == "xl/sharedStrings.xml" { found_shared = true }
        if f.name == "[Content_Types].xml" { found_content_types = true }
      }
      assert_true(found_sheet1)
      assert_true(found_shared)
      assert_true(found_content_types)

      let sheet1 = files.filter(fn(f) { f.name == "xl/worksheets/sheet1.xml" })[0]
      let sheet_str = @utf8.decode_lossy(Bytes::from_fixedarray(sheet1.data)[:])
      assert_true(sheet_str.has_prefix("<?xml"))
      assert_true(sheet_str.contains("<sheetData>"))

      let sst = files.filter(fn(f) { f.name == "xl/sharedStrings.xml" })[0]
      let sst_str = @utf8.decode_lossy(Bytes::from_fixedarray(sst.data)[:])
      assert_true(sst_str.contains("<t>Charizard</t>"))
    }
    Err(_) => panic()
  }
}

test "inflated entries are whole and directories dropped" {
  let xlsx_bytes = hex_to_bytes(CARDS_XLSX_HEX)
  match extract(xlsx_bytes) {
    Ok(files) => {
      for f in files {
        assert_true(f.data.length() > 0)
        assert_false(f.name.has_suffix("/"))
        // Check ends with '>'
        assert_eq(f.data[f.data.length() - 1], '>'.to_int().to_byte())
      }
    }
    Err(_) => panic()
  }
}
