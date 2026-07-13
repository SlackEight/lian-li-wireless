/// Compiles the vendored tinyuz C++ library (RGB payload compression —
/// the wireless device firmware decompresses this format).
fn main() {
    let vendor = std::path::Path::new("../../vendor");
    let tinyuz = vendor.join("tinyuz");
    let hdiff = vendor.join("HDiffPatch");

    let cpp_sources = [
        vendor.join("tuz_wrapper.cpp"),
        tinyuz.join("compress/tuz_enc.cpp"),
        tinyuz.join("compress/tuz_enc_private/tuz_enc_clip.cpp"),
        tinyuz.join("compress/tuz_enc_private/tuz_enc_code.cpp"),
        tinyuz.join("compress/tuz_enc_private/tuz_enc_match.cpp"),
        tinyuz.join("compress/tuz_enc_private/tuz_sstring.cpp"),
        hdiff.join("libHDiffPatch/HDiff/private_diff/libdivsufsort/divsufsort.cpp"),
    ];

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .opt_level(3)
        .define("NDEBUG", None)
        .define("_IS_USED_MULTITHREAD", "0")
        .include(vendor)
        .include(&tinyuz)
        .include(&hdiff)
        .warnings(false);

    for src in &cpp_sources {
        build.file(src);
    }

    build.compile("tinyuz");

    println!("cargo:rerun-if-changed=../../vendor/tuz_wrapper.cpp");
    println!("cargo:rerun-if-changed=../../vendor/tinyuz/compress/");
}
