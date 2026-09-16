fn main() {
    let catalog = comp_reconciler::plug::Catalog::scan(&comp_reconciler::plug::default_dirs(&comp_reconciler::fleet::repo_root()));
    if let Some(surface) = catalog.surface("vision-describe") {
        println!("vision-describe exports: {:?}", surface.exports);
    } else {
        println!("vision-describe not found in catalog");
    }
}
