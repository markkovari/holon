fn main() {
    let catalog = comp_reconciler::plug::Catalog::scan(&comp_reconciler::plug::default_dirs(&comp_reconciler::fleet::repo_root()));
    let package = |iface: &str| iface.split('/').next().unwrap_or(iface).to_string();
    let exported: std::collections::BTreeSet<String> = catalog
        .names()
        .filter_map(|n| catalog.surface(n))
        .flat_map(|s| s.exports.iter().map(|e| package(e)))
        .collect();
    println!("Does exported contain vision:describe? {}", exported.contains("vision:describe"));
}
