use comp_reconciler::plug::{default_dirs, Catalog};
#[test]
fn sweep() {
    let cat = Catalog::scan(&default_dirs(&comp_reconciler::fleet::repo_root()));
    println!("catalogue: {} components", cat.len());
    let mut lines: Vec<String> = Vec::new();
    let names: Vec<String> = cat.names().map(String::from).collect();
    for name in &names {
        for iface in cat.unmet(name) {
            lines.push(format!("{name} -> {iface}"));
        }
    }
    println!("components with unmet imports: {}", lines.len());
    for l in &lines { println!("  {l}"); }
}
