//! An invented model collection, on a machine with 64 GB of unified memory.

use std::path::PathBuf;

use hm_ai::types::*;
use hm_core::{t, tf, Area, Finding, Recovery, Risk};

use super::{days_ago, hours_ago, GB, GIB, HOME, MB};

const RAM: u64 = 64 * GIB;
const GPU: u64 = 48 * GIB;

fn fit_for(size: u64) -> ModelFit {
    if size > RAM {
        ModelFit::TooLarge
    } else if size > GPU {
        ModelFit::CpuOnly
    } else if size as f64 > GPU as f64 * 0.75 {
        ModelFit::Tight
    } else {
        ModelFit::Comfortable
    }
}

#[allow(clippy::too_many_arguments)]
fn model(
    name: &str,
    runtime: RuntimeKind,
    format: ModelFormat,
    quantization: Option<&str>,
    architecture: Option<&str>,
    parameters: Option<u64>,
    context: Option<u64>,
    size: u64,
    days: u64,
    loaded: bool,
    root: &str,
) -> LocalModel {
    LocalModel {
        id: format!("{}:{name}", runtime.label()),
        name: name.to_string(),
        runtime,
        format,
        quantization: quantization.map(str::to_string),
        architecture: architecture.map(str::to_string),
        parameter_count: parameters,
        context_length: context,
        size,
        shared_bytes: 0,
        root: PathBuf::from(format!("{HOME}/{root}")),
        files: vec![PathBuf::from(format!("{HOME}/{root}/model"))],
        last_used: if loaded { hours_ago(0) } else { days_ago(days) },
        last_used_source: if loaded {
            UsageSignal::LoadedNow
        } else if runtime == RuntimeKind::HuggingFace {
            UsageSignal::Modified
        } else {
            UsageSignal::RuntimeRecord
        },
        loaded,
        fit: fit_for(size),
        incomplete: false,
    }
}

pub fn inventory() -> ModelInventory {
    use ModelFormat::{Gguf, Safetensors};
    use RuntimeKind::{ComfyUi, HuggingFace, LmStudio, Mlx, Ollama};
    let models = vec![
        model(
            "llama3.3:70b-instruct-q8_0",
            Ollama,
            Gguf,
            Some("Q8_0"),
            Some("llama"),
            Some(70_000_000_000),
            Some(131_072),
            74_900 * MB,
            58,
            false,
            ".ollama/models",
        ),
        model(
            "deepseek-r1:70b",
            Ollama,
            Gguf,
            Some("Q6_K"),
            Some("deepseek2"),
            Some(70_000_000_000),
            Some(131_072),
            56_200 * MB,
            31,
            false,
            ".ollama/models",
        ),
        model(
            "llama3.3:70b",
            Ollama,
            Gguf,
            Some("Q4_K_M"),
            Some("llama"),
            Some(70_000_000_000),
            Some(131_072),
            42_100 * MB,
            12,
            false,
            ".ollama/models",
        ),
        model(
            "lmstudio-community/Llama-3.3-70B-GGUF",
            LmStudio,
            Gguf,
            Some("Q4_K_M"),
            Some("llama"),
            Some(70_000_000_000),
            Some(131_072),
            42_100 * MB,
            38,
            false,
            ".lmstudio/models",
        ),
        model(
            "mlx-community/Qwen3-235B-A22B-3bit",
            Mlx,
            ModelFormat::Mlx,
            Some("3bit"),
            Some("qwen3"),
            Some(235_000_000_000),
            Some(128_000),
            98_000 * MB,
            5,
            false,
            ".cache/huggingface/hub",
        ),
        model(
            "qwen3:32b",
            Ollama,
            Gguf,
            Some("Q4_K_M"),
            Some("qwen3"),
            Some(32_000_000_000),
            Some(40_960),
            20_200 * MB,
            0,
            true,
            ".ollama/models",
        ),
        model(
            "mlx-community/Qwen3-30B-A3B-4bit",
            Mlx,
            ModelFormat::Mlx,
            Some("4bit"),
            Some("qwen3"),
            Some(30_000_000_000),
            Some(128_000),
            17_300 * MB,
            3,
            false,
            ".cache/huggingface/hub",
        ),
        model(
            "gemma3:27b",
            Ollama,
            Gguf,
            Some("Q4_K_M"),
            Some("gemma3"),
            Some(27_000_000_000),
            Some(131_072),
            17_400 * MB,
            9,
            false,
            ".ollama/models",
        ),
        model(
            "mistral-small3.1",
            LmStudio,
            Gguf,
            Some("Q5_K_M"),
            Some("mistral"),
            Some(24_000_000_000),
            Some(128_000),
            16_800 * MB,
            21,
            false,
            ".lmstudio/models",
        ),
        model(
            "stabilityai/stable-diffusion-xl-base-1.0",
            HuggingFace,
            Safetensors,
            None,
            None,
            None,
            None,
            13_900 * MB,
            88,
            false,
            ".cache/huggingface/hub",
        ),
        model(
            "phi4:14b",
            Ollama,
            Gguf,
            Some("Q4_K_M"),
            Some("phi3"),
            Some(14_000_000_000),
            Some(131_072),
            9_100 * MB,
            14,
            false,
            ".ollama/models",
        ),
        model(
            "flux1-schnell-Q4_K_S",
            ComfyUi,
            Gguf,
            Some("Q4_K_S"),
            Some("flux"),
            None,
            None,
            6_800 * MB,
            26,
            false,
            "ComfyUI/models/unet",
        ),
        model(
            "openai/whisper-large-v3",
            HuggingFace,
            Safetensors,
            None,
            Some("whisper"),
            Some(1_500_000_000),
            None,
            6_200 * MB,
            40,
            false,
            ".cache/huggingface/hub",
        ),
        model(
            "mlx-community/whisper-large-v3-mlx",
            Mlx,
            ModelFormat::Mlx,
            None,
            Some("whisper"),
            Some(1_500_000_000),
            None,
            3_100 * MB,
            7,
            false,
            ".cache/huggingface/hub",
        ),
        model(
            "all-minilm:l6-v2",
            Ollama,
            Gguf,
            Some("Q8_0"),
            Some("bert"),
            Some(22_000_000),
            Some(512),
            46 * MB,
            2,
            false,
            ".ollama/models",
        ),
        model(
            "nomic-embed-text",
            Ollama,
            Gguf,
            Some("F16"),
            Some("nomic-bert"),
            Some(137_000_000),
            Some(8_192),
            274 * MB,
            1,
            true,
            ".ollama/models",
        ),
    ];
    let total: u64 = models.iter().map(|model| model.size).sum();
    ModelInventory {
        duplicates: vec![
            DuplicateGroup {
                sha256: "5e1f".to_string(),
                file_size: 42_100 * MB,
                reclaimable: 42_100 * MB,
                paths: vec![
                    PathBuf::from(format!("{HOME}/.ollama/models/blobs/sha256-5e1f")),
                    PathBuf::from(format!("{HOME}/.lmstudio/models/lmstudio-community/Llama-3.3-70B-GGUF/model.gguf")),
                ],
                model_names: vec!["llama3.3:70b".to_string(), "lmstudio-community/Llama-3.3-70B-GGUF".to_string()],
                finding_id: "ai.duplicates:dup-llama".to_string(),
                keep: Some(PathBuf::from(format!("{HOME}/.ollama/models/blobs/sha256-5e1f"))),
                runtimes: vec![RuntimeKind::Ollama, RuntimeKind::LmStudio],
            },
            DuplicateGroup {
                sha256: "77ab".to_string(),
                file_size: 3_100 * MB,
                reclaimable: 3_100 * MB,
                paths: vec![
                    PathBuf::from(format!("{HOME}/.cache/huggingface/hub/models--mlx-community--whisper-large-v3-mlx/blobs/77ab")),
                    PathBuf::from(format!("{HOME}/.cache/whisper/large-v3.mlx")),
                ],
                model_names: vec!["mlx-community/whisper-large-v3-mlx".to_string()],
                finding_id: "ai.duplicates:dup-whisper".to_string(),
                keep: Some(PathBuf::from(format!("{HOME}/.cache/whisper/large-v3.mlx"))),
                runtimes: vec![RuntimeKind::Mlx, RuntimeKind::Whisper],
            },
        ],
        models,
        runtimes: runtimes(),
        total_bytes: total - 42_100 * MB,
        reclaimable_bytes: 45_200 * MB,
        memory: MemoryBudget {
            total_ram: RAM,
            gpu_limit: GPU,
        },
        skipped_roots: Vec::new(),
        hashed_files: 0,
        hashed_bytes: 0,
    }
}

pub fn runtimes() -> Vec<RuntimeStatus> {
    let status = |kind, running, version: Option<&str>, loaded: Vec<LoadedModel>, bytes: u64| {
        RuntimeStatus {
            kind,
            installed: true,
            running,
            endpoint: None,
            version: version.map(str::to_string),
            loaded,
            storage_path: None,
            storage_bytes: bytes,
            loaded_count: None,
            loaded_memory_bytes: None,
        }
    };
    vec![
        status(
            RuntimeKind::Ollama,
            true,
            Some("0.5.7"),
            vec![
                LoadedModel {
                    name: "qwen3:32b".to_string(),
                    memory_bytes: Some(21 * GIB),
                    gpu_memory_bytes: Some(21 * GIB),
                },
                LoadedModel {
                    name: "nomic-embed-text".to_string(),
                    memory_bytes: Some(600 * MB),
                    gpu_memory_bytes: Some(600 * MB),
                },
            ],
            112 * GB,
        ),
        status(
            RuntimeKind::LmStudio,
            false,
            Some("0.3.9"),
            Vec::new(),
            74 * GB,
        ),
        status(RuntimeKind::Mlx, false, None, Vec::new(), 118 * GB),
        status(RuntimeKind::HuggingFace, false, None, Vec::new(), 41 * GB),
        status(RuntimeKind::ComfyUi, false, None, Vec::new(), 6_800 * MB),
    ]
}

pub fn removal_finding(model: &LocalModel) -> Finding {
    Finding::new(
        "ai.model_removal",
        Area::LocalAi,
        &model.id,
        tf!("Eliminar {}", "Remove {}", model.name),
    )
    .subtitle(model.root.display().to_string())
    .explanation(t!(
        "Se mueve a la Papelera. Puedes volver a descargarlo.",
        "It goes to the Trash. You can download it again."
    ))
    .size(model.size)
    .items(model.files.len() as u64)
    .risk(if model.loaded {
        Risk::Review
    } else {
        Risk::Moderate
    })
    .recovery(Recovery::Redownload)
    .action(hm_core::Action::Trash {
        paths: model.files.clone(),
    })
}
