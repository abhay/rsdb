use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::SystemTime;

fn main() {
    println!("cargo:rerun-if-changed=../../package.json");
    println!("cargo:rerun-if-changed=../../bun.lock");
    println!("cargo:rerun-if-changed=../../tsconfig.json");
    println!("cargo:rerun-if-changed=../../web/src");
    println!("cargo:rerun-if-changed=../../web/static");
    println!("cargo:rerun-if-changed=../../web/dist/app.js");

    let bundle = Path::new("../../web/dist/app.js");
    let bundle_modified = modified_at(bundle).unwrap_or_else(|error| {
        fail(&format!(
            "{error}. Run `bun run build:web` before building with the usb feature."
        ))
    });

    for source in bundle_inputs() {
        let source_modified = modified_at(&source).unwrap_or_else(|error| fail(&error));
        if source_modified > bundle_modified {
            fail(&format!(
                "../../web/dist/app.js is older than {}. Run `bun run build:web`.",
                source.display()
            ));
        }
    }
}

fn bundle_inputs() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("../../package.json"),
        PathBuf::from("../../bun.lock"),
        PathBuf::from("../../tsconfig.json"),
    ];
    collect_files(Path::new("../../web/src"), &mut paths);
    collect_files(Path::new("../../web/static"), &mut paths);
    paths
}

fn collect_files(dir: &Path, paths: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|error| {
        fail(&format!("failed to read {}: {error}", dir.display()));
    });

    for entry in entries {
        let entry =
            entry.unwrap_or_else(|error| fail(&format!("failed to read web source: {error}")));
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, paths);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
            paths.push(path);
        }
    }
}

fn modified_at(path: &Path) -> Result<SystemTime, String> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| format!("failed to stat {}: {error}", path.display()))
}

fn fail(message: &str) -> ! {
    eprintln!("RSDB web bundle check failed: {message}");
    process::exit(1);
}
