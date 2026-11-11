// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0

use aptu_coder_core::graph::StructuralGraph;
use criterion::{Criterion, criterion_group, criterion_main};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use tokio_util::sync::CancellationToken;

fn overview_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("overview");
    group.sample_size(10);

    group.bench_function("analyze_directory_src", |b| {
        b.iter(|| {
            let path = std::hint::black_box(Path::new("src"));
            let entries = aptu_coder_core::traversal::walk_directory(path, None).unwrap();
            let progress = Arc::new(AtomicUsize::new(0));
            let ct = CancellationToken::new();

            aptu_coder_core::analyze::analyze_directory_with_progress(path, entries, progress, ct)
        });
    });

    group.finish();
}

fn file_details_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("file_details");
    group.sample_size(10);

    group.bench_function("analyze_file_lib_rs", |b| {
        b.iter(|| {
            let path = std::hint::black_box("src/lib.rs");
            let ast_recursion_limit = std::hint::black_box(None);

            aptu_coder_core::analyze::analyze_file(path, ast_recursion_limit)
        });
    });

    group.finish();
}

fn subtree_count_overhead(c: &mut Criterion) {
    use std::fs;
    use tempfile::TempDir;

    // Create fixture: root/ with 3 levels and 120 files (5 * 4 * 6)
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    for i in 0..5usize {
        for j in 0..4usize {
            let subsub = root.join(format!("sub{}", i)).join(format!("subsub{}", j));
            fs::create_dir_all(&subsub).unwrap();
            for k in 0..6usize {
                fs::write(subsub.join(format!("file{}.rs", k)), b"fn main() {}").unwrap();
            }
        }
    }

    let mut group = c.benchmark_group("subtree_count_overhead");
    group.sample_size(10);

    group.bench_function("baseline_walk_only", |b| {
        b.iter(|| {
            let entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            std::hint::black_box(entries)
        })
    });

    group.bench_function("with_single_walk_and_count", |b| {
        b.iter(|| {
            // Single unbounded walk; compute counts in-memory; filter for bounded subset.
            let all_entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            let counts = aptu_coder_core::traversal::subtree_counts_from_entries(
                std::hint::black_box(root),
                &all_entries,
            );
            let bounded: Vec<_> = all_entries.into_iter().filter(|e| e.depth <= 2).collect();
            std::hint::black_box((bounded, counts))
        })
    });

    group.finish();
    // Keep dir alive until benchmarks are done
    drop(dir);
}

fn subtree_count_overhead_500(c: &mut Criterion) {
    use std::fs;
    use tempfile::TempDir;

    // Create fixture: 3 directory levels deep; files sit at depth 4 (root=0, sub=1, subsub=2, subsubsub=3, file=4).
    // Total: 5 * 5 * 4 * 5 = 500 files.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    for i in 0..5usize {
        for j in 0..5usize {
            for k in 0..4usize {
                let subdir = root
                    .join(format!("sub{}", i))
                    .join(format!("subsub{}", j))
                    .join(format!("subsubsub{}", k));
                fs::create_dir_all(&subdir).unwrap();
                for m in 0..5usize {
                    fs::write(subdir.join(format!("file{}.rs", m)), b"fn main() {}").unwrap();
                }
            }
        }
    }

    let mut group = c.benchmark_group("subtree_count_overhead_500");
    group.sample_size(10);

    group.bench_function("baseline_walk_only", |b| {
        b.iter(|| {
            let entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            std::hint::black_box(entries)
        })
    });

    group.bench_function("with_single_walk_and_count", |b| {
        b.iter(|| {
            // Single unbounded walk; compute counts in-memory.
            // Both this and baseline_walk_only do an unbounded walk; the only difference is the counting step.
            let all_entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            let counts = aptu_coder_core::traversal::subtree_counts_from_entries(
                std::hint::black_box(root),
                &all_entries,
            );
            std::hint::black_box((all_entries, counts))
        })
    });

    group.finish();
    // Keep dir alive until benchmarks are done
    drop(dir);
}

fn subtree_count_overhead_1000(c: &mut Criterion) {
    use std::fs;
    use tempfile::TempDir;

    // Create fixture: 5 * 5 * 5 * 8 = 1000 files.
    // Directory structure: root/sub{0-4}/subsub{0-4}/subsubsub{0-4}/file{0-7}.rs
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    for i in 0..5usize {
        for j in 0..5usize {
            for k in 0..5usize {
                let subdir = root
                    .join(format!("sub{}", i))
                    .join(format!("subsub{}", j))
                    .join(format!("subsubsub{}", k));
                fs::create_dir_all(&subdir).unwrap();
                for m in 0..8usize {
                    fs::write(subdir.join(format!("file{}.rs", m)), b"fn main() {}").unwrap();
                }
            }
        }
    }

    let mut group = c.benchmark_group("subtree_count_overhead_1000");
    group.sample_size(10);

    group.bench_function("baseline_walk_only_1000", |b| {
        b.iter(|| {
            let entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            std::hint::black_box(entries)
        })
    });

    group.bench_function("with_single_walk_and_count_1000", |b| {
        b.iter(|| {
            // Single unbounded walk; compute counts in-memory.
            let all_entries = aptu_coder_core::traversal::walk_directory(
                std::hint::black_box(root),
                std::hint::black_box(None),
            )
            .unwrap();
            let counts = aptu_coder_core::traversal::subtree_counts_from_entries(
                std::hint::black_box(root),
                &all_entries,
            );
            std::hint::black_box((all_entries, counts))
        })
    });

    group.finish();
    // Keep dir alive until benchmarks are done
    drop(dir);
}

fn analyze_module_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("analyze_module");
    group.sample_size(10);

    group.bench_function("analyze_module_file_lib_rs", |b| {
        b.iter(|| {
            let path = std::hint::black_box("src/lib.rs");
            aptu_coder_core::analyze::analyze_module_file(path)
        });
    });

    group.finish();
}

fn analyze_directory_depth_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("analyze_directory_depth");
    group.sample_size(10);

    // Benchmark max_depth=1 at repo root (sync walker path)
    group.bench_function("depth_1_repo_root", |b| {
        b.iter(|| {
            let path = std::hint::black_box(Path::new("."));
            let entries =
                aptu_coder_core::traversal::walk_directory(path, std::hint::black_box(Some(1)))
                    .unwrap();
            let progress = Arc::new(AtomicUsize::new(0));
            let ct = CancellationToken::new();

            aptu_coder_core::analyze::analyze_directory_with_progress(path, entries, progress, ct)
        });
    });

    // Benchmark max_depth=2 at repo root (parallel walker path for comparison)
    group.bench_function("depth_2_repo_root", |b| {
        b.iter(|| {
            let path = std::hint::black_box(Path::new("."));
            let entries =
                aptu_coder_core::traversal::walk_directory(path, std::hint::black_box(Some(2)))
                    .unwrap();
            let progress = Arc::new(AtomicUsize::new(0));
            let ct = CancellationToken::new();

            aptu_coder_core::analyze::analyze_directory_with_progress(path, entries, progress, ct)
        });
    });

    group.finish();
}

fn structural_graph_query_benchmark(c: &mut Criterion) {
    let root = Path::new("src");
    let entries = aptu_coder_core::traversal::walk_directory(root, None).unwrap();

    let file_outputs: Vec<_> = entries
        .iter()
        .filter(|e| !e.is_dir && e.path.extension().is_some_and(|ext| ext == "rs"))
        .map(|e| aptu_coder_core::analyze::analyze_file(e.path.to_str().unwrap(), None).unwrap())
        .collect();

    let graph = std::sync::Arc::new(StructuralGraph::build_from_analysis(&file_outputs));

    let mut group = c.benchmark_group("structural_graph_query");
    group.sample_size(10);

    group.bench_function("render_subgraph_text", |b| {
        let seed_nodes = graph
            .blast_radius_subgraph("analyze_directory", 3)
            .map(|(nodes, _)| nodes)
            .unwrap_or_default();
        b.iter(|| graph.render_subgraph_text(std::hint::black_box(&seed_nodes)))
    });

    group.bench_function("blast_radius_bidirectional", |b| {
        let seeds = graph.find_symbols(&["analyze_directory", "analyze_file"]);
        b.iter(|| {
            graph.blast_radius_bidirectional(
                std::hint::black_box(&seeds),
                std::hint::black_box(50),
                std::hint::black_box(3),
            )
        })
    });

    group.finish();
}

fn cache_paths_benchmark(c: &mut Criterion) {
    use aptu_coder_core::cache::DirectoryCacheKey;
    use aptu_coder_core::types::AnalysisMode;

    let root = Path::new("src");
    let entries = aptu_coder_core::traversal::walk_directory(root, None).unwrap();
    // AnalysisMode is #[non_exhaustive]; deserialize the Overview variant.
    let mode: AnalysisMode =
        serde_json::from_str("\"overview\"").expect("valid AnalysisMode variant");

    let mut group = c.benchmark_group("cache_paths");
    group.sample_size(10);

    // Directory key build: parallel map + sort over walk entries.
    group.bench_function("directory_key_build", |b| {
        b.iter(|| {
            DirectoryCacheKey::from_entries(
                std::hint::black_box(&entries),
                None,
                mode.clone(),
                None,
            )
        });
    });

    // Disk key: blake3 streaming over the sorted key fields.
    let key = DirectoryCacheKey::from_entries(&entries, None, mode, None);
    group.bench_function("directory_disk_key_hash", |b| {
        b.iter(|| key.disk_key(root, &entries));
    });

    // File-hash path: single read + blake3 (analyze_file L2 key).
    group.bench_function("file_read_hash", |b| {
        b.iter(|| {
            let bytes = std::fs::read("src/lib.rs").unwrap_or_default();
            blake3::hash(std::hint::black_box(&bytes))
        });
    });

    // Symbol put path: blake3 over stored formatted text.
    let formatted = String::from_utf8(std::fs::read("src/lib.rs").unwrap_or_default()).unwrap();
    group.bench_function("symbol_output_hash", |b| {
        b.iter(|| blake3::hash(std::hint::black_box(formatted.as_bytes())))
    });

    group.finish();
}

criterion_group!(
    benches,
    overview_benchmark,
    file_details_benchmark,
    subtree_count_overhead,
    subtree_count_overhead_500,
    subtree_count_overhead_1000,
    analyze_module_benchmark,
    analyze_directory_depth_benchmark,
    structural_graph_query_benchmark,
    cache_paths_benchmark
);
criterion_main!(benches);
