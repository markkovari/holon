const std = @import("std");

pub fn build(b: *std.Build) void {
    // Standard optimization options
    const optimize = b.standardOptimizeOption(.{});
    _ = optimize;

    // ------------------------------------------------------------------------
    // Step: test (runs parallel nextest across workspaces)
    // ------------------------------------------------------------------------
    const test_step = b.step("test", "Run all tests across workspaces in parallel via cargo-nextest");

    // 1. CLI tests
    const nextest_cli = b.addSystemCommand(&.{
        "cargo",
        "nextest",
        "run",
        "--config-file",
        ".config/nextest.toml",
        "--manifest-path",
        "cli/Cargo.toml",
    });
    test_step.dependOn(&nextest_cli.step);

    // 2. Host tests
    const nextest_host = b.addSystemCommand(&.{
        "cargo",
        "nextest",
        "run",
        "--config-file",
        ".config/nextest.toml",
        "--manifest-path",
        "host/Cargo.toml",
    });
    test_step.dependOn(&nextest_host.step);

    // 3. Lattice tests
    const nextest_lattice = b.addSystemCommand(&.{
        "cargo",
        "nextest",
        "run",
        "--config-file",
        ".config/nextest.toml",
        "--manifest-path",
        "lattice/Cargo.toml",
    });
    test_step.dependOn(&nextest_lattice.step);

    // 4. Reconciler library & gate unit tests
    const nextest_rec = b.addSystemCommand(&.{
        "cargo",
        "nextest",
        "run",
        "--config-file",
        ".config/nextest.toml",
        "--manifest-path",
        "reconciler/Cargo.toml",
        "--lib",
    });
    test_step.dependOn(&nextest_rec.step);

    // ------------------------------------------------------------------------
    // Step: test-fast (Instant local feedback for quick edit cycles)
    // ------------------------------------------------------------------------
    const test_fast_step = b.step("test-fast", "Run fast unit tests (CLI, host, lattice)");
    test_fast_step.dependOn(&nextest_cli.step);
    test_fast_step.dependOn(&nextest_host.step);
    test_fast_step.dependOn(&nextest_lattice.step);

    // ------------------------------------------------------------------------
    // Step: compose-grocery (Programmatic pipeline for grocery app)
    // ------------------------------------------------------------------------
    const compose_grocery_step = b.step("compose-grocery", "Build grocery UI and compose grocery-domain WASM component");
    const ui_build = b.addSystemCommand(&.{
        "npm",
        "--prefix",
        "examples/grocery/ui",
        "run",
        "build",
    });

    const wasm_build = b.addSystemCommand(&.{
        "cargo",
        "build",
        "--manifest-path",
        "components/Cargo.toml",
        "--release",
        "--target",
        "wasm32-wasip2",
        "-p",
        "grocery-assets",
        "-p",
        "grocery-domain",
        "-p",
        "barcode-read",
    });
    wasm_build.step.dependOn(&ui_build.step);

    const comp_plug = b.addSystemCommand(&.{
        "cargo",
        "run",
        "--manifest-path",
        "reconciler/Cargo.toml",
        "--release",
        "--bin",
        "comp-plug",
        "--",
        "grocery-domain",
        "--dir",
        "components/target/grocery-override",
        "--out",
        "components/target/composed",
    });
    comp_plug.step.dependOn(&wasm_build.step);
    compose_grocery_step.dependOn(&comp_plug.step);

    // ------------------------------------------------------------------------
    // Step: check (Fast workspace check)
    // ------------------------------------------------------------------------
    const check_step = b.step("check", "Run cargo check across grocery domain and wasip2 components");
    const check_cmd = b.addSystemCommand(&.{
        "cargo",
        "check",
        "--manifest-path",
        "components/Cargo.toml",
        "--target",
        "wasm32-wasip2",
        "-p",
        "grocery-domain",
    });
    check_step.dependOn(&check_cmd.step);
}
