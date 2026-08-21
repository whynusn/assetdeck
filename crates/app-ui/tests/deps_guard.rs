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
