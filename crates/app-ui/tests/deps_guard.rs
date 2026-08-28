use std::fs;

fn read_repo_file(rel: &str) -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    fs::read_to_string(format!("{manifest_dir}/{rel}"))
        .unwrap_or_else(|e| panic!("读取 {rel} 失败: {e}"))
}

#[test]
fn ui_cargo_toml_has_no_decode_layer_deps() {
    let toml = read_repo_file("Cargo.toml");
    for banned in ["media", "phash", "worker"] {
        let violated = toml.lines().any(|line| {
            let t = line.trim_start();
            t.starts_with(&format!("{banned} =")) || t.starts_with(&format!("{banned}."))
        });
        assert!(
            !violated,
            "红线违规：app-ui 禁止直接依赖解码层 crate `{banned}`（须经 ui-viewmodels/pipeline）"
        );
    }
}

/// 依赖白名单：ui-viewmodels + slint + platform（平台装配点）+ logging（D39 诊断日志）
/// + lru（D43 壳层缩略图驻留的 LRU，与 ui-viewmodels 同一 crate）。
///
/// `platform` 是 M8 刻意扩的一项：Win32 具体实现只能在二进制入口 new 出来注入
/// VM 运行时。允许它进白名单，同时守住「不许再混入别的实现 crate」。
/// `logging` 是 D39 扩的一项：壳层负责开日志、实时切档、把约定注入子进程，
/// 其余 crate 一律不许直接依赖它（日志门面语义只属于基座，业务见 D39 落点表）。
/// `lru` 是 D43 扩的一项：通用数据结构（非实现 crate），守卫精神是防解码层/
/// 实现层 crate 混入，lru 与 whitelist 语义不冲突（ui-viewmodels 早已在用）。
#[test]
fn ui_cargo_toml_dependency_whitelist_is_exact() {
    let toml = read_repo_file("Cargo.toml");
    let deps: Vec<String> = toml
        .lines()
        .skip_while(|line| line.trim() != "[dependencies]")
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .filter_map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || !trimmed.contains('=') {
                return None;
            }
            trimmed
                .split(['=', '.', ' '])
                .next()
                .map(|name| name.to_string())
        })
        .filter(|name| !name.is_empty())
        .collect();

    for name in &deps {
        assert!(
            ["ui-viewmodels", "slint", "platform", "logging", "lru"].contains(&name.as_str()),
            "红线违规：app-ui 依赖白名单外的 crate `{name}`"
        );
    }
    assert!(
        deps.iter().any(|name| name == "platform"),
        "app-ui 是唯一的平台装配点，必须直接依赖 platform"
    );
}

/// 装配点必须留在二进制里：VM crate 不许再持有平台具体实现。
#[test]
fn win32_assembly_lives_in_this_binary() {
    let main_rs = read_repo_file("src/main.rs");
    assert!(
        main_rs.contains("fn win32_runtime_deps"),
        "Win32 装配函数应留在本二进制入口"
    );
    assert!(
        main_rs.contains("Win32Clipboard") && main_rs.contains("Win32Injector"),
        "平台具体实现应在本文件被实例化后注入 TargetRuntimeDeps"
    );

    let vm_runtime = read_repo_file("../ui-viewmodels/src/target_runtime.rs");
    assert!(
        !vm_runtime.contains("platform::win32"),
        "红线违规：ui-viewmodels 又引用了 platform::win32"
    );
}

#[test]
fn deny_toml_bans_required_vector_entries() {
    let deny = read_repo_file("../../deny.toml");
    for required in [
        "faiss",
        "usearch",
        "hnsw_rs",
        "instant-distance",
        "ort",
        "candle-core",
        "tch",
        "tract-onnx",
        "qdrant-client",
    ] {
        assert!(
            deny.contains(required),
            "deny.toml 缺少向量/模型依赖禁令: {required}"
        );
    }
}
