// comp-media-apple — the macOS half of comp-media (ADR-0098).
//
// Core Image develops the raw file into three renditions, Vision looks at the
// AI copy, and a Metal kernel computes `green-stab-v1` sharpness on the green
// plane comp-media already pulled out of the Bayer data. One process per photo:
// comp-media runs it, reads the JPEGs it wrote, and parses the one JSON object
// it prints. Everything here is on-device; nothing is sent anywhere.
//
//   swiftc -O -o comp-media-apple tools/media-apple/main.swift
//   comp-media-apple --original <file.arw> --green <plane.f32> \
//                    --green-width W --green-height H --out <dir>
//
// The wire shapes (renditions / sharpness / vision) are the callback's, in
// components/media-pipeline/CONTRACT.md. Every box printed is [x, y, w, h],
// normalised, TOP-LEFT origin — Vision's own boxes are bottom-left, and that
// conversion happens here and nowhere else.
//
// The Metal kernel must match comp-media's CPU `green_stab_v1` tile for tile
// (sqrt -> separable [1 2 1]/4 leaving borders -> 4-neighbour Laplacian ->
// per-tile variance x1e6, last tile row/column taking the remainder). The spike
// that produced it printed identical tile values from both.

import CoreImage
import Foundation
import ImageIO
import Metal
import Vision

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write("comp-media-apple: \(msg)\n".data(using: .utf8)!)
    exit(1)
}

func ms(_ t: DispatchTime) -> Int { Int((DispatchTime.now().uptimeNanoseconds - t.uptimeNanoseconds) / 1_000_000) }
func r3(_ v: Double) -> Double { (v * 1000).rounded() / 1000 }

// ---- arguments ---------------------------------------------------------------
var opts: [String: String] = [:]
var it = CommandLine.arguments.dropFirst().makeIterator()
while let k = it.next() {
    guard k.hasPrefix("--"), let v = it.next() else { fail("usage: --original F --green F --green-width W --green-height H --out DIR") }
    opts[String(k.dropFirst(2))] = v
}
guard let originalPath = opts["original"], let greenPath = opts["green"], let outPath = opts["out"],
      let gw = Int(opts["green-width"] ?? ""), let gh = Int(opts["green-height"] ?? "") else {
    fail("usage: --original F --green F --green-width W --green-height H --out DIR")
}
let original = URL(fileURLWithPath: originalPath)
let outdir = URL(fileURLWithPath: outPath)
let device = MTLCreateSystemDefaultDevice()
let ci = device.map { CIContext(mtlDevice: $0) } ?? CIContext()
let srgb = CGColorSpace(name: CGColorSpace.sRGB)!
var timings: [String: Int] = [:]

// ---- develop -----------------------------------------------------------------
// The largest rendition is 4096 px on the long edge, so develop at just over
// that: CIRAWFilter's scaleFactor demosaics at reduced size, which is most of
// what made a full-size develop cost ~1.35 s. A JPEG original is not a raw
// file; Core Image opens it directly, upright.
var t = DispatchTime.now()
let developer: String
let developed: CIImage
if let raw = CIRAWFilter(imageURL: original) {
    let native = raw.nativeSize
    let long = max(native.width, native.height)
    if long > 4096 { raw.scaleFactor = Float(min(1.0, 4200.0 / long)) }
    guard let img = raw.outputImage else { fail("Core Image could not develop \(originalPath)") }
    developed = img
    developer = "coreimage"
} else if let img = CIImage(contentsOf: original, options: [.applyOrientationProperty: true]) {
    developed = img
    developer = "coreimage"
} else {
    fail("Core Image cannot open \(originalPath)")
}
// No camera metadata in a rendition: Core Image would otherwise copy the raw
// file's EXIF (serial number, GPS when the camera had it) into every JPEG, and
// the share copy is meant to leave the building. The callback carries what the
// app needs; the pixels are already upright.
let base = developed
    .transformed(by: CGAffineTransform(translationX: -developed.extent.minX, y: -developed.extent.minY))
    .settingProperties([:])

/// Scale to exactly `long` px on the long edge, integer dimensions, no
/// half-covered edge pixel.
func fit(_ img: CIImage, long: CGFloat) -> (CIImage, Int, Int) {
    let (w, h) = (img.extent.width, img.extent.height)
    let s = min(1.0, long / max(w, h))
    let (tw, th) = (max(1, Int((w * s).rounded())), max(1, Int((h * s).rounded())))
    let sy = CGFloat(th) / h, sx = CGFloat(tw) / w
    let f = CIFilter(name: "CILanczosScaleTransform")!
    f.setValue(img, forKey: kCIInputImageKey)
    f.setValue(sy, forKey: kCIInputScaleKey)
    f.setValue(sx / sy, forKey: kCIInputAspectRatioKey)
    return (f.outputImage!.cropped(to: CGRect(x: 0, y: 0, width: tw, height: th)), tw, th)
}

func writeJPEG(_ img: CIImage, _ name: String, quality: Double) -> Int {
    let u = outdir.appendingPathComponent(name)
    let key = CIImageRepresentationOption(rawValue: kCGImageDestinationLossyCompressionQuality as String)
    do { try ci.writeJPEGRepresentation(of: img, to: u, colorSpace: srgb, options: [key: quality]) } catch { fail("writing \(name): \(error)") }
    return (try? FileManager.default.attributesOfItem(atPath: u.path)[.size] as? Int) ?? 0
}

var renditions: [String: Any] = [:]
let (share, sw, sh) = fit(base, long: 4096)
var q = 0.85
var shareBytes = writeJPEG(share, "share.jpg", quality: q)
while shareBytes >= 10 * 1024 * 1024 && q > 0.5 { q -= 0.1; shareBytes = writeJPEG(share, "share.jpg", quality: q) }
renditions["share"] = ["width": sw, "height": sh, "bytes": shareBytes]
let (ai, aw, ah) = fit(base, long: 1568)
renditions["ai"] = ["width": aw, "height": ah, "bytes": writeJPEG(ai, "ai.jpg", quality: 0.85)]
let (thumb, tw, th) = fit(base, long: 512)
renditions["thumb"] = ["width": tw, "height": th, "bytes": writeJPEG(thumb, "thumb.jpg", quality: 0.8)]
timings["develop"] = ms(t)

// ---- Vision, on the AI copy --------------------------------------------------
t = DispatchTime.now()
/// Vision's normalised rect (bottom-left origin) as [x, y, w, h] top-left.
func box(_ r: CGRect) -> [Double] { [r3(r.minX), r3(1 - r.maxY), r3(r.width), r3(r.height)] }

guard let cg = ci.createCGImage(ai, from: ai.extent, format: .RGBA8, colorSpace: srgb) else { fail("no CGImage for Vision") }
let classify = VNClassifyImageRequest()
let attention = VNGenerateAttentionBasedSaliencyImageRequest()
let objectness = VNGenerateObjectnessBasedSaliencyImageRequest()
let faces = VNDetectFaceCaptureQualityRequest()
let horizon = VNDetectHorizonRequest()
var requests: [VNRequest] = [classify, attention, objectness, faces, horizon]
var aestheticsReq: VNRequest? = nil
if #available(macOS 15.0, *) {
    let a = VNCalculateImageAestheticsScoresRequest()
    aestheticsReq = a
    requests.append(a)
}
do { try VNImageRequestHandler(cgImage: cg).perform(requests) } catch { fail("Vision: \(error)") }

let faceObs = faces.results ?? []
var vision: [String: Any] = [
    "labels": (classify.results ?? []).filter { $0.confidence > 0.1 }.prefix(8).map { ["id": $0.identifier, "confidence": r3(Double($0.confidence))] },
    "faces": faceObs.map { ["box": box($0.boundingBox), "quality": r3(Double($0.faceCaptureQuality ?? -1))] },
    "attention": (attention.results?.first?.salientObjects ?? []).map { box($0.boundingBox) },
    "objectness": (objectness.results?.first?.salientObjects ?? []).map { box($0.boundingBox) },
    "aesthetics": NSNull(),
    "horizon_deg": horizon.results?.first.map { r3(Double($0.angle) * 180 / .pi) } ?? NSNull(),
]
if #available(macOS 15.0, *), let a = (aestheticsReq as? VNCalculateImageAestheticsScoresRequest)?.results?.first {
    vision["aesthetics"] = ["overall": r3(Double(a.overallScore)), "utility": a.isUtility]
}
timings["vision"] = ms(t)

// ---- Metal: green-stab-v1 on the green plane ---------------------------------
let grid = 16
var sharpness: Any = NSNull()
if let device {
    guard let green = try? Data(contentsOf: URL(fileURLWithPath: greenPath)), green.count == gw * gh * 4 else {
        fail("green plane is not \(gw)x\(gh) little-endian f32")
    }
    let src = """
    #include <metal_stdlib>
    using namespace metal;
    kernel void stab(device const float* g [[buffer(0)]], device float* s [[buffer(1)]], constant uint2& d [[buffer(2)]], uint2 p [[thread_position_in_grid]]) {
      if (p.x >= d.x || p.y >= d.y) return; s[p.y*d.x+p.x] = sqrt(max(g[p.y*d.x+p.x], 0.0f));
    }
    kernel void blurx(device const float* a [[buffer(0)]], device float* b [[buffer(1)]], constant uint2& d [[buffer(2)]], uint2 p [[thread_position_in_grid]]) {
      if (p.x >= d.x || p.y >= d.y) return; uint i = p.y*d.x+p.x;
      b[i] = (p.x == 0 || p.x == d.x-1) ? a[i] : (a[i-1] + 2*a[i] + a[i+1]) * 0.25f;
    }
    kernel void blury(device const float* a [[buffer(0)]], device float* b [[buffer(1)]], constant uint2& d [[buffer(2)]], uint2 p [[thread_position_in_grid]]) {
      if (p.x >= d.x || p.y >= d.y) return; uint i = p.y*d.x+p.x;
      b[i] = (p.y == 0 || p.y == d.y-1) ? a[i] : (a[i-d.x] + 2*a[i] + a[i+d.x]) * 0.25f;
    }
    // One threadgroup per tile: Laplacian, then sum and sum of squares reduced in threadgroup memory.
    kernel void tiles(device const float* a [[buffer(0)]], device float* out [[buffer(1)]], constant uint2& d [[buffer(2)]], constant uint& grid [[buffer(3)]],
                      uint2 tg [[threadgroup_position_in_grid]], uint2 tid2 [[thread_position_in_threadgroup]], uint2 tn2 [[threads_per_threadgroup]]) {
      uint tid = tid2.x, tn = tn2.x;
      threadgroup float ss[256]; threadgroup float s1[256]; threadgroup float sn[256];
      uint tw = max(d.x / grid, 1u), th = max(d.y / grid, 1u);
      uint x0 = tg.x * tw, y0 = tg.y * th;
      uint x1 = (tg.x == grid-1) ? d.x : x0 + tw, y1 = (tg.y == grid-1) ? d.y : y0 + th;
      x0 = max(x0, 1u); y0 = max(y0, 1u); x1 = min(x1, d.x-1); y1 = min(y1, d.y-1);
      float a1 = 0, a2 = 0, an = 0; uint cols = x1 > x0 ? x1 - x0 : 0; uint rows = y1 > y0 ? y1 - y0 : 0;
      for (uint k = tid; k < cols * rows; k += tn) {
        uint x = x0 + k % cols, y = y0 + k / cols, i = y*d.x+x;
        float l = a[i-d.x] + a[i+d.x] + a[i-1] + a[i+1] - 4*a[i];
        a1 += l; a2 += l*l; an += 1;
      }
      s1[tid] = a1; ss[tid] = a2; sn[tid] = an;
      threadgroup_barrier(mem_flags::mem_threadgroup);
      for (uint o = tn/2; o > 0; o >>= 1) { if (tid < o) { s1[tid] += s1[tid+o]; ss[tid] += ss[tid+o]; sn[tid] += sn[tid+o]; } threadgroup_barrier(mem_flags::mem_threadgroup); }
      if (tid == 0) { uint j = tg.y*grid+tg.x; out[j*3] = s1[0]; out[j*3+1] = ss[0]; out[j*3+2] = sn[0]; }
    }
    """
    do {
        let lib = try device.makeLibrary(source: src, options: nil)
        func pipe(_ n: String) throws -> MTLComputePipelineState { try device.makeComputePipelineState(function: lib.makeFunction(name: n)!) }
        let (pStab, pBx, pBy, pTiles) = (try pipe("stab"), try pipe("blurx"), try pipe("blury"), try pipe("tiles"))
        t = DispatchTime.now()
        let queue = device.makeCommandQueue()!
        let n = gw * gh * 4
        let bG = green.withUnsafeBytes { device.makeBuffer(bytes: $0.baseAddress!, length: n, options: .storageModeShared)! }
        let (bA, bB) = (device.makeBuffer(length: n, options: .storageModePrivate)!, device.makeBuffer(length: n, options: .storageModePrivate)!)
        let bOut = device.makeBuffer(length: grid * grid * 3 * 4, options: .storageModeShared)!
        var d = SIMD2<UInt32>(UInt32(gw), UInt32(gh)); var gr = UInt32(grid)
        let cb = queue.makeCommandBuffer()!, e = cb.makeComputeCommandEncoder()!
        let tgs = MTLSize(width: 16, height: 16, depth: 1), all = MTLSize(width: gw, height: gh, depth: 1)
        for (p, i, o) in [(pStab, bG, bA), (pBx, bA, bB), (pBy, bB, bA)] {
            e.setComputePipelineState(p); e.setBuffer(i, offset: 0, index: 0); e.setBuffer(o, offset: 0, index: 1)
            e.setBytes(&d, length: 8, index: 2); e.dispatchThreads(all, threadsPerThreadgroup: tgs)
        }
        e.setComputePipelineState(pTiles); e.setBuffer(bA, offset: 0, index: 0); e.setBuffer(bOut, offset: 0, index: 1)
        e.setBytes(&d, length: 8, index: 2); e.setBytes(&gr, length: 4, index: 3)
        e.dispatchThreadgroups(MTLSize(width: grid, height: grid, depth: 1), threadsPerThreadgroup: MTLSize(width: 256, height: 1, depth: 1))
        e.endEncoding(); cb.commit(); cb.waitUntilCompleted()

        let o = bOut.contents().bindMemory(to: Float.self, capacity: grid * grid * 3)
        var tiles = [Double](repeating: 0, count: grid * grid)
        for j in 0..<(grid * grid) {
            let (s, s2, nn) = (Double(o[j*3]), Double(o[j*3+1]), Double(o[j*3+2]))
            tiles[j] = nn > 0 ? (s2 / nn - (s / nn) * (s / nn)) * 1e6 : 0
        }
        let floor = tiles.sorted()[tiles.count / 2]
        let above = tiles.map { max($0 - floor, 0) }
        let peak = above.max() ?? 0
        // Sharpness where the subject is: the mean above-floor score of the
        // tiles under each face. The green plane is already upright and
        // cropped to the picture, so a box maps straight onto the grid.
        let subjects: [[String: Any]] = faceObs.map { f in
            let b = box(f.boundingBox)
            let clampT = { (v: Double) in min(grid - 1, max(0, Int(v * Double(grid)))) }
            let (tx0, tx1, ty0, ty1) = (clampT(b[0]), clampT(b[0] + b[2]), clampT(b[1]), clampT(b[1] + b[3]))
            var sum = 0.0, cnt = 0.0
            for ty in ty0...ty1 { for tx in tx0...tx1 { sum += above[ty * grid + tx]; cnt += 1 } }
            let s = sum / max(cnt, 1)
            return ["kind": "face", "box": b, "sharpness": r3(s), "ratio": r3(floor > 0 ? s / floor : 0)]
        }
        sharpness = [
            "method": "green-stab-v1", "grid": grid, "tiles": tiles.map(r3), "floor": r3(floor),
            "peak": r3(peak), "focus_ratio": r3(floor > 0 ? peak / floor : 0), "subjects": subjects,
        ] as [String: Any]
        timings["sharpness"] = ms(t)
    } catch {
        // No sharpness rather than no result: comp-media computes it on the CPU.
        FileHandle.standardError.write("comp-media-apple: Metal sharpness failed (\(error)); leaving it to the CPU\n".data(using: .utf8)!)
    }
}

let report: [String: Any] = [
    "develop": developer,
    "renditions": renditions,
    "sharpness": sharpness,
    "vision": vision,
    "timings_ms": timings,
]
let json = try! JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
FileHandle.standardOutput.write(json)
FileHandle.standardOutput.write("\n".data(using: .utf8)!)
