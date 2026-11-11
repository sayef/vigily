//! Embeds the built web app (`dist`, from `npm run build`). Build the UI before cargo: this
//! script embeds whatever `dist` holds at build time. Without a build, a short page explains how
//! to make one.

use std::path::{Path, PathBuf};

fn main() {
    let dist = Path::new("dist");
    println!("cargo:rerun-if-changed=dist");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut files = Vec::new();
    if dist.join("index.html").is_file() {
        collect(dist, dist, &mut files);
    } else {
        let page = out.join("placeholder.html");
        std::fs::write(&page, PLACEHOLDER).unwrap();
        files.push(("index.html".to_string(), page));
    }
    files.sort();
    let mut code = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for (name, path) in files {
        let abs = path.canonicalize().unwrap();
        code.push_str(&format!(
            "    ({name:?}, include_bytes!({:?})),\n",
            abs.display().to_string()
        ));
    }
    code.push_str("];\n");
    std::fs::write(out.join("ui_assets.rs"), code).unwrap();
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            println!("cargo:rerun-if-changed={}", path.display());
            collect(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
}

const PLACEHOLDER: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>vigily</title>
<style>body{font:15px system-ui;max-width:36rem;margin:15vh auto;padding:0 1rem;color:#151719}
code{background:#eaecee;padding:.1rem .35rem;border-radius:4px}</style></head><body>
<h1>vigily</h1><p>This binary was built without the web app. Build it with
<code>npm ci &amp;&amp; npm run build</code> in <code>crates/vigily-ui</code>, then rebuild vigily.
The JSON API is available under <code>/api/</code>.</p></body></html>"#;
