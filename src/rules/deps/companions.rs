//! Dependencies consumed by an imported library rather than by its caller.

use crate::manifest::normalize_distribution_name;

// These libraries load optional backends, metrics, or plugins at runtime.
// A declaration without a direct import is not evidence the backend is unused.
const RUNTIME_PEERS: &[(&str, &[&str])] = &[
    ("python-multipart", &["starlette", "fastapi"]),
    ("accelerate", &["transformers"]),
    ("torch", &["transformers"]),
    ("sentencepiece", &["transformers"]),
    ("protobuf", &["transformers"]),
    ("timm", &["transformers"]),
    ("librosa", &["transformers", "datasets"]),
    ("torchaudio", &["transformers", "datasets"]),
    ("torchvision", &["transformers"]),
    ("kernels", &["transformers"]),
    ("huggingface-hub", &["transformers"]),
    ("scikit-learn", &["transformers", "evaluate"]),
    ("scipy", &["evaluate"]),
    ("pycocotools", &["torchmetrics"]),
    ("py7zr", &["datasets"]),
    ("jiwer", &["evaluate"]),
    ("rouge-score", &["evaluate"]),
    ("seqeval", &["evaluate"]),
    ("sacrebleu", &["evaluate"]),
    ("openai", &["inspect-ai"]),
    ("inspect-evals", &["inspect-ai"]),
];

/// Shared by project and PEP 723 scopes; an unused provider does not use its peers.
pub(super) fn companion_is_used(name: &str, used: impl Fn(&str) -> bool) -> bool {
    name.strip_prefix("types-")
        .or_else(|| name.strip_suffix("-stubs"))
        .is_some_and(|runtime| used(&normalize_distribution_name(runtime)))
        || RUNTIME_PEERS.iter().any(|(peer, providers)| {
            name == *peer && providers.iter().any(|provider| used(provider))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_providers_do_not_hide_their_peers_or_stubs() {
        for name in [
            "accelerate",
            "torch",
            "rouge-score",
            "types-requests",
            "pandas-stubs",
        ] {
            assert!(!companion_is_used(name, |_| false), "{name}");
        }
        assert!(companion_is_used("types-requests", |name| name == "requests"));
        assert!(companion_is_used("pandas-stubs", |name| name == "pandas"));
    }
}
