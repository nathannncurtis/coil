// Computes the IR schema hash from the source of ir.rs and types.rs: any
// change to the data structures changes the hash and invalidates cached IR.
use std::env;
use std::fs;
use std::path::Path;

fn fnv1a64(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &b in *part {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= 0x1f;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn main() {
    let dir = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("src");
    let ir = fs::read(dir.join("ir.rs")).expect("read src/ir.rs");
    let types = fs::read(dir.join("types.rs")).expect("read src/types.rs");
    let hash = fnv1a64(&[&ir, &types]);
    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("schema_hash.rs");
    fs::write(
        out,
        format!("pub const IR_SCHEMA_HASH: u64 = 0x{hash:016x};\n"),
    )
    .expect("write schema_hash.rs");
    println!("cargo:rerun-if-changed=src/ir.rs");
    println!("cargo:rerun-if-changed=src/types.rs");
}
