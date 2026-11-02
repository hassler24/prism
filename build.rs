use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/prism.ico");
    println!("cargo:rerun-if-changed=prism.rc");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }

    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found; skipping EXE icon and version info");
        return;
    };

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("prism.res");
    let status = Command::new(&rc)
        .current_dir(&manifest)
        .arg("/nologo")
        .arg("/fo")
        .arg(&out)
        .arg("prism.rc")
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-arg-bins={}", out.display());
        }
        Ok(s) => println!("cargo:warning=rc.exe exited with {s}; icon not embedded"),
        Err(e) => println!("cargo:warning=could not run rc.exe: {e}; icon not embedded"),
    }
}

fn find_rc() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RC") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Ok(out) = Command::new("where.exe").arg("rc.exe").output() {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let path = PathBuf::from(line.trim());
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }

    let kits = Path::new(&std::env::var_os("ProgramFiles(x86)")?)
        .join("Windows Kits")
        .join("10")
        .join("bin");
    let mut candidates: Vec<PathBuf> = fs::read_dir(kits)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path().join("x64").join("rc.exe"))
        .filter(|p| p.is_file())
        .collect();
    candidates.sort();
    candidates.pop()
}
