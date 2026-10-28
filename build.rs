use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Odin コンパイラを探す: 環境変数 ODIN → PATH の odin → ame-shell のインストーラが FreeBSD に作るもの
/// (FreeBSD には Odin のパッケージが無いため)
fn find_odin() -> PathBuf {
    if let Ok(p) = env::var("ODIN") {
        return PathBuf::from(p);
    }
    if Command::new("odin").arg("version").output().is_ok() {
        return PathBuf::from("odin");
    }
    if let Ok(entries) = fs::read_dir("/usr/local/ame-toolchain") {
        for e in entries.flatten() {
            let p = e.path().join("odin");
            if e.file_name().to_string_lossy().starts_with("odin") && p.exists() {
                return p;
            }
        }
    }
    PathBuf::from("odin")
}

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let src_dir = env::current_dir().unwrap().join("src");
    let source_file = src_dir.join("braille_renderer.odin");

    // 1. Odin コンパイル実行
    let status = Command::new(find_odin())
        .current_dir(&out_dir)
        .arg("build")
        .arg(&source_file)
        .arg("-file")
        .arg("-build-mode:obj")
        .arg("-reloc-mode:pic")
        .arg("-out:braille")
        .status()
        .expect("Failed to execute odin compiler (install Odin or set ODIN=/path/to/odin)");

    if !status.success() {
        panic!("Odin compilation failed");
    }

    // 2. OUT_DIR 内にあるすべての .obj および .o ファイルを収集
    let mut obj_files = Vec::new();
    if let Ok(entries) = fs::read_dir(&out_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if ext == "obj" || ext == "o" {
                    obj_files.push(path);
                }
            }
        }
    }

    if obj_files.is_empty() {
        panic!("Odin compilation succeeded, but no .obj or .o files were found in OUT_DIR");
    }

    // 3. 検出されたすべてのオブジェクトファイルを libbraille.a にまとめ（アーカイブ）
    let lib_path = out_dir.join("libbraille.a");
    let mut ar_cmd = Command::new("ar");
    ar_cmd.arg("rcs").arg(&lib_path);
    for obj in &obj_files {
        ar_cmd.arg(obj);
    }

    let ar_status = ar_cmd.status().expect("Failed to run ar");

    if !ar_status.success() {
        panic!("Failed to archive Odin object files");
    }

    // 4. Rust リンカー設定
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=braille");
    println!("cargo:rerun-if-changed=src/braille_renderer.odin");
}
