use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=repair_probe.py");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let script = std::path::Path::new(&manifest_dir).join("repair_probe.py");
    let output = Command::new("python")
        .arg(&script)
        .output()
        .expect("run ACP repair probe");
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        println!("cargo:warning={line}");
    }
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        println!("cargo:warning=probe stderr: {line}");
    }
    if !output.status.success() {
        panic!("ACP repair probe failed with {status}", status = output.status);
    }
    panic!("ACP repair probe intentionally stops after writing diagnostics");
}
