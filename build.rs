fn main() {
    // Linux: cef-dll-sys кладёт libcef.so и ресурсы рядом с бинарником
    // (target/debug или target/release). rpath=$ORIGIN позволяет
    // запускать просто `cargo run`, без LD_LIBRARY_PATH.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=ui");
}
