//! Builds moonlight-common-c (plus Moonlight's ENet fork and nanors) as upstream's CMakeLists
//! does, minus PlatformCrypto.c: src/crypto.rs implements those functions.

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/moonlight-common-c");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let windows = target == "windows";
    let mut b = cc::Build::new();
    b.include(root.join("src")).include(root.join("enet/include")).include(root.join("nanors")).include(root.join("nanors/deps")).include(root.join("nanors/deps/obl"));
    b.define("NDEBUG", None).define("HAS_SOCKLEN_T", "1").std("c11").warnings(false);
    if windows {
        b.define("_WIN32_WINNT", "0x0A00");
        // MSVC's qos2.h has these types, MinGW's does not (enet/win32.c defines them then)
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            b.define("HAS_QOS_FLOWID", "1").define("HAS_PQOS_FLOWID", "1");
        }
    } else {
        for d in ["HAS_FCNTL", "HAS_IOCTL", "HAS_POLL", "HAS_GETADDRINFO", "HAS_GETNAMEINFO", "HAS_INET_PTON", "HAS_INET_NTOP", "HAS_MSGHDR_FLAGS", "HAS_CLOCK_GETTIME"] {
            b.define(d, "1");
        }
        b.define("_GNU_SOURCE", None);
    }
    for f in std::fs::read_dir(root.join("src")).unwrap() {
        let p = f.unwrap().path();
        if p.extension().is_some_and(|e| e == "c") && p.file_name().unwrap() != "PlatformCrypto.c" {
            println!("cargo:rerun-if-changed={}", p.display());
            b.file(p);
        }
    }
    for f in ["callbacks.c", "compress.c", "host.c", "list.c", "packet.c", "peer.c", "protocol.c", if windows { "win32.c" } else { "unix.c" }] {
        b.file(root.join("enet").join(f));
    }
    for f in ["nanors/rs.c", "nanors/deps/obl/oblas_common.c", "nanors/deps/obl/oblas_lite.c"] {
        b.file(root.join(f));
    }
    b.compile("moonlight-common-c");
    if windows {
        for l in ["ws2_32", "winmm"] {
            println!("cargo:rustc-link-lib={l}");
        }
    }
    println!("cargo:include={}", root.join("src").display());
}
